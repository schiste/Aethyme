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

pub(super) fn canonical_fetch_args(remote: &str, branch: &str) -> Option<Vec<String>> {
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

fn unavailable(reason: &str) -> BrokerOpError {
    BrokerOpError::InvalidCoordinatedOperation {
        reason: format!(
            "strict Git resource profile unavailable: {reason}; use the repository-wide spelling"
        ),
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
    let named_push = strict_push(args).is_some_and(|push| push.named_remote());
    let hooks: &[&str] = if fetch_footprint(args).is_some() {
        &["reference-transaction"]
    } else if session_branch_hooks && strict_push(args).is_some() {
        &[]
    } else {
        &["reference-transaction", "pre-push"]
    };
    for hook in hooks {
        if hook_directory
            .join(hook)
            .try_exists()
            .map_err(|_| unavailable("hook state is unreadable"))?
        {
            return Err(unavailable("a hook can expand this resource footprint"));
        }
    }
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
        if key == "core.sshcommand"
            || key.starts_with("extensions.")
            || key.starts_with("url.")
            || key == "protocol.ext.allow"
            || key.ends_with(".vcs")
            || key.ends_with(".uploadpack")
            || key.ends_with(".receivepack")
        {
            return Err(unavailable(
                "custom hooks or repository extensions need the wide lane",
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
        validate_repository(cwd, args, deadline, &operation.repository)?;
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
}
