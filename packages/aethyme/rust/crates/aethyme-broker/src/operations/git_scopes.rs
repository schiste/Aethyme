//! Closed Git profiles whose complete ref write footprint is known.
//!
//! Other Git spellings retain the repository-exclusive lane. Scoped profiles
//! additionally hold that same lane shared, so old/wide writers still exclude
//! them. Scope is derived from argv, never from the caller's audit label.

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
    // A literal URL selects an anonymous remote: no configured fetch refmap
    // can make this push update unrelated local tracking refs. Named remotes,
    // mutable source names, implicit tags and multi-ref pushes stay wide.
    let [command, no_tags, porcelain, separator, remote, refspec] = args else {
        return None;
    };
    if command != "push"
        || no_tags != "--no-follow-tags"
        || porcelain != "--porcelain"
        || separator != "--"
        || !(remote.starts_with("https://") || remote.starts_with("ssh://"))
    {
        return None;
    }
    let (source, destination) = refspec.split_once(':')?;
    if source.len() != 40 || !source.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let branch = destination.strip_prefix("refs/heads/")?;
    ref_component(branch).then(|| format!("git-ref/{destination}"))
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
    pub provider: Option<crate::artifact_use::ProcessStartIdentity>,
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

/// Reject hidden ref writers and set-expanding refmaps rather than assuming
/// that command-line operands alone constrain an arbitrary Git configuration.
pub(super) fn validate_repository(
    cwd: &Path,
    args: &[String],
    deadline: AdmissionDeadline,
    repository: &str,
) -> Result<(), BrokerOpError> {
    use std::os::unix::fs::MetadataExt;
    for name in [
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
        "GIT_SSH_COMMAND",
        "GIT_SSH",
        "GIT_PROXY_COMMAND",
    ] {
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
    let hooks: &[&str] = if fetch_footprint(args).is_some() {
        &["reference-transaction"]
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
            r"^(remote\..*\.(fetch|vcs|uploadpack|receivepack)|core\.(hookspath|sshcommand)|extensions\..*|url\..*\.(insteadof|pushinsteadof)|protocol\.ext\.allow)$",
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

#[cfg(test)]
mod tests {
    use super::*;

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
        push[4] = "origin".into();
        assert!(resource_scope(&push).is_none());
        push[4] = "https://github.com/example/repo".into();
        push[5] = "HEAD:refs/heads/main".into();
        assert!(resource_scope(&push).is_none());
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
        journal.provider = Some(crate::artifact_use::ProcessStartIdentity {
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
