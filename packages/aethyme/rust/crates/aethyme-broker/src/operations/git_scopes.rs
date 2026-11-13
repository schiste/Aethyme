//! Closed Git profiles whose complete ref write footprint is known.
//!
//! Other Git spellings retain the repository-exclusive lane. Scoped profiles
//! additionally hold that same lane shared, so old/wide writers still exclude
//! them. Scope is derived from argv, never from the caller's audit label.

use super::push_and_coordinated::remove_inherited_git_config_overrides;
use super::*;

pub(super) const FETCH_FLAGS: &[&str] = &[
    "--atomic",
    "--no-tags",
    "--no-write-fetch-head",
    "--no-recurse-submodules",
    "--no-auto-maintenance",
    "--no-write-commit-graph",
    "--no-prune",
    "--no-prune-tags",
    "--refmap=",
];

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct FetchFootprint {
    pub remote: String,
    pub source_ref: String,
    pub destination_ref: String,
}

fn ref_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 1024
        && !value.starts_with('-')
        && value.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.starts_with('.')
                && !part.ends_with('.')
                && !part.ends_with(".lock")
        })
        && !value.contains("..")
        && !value.contains("@{")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'_' | b'-' | b'.'))
}

/// The one fetch spelling with a closed ref footprint: exactly
/// `refs/heads/<branch>` into `refs/remotes/<remote>/<branch>`, no tags, no
/// FETCH_HEAD, no maintenance. It takes that tracking ref's lane instead of
/// the repository-wide one when the repository's configuration proves it.
pub(crate) fn canonical_fetch_args(remote: &str, branch: &str) -> Option<Vec<String>> {
    if !ref_component(remote) || remote.contains('/') || !ref_component(branch) {
        return None;
    }
    let mut args = vec!["fetch".to_owned()];
    args.extend(FETCH_FLAGS.iter().map(|flag| (*flag).to_owned()));
    args.extend([
        "--".into(),
        remote.into(),
        format!("refs/heads/{branch}:refs/remotes/{remote}/{branch}"),
    ]);
    Some(args)
}

pub(super) fn fetch_footprint(args: &[String]) -> Option<FetchFootprint> {
    let [command, tail @ ..] = args else {
        return None;
    };
    if command != "fetch" || tail.len() != FETCH_FLAGS.len() + 3 {
        return None;
    }
    for (actual, required) in tail.iter().zip(FETCH_FLAGS) {
        if actual != required {
            return None;
        }
    }
    let operands = &tail[FETCH_FLAGS.len()..];
    if operands[0] != "--" {
        return None;
    }
    let remote = &operands[1];
    let (source, destination) = operands[2].split_once(':')?;
    let branch = source.strip_prefix("refs/heads/")?;
    if canonical_fetch_args(remote, branch)?.as_slice() != args {
        return None;
    }
    Some(FetchFootprint {
        remote: remote.clone(),
        source_ref: source.into(),
        destination_ref: destination.into(),
    })
}

pub(super) fn resource_scope(args: &[String]) -> Option<String> {
    if let Some(fetch) = fetch_footprint(args) {
        return Some(format!("git-ref/{}", fetch.source_ref));
    }
    strict_push(args).map(|push| format!("git-ref/refs/heads/{}", push.branch))
}

/// The one push spelling with a closed ref footprint: an exact object ID to
/// one branch, no implicit tags, options ended before the remote.
struct StrictPush<'a> {
    remote: &'a str,
    branch: &'a str,
}

impl StrictPush<'_> {
    /// A literal URL selects an anonymous remote, which updates no local ref.
    fn named_remote(&self) -> bool {
        !(self.remote.starts_with("https://") || self.remote.starts_with("ssh://"))
    }
}

fn strict_push(args: &[String]) -> Option<StrictPush<'_>> {
    // A literal URL updates no local ref. A configured remote name also
    // updates exactly `refs/remotes/<name>/<branch>`, provided its fetch
    // refmap is the standard one, which `validate_repository` proves under
    // the lock; that ref is then locked like a fetch destination. Mutable
    // source names, implicit tags and multi-ref pushes stay wide.
    let [command, no_tags, porcelain, separator, remote, refspec] = args else {
        return None;
    };
    if command != "push"
        || no_tags != "--no-follow-tags"
        || porcelain != "--porcelain"
        || separator != "--"
        || !(remote.starts_with("https://")
            || remote.starts_with("ssh://")
            || (ref_component(remote) && !remote.contains('/')))
    {
        return None;
    }
    let (source, destination) = refspec.split_once(':')?;
    if source.len() != 40 || !source.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let branch = destination.strip_prefix("refs/heads/")?;
    ref_component(branch).then_some(StrictPush {
        remote: remote.as_str(),
        branch,
    })
}

/// Exact spelling `broker push` uses for a session branch when the strict
/// profile is available.
pub(crate) fn strict_push_args(remote: &str, source: &str, branch_ref: &str) -> Vec<String> {
    vec![
        "push".into(),
        "--no-follow-tags".into(),
        "--porcelain".into(),
        "--".into(),
        remote.into(),
        format!("{source}:{branch_ref}"),
    ]
}

/// The strict spelling of `push <remote> <sha>:refs/heads/<branch>`, the
/// spelling `broker push` falls back to when the strict profile is refused.
pub(super) fn strict_equivalent_of_plain_push(args: &[String]) -> Option<Vec<String>> {
    let [command, remote, refspec] = args else {
        return None;
    };
    if command != "push" {
        return None;
    }
    let (source, destination) = refspec.split_once(':')?;
    let strict = strict_push_args(remote, source, destination);
    strict_push(&strict).is_some().then_some(strict)
}

/// The branch a strict push writes, as `refs/heads/<branch>`.
pub(super) fn strict_push_destination(args: &[String]) -> Option<String> {
    strict_push(args).map(|push| format!("refs/heads/{}", push.branch))
}

/// The one local ref a strictly profiled operation writes besides its lane
/// ref: a fetch's tracking destination, or the tracking ref a push to a
/// configured remote name updates. Two operations writing the same local ref
/// serialise on it even when they reach the remote through different keys.
pub(super) fn local_destination_ref(args: &[String]) -> Option<String> {
    if let Some(fetch) = fetch_footprint(args) {
        return Some(fetch.destination_ref);
    }
    strict_push(args)
        .filter(|push| push.named_remote())
        .map(|push| format!("refs/remotes/{}/{}", push.remote, push.branch))
}

/// Recorded-scope prefix of a commit admitted to its session's branch lane.
///
/// Reserved: callers cannot declare it (admission refuses), so a record that
/// carries it was derived by the broker after proving the commit ran in the
/// session's own linked worktree on the session's own branch.
pub(super) const SESSION_COMMIT_SCOPE_PREFIX: &str = "session-commit:";

/// Whether `args` is a plain `git commit` with no leading global options.
/// Global options (`-C`, `--git-dir`, `-c`) could move the commit elsewhere.
pub(super) fn is_plain_commit(args: &[String]) -> bool {
    args.first().is_some_and(|command| command == "commit")
}

/// The audit scope recorded for a session-branch commit.
pub(super) fn session_commit_record_scope(branch_ref: &str) -> String {
    format!("{SESSION_COMMIT_SCOPE_PREFIX}{branch_ref}")
}

/// The lane a journaled session-branch commit holds, recomputed from its
/// record. `None` keeps any other record repository-wide.
pub(super) fn recorded_session_commit_scope(args: &[String], scope: &str) -> Option<String> {
    if !is_plain_commit(args) {
        return None;
    }
    let branch_ref = scope.strip_prefix(SESSION_COMMIT_SCOPE_PREFIX)?;
    let branch = branch_ref.strip_prefix("refs/heads/")?;
    ref_component(branch).then(|| format!("git-ref/{branch_ref}"))
}

/// Prove a commit is confined to `session_branch` in the session's own linked
/// worktree, and return the branch ref it writes.
///
/// A commit writes the checked-out branch ref, that worktree's index and
/// `HEAD`, and objects. Objects, `packed-refs` and `gc --auto` are protected
/// by Git's own lock files. Anything not proven here keeps the
/// repository-wide lane: the main checkout, a detached or foreign `HEAD`, a
/// closed session, or an ambient override that could reroute the repository.
pub(super) fn validate_session_commit(
    cwd: &Path,
    main_root: &Path,
    session_worktree: &Path,
    session_branch: &str,
    deadline: AdmissionDeadline,
    repository: &str,
) -> Result<String, BrokerOpError> {
    for name in GIT_REPOSITORY_ROUTING_ENV_NAMES {
        if std::env::var_os(name).is_some() {
            return Err(unavailable("ambient Git routing override"));
        }
    }
    if !ref_component(session_branch) {
        return Err(unavailable("session branch is not a plain branch name"));
    }
    let canonical = |path: &Path| std::fs::canonicalize(path).ok();
    let checkout = crate::GitRepo::discover(cwd)?;
    let root = canonical(checkout.root());
    if root.is_none() || root != canonical(session_worktree) {
        return Err(unavailable(
            "the commit does not run in the session's worktree",
        ));
    }
    if root == canonical(main_root) {
        return Err(unavailable(
            "a main-checkout commit keeps the repository lane",
        ));
    }
    if common_lock_key(cwd)? != common_lock_key(main_root)? {
        return Err(unavailable("the worktree belongs to another repository"));
    }
    // As for fetches and pushes: a `reference-transaction` hook sees and can
    // extend every ref update, so it keeps the commit repository-wide.
    refuse_hooks(
        cwd,
        &common_directory(cwd)?,
        &["reference-transaction"],
        deadline,
        repository,
    )?;
    let head = probe(cwd, &["symbolic-ref", "-q", "HEAD"], deadline, repository)?;
    let branch_ref = format!("refs/heads/{session_branch}");
    if !head.status.success() || head.stdout.trim_ascii() != branch_ref.as_bytes() {
        return Err(unavailable("HEAD is not the session's own branch"));
    }
    Ok(branch_ref)
}

pub(super) fn scopes_conflict(
    _provider: OperationProvider,
    left: Option<&str>,
    right: Option<&str>,
) -> bool {
    if left.is_some_and(|scope| scope.starts_with("git-ref/"))
        || right.is_some_and(|scope| scope.starts_with("git-ref/"))
    {
        // A legacy/wide unknown operation may have touched any Git resource.
        // Narrowing a new command cannot evade that existing uncertainty.
        left.is_none() || right.is_none() || left == right
    } else {
        left == right
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FetchJournal {
    pub schema_version: u32,
    pub operation_id: i64,
    pub repository: String,
    pub footprint: FetchFootprint,
    pub common_directory: PathBuf,
    pub common_device: u64,
    pub common_inode: u64,
    pub pre_destination: Option<String>,
    pub provider: Option<crate::file_lock::artifact_use::ProcessStartIdentity>,
    pub completion: Option<FetchCompletion>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FetchCompletion {
    pub pid: u32,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub observed_at: i64,
}

const UNAVAILABLE_PREFIX: &str = "strict Git resource profile unavailable: ";

fn unavailable(reason: &str) -> BrokerOpError {
    BrokerOpError::InvalidCoordinatedOperation {
        reason: format!("{UNAVAILABLE_PREFIX}{reason}; use the repository-wide spelling"),
    }
}

/// `push.followTags` is set: the strict spelling would stop following tags.
pub(crate) const LANE_PUSH_FOLLOWS_TAGS: &str = "push_follow_tags";
/// `coordination.session_hooks_hold_repository_lock = true`, and a hook the
/// opt-out names exists (or the operation is a commit).
pub(crate) const LANE_SESSION_HOOKS_HOLD_LOCK: &str = "session_hooks_hold_repository_lock";
/// The caller declared `--scope`; a session commit lane is derived, never declared.
pub(crate) const LANE_DECLARED_SCOPE: &str = "declared_scope";
/// The operation's session is missing or closed.
pub(crate) const LANE_NO_LIVE_SESSION: &str = "no_live_session";
/// The argv is not one of the closed profiles (`unrecognized resource profile`).
pub(crate) const LANE_NOT_STRICT_SPELLING: &str = "not_strict_spelling";
/// A `--force-with-lease` replacement: never a closed profile.
pub(crate) const LANE_FORCE_WITH_LEASE: &str = "force_with_lease_push";
/// A push that writes a branch other than the caller's own session branch.
pub(crate) const LANE_NOT_SESSION_BRANCH: &str = "not_session_branch";
/// The admission budget ran out while proving the footprint.
pub(crate) const LANE_ADMISSION_TIMED_OUT: &str = "admission_timed_out";
/// A probe failed for a reason that is not one of the coded refusals.
pub(crate) const LANE_PROBE_FAILED: &str = "probe_failed";
/// A refusal whose text is not in [`LANE_REASONS`]; a test keeps this empty.
pub(crate) const LANE_UNCLASSIFIED: &str = "unclassified";

/// Stable reason codes for every refusal [`unavailable`] can produce, keyed by
/// the start of its human detail. Codes are recorded in operation journals and
/// aggregated by `operations stats`, so they are never renamed.
const LANE_REASONS: &[(&str, &str)] = &[
    ("ambient Git routing", "ambient_git_override"),
    (
        "session branch is not a plain branch name",
        "session_branch_not_plain",
    ),
    (
        "the commit does not run in the session's worktree",
        "commit_outside_session_worktree",
    ),
    ("a main-checkout commit", "main_checkout_commit"),
    (
        "the worktree belongs to another repository",
        "foreign_repository_worktree",
    ),
    (
        "HEAD is not the session's own branch",
        "head_not_session_branch",
    ),
    ("resource evidence was truncated", "evidence_truncated"),
    (
        "common Git directory identity",
        "common_directory_identity_unavailable",
    ),
    (
        "exact local ref inventory failed",
        "ref_evidence_unreadable",
    ),
    ("non-UTF-8 ref inventory", "ref_evidence_unreadable"),
    ("malformed ref inventory", "ref_evidence_unreadable"),
    ("unrecognized object format", "ref_evidence_unreadable"),
    ("duplicate exact ref evidence", "ref_evidence_unreadable"),
    ("non-UTF-8 remote ref evidence", "ref_evidence_unreadable"),
    ("non-UTF-8 hook directory", "hook_directory_unreadable"),
    ("incomplete hook directory", "hook_directory_unreadable"),
    ("ambiguous hook directory", "hook_directory_unreadable"),
    (
        "hook directory inventory failed",
        "hook_directory_unreadable",
    ),
    ("hook state is unreadable", "hook_directory_unreadable"),
    ("a reference-transaction hook", "reference_transaction_hook"),
    ("a pre-push hook", "pre_push_hook"),
    (
        "push.followTags is unreadable",
        "push_follow_tags_unreadable",
    ),
    ("shallow", "shallow_repository"),
    ("unrecognized resource profile", LANE_NOT_STRICT_SPELLING),
    (
        "Git configuration inventory failed",
        "config_inventory_unreadable",
    ),
    (
        "non-UTF-8 configuration inventory",
        "config_inventory_unreadable",
    ),
    ("malformed Git configuration", "config_inventory_unreadable"),
    ("a mirror or push URL", "push_url_or_mirror"),
    ("a custom SSH command", "custom_ssh_command"),
    ("repository extension", "repository_extension"),
    ("a URL rewrite", "url_rewrite"),
    ("the ext transport", "ext_transport"),
    (
        "a custom remote helper or pack program",
        "custom_remote_program",
    ),
    (
        "nonstandard remote fetch mapping",
        "nonstandard_remote_fetch",
    ),
    (
        "unrecognized Git configuration evidence",
        "unrecognized_config",
    ),
];

/// The stable reason code for an error a footprint proof returned.
pub(crate) fn lane_unavailable_code(error: &BrokerOpError) -> &'static str {
    match error {
        BrokerOpError::InvalidCoordinatedOperation { reason } => reason
            .strip_prefix(UNAVAILABLE_PREFIX)
            .and_then(|detail| {
                LANE_REASONS
                    .iter()
                    .find(|(prefix, _)| detail.starts_with(*prefix))
            })
            .map_or(LANE_UNCLASSIFIED, |(_, code)| *code),
        BrokerOpError::AdmissionTimedOut { .. } => LANE_ADMISSION_TIMED_OUT,
        _ => LANE_PROBE_FAILED,
    }
}

fn probe(
    cwd: &Path,
    args: &[&str],
    deadline: AdmissionDeadline,
    repository: &str,
) -> Result<CapturedOutput, BrokerOpError> {
    let mut command = provider_command(OperationProvider::Git);
    remove_inherited_git_config_overrides(&mut command);
    command
        .args(args)
        .current_dir(cwd)
        .env("GIT_OPTIONAL_LOCKS", "0");
    let output = output_within_capture_limit(
        command,
        deadline,
        repository,
        "checking the exact Git resource footprint",
        None,
        64 * 1024,
    )?;
    if output.stdout_truncated || output.stderr_truncated {
        return Err(unavailable("resource evidence was truncated"));
    }
    Ok(output)
}

fn common_directory(cwd: &Path) -> Result<PathBuf, BrokerOpError> {
    let common = crate::GitRepo::discover(cwd)?.git_common_dir()?;
    std::fs::canonicalize(&common).map_err(|source| BrokerOpError::OperationIo {
        path: common,
        source,
    })
}

pub(super) fn common_lock_key(cwd: &Path) -> Result<String, BrokerOpError> {
    use std::os::unix::fs::MetadataExt;
    let common = common_directory(cwd)?;
    let metadata = std::fs::metadata(&common).map_err(|source| BrokerOpError::OperationIo {
        path: common,
        source,
    })?;
    if !metadata.is_dir() || metadata.ino() == 0 {
        return Err(unavailable("common Git directory identity unavailable"));
    }
    Ok(format!("git-common:{}:{}", metadata.dev(), metadata.ino()))
}

pub(super) fn observe_ref(
    cwd: &Path,
    reference: &str,
    deadline: AdmissionDeadline,
    repository: &str,
) -> Result<Option<String>, BrokerOpError> {
    let output = probe(
        cwd,
        &[
            "for-each-ref",
            "--count=2",
            "--format=%(refname) %(objectname)",
            "--",
            reference,
        ],
        deadline,
        repository,
    )?;
    if !output.status.success() {
        return Err(unavailable("exact local ref inventory failed"));
    }
    let text =
        std::str::from_utf8(&output.stdout).map_err(|_| unavailable("non-UTF-8 ref inventory"))?;
    let mut found = None;
    for line in text.lines() {
        let Some((name, oid)) = line.split_once(' ') else {
            return Err(unavailable("malformed ref inventory"));
        };
        if oid.len() != 40 || !oid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(unavailable("unrecognized object format"));
        }
        if name == reference {
            if found.is_some() {
                return Err(unavailable("duplicate exact ref evidence"));
            }
            found = Some(oid.to_owned());
        }
    }
    Ok(found)
}

/// Refuse the strict profile when any of `hooks` exists in the effective hook
/// directory (`core.hooksPath`, else `<common>/hooks`).
fn refuse_hooks(
    cwd: &Path,
    common: &Path,
    hooks: &[&str],
    deadline: AdmissionDeadline,
    repository: &str,
) -> Result<(), BrokerOpError> {
    let hook_config = probe(
        cwd,
        &["config", "--path", "--null", "--get", "core.hooksPath"],
        deadline,
        repository,
    )?;
    let hook_directory = if hook_config.status.success() {
        let text = std::str::from_utf8(&hook_config.stdout)
            .map_err(|_| unavailable("non-UTF-8 hook directory"))?;
        let Some(value) = text.strip_suffix('\0') else {
            return Err(unavailable("incomplete hook directory"));
        };
        if value.is_empty() || value.contains('\0') || value.chars().any(char::is_control) {
            return Err(unavailable("ambiguous hook directory"));
        }
        let path = PathBuf::from(value);
        if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        }
    } else if hook_config.status.code() == Some(1) {
        common.join("hooks")
    } else {
        return Err(unavailable("hook directory inventory failed"));
    };
    for hook in hooks {
        if hook_directory
            .join(hook)
            .try_exists()
            .map_err(|_| unavailable("hook state is unreadable"))?
        {
            return Err(unavailable(&format!(
                "a {hook} hook can expand this resource footprint"
            )));
        }
    }
    Ok(())
}

/// Whether Git would follow tags on a push from `cwd` (`push.followTags`).
/// The strict session-push spelling passes `--no-follow-tags`, so a repository
/// that publishes tags this way keeps the historical spelling.
pub(super) fn push_follows_tags(
    cwd: &Path,
    deadline: AdmissionDeadline,
    repository: &str,
) -> Result<bool, BrokerOpError> {
    let output = probe(
        cwd,
        &["config", "--type=bool", "--get", "push.followTags"],
        deadline,
        repository,
    )?;
    match output.status.code() {
        Some(0) => Ok(output.stdout.trim_ascii() == b"true"),
        Some(1) => Ok(false),
        _ => Err(unavailable("push.followTags is unreadable")),
    }
}

/// Environment selectors that can change a coordinated Git resource footprint.
pub(super) const GIT_REPOSITORY_ROUTING_ENV_NAMES: &[&str] = &[
    "GIT_DIR",
    "GIT_COMMON_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_QUARANTINE_PATH",
    "GIT_SHALLOW_FILE",
    "GIT_GRAFT_FILE",
    "GIT_NAMESPACE",
    "GIT_REPLACE_REF_BASE",
    "GIT_EXEC_PATH",
];

/// Reject hidden ref writers and set-expanding refmaps rather than assuming
/// that command-line operands alone constrain an arbitrary Git configuration.
///
/// `session_branch_hooks` is true only for a push of the caller's own session
/// branch while `coordination.session_hooks_hold_repository_lock` is off: the
/// repository's hooks still run inside the push exactly as before, but they do
/// not force the repository-wide lane.
pub(super) fn validate_repository(
    cwd: &Path,
    args: &[String],
    deadline: AdmissionDeadline,
    repository: &str,
    session_branch_hooks: bool,
) -> Result<(), BrokerOpError> {
    use std::os::unix::fs::MetadataExt;
    for name in GIT_REPOSITORY_ROUTING_ENV_NAMES.iter().copied().chain([
        "GIT_SSH_COMMAND",
        "GIT_SSH",
        "GIT_PROXY_COMMAND",
    ]) {
        if std::env::var_os(name).is_some() {
            return Err(unavailable("ambient Git routing or transport override"));
        }
    }
    let common = common_directory(cwd)?;
    let metadata = std::fs::metadata(&common).map_err(|source| BrokerOpError::OperationIo {
        path: common.clone(),
        source,
    })?;
    if !metadata.is_dir() || metadata.ino() == 0 {
        return Err(unavailable("common Git directory identity is unavailable"));
    }
    if common
        .join("shallow")
        .try_exists()
        .map_err(|_| unavailable("shallow state is unreadable"))?
    {
        return Err(unavailable(
            "shallow repository has an additional write footprint",
        ));
    }
    let named_push = strict_push(args).is_some_and(|push| push.named_remote());
    // A `reference-transaction` hook sees, and can extend, every ref update,
    // so it always widens. A session branch's own `pre-push` hook does not,
    // unless the repository opted out.
    let hooks: &[&str] = if fetch_footprint(args).is_some()
        || (session_branch_hooks && strict_push(args).is_some())
    {
        &["reference-transaction"]
    } else {
        &["reference-transaction", "pre-push"]
    };
    refuse_hooks(cwd, &common, hooks, deadline, repository)?;
    if resource_scope(args).is_none() {
        return Err(unavailable("unrecognized resource profile"));
    }
    let output = probe(
        cwd,
        &[
            "config",
            "--null",
            "--get-regexp",
            r"^(remote\..*\.(fetch|vcs|uploadpack|receivepack|mirror|pushurl)|core\.(hookspath|sshcommand)|extensions\..*|url\..*\.(insteadof|pushinsteadof)|protocol\.ext\.allow)$",
        ],
        deadline,
        repository,
    )?;
    if !output.status.success() && output.status.code() != Some(1) {
        return Err(unavailable("Git configuration inventory failed"));
    }
    let text = std::str::from_utf8(&output.stdout)
        .map_err(|_| unavailable("non-UTF-8 configuration inventory"))?;
    for entry in text.split('\0').filter(|entry| !entry.is_empty()) {
        let Some((key, value)) = entry.split_once('\n') else {
            return Err(unavailable("malformed Git configuration"));
        };
        let key = key.to_ascii_lowercase();
        if key == "core.hookspath" {
            continue;
        }
        if key.ends_with(".mirror") || key.ends_with(".pushurl") {
            // A mirror or a separate push URL changes what a push to a
            // configured remote name writes; fetches and URL pushes ignore it.
            if named_push {
                return Err(unavailable(
                    "a mirror or push URL changes this remote's push footprint",
                ));
            }
            continue;
        }
        // `extensions.worktreeConfig` only makes Git also read the current
        // worktree's `config.worktree`. This inventory runs in that worktree,
        // so every key that file sets is already in front of us and judged
        // like any other: the extension itself writes no ref and moves no
        // transport. Linked-worktree tooling (`git sparse-checkout`, editors)
        // sets it, which kept every session push in such repositories wide.
        if key == "extensions.worktreeconfig" {
            continue;
        }
        if key == "core.sshcommand" {
            return Err(unavailable("a custom SSH command needs the wide lane"));
        }
        if key.starts_with("extensions.") {
            return Err(unavailable(&format!(
                "repository extension {key} needs the wide lane"
            )));
        }
        if key.starts_with("url.") {
            return Err(unavailable("a URL rewrite needs the wide lane"));
        }
        if key == "protocol.ext.allow" {
            return Err(unavailable("the ext transport needs the wide lane"));
        }
        if key.ends_with(".vcs") || key.ends_with(".uploadpack") || key.ends_with(".receivepack") {
            return Err(unavailable(
                "a custom remote helper or pack program needs the wide lane",
            ));
        }
        if let Some(remote) = key
            .strip_prefix("remote.")
            .and_then(|key| key.strip_suffix(".fetch"))
        {
            if !ref_component(remote)
                || remote.contains('/')
                || value != format!("+refs/heads/*:refs/remotes/{remote}/*")
            {
                return Err(unavailable("nonstandard remote fetch mapping"));
            }
        } else {
            return Err(unavailable("unrecognized Git configuration evidence"));
        }
    }
    Ok(())
}

impl FetchJournal {
    pub(super) fn prepare(
        operation_id: i64,
        cwd: &Path,
        args: &[String],
        deadline: AdmissionDeadline,
        repository: &str,
    ) -> Result<Option<Self>, BrokerOpError> {
        use std::os::unix::fs::MetadataExt;
        let Some(footprint) = fetch_footprint(args) else {
            return Ok(None);
        };
        let common_directory = common_directory(cwd)?;
        let metadata =
            std::fs::metadata(&common_directory).map_err(|source| BrokerOpError::OperationIo {
                path: common_directory.clone(),
                source,
            })?;
        let pre_destination = observe_ref(cwd, &footprint.destination_ref, deadline, repository)?;
        Ok(Some(Self {
            schema_version: 1,
            operation_id,
            repository: repository.into(),
            footprint,
            common_directory,
            common_device: metadata.dev(),
            common_inode: metadata.ino(),
            pre_destination,
            provider: None,
            completion: None,
        }))
    }

    pub(super) fn observe(&mut self, observation: ProviderObservation) {
        if self.provider.is_none() {
            self.provider = observation.identity;
        }
        if let Some((success, exit_code)) = observation.exit {
            self.completion = Some(FetchCompletion {
                pid: observation.pid,
                success,
                exit_code,
                observed_at: unix_now_ms(),
            });
        }
    }

    pub(super) fn proves_provider_completion(&self) -> bool {
        matches!((&self.provider, &self.completion), (Some(provider), Some(completion))
            if provider.start_time.is_some() && provider.pid > 1
                && provider.pid as u32 == completion.pid
                && completion.observed_at > 0 && completion.exit_code.is_some()
                && (completion.success == (completion.exit_code == Some(0))))
    }

    pub(super) fn matches_repository(&self, cwd: &Path) -> bool {
        use std::os::unix::fs::MetadataExt;
        let Ok(common) = common_directory(cwd) else {
            return false;
        };
        let Ok(metadata) = std::fs::metadata(&common) else {
            return false;
        };
        common == self.common_directory
            && metadata.dev() == self.common_device
            && metadata.ino() == self.common_inode
    }
}

struct VerifiedFetch {
    journal: FetchJournal,
    local: Option<String>,
    remote: Option<String>,
}

/// Recover only a fetch whose actual child completion was durably observed.
/// Endpoint/ref equality without that witness never authorizes reconciliation.
pub(super) fn try_reconcile(
    broker: &mut Broker,
    operation: &CoordinatedOperation,
    cwd: &Path,
    deadline: AdmissionDeadline,
) -> Result<bool, BrokerOpError> {
    if operation.provider != OperationProvider::Git
        || operation.status != OperationStatus::OutcomeUnknown
    {
        return Ok(false);
    }
    let evidence = (|| -> Result<Option<VerifiedFetch>, BrokerOpError> {
        let Some(command) = serde_json::from_str::<Vec<String>>(&operation.command_json).ok()
        else {
            return Ok(None);
        };
        let Some(args) = command.get(1..) else {
            return Ok(None);
        };
        let Some(footprint) = fetch_footprint(args) else {
            return Ok(None);
        };
        let Some(journal) = operation
            .details_json
            .as_deref()
            .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
            .and_then(|value| value.get("fetch_reconciliation").cloned())
            .and_then(|value| serde_json::from_value::<FetchJournal>(value).ok())
        else {
            return Ok(None);
        };
        if journal.schema_version != 1
            || journal.operation_id != operation.id
            || journal.repository != operation.repository
            || journal.footprint != footprint
            || !journal.proves_provider_completion()
            || !journal.matches_repository(cwd)
        {
            return Ok(None);
        }
        validate_repository(cwd, args, deadline, &operation.repository, false)?;
        let target = crate::GitRepo::discover(cwd)?.resolve_remote_command_target(args, None)?;
        if target.coordination_key != operation.repository {
            return Ok(None);
        }
        let local = observe_ref(
            cwd,
            &footprint.destination_ref,
            deadline,
            &operation.repository,
        )?;
        let remote_output = probe(
            cwd,
            &[
                "ls-remote",
                "--refs",
                "--",
                &footprint.remote,
                &footprint.source_ref,
            ],
            deadline,
            &operation.repository,
        )?;
        if !remote_output.status.success() {
            return Ok(None);
        }
        let remote_text = std::str::from_utf8(&remote_output.stdout)
            .map_err(|_| unavailable("non-UTF-8 remote ref evidence"))?;
        let mut remote = None;
        for line in remote_text.lines() {
            let Some((oid, name)) = line.split_once('\t') else {
                return Ok(None);
            };
            if name != footprint.source_ref
                || oid.len() != 40
                || !oid.bytes().all(|byte| byte.is_ascii_hexdigit())
                || remote.is_some()
            {
                return Ok(None);
            }
            remote = Some(oid.to_owned());
        }
        let completion = journal.completion.as_ref().expect("validated completion");
        if completion.observed_at < operation.created_at || completion.observed_at > unix_now_ms() {
            return Ok(None);
        }
        let settled = if completion.success {
            local.is_some() && local == remote
        } else {
            local == journal.pre_destination
        };
        Ok(settled.then_some(VerifiedFetch {
            journal,
            local,
            remote,
        }))
    })();
    // Failed/missing evidence leaves the original unknown outcome intact.
    let Ok(Some(verified)) = evidence else {
        return Ok(false);
    };
    let VerifiedFetch {
        journal,
        local,
        remote,
    } = verified;
    let completion = journal.completion.as_ref().expect("validated completion");
    let succeeded = completion.success;
    if let Some(host_id) = &operation.host_operation_id {
        crate::host_operations::reconcile_host_operation_with_deadline(
            &broker.host_operation_database_path()?,
            host_id,
            succeeded,
            deadline.at,
        )?;
    }
    let status = if succeeded {
        OperationStatus::ReconciledSucceeded
    } else {
        OperationStatus::ReconciledFailed
    };
    let mut details = serde_json::from_str::<serde_json::Value>(
        operation.details_json.as_deref().unwrap_or("{}"),
    )
    .unwrap_or_else(|_| json!({}));
    details["reconciliation"] = json!({
        "source": "observed_fetch_completion", "outcome": status.as_str(),
        "local_destination": local, "remote_source": remote, "observed_at": unix_now_ms(),
        "common_directory_identity_verified": true,
    });
    Ok(broker
        .store()
        .reconcile_observed_operation(
            operation,
            &details.to_string(),
            status,
            completion.exit_code.map(i64::from),
        )?
        .is_some())
}

/// `reconciliation.source` of a repeatable fetch closed without an operator.
pub(crate) const REPEATABLE_FETCH_AUTO_RESOLVED: &str =
    crate::events::FETCH_RECONCILED_REPEATABLE_AUTO_RESOLVED;

/// Whether `args` fetch exactly one branch into its own remote-tracking ref.
///
/// Such a fetch reads the remote and moves one local remote-tracking ref,
/// atomically under Git's own ref lock. It cannot leave the remote in an
/// unknown state, and repeating it converges on whatever the remote holds, so
/// an interrupted or failed attempt never needs anyone to inspect external
/// state. Both spellings the broker issues qualify: the strict profile above
/// and `fetch <remote> refs/heads/<b>:refs/remotes/<remote>/<b>`. Any other
/// fetch (extra flags, globs, forced or several refspecs, leading global
/// options, a URL remote) keeps the conservative unknown-outcome handling.
pub(crate) fn is_repeatable_tracking_fetch(args: &[String]) -> bool {
    if fetch_footprint(args).is_some() {
        return true;
    }
    let [command, remote, refspec] = args else {
        return false;
    };
    if command != "fetch" || !ref_component(remote) || remote.contains('/') {
        return false;
    }
    let Some((source, destination)) = refspec.split_once(':') else {
        return false;
    };
    source.strip_prefix("refs/heads/").is_some_and(|branch| {
        ref_component(branch) && destination == format!("refs/remotes/{remote}/{branch}")
    })
}

/// [`is_repeatable_tracking_fetch`] for a journaled operation.
pub(crate) fn records_repeatable_tracking_fetch(operation: &CoordinatedOperation) -> bool {
    operation.provider == OperationProvider::Git
        && serde_json::from_str::<Vec<String>>(&operation.command_json)
            .ok()
            .is_some_and(|command| command.get(1..).is_some_and(is_repeatable_tracking_fetch))
}

/// Close an `outcome_unknown` repeatable fetch without operator inspection.
///
/// The row is kept and becomes `reconciled_failed`: nothing is trusted to
/// have been refreshed, so the next caller simply fetches again. The
/// `operation.fetch_reconciled` event and `reconciliation.source` say it was
/// resolved automatically and why. Returns whether this call resolved it.
pub(crate) fn auto_resolve_repeatable_fetch(
    broker: &mut Broker,
    operation: &CoordinatedOperation,
    deadline: Option<std::time::Instant>,
) -> Result<bool, BrokerOpError> {
    if operation.status != OperationStatus::OutcomeUnknown
        || !records_repeatable_tracking_fetch(operation)
    {
        return Ok(false);
    }
    let started = Instant::now();
    if let Some(host_id) = &operation.host_operation_id {
        crate::host_operations::reconcile_host_operation_with_deadline(
            &broker.host_operation_database_path()?,
            host_id,
            false,
            deadline,
        )?;
    }
    let mut details = serde_json::from_str::<serde_json::Value>(
        operation.details_json.as_deref().unwrap_or("{}"),
    )
    .ok()
    .filter(serde_json::Value::is_object)
    .unwrap_or_else(|| json!({}));
    details["reconciliation"] = json!({
        "source": REPEATABLE_FETCH_AUTO_RESOLVED,
        "outcome": OperationStatus::ReconciledFailed.as_str(),
        "reason": "a fetch only moves one local remote-tracking ref and is safe to repeat; it cannot leave the remote in an unknown state",
        "recovery": "fetch again",
        "resolved_at": unix_now_ms(),
    });
    let resolved = broker
        .store()
        .reconcile_observed_operation(
            operation,
            &details.to_string(),
            OperationStatus::ReconciledFailed,
            operation.exit_code,
        )?
        .is_some();
    if resolved {
        crate::phase_telemetry::add("coordinated_operation_auto_resolved", started.elapsed());
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_single_branch_tracking_fetches_are_repeatable() {
        let args = |parts: &[&str]| {
            parts
                .iter()
                .map(|part| (*part).to_owned())
                .collect::<Vec<_>>()
        };
        assert!(is_repeatable_tracking_fetch(
            &canonical_fetch_args("origin", "main").unwrap()
        ));
        assert!(is_repeatable_tracking_fetch(&args(&[
            "fetch",
            "origin",
            "refs/heads/main:refs/remotes/origin/main",
        ])));
        assert!(is_repeatable_tracking_fetch(&args(&[
            "fetch",
            "upstream",
            "refs/heads/release/1.0:refs/remotes/upstream/release/1.0",
        ])));
        for unsafe_spelling in [
            &["fetch", "origin"][..],
            &["fetch", "--all"],
            &[
                "fetch",
                "origin",
                "+refs/heads/main:refs/remotes/origin/main",
            ],
            &["fetch", "origin", "refs/heads/main:refs/heads/main"],
            &["fetch", "origin", "refs/heads/main:refs/remotes/other/main"],
            &["fetch", "origin", "refs/heads/*:refs/remotes/origin/*"],
            &[
                "fetch",
                "--upload-pack=x",
                "refs/heads/main:refs/remotes/--upload-pack=x/main",
            ],
            &[
                "fetch",
                "/tmp/remote.git",
                "refs/heads/main:refs/remotes/origin/main",
            ],
            &[
                "-c",
                "core.sshCommand=x",
                "fetch",
                "origin",
                "refs/heads/main:refs/remotes/origin/main",
            ],
            &[
                "fetch",
                "--prune",
                "origin",
                "refs/heads/main:refs/remotes/origin/main",
            ],
            &["push", "origin", "refs/heads/main:refs/remotes/origin/main"],
        ] {
            assert!(
                !is_repeatable_tracking_fetch(&args(unsafe_spelling)),
                "{unsafe_spelling:?}"
            );
        }
    }

    #[test]
    fn canonical_fetch_footprint_has_one_destination_and_no_auxiliary_ref_writes() {
        let args = canonical_fetch_args("origin", "topic/a").unwrap();
        let footprint = fetch_footprint(&args).unwrap();
        assert_eq!(footprint.destination_ref, "refs/remotes/origin/topic/a");
        assert_eq!(
            resource_scope(&args).as_deref(),
            Some("git-ref/refs/heads/topic/a")
        );
        for index in 1..=FETCH_FLAGS.len() {
            let mut missing = args.clone();
            missing.remove(index);
            assert!(resource_scope(&missing).is_none(), "missing flag {index}");
        }
        let mut expanded = args.clone();
        expanded.push("refs/heads/other:refs/remotes/origin/other".into());
        assert!(resource_scope(&expanded).is_none());
        assert!(canonical_fetch_args("origin", "a/../b").is_none());
    }

    #[test]
    fn push_and_fetch_share_the_branch_scope_but_unsafe_spellings_stay_wide() {
        let mut push = vec![
            "push",
            "--no-follow-tags",
            "--porcelain",
            "--",
            "https://github.com/example/repo",
            "",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        push[5] = format!("{}:refs/heads/main", "a".repeat(40));
        assert_eq!(
            resource_scope(&push),
            resource_scope(&canonical_fetch_args("origin", "main").unwrap())
        );
        // A URL remote updates no local ref.
        assert_eq!(local_destination_ref(&push), None);
        // A configured remote name shares the branch lane and also locks the
        // one tracking ref the push updates, exactly like a fetch of it.
        push[4] = "origin".into();
        assert_eq!(
            resource_scope(&push).as_deref(),
            Some("git-ref/refs/heads/main")
        );
        assert_eq!(
            local_destination_ref(&push),
            local_destination_ref(&canonical_fetch_args("origin", "main").unwrap())
        );
        assert_eq!(
            strict_push_destination(&push).as_deref(),
            Some("refs/heads/main")
        );
        for remote in [
            "/tmp/remote.git",
            "git@host:o/r",
            "../remote",
            "-origin",
            "",
        ] {
            push[4] = remote.into();
            assert!(resource_scope(&push).is_none(), "{remote}");
        }
        push[4] = "https://github.com/example/repo".into();
        push[5] = "HEAD:refs/heads/main".into();
        assert!(resource_scope(&push).is_none());
        push[5] = format!("{}:refs/tags/v1", "a".repeat(40));
        assert!(resource_scope(&push).is_none());
    }

    #[test]
    fn session_push_uses_the_strict_spelling() {
        let sha = "b".repeat(40);
        let args = strict_push_args("origin", &sha, "refs/heads/agent/a");
        assert_eq!(
            resource_scope(&args).as_deref(),
            Some("git-ref/refs/heads/agent/a")
        );
        assert_eq!(
            local_destination_ref(&args).as_deref(),
            Some("refs/remotes/origin/agent/a")
        );
    }

    #[test]
    fn only_a_reserved_commit_record_recomputes_to_its_branch_lane() {
        let args = |parts: &[&str]| {
            parts
                .iter()
                .map(|part| (*part).to_owned())
                .collect::<Vec<_>>()
        };
        let scope = session_commit_record_scope("refs/heads/agent/a");
        assert_eq!(scope, "session-commit:refs/heads/agent/a");
        assert_eq!(
            recorded_session_commit_scope(&args(&["commit", "-m", "x"]), &scope).as_deref(),
            Some("git-ref/refs/heads/agent/a")
        );
        // Wrong command, global options, or an unreserved scope stay wide.
        for (command, recorded) in [
            (
                &["-C", "elsewhere", "commit", "-m", "x"][..],
                scope.as_str(),
            ),
            (&["branch", "-f", "agent/a"], scope.as_str()),
            (&["commit", "-m", "x"], "refs/heads/agent/a"),
            (&["commit", "-m", "x"], "repository"),
            (&["commit", "-m", "x"], "session-commit:refs/tags/v1"),
            (&["commit", "-m", "x"], "session-commit:refs/heads/../main"),
        ] {
            assert!(
                recorded_session_commit_scope(&args(command), recorded).is_none(),
                "{command:?} {recorded}"
            );
        }
        assert!(is_plain_commit(&args(&["commit"])));
        assert!(!is_plain_commit(&args(&["-c", "x=y", "commit"])));
    }

    #[test]
    fn unknown_wide_git_records_still_block_scoped_writers() {
        assert!(scopes_conflict(
            OperationProvider::Git,
            None,
            Some("git-ref/refs/heads/main")
        ));
        assert!(scopes_conflict(
            OperationProvider::Git,
            Some("git-ref/refs/heads/main"),
            None
        ));
        assert!(!scopes_conflict(
            OperationProvider::Git,
            Some("git-ref/refs/heads/main"),
            Some("git-ref/refs/heads/other")
        ));
        assert!(!scopes_conflict(
            OperationProvider::Github,
            None,
            Some("pull/1/comments")
        ));
    }
    #[test]
    fn narrow_lanes_share_compatibility_lock_but_wide_writers_remain_exclusive() {
        let root = tempfile::tempdir().unwrap();
        let first = RepositoryWriteLock::acquire_shared(
            root.path(),
            "example/repo",
            (1, None),
            || Ok("test".into()),
            QueueWait::Refuse,
            |_, _| {},
        )
        .unwrap();
        let second = RepositoryWriteLock::acquire_shared(
            root.path(),
            "example/repo",
            (2, None),
            || Ok("test".into()),
            QueueWait::Refuse,
            |_, _| {},
        )
        .unwrap();
        assert!(
            RepositoryWriteLock::try_acquire_nonblocking(root.path(), "example/repo")
                .unwrap()
                .is_none()
        );
        let lane = RepositoryWriteLock::try_acquire_nonblocking(
            root.path(),
            "example/repo::git-ref/refs/heads/main",
        )
        .unwrap()
        .unwrap();
        assert!(
            RepositoryWriteLock::try_acquire_nonblocking(
                root.path(),
                "example/repo::git-ref/refs/heads/main",
            )
            .unwrap()
            .is_none()
        );
        assert!(
            RepositoryWriteLock::try_acquire_nonblocking(
                root.path(),
                "example/repo::git-ref/refs/heads/other",
            )
            .unwrap()
            .is_some()
        );
        drop(lane);
        drop(first);
        drop(second);
        assert!(
            RepositoryWriteLock::try_acquire_nonblocking(root.path(), "example/repo")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn fetch_completion_requires_actual_matching_generation_and_normal_exit() {
        let mut journal = FetchJournal {
            schema_version: 1,
            operation_id: 1,
            repository: "example/repo".into(),
            footprint: fetch_footprint(&canonical_fetch_args("origin", "main").unwrap()).unwrap(),
            common_directory: PathBuf::from("/not-used"),
            common_device: 1,
            common_inode: 2,
            pre_destination: None,
            provider: None,
            completion: None,
        };
        assert!(!journal.proves_provider_completion());
        journal.provider = Some(crate::file_lock::artifact_use::ProcessStartIdentity {
            pid: 123,
            process_group: 123,
            start_time: Some(99),
            start_time_clock: "test".into(),
        });
        assert!(
            !journal.proves_provider_completion(),
            "PID existence is insufficient"
        );
        journal.completion = Some(FetchCompletion {
            pid: 123,
            success: true,
            exit_code: Some(0),
            observed_at: 1,
        });
        assert!(journal.proves_provider_completion());
        journal.completion.as_mut().unwrap().pid = 124;
        assert!(!journal.proves_provider_completion());
        journal.completion.as_mut().unwrap().pid = 123;
        journal.provider.as_mut().unwrap().start_time = None;
        assert!(!journal.proves_provider_completion());
        journal.provider.as_mut().unwrap().start_time = Some(99);
        journal.completion.as_mut().unwrap().success = false;
        journal.completion.as_mut().unwrap().exit_code = None;
        assert!(
            !journal.proves_provider_completion(),
            "signal death cannot settle descendants"
        );
    }

    /// Every refusal text `unavailable` is called with maps to a stable code,
    /// so a new refusal cannot silently journal as `unclassified`.
    #[test]
    fn every_unavailable_refusal_has_a_stable_reason_code() {
        let source = include_str!("git_scopes.rs");
        let needle = concat!("unavail", "able(");
        let mut literals = Vec::new();
        for (index, _) in source.match_indices(needle) {
            let rest = source[index + needle.len()..].trim_start();
            if let Some(rest) = rest.strip_prefix('"') {
                literals.push(&rest[..rest.find('"').unwrap()]);
            }
        }
        assert!(literals.len() > 25, "{literals:?}");
        for detail in literals {
            assert_ne!(
                lane_unavailable_code(&unavailable(detail)),
                LANE_UNCLASSIFIED,
                "{detail}"
            );
        }
        for (detail, code) in [
            (
                "a pre-push hook can expand this resource footprint",
                "pre_push_hook",
            ),
            (
                "a reference-transaction hook can expand this resource footprint",
                "reference_transaction_hook",
            ),
            (
                "repository extension extensions.partialclone needs the wide lane",
                "repository_extension",
            ),
        ] {
            assert_eq!(lane_unavailable_code(&unavailable(detail)), code);
        }
        assert_eq!(
            lane_unavailable_code(&BrokerOpError::InvalidCoordinatedOperation {
                reason: "something else".into()
            }),
            LANE_UNCLASSIFIED
        );
    }

    fn git_in(root: &Path, arguments: &[&str]) {
        let output = std::process::Command::new("git")
            .args(arguments)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn repository_with_origin(origin: &str) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        git_in(root.path(), &["init", "-q", "-b", "main"]);
        git_in(root.path(), &["remote", "add", "origin", origin]);
        root
    }

    /// The busiest repository kept every session push wide on 0.8.85 and
    /// 0.8.86 because its only unexplained configuration key was
    /// `extensions.worktreeConfig = true`. That extension only makes Git read
    /// the worktree's own `config.worktree`, which the inventory already
    /// reads, so it no longer widens; every other refusal names its reason.
    #[test]
    fn a_worktree_config_extension_keeps_the_lane_and_each_refusal_names_its_reason() {
        let push = strict_push_args("origin", &"a".repeat(40), "refs/heads/agent/a");
        let deadline = || AdmissionDeadline::start(QueueWait::Refuse);
        let check = |root: &Path, session_branch_hooks: bool| {
            validate_repository(root, &push, deadline(), "test", session_branch_hooks)
                .map_err(|error| lane_unavailable_code(&error))
        };

        let root = repository_with_origin("https://example.invalid/acme/project.git");
        assert_eq!(check(root.path(), true), Ok(()));
        git_in(
            root.path(),
            &["config", "extensions.worktreeConfig", "true"],
        );
        assert_eq!(check(root.path(), true), Ok(()));
        // The per-worktree file is still inventoried.
        git_in(
            root.path(),
            &["config", "--worktree", "core.sshCommand", "ssh -v"],
        );
        assert_eq!(check(root.path(), true), Err("custom_ssh_command"));

        for (setting, code) in [
            (&["core.sshCommand", "ssh -v"][..], "custom_ssh_command"),
            (
                &[
                    "url.https://mirror.invalid/.insteadOf",
                    "https://example.invalid/",
                ],
                "url_rewrite",
            ),
            (
                &["extensions.partialClone", "origin"],
                "repository_extension",
            ),
            (
                &["remote.origin.pushurl", "https://example.invalid/other.git"],
                "push_url_or_mirror",
            ),
            (&["protocol.ext.allow", "always"], "ext_transport"),
            (
                &["remote.origin.receivepack", "git-receive-pack"],
                "custom_remote_program",
            ),
            (
                &["--add", "remote.origin.fetch", "+refs/tags/*:refs/tags/*"],
                "nonstandard_remote_fetch",
            ),
        ] {
            let root = repository_with_origin("https://example.invalid/acme/project.git");
            let mut arguments = vec!["config"];
            arguments.extend_from_slice(setting);
            git_in(root.path(), &arguments);
            assert_eq!(check(root.path(), true), Err(code), "{setting:?}");
        }

        // A session branch's own pre-push hook does not widen its push; any
        // other strict push, and a reference-transaction hook, do.
        let root = repository_with_origin("https://example.invalid/acme/project.git");
        let hooks = root.path().join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        std::fs::write(hooks.join("pre-push"), "#!/bin/sh\n").unwrap();
        assert_eq!(check(root.path(), true), Ok(()));
        assert_eq!(check(root.path(), false), Err("pre_push_hook"));
        std::fs::write(hooks.join("reference-transaction"), "#!/bin/sh\n").unwrap();
        assert_eq!(check(root.path(), true), Err("reference_transaction_hook"));

        // A relative `core.hooksPath` (husky's `.husky`) resolves against the
        // worktree and widens only for the hook it actually holds.
        let root = repository_with_origin("https://example.invalid/acme/project.git");
        std::fs::create_dir(root.path().join(".husky")).unwrap();
        std::fs::write(root.path().join(".husky/pre-push"), "#!/bin/sh\n").unwrap();
        git_in(root.path(), &["config", "core.hooksPath", ".husky"]);
        assert_eq!(check(root.path(), true), Ok(()));
        std::fs::write(
            root.path().join(".husky/reference-transaction"),
            "#!/bin/sh\n",
        )
        .unwrap();
        assert_eq!(check(root.path(), true), Err("reference_transaction_hook"));

        // `broker push`'s own check names `push.followTags` and the opt-out.
        let root = repository_with_origin("https://example.invalid/acme/project.git");
        assert_eq!(
            crate::operations::session_branch_push_lane(root.path(), root.path(), &push),
            Ok(())
        );
        git_in(root.path(), &["config", "push.followTags", "true"]);
        assert_eq!(
            crate::operations::session_branch_push_lane(root.path(), root.path(), &push),
            Err(LANE_PUSH_FOLLOWS_TAGS)
        );
        let root = repository_with_origin("https://example.invalid/acme/project.git");
        std::fs::create_dir_all(root.path().join(".git/hooks")).unwrap();
        std::fs::write(root.path().join(".git/hooks/pre-push"), "#!/bin/sh\n").unwrap();
        std::fs::create_dir(root.path().join(".aethyme")).unwrap();
        std::fs::write(
            root.path().join(".aethyme/config.toml"),
            "[coordination]\nsession_hooks_hold_repository_lock = true\n",
        )
        .unwrap();
        assert_eq!(
            crate::operations::session_branch_push_lane(root.path(), root.path(), &push),
            Err(LANE_SESSION_HOOKS_HOLD_LOCK)
        );

        // The fallback spelling `broker push` uses re-derives its strict twin.
        let plain = vec![
            "push".to_owned(),
            "origin".to_owned(),
            format!("{}:refs/heads/agent/a", "a".repeat(40)),
        ];
        assert_eq!(strict_equivalent_of_plain_push(&plain), Some(push.clone()));
        let mut moving = plain.clone();
        moving[2] = "HEAD:refs/heads/agent/a".into();
        assert_eq!(strict_equivalent_of_plain_push(&moving), None);
    }

    /// An exclusive holder writes who it is next to the lock, so a waiter
    /// can name a holder the journal does not know, and the record goes
    /// away with the lock. Shared holders write none.
    #[test]
    fn an_exclusive_holder_is_recorded_next_to_its_lock_until_it_releases() {
        let root = tempfile::tempdir().unwrap();
        let held = RepositoryWriteLock::acquire(
            root.path(),
            "example/repo",
            7,
            Some(3),
            || Ok("test".into()),
            QueueWait::Refuse,
            |_, _| {},
        )
        .unwrap();
        let record = recorded_lock_holder(root.path(), "example/repo").expect("a holder record");
        assert_eq!(record.kind, "coordinated_operation");
        assert_eq!(record.pid, std::process::id());
        assert_eq!(record.session_id, Some(3));
        assert_eq!(record.operation_id, Some(7));
        assert!(
            RepositoryWriteLock::acquire(
                root.path(),
                "example/repo",
                8,
                None,
                || Ok("test".into()),
                QueueWait::Refuse,
                |_, _| {},
            )
            .is_err()
        );
        drop(held);
        assert!(recorded_lock_holder(root.path(), "example/repo").is_none());

        let shared = RepositoryWriteLock::acquire_shared(
            root.path(),
            "example/repo",
            (9, Some(4)),
            || Ok("test".into()),
            QueueWait::Refuse,
            |_, _| {},
        )
        .unwrap();
        assert!(recorded_lock_holder(root.path(), "example/repo").is_none());
        drop(shared);
    }

    /// An unjournaled exclusive hold (maintenance) has no journal row. A
    /// coordinated write refused behind it names it: kind, pid, session, hold
    /// time, and the phase the holder last recorded, on the common-directory
    /// lane it nests as well as its own.
    #[test]
    fn a_refusal_behind_an_unjournaled_hold_names_the_holder_and_its_phase() {
        let root = repository_with_origin("https://github.com/example/repo.git");
        let root = root.path().canonicalize().unwrap();
        let checkout = crate::GitRepo::discover(&root).unwrap();
        let (repository, mut held) = crate::operations::try_acquire_local_repository_write_lock_as(
            &root,
            &checkout,
            "git_maintenance_lock_apply",
            Some(5),
        )
        .unwrap()
        .expect("an uncontended hold");
        let common_key = common_lock_key(&root).unwrap();
        let record = recorded_lock_holder(&root, &common_key).expect("a holder record");
        assert_eq!(record.kind, "git_maintenance_lock_apply");
        assert_eq!(record.session_id, Some(5));
        assert_eq!(record.phase, None);
        held.record_phase("re-planning under the write locks");
        for key in [common_key.as_str(), repository.as_str()] {
            let record = recorded_lock_holder(&root, key).expect("a holder record");
            assert_eq!(
                record.phase.as_deref(),
                Some("re-planning under the write locks"),
                "{key}"
            );
            assert!(record.phase_started_at_ms.is_some(), "{key}");
        }

        let refused = RepositoryWriteLock::acquire_mode(
            &root,
            &common_key,
            (11, Some(6)),
            || Ok(unidentified_lock_holder().description),
            QueueWait::Refuse,
            Duration::from_secs(2),
            |_, _| {},
            false,
        );
        assert!(refused.is_err());
        let holder = with_recorded_holder(unidentified_lock_holder(), &root, &common_key);
        let details = lane_refusal_details(
            "local_git_lane_unavailable",
            &common_key,
            Some(unix_now_ms() - 59_000),
            &LaneTelemetry::default(),
            &holder,
        );
        assert_eq!(details["reason"], "local_git_lane_unavailable");
        assert_eq!(details["holder"]["scope"], "git_maintenance_lock_apply");
        assert_eq!(details["holder"]["session_id"], 5);
        assert_eq!(details["holder"]["pid"], i64::from(std::process::id()));
        assert!(details["holder"]["held_ms"].as_i64().is_some());
        assert_eq!(
            details["holder"]["phase"],
            "re-planning under the write locks"
        );
        let description = details["holder"]["description"].as_str().unwrap();
        assert!(
            description.contains("git_maintenance_lock_apply")
                && description.contains("now re-planning under the write locks"),
            "{details}"
        );
        assert!(
            details["coordination_timing"]["queue_wait_ms"]
                .as_i64()
                .unwrap()
                >= 59_000
        );
        drop(held);
        assert!(recorded_lock_holder(&root, &common_key).is_none());
    }

    /// The broker's own default-branch fetch (push, sync) uses the strict
    /// spelling, so it takes its tracking ref's lane instead of the
    /// repository-wide one (`not_strict_spelling` on every push before). A
    /// name the strict profile cannot express keeps the plain spelling.
    #[test]
    fn the_default_branch_fetch_takes_its_tracking_ref_lane() {
        let tracked = |remote: &str, branch: &str| crate::session_push::TrackedDefault {
            remote: remote.into(),
            branch: branch.into(),
            tracking_ref: format!("refs/remotes/{remote}/{branch}"),
            commit: "0".repeat(40),
        };
        let args = crate::session_push::default_branch_fetch_args(&tracked("origin", "main"));
        assert_eq!(args, canonical_fetch_args("origin", "main").unwrap());
        assert_eq!(
            resource_scope(&args).as_deref(),
            Some("git-ref/refs/heads/main")
        );
        assert_eq!(
            local_destination_ref(&args).as_deref(),
            Some("refs/remotes/origin/main")
        );
        assert!(is_repeatable_tracking_fetch(&args));

        let nested =
            crate::session_push::default_branch_fetch_args(&tracked("origin", "release/2"));
        assert_eq!(
            resource_scope(&nested).as_deref(),
            Some("git-ref/refs/heads/release/2")
        );

        let unusual = crate::session_push::default_branch_fetch_args(&tracked("origin", "a@{b"));
        assert_eq!(
            unusual,
            vec![
                "fetch",
                "origin",
                "refs/heads/a@{b:refs/remotes/origin/a@{b"
            ]
        );
        assert_eq!(resource_scope(&unusual), None);
    }

    /// The shared read lanes `submit` holds for its short history-view checks
    /// admit every narrow writer and exclude only exclusive (wide) ones, and
    /// they yield to a wide writer already waiting at its intent lock.
    #[test]
    fn shared_read_lanes_admit_narrow_writers_and_exclude_wide_ones() {
        let root = repository_with_origin("https://github.com/example/repo.git");
        let root = root.path().canonicalize().unwrap();
        let checkout = crate::GitRepo::discover(&root).unwrap();
        let common_key = common_lock_key(&root).unwrap();
        let (repository, read) =
            crate::operations::try_acquire_local_repository_read_lanes(&root, &checkout)
                .unwrap()
                .expect("uncontended read lanes");
        // A narrow writer's lanes: wide intent passed shared, then both lanes
        // shared, exactly as `run_coordinated_operation` takes them.
        for key in [
            wide_intent_key(&common_key),
            common_key.clone(),
            wide_intent_key(&repository),
            repository.clone(),
        ] {
            let narrow = RepositoryWriteLock::acquire_shared(
                &root,
                &key,
                (21, Some(7)),
                || Ok("test".into()),
                QueueWait::Refuse,
                |_, _| {},
            );
            assert!(narrow.is_ok(), "a narrow writer waited on {key}");
        }
        assert!(
            RepositoryWriteLock::try_acquire_nonblocking(&root, &common_key)
                .unwrap()
                .is_none(),
            "a wide writer must still be excluded while the read lanes are held"
        );
        drop(read);

        let waiting_wide_writer =
            RepositoryWriteLock::try_acquire_nonblocking(&root, &wide_intent_key(&common_key))
                .unwrap()
                .expect("an uncontended wide intent");
        assert!(
            crate::operations::try_acquire_local_repository_read_lanes(&root, &checkout)
                .unwrap()
                .is_none(),
            "the read lanes must yield to a wide writer waiting at its intent"
        );
        drop(waiting_wide_writer);
    }

    /// `queue_wait_ms` is the whole wait, not only the last lock's: a 15 s
    /// common-directory wait used to journal `queue_wait_ms: 0`.
    #[test]
    fn queue_wait_includes_every_lane_and_the_lane_decision_is_recorded() {
        let root = tempfile::tempdir().unwrap();
        let lock = RepositoryWriteLock::try_acquire_nonblocking(root.path(), "example/repo")
            .unwrap()
            .unwrap();
        let lanes = LaneTelemetry {
            narrow_lane: Some(NarrowLaneDecision {
                kind: "push",
                unavailable: Some("repository_extension"),
            }),
            git_common_lane_wait_ms: Some(15_000),
            ..LaneTelemetry::default()
        };
        let timing = coordination_timing_value(
            "example/repo",
            Some(lock.acquired_at_ms - 15_000),
            Some(&lock),
            false,
            None,
            &lanes,
        );
        assert!(
            timing["queue_wait_ms"].as_i64().unwrap() >= 15_000,
            "{timing}"
        );
        assert_eq!(timing["lock_queue_wait_ms"], 0);
        assert_eq!(timing["git_common_lane_wait_ms"], 15_000);
        assert_eq!(timing["narrow_lane"], "unavailable");
        assert_eq!(timing["narrow_lane_kind"], "push");
        assert_eq!(
            timing["narrow_lane_unavailable_reason"],
            "repository_extension"
        );

        let taken = NarrowLaneDecision {
            kind: "commit",
            unavailable: None,
        };
        assert_eq!(taken.phase(), "commit_narrow_lane_taken");
        assert_eq!(
            lanes.narrow_lane.unwrap().phase(),
            "push_narrow_lane_unavailable"
        );
        assert_eq!(
            narrow_lane_kind(
                OperationProvider::Git,
                OperationEffect::Write,
                &["commit".to_owned(), "-m".to_owned(), "x".to_owned()]
            ),
            Some("commit")
        );
        assert_eq!(
            narrow_lane_kind(
                OperationProvider::Git,
                OperationEffect::Write,
                &["add".to_owned(), "x".to_owned()]
            ),
            None
        );
    }
}
