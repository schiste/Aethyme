//! Durable coordination for Git and GitHub CLI operations.
//!
//! The broker does not reimplement either CLI. It fixes the executable,
//! classifies the requested argv, journals a redacted intent, serializes
//! repository writes with `flock`, and records the outcome. A process death
//! after the command starts becomes `outcome_unknown`; later writes fail
//! closed until an operator reconciles that journal row.

use std::collections::BTreeSet;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::json;

use crate::broker::{Broker, BrokerOpError};
use crate::types::{
    CoordinatedOperation, NewCoordinatedOperation, OperationEffect, OperationIdentityProvenance,
    OperationProvider, OperationStatus,
};

#[derive(Debug, Clone)]
pub struct CoordinatedCommand {
    pub session_id: i64,
    pub provider: OperationProvider,
    /// Required for GitHub operations and remote Git operations. `owner/repo`.
    pub repository: Option<String>,
    /// Broker-resolved identity for an internal remote Git workflow. This is
    /// distinct from the caller's `--repo owner/name` assertion.
    pub resolved_target: Option<crate::ResolvedRemoteTarget>,
    /// Audit scope. V1 deliberately locks the whole repository regardless.
    pub scope: Option<String>,
    pub declared_effect: Option<OperationEffect>,
    pub destructive_confirmed: bool,
    /// Required for writes; identifies the user request or documented workflow.
    pub authorization_reason: Option<String>,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CoordinatedOperationReport {
    pub operation: CoordinatedOperation,
    pub classification: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_target: Option<crate::ResolvedRemoteTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub github_target: Option<crate::ResolvedGithubTarget>,
    pub command_success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl CoordinatedOperationReport {
    pub fn ok(&self) -> bool {
        self.operation.status == OperationStatus::Succeeded
    }

    pub fn unknown_outcome_recovery(&self) -> Option<UnknownOutcomeRecovery> {
        (self.operation.status == OperationStatus::OutcomeUnknown)
            .then(|| UnknownOutcomeRecovery::from_operation(&self.operation))
    }
}

#[derive(Debug, Clone, serde::Serialize)]
struct ExactPushDestination {
    destination_ref: String,
    pre_push_sha: Option<String>,
    proposed_sha: String,
}

#[derive(Debug, Clone, serde::Serialize)]
struct ExactPushPlan {
    remote: String,
    destinations: Vec<ExactPushDestination>,
}

#[derive(Debug, Clone)]
enum PushPlanning {
    NotApplicable,
    Unsupported { reason: &'static str },
    Unavailable { reason: &'static str },
    Planned(ExactPushPlan),
}

impl PushPlanning {
    fn journal_value(&self) -> Option<serde_json::Value> {
        match self {
            Self::NotApplicable => None,
            Self::Unsupported { reason } => Some(json!({
                "planning": "unsupported",
                "reason": reason,
            })),
            Self::Unavailable { reason } => Some(json!({
                "planning": "unavailable",
                "reason": reason,
            })),
            Self::Planned(plan) => Some(json!({
                "planning": "planned",
                "plan": plan,
            })),
        }
    }
}

/// Complete operator handoff for a write whose external outcome is unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownOutcomeRecovery {
    pub canonical_repository: String,
    pub operation_id: i64,
    pub provider: OperationProvider,
    pub scope: String,
    pub remote_write: bool,
}

impl UnknownOutcomeRecovery {
    pub fn from_operation(operation: &CoordinatedOperation) -> Self {
        Self {
            canonical_repository: operation.repository.clone(),
            operation_id: operation.id,
            provider: operation.provider,
            scope: operation.scope.clone(),
            remote_write: operation.host_operation_id.is_some(),
        }
    }

    fn inspection_instruction(&self) -> String {
        match self.provider {
            OperationProvider::Git if !self.remote_write => format!(
                "Inspect local Git refs and worktree state for {} at scope {} to determine whether the write took effect.",
                self.canonical_repository, self.scope
            ),
            OperationProvider::Git => format!(
                "Inspect remote Git refs for canonical repository {} at scope {} to determine whether the write took effect.",
                self.canonical_repository, self.scope
            ),
            OperationProvider::Github => format!(
                "Inspect GitHub state for canonical repository {} at scope {} to determine whether the write took effect.",
                self.canonical_repository, self.scope
            ),
        }
    }

    pub fn succeeded_command(&self) -> String {
        format!(
            "aethyme broker operations reconcile --operation {} --outcome succeeded --reason \"external inspection confirmed operation {} took effect\"",
            self.operation_id, self.operation_id
        )
    }

    pub fn failed_command(&self) -> String {
        format!(
            "aethyme broker operations reconcile --operation {} --outcome failed --reason \"external inspection confirmed operation {} did not take effect\"",
            self.operation_id, self.operation_id
        )
    }
}

impl fmt::Display for UnknownOutcomeRecovery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            formatter,
            "Canonical repository {} is now write-blocked because a coordinated write has an unknown outcome.",
            self.canonical_repository
        )?;
        writeln!(formatter, "Operation ID: {}", self.operation_id)?;
        writeln!(formatter, "{}", self.inspection_instruction())?;
        writeln!(
            formatter,
            "If external inspection proves the write succeeded, run:"
        )?;
        writeln!(formatter, "  {}", self.succeeded_command())?;
        writeln!(
            formatter,
            "If external inspection proves the write failed, run:"
        )?;
        writeln!(formatter, "  {}", self.failed_command())?;
        write!(
            formatter,
            "Blind retry is forbidden until operation {} is reconciled.",
            self.operation_id
        )
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct OperationReconcileReport {
    pub operation: CoordinatedOperation,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationReconciliationState {
    NotRequired,
    Required,
    ReconciledSucceeded,
    ReconciledFailed,
}

impl OperationReconciliationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotRequired => "not_required",
            Self::Required => "required",
            Self::ReconciledSucceeded => "reconciled_succeeded",
            Self::ReconciledFailed => "reconciled_failed",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct OperationReconciliationRecovery {
    pub inspection: String,
    pub succeeded_command: String,
    pub failed_command: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct OperationReconciliation {
    pub state: OperationReconciliationState,
    pub required: bool,
    pub write_blocked: bool,
    /// The broker never turns an inspection result into an automatic retry.
    pub automatic_retry_allowed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operator_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery: Option<OperationReconciliationRecovery>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct OperationShowReport {
    pub operation: CoordinatedOperation,
    pub reconciliation: OperationReconciliation,
}

impl OperationShowReport {
    fn from_operation(operation: CoordinatedOperation) -> Self {
        let details = operation
            .details_json
            .as_deref()
            .and_then(|details| serde_json::from_str::<serde_json::Value>(details).ok());
        let state = match operation.status {
            OperationStatus::OutcomeUnknown => OperationReconciliationState::Required,
            OperationStatus::ReconciledSucceeded => {
                OperationReconciliationState::ReconciledSucceeded
            }
            OperationStatus::ReconciledFailed => OperationReconciliationState::ReconciledFailed,
            _ => OperationReconciliationState::NotRequired,
        };
        let recovery = (state == OperationReconciliationState::Required).then(|| {
            let recovery = UnknownOutcomeRecovery::from_operation(&operation);
            OperationReconciliationRecovery {
                inspection: recovery.inspection_instruction(),
                succeeded_command: recovery.succeeded_command(),
                failed_command: recovery.failed_command(),
            }
        });
        let evidence = details
            .as_ref()
            .and_then(|details| details.get("push_reconciliation"))
            .cloned();
        let operator_reason = details.as_ref().and_then(|details| {
            details
                .get("reconciliation")
                .and_then(|reconciliation| reconciliation.get("operator_reason"))
                .or_else(|| details.get("operator_reason"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });
        Self {
            operation,
            reconciliation: OperationReconciliation {
                state,
                required: state == OperationReconciliationState::Required,
                write_blocked: state == OperationReconciliationState::Required,
                automatic_retry_allowed: false,
                evidence,
                operator_reason,
                recovery,
            },
        }
    }
}

struct RepositoryWriteLock {
    file: File,
}

impl RepositoryWriteLock {
    fn acquire(main_root: &Path, repository: &str) -> Result<Self, BrokerOpError> {
        let dir = main_root.join(".aethyme/locks/operations");
        std::fs::create_dir_all(&dir).map_err(|source| BrokerOpError::OperationIo {
            path: dir.clone(),
            source,
        })?;
        let path = dir.join(format!("{:016x}.lock", stable_hash(repository.as_bytes())));
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| BrokerOpError::OperationIo {
                path: path.clone(),
                source,
            })?;
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if rc != 0 {
            return Err(BrokerOpError::OperationIo {
                path,
                source: std::io::Error::last_os_error(),
            });
        }
        Ok(Self { file })
    }
}

impl Drop for RepositoryWriteLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn validate_repository(value: &str) -> Result<(), BrokerOpError> {
    let mut parts = value.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    let valid_component = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    };
    if !valid_component(owner) || !valid_component(name) || parts.next().is_some() {
        return Err(BrokerOpError::InvalidCoordinatedOperation {
            reason: format!("repository must be an exact owner/name slug, got {value:?}"),
        });
    }
    Ok(())
}

fn validate_scope(value: &str) -> Result<(), BrokerOpError> {
    if value.is_empty() || value.len() > 256 || value.chars().any(|ch| matches!(ch, '\n' | '\r')) {
        return Err(BrokerOpError::InvalidCoordinatedOperation {
            reason: "scope must be 1-256 characters without newlines".into(),
        });
    }
    Ok(())
}

fn validate_authorization_reason(value: Option<&str>) -> Result<Option<String>, BrokerOpError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() || value.len() > 500 || value.chars().any(|ch| matches!(ch, '\n' | '\r')) {
        return Err(BrokerOpError::InvalidCoordinatedOperation {
            reason: "--reason must be 1-500 characters without newlines".into(),
        });
    }
    Ok(Some(value.into()))
}

fn effect_rank(effect: OperationEffect) -> u8 {
    match effect {
        OperationEffect::Read => 0,
        OperationEffect::Write => 1,
        OperationEffect::Destructive => 2,
    }
}

fn resolve_effect(
    inferred: Option<OperationEffect>,
    declared: Option<OperationEffect>,
) -> Result<(OperationEffect, &'static str), BrokerOpError> {
    match (inferred, declared) {
        (Some(inferred), Some(declared)) if effect_rank(declared) < effect_rank(inferred) => {
            Err(BrokerOpError::InvalidCoordinatedOperation {
                reason: format!(
                    "--effect {} cannot downgrade inferred {} operation",
                    declared.as_str(),
                    inferred.as_str()
                ),
            })
        }
        (Some(_), Some(declared)) => Ok((declared, "declared")),
        (Some(inferred), None) => Ok((inferred, "inferred")),
        (None, Some(declared)) => Ok((declared, "declared")),
        (None, None) => Err(BrokerOpError::InvalidCoordinatedOperation {
            reason: "operation is ambiguous; declare --effect read|write|destructive and --scope"
                .into(),
        }),
    }
}

fn has_any(args: &[String], needles: &[&str]) -> bool {
    args.iter().any(|arg| needles.contains(&arg.as_str()))
}

fn git_subcommand_args(args: &[String]) -> Option<&[String]> {
    let mut index = 0;
    while args.get(index).is_some_and(|arg| arg == "-C") {
        if args.get(index + 1).is_none_or(|path| path.is_empty()) {
            return None;
        }
        index += 2;
    }
    args.get(index..).filter(|remaining| !remaining.is_empty())
}

fn git_explicit_directory(args: &[String], cwd: &Path) -> Result<Option<PathBuf>, BrokerOpError> {
    let mut index = 0;
    let mut directory = cwd.to_path_buf();
    let mut explicit = false;
    while args.get(index).is_some_and(|arg| arg == "-C") {
        let path =
            args.get(index + 1)
                .ok_or_else(|| BrokerOpError::InvalidCoordinatedOperation {
                    reason: "git -C requires a non-empty checkout path".into(),
                })?;
        if path.is_empty() {
            return Err(BrokerOpError::InvalidCoordinatedOperation {
                reason: "git -C requires a non-empty checkout path".into(),
            });
        }
        let path = Path::new(path);
        directory = if path.is_absolute() {
            path.to_path_buf()
        } else {
            directory.join(path)
        };
        explicit = true;
        index += 2;
    }
    Ok(explicit.then_some(directory))
}

pub fn classify_git(args: &[String]) -> Option<OperationEffect> {
    let args = git_subcommand_args(args)?;
    let command = args.first()?.as_str();
    match command {
        "status" | "diff" | "log" | "show" | "rev-parse" | "ls-files" | "ls-tree" | "cat-file"
        | "grep" | "blame" | "describe" | "shortlog" | "whatchanged" | "merge-base"
        | "name-rev" | "for-each-ref" | "check-ignore" | "count-objects" | "fsck" | "help"
        | "ls-remote" | "version" | "--version" => Some(OperationEffect::Read),
        "branch" => {
            if has_any(args, &["-d", "-D", "--delete"]) {
                Some(OperationEffect::Destructive)
            } else if args.len() == 1
                || has_any(
                    args,
                    &[
                        "-l",
                        "--list",
                        "--show-current",
                        "--contains",
                        "--no-contains",
                        "--merged",
                        "--no-merged",
                    ],
                )
            {
                Some(OperationEffect::Read)
            } else {
                Some(OperationEffect::Write)
            }
        }
        "tag" => {
            if has_any(args, &["-d", "--delete"]) {
                Some(OperationEffect::Destructive)
            } else if args.len() == 1 || has_any(args, &["-l", "--list", "--contains"]) {
                Some(OperationEffect::Read)
            } else {
                Some(OperationEffect::Write)
            }
        }
        "remote" => {
            if args.len() == 1
                || has_any(args, &["-v", "--verbose", "get-url", "show"])
                    && !has_any(
                        args,
                        &["add", "remove", "rename", "set-url", "prune", "update"],
                    )
            {
                Some(OperationEffect::Read)
            } else if has_any(args, &["remove", "rm", "set-url"]) {
                Some(OperationEffect::Destructive)
            } else {
                Some(OperationEffect::Write)
            }
        }
        "push" => {
            let destructive = args.iter().any(|arg| {
                matches!(
                    arg.as_str(),
                    "-f" | "--force" | "--force-with-lease" | "--delete" | "--mirror" | "--prune"
                ) || arg.starts_with("--force-with-lease=")
                    || (arg.starts_with(':') && arg.len() > 1)
            });
            Some(if destructive {
                OperationEffect::Destructive
            } else {
                OperationEffect::Write
            })
        }
        "reset" | "clean" | "reflog"
            if has_any(args, &["delete", "expire", "--hard", "-f", "-d"]) =>
        {
            Some(OperationEffect::Destructive)
        }
        "add" | "am" | "apply" | "checkout" | "cherry-pick" | "clone" | "commit" | "fetch"
        | "gc" | "init" | "merge" | "mv" | "notes" | "pull" | "rebase" | "replace" | "restore"
        | "revert" | "rm" | "stash" | "submodule" | "switch" | "worktree" | "reset" | "clean"
        | "reflog" => Some(OperationEffect::Write),
        _ => None,
    }
}

fn gh_method(args: &[String]) -> Option<&str> {
    args.windows(2)
        .find(|pair| matches!(pair[0].as_str(), "-X" | "--method"))
        .map(|pair| pair[1].as_str())
        .or_else(|| args.iter().find_map(|arg| arg.strip_prefix("--method=")))
}

pub fn classify_gh(args: &[String]) -> Option<OperationEffect> {
    let command = args.first()?.as_str();
    let action = args.get(1).map(String::as_str);
    if command == "api" {
        let method = gh_method(args).unwrap_or_else(|| {
            if has_any(args, &["-f", "--raw-field", "-F", "--field", "--input"]) {
                "POST"
            } else {
                "GET"
            }
        });
        return Some(match method.to_ascii_uppercase().as_str() {
            "GET" | "HEAD" => OperationEffect::Read,
            "DELETE" => OperationEffect::Destructive,
            _ => OperationEffect::Write,
        });
    }
    let read_actions = [
        "list", "view", "status", "diff", "checks", "watch", "download", "get", "token",
    ];
    let destructive_actions = ["delete", "remove", "archive"];
    match command {
        "search" | "status" | "browse" => Some(OperationEffect::Read),
        "auth" => match action {
            Some("status" | "token") => Some(OperationEffect::Read),
            Some("login" | "logout" | "refresh" | "setup-git") => Some(OperationEffect::Write),
            _ => None,
        },
        "pr" | "issue" | "run" | "workflow" | "release" | "repo" | "secret" | "variable"
        | "label" | "cache" | "codespace" | "ssh-key" | "gpg-key" => {
            if action.is_some_and(|value| destructive_actions.contains(&value)) {
                Some(OperationEffect::Destructive)
            } else if action.is_some_and(|value| read_actions.contains(&value)) {
                Some(OperationEffect::Read)
            } else if action.is_some() {
                Some(OperationEffect::Write)
            } else {
                None
            }
        }
        "project" => match action {
            Some("list" | "view" | "item-list" | "field-list") => Some(OperationEffect::Read),
            Some(_) => Some(OperationEffect::Write),
            None => None,
        },
        "extension" | "alias" | "config" => None,
        _ => None,
    }
}

fn remote_git_operation(args: &[String]) -> bool {
    match args.first().map(String::as_str) {
        Some("clone" | "fetch" | "pull" | "push" | "ls-remote" | "submodule") => true,
        Some("remote") => args
            .get(1)
            .is_some_and(|arg| matches!(arg.as_str(), "show" | "prune" | "update")),
        _ => false,
    }
}

fn journal_details(
    classification: &'static str,
    resolved_target: Option<&crate::ResolvedRemoteTarget>,
    github_target: Option<&crate::ResolvedGithubTarget>,
    extra: serde_json::Value,
) -> serde_json::Value {
    let mut details = json!({ "classification": classification });
    if let Some(target) = resolved_target {
        details["resolved_target"] = json!(target);
    }
    if let Some(target) = github_target {
        details["github_target"] = json!(target);
    }
    if let (Some(details), Some(extra)) = (details.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            details.insert(key.clone(), value.clone());
        }
    }
    details
}

fn with_push_planning(mut extra: serde_json::Value, planning: &PushPlanning) -> serde_json::Value {
    if let (Some(extra), Some(push)) = (extra.as_object_mut(), planning.journal_value()) {
        extra.insert("push_reconciliation".into(), push);
    }
    extra
}

fn plan_exact_push(
    cwd: &Path,
    args: &[String],
    target: Option<&crate::ResolvedRemoteTarget>,
) -> PushPlanning {
    if args.first().map(String::as_str) != Some("push") {
        return PushPlanning::NotApplicable;
    }
    let Some(target) = target else {
        return PushPlanning::Unsupported {
            reason: "push_target_is_not_a_resolved_remote",
        };
    };
    if args.iter().any(|argument| {
        matches!(
            argument.as_str(),
            "--all"
                | "--delete"
                | "--dry-run"
                | "--follow-tags"
                | "--mirror"
                | "--prune"
                | "--tags"
        )
    }) {
        return PushPlanning::Unsupported {
            reason: "push_uses_implicit_or_set_expanding_options",
        };
    }
    let remote_positions = args
        .iter()
        .enumerate()
        .skip(1)
        .filter_map(|(index, argument)| (argument == &target.remote_name).then_some(index))
        .collect::<Vec<_>>();
    let [remote_index] = remote_positions.as_slice() else {
        return PushPlanning::Unsupported {
            reason: "push_remote_position_is_not_unique",
        };
    };
    let refspecs = &args[*remote_index + 1..];
    if refspecs.is_empty()
        || refspecs
            .iter()
            .any(|refspec| refspec.starts_with('-') || refspec == "--")
    {
        return PushPlanning::Unsupported {
            reason: "push_does_not_have_only_explicit_refspecs",
        };
    }

    let Ok(repo) = crate::GitRepo::discover(cwd) else {
        return PushPlanning::Unavailable {
            reason: "local_repository_evidence_unavailable",
        };
    };
    let mut seen_destinations = BTreeSet::new();
    let mut destinations = Vec::with_capacity(refspecs.len());
    for refspec in refspecs {
        let refspec = refspec.strip_prefix('+').unwrap_or(refspec);
        let Some((source, destination)) = refspec.split_once(':') else {
            return PushPlanning::Unsupported {
                reason: "push_refspec_is_not_explicit_source_and_destination",
            };
        };
        if source.is_empty()
            || destination.is_empty()
            || source.starts_with('-')
            || source.contains(':')
            || destination.contains(':')
            || !destination.starts_with("refs/")
            || !seen_destinations.insert(destination.to_string())
            || repo.validate_push_destination(destination).is_err()
        {
            return PushPlanning::Unsupported {
                reason: "push_refspec_is_not_one_unique_full_destination",
            };
        }
        let Ok(proposed_sha) = repo.resolve_push_source(source) else {
            return PushPlanning::Unavailable {
                reason: "push_source_object_is_unavailable",
            };
        };
        if proposed_sha.len() != 40 || !proposed_sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return PushPlanning::Unavailable {
                reason: "push_source_object_is_not_a_full_sha",
            };
        }
        destinations.push(ExactPushDestination {
            destination_ref: destination.into(),
            pre_push_sha: None,
            proposed_sha: proposed_sha.to_ascii_lowercase(),
        });
    }

    let destination_refs = destinations
        .iter()
        .map(|destination| destination.destination_ref.clone())
        .collect::<Vec<_>>();
    let Ok(pre_push) = repo.remote_ref_oids(&target.remote_name, &destination_refs) else {
        return PushPlanning::Unavailable {
            reason: "pre_push_remote_evidence_unavailable",
        };
    };
    for destination in &mut destinations {
        destination.pre_push_sha = pre_push
            .get(&destination.destination_ref)
            .cloned()
            .flatten();
    }
    PushPlanning::Planned(ExactPushPlan {
        remote: target.remote_name.clone(),
        destinations,
    })
}

fn reconcile_failed_push(
    cwd: &Path,
    planning: &PushPlanning,
) -> Option<(OperationStatus, serde_json::Value)> {
    let PushPlanning::Planned(plan) = planning else {
        return planning.journal_value().map(|mut value| {
            value["evidence"] = json!({
                "classification": "unknown",
                "reason": "exact_push_plan_unavailable",
            });
            (OperationStatus::OutcomeUnknown, value)
        });
    };
    let Ok(repo) = crate::GitRepo::discover(cwd) else {
        let mut value = planning.journal_value().expect("planned push");
        value["evidence"] = json!({
            "classification": "unknown",
            "reason": "local_repository_evidence_unavailable",
        });
        return Some((OperationStatus::OutcomeUnknown, value));
    };
    let destination_refs = plan
        .destinations
        .iter()
        .map(|destination| destination.destination_ref.clone())
        .collect::<Vec<_>>();
    let Ok(observed) = repo.remote_ref_oids(&plan.remote, &destination_refs) else {
        let mut value = planning.journal_value().expect("planned push");
        value["evidence"] = json!({
            "classification": "unknown",
            "reason": "post_push_remote_evidence_unavailable",
        });
        return Some((OperationStatus::OutcomeUnknown, value));
    };

    let mut all_pre_push = true;
    let mut all_proposed = true;
    let mut every_observation_is_expected = true;
    let observations = plan
        .destinations
        .iter()
        .map(|destination| {
            let observed_sha = observed
                .get(&destination.destination_ref)
                .cloned()
                .flatten();
            all_pre_push &= observed_sha == destination.pre_push_sha;
            all_proposed &= observed_sha.as_deref() == Some(destination.proposed_sha.as_str());
            every_observation_is_expected &= observed_sha == destination.pre_push_sha
                || observed_sha.as_deref() == Some(destination.proposed_sha.as_str());
            json!({
                "destination_ref": destination.destination_ref,
                "observed_sha": observed_sha,
            })
        })
        .collect::<Vec<_>>();
    let (status, classification) = if all_proposed {
        (OperationStatus::Succeeded, "succeeded")
    } else if all_pre_push {
        (OperationStatus::Failed, "failed")
    } else if every_observation_is_expected {
        (OperationStatus::OutcomeUnknown, "partial")
    } else {
        (OperationStatus::OutcomeUnknown, "unknown")
    };
    let mut value = planning.journal_value().expect("planned push");
    value["evidence"] = json!({
        "classification": classification,
        "destinations": observations,
    });
    Some((status, value))
}

fn redacted_command(provider: OperationProvider, args: &[String]) -> Result<String, BrokerOpError> {
    let sensitive_flags = [
        "-m",
        "--message",
        "--body",
        "--body-file",
        "--title",
        "--notes",
        "--notes-file",
        "--description",
        "--token",
        "--password",
        "--client-secret",
        "--value",
        "-f",
        "-F",
        "--field",
        "--raw-field",
        "--input",
    ];
    let executable = match provider {
        OperationProvider::Git => "git",
        OperationProvider::Github => "gh",
    };
    let mut redacted = vec![executable.to_string()];
    let mut hide_next = false;
    for arg in args {
        if hide_next {
            redacted.push("[REDACTED]".into());
            hide_next = false;
            continue;
        }
        if sensitive_flags.contains(&arg.as_str()) {
            redacted.push(arg.clone());
            hide_next = true;
        } else if sensitive_flags
            .iter()
            .any(|flag| arg.starts_with(&format!("{flag}=")))
        {
            let flag = arg.split('=').next().unwrap_or(arg);
            redacted.push(format!("{flag}=[REDACTED]"));
        } else if arg.contains("://") && arg.contains('@') {
            redacted.push("[REDACTED_URL]".into());
        } else {
            redacted.push(arg.clone());
        }
    }
    Ok(serde_json::to_string(&redacted)?)
}

impl Broker {
    pub fn show_coordinated_operation(
        &mut self,
        operation_id: i64,
    ) -> Result<OperationShowReport, BrokerOpError> {
        let operation = self.store().coordinated_operation(operation_id)?.ok_or(
            crate::BrokerError::CoordinatedOperationNotFound(operation_id),
        )?;
        Ok(OperationShowReport::from_operation(operation))
    }

    pub fn run_coordinated_operation(
        &mut self,
        request: CoordinatedCommand,
    ) -> Result<CoordinatedOperationReport, BrokerOpError> {
        let session = self.store().session(request.session_id)?;
        if session.status.is_closed() {
            return Err(BrokerOpError::ClosedSessionOperation {
                session_id: session.id,
            });
        }
        self.run_coordinated_operation_at(request, Path::new(&session.worktree_path))
    }

    pub(crate) fn run_coordinated_operation_at(
        &mut self,
        request: CoordinatedCommand,
        cwd: &Path,
    ) -> Result<CoordinatedOperationReport, BrokerOpError> {
        self.run_coordinated_operation_at_with_hooks(request, cwd, || Ok(()), |_, _| Ok(None))
    }

    /// Execute through the normal coordinated-operation state machine while
    /// allowing a caller to revalidate local state under the repository lock,
    /// then durably journal structured successful stdout before success.
    pub(crate) fn run_coordinated_operation_at_with_hooks<P, F>(
        &mut self,
        request: CoordinatedCommand,
        cwd: &Path,
        pre_execute: P,
        on_success: F,
    ) -> Result<CoordinatedOperationReport, BrokerOpError>
    where
        P: FnOnce() -> Result<(), String>,
        F: FnOnce(&[u8], i64) -> Result<Option<serde_json::Value>, String>,
    {
        if request.args.is_empty() {
            return Err(BrokerOpError::InvalidCoordinatedOperation {
                reason: format!(
                    "broker {} requires arguments after --",
                    request.provider.as_str()
                ),
            });
        }
        if request.provider == OperationProvider::Git
            && let Some(directory) = git_explicit_directory(&request.args, cwd)?
        {
            let selected = crate::GitRepo::discover(&directory)?;
            if selected.git_common_dir()? != self.repo_handle().git_common_dir()? {
                return Err(BrokerOpError::InvalidCoordinatedOperation {
                    reason: format!(
                        "git -C target {:?} is outside this broker repository; run the operation through that repository's broker",
                        directory
                    ),
                });
            }
        }
        let github_target = if request.provider == OperationProvider::Github {
            request
                .repository
                .as_deref()
                .map(|repository| crate::resolve_github_target(repository, &request.args))
                .transpose()?
        } else {
            None
        };
        let inferred = match request.provider {
            OperationProvider::Git => classify_git(&request.args),
            OperationProvider::Github => classify_gh(&request.args),
        };
        let (effect, classification) = resolve_effect(inferred, request.declared_effect)?;
        if effect == OperationEffect::Destructive && !request.destructive_confirmed {
            return Err(BrokerOpError::InvalidCoordinatedOperation {
                reason:
                    "destructive operation requires --destructive after resolving exact targets"
                        .into(),
            });
        }
        let authorization_reason =
            validate_authorization_reason(request.authorization_reason.as_deref())?;
        if effect != OperationEffect::Read && authorization_reason.is_none() {
            return Err(BrokerOpError::InvalidCoordinatedOperation {
                reason: "coordinated writes require --reason identifying their authorization"
                    .into(),
            });
        }

        let is_remote_git =
            request.provider == OperationProvider::Git && remote_git_operation(&request.args);
        let resolved_target = match (
            &request.resolved_target,
            &request.repository,
            request.provider,
        ) {
            (Some(expected), None, OperationProvider::Git) if is_remote_git => {
                let repo = crate::GitRepo::discover(cwd)?;
                let actual = repo.resolve_remote_command_target(&request.args, None)?;
                if actual.remote_name != expected.remote_name
                    || actual.coordination_key != expected.coordination_key
                {
                    return Err(BrokerOpError::InvalidCoordinatedOperation {
                        reason: format!(
                            "Git command resolved to {} via remote {:?}, but the internal workflow authorized {} via remote {:?}",
                            actual.coordination_key,
                            actual.remote_name,
                            expected.coordination_key,
                            expected.remote_name
                        ),
                    });
                }
                Some(actual)
            }
            (Some(_), _, _) => {
                return Err(BrokerOpError::InvalidCoordinatedOperation {
                    reason: "a resolved remote target is only valid for internal Git operations without a second repository assertion".into(),
                });
            }
            (None, Some(repository), OperationProvider::Git) if is_remote_git => {
                validate_repository(repository)?;
                let repo = crate::GitRepo::discover(cwd)?;
                Some(repo.resolve_remote_command_target(&request.args, Some(repository))?)
            }
            (None, Some(_), OperationProvider::Github) => {
                debug_assert!(github_target.is_some());
                None
            }
            (None, Some(repository), OperationProvider::Git) => {
                validate_repository(repository)?;
                None
            }
            (None, None, OperationProvider::Github) => {
                return Err(BrokerOpError::InvalidCoordinatedOperation {
                    reason: "broker gh requires --repo owner/name".into(),
                });
            }
            (None, None, OperationProvider::Git) if is_remote_git => {
                return Err(BrokerOpError::InvalidCoordinatedOperation {
                    reason: "remote Git operation requires --repo owner/name".into(),
                });
            }
            (None, None, OperationProvider::Git) => None,
        };
        let repository = match (&resolved_target, &request.repository, request.provider) {
            (Some(target), _, OperationProvider::Git) => target.coordination_key.clone(),
            (None, Some(_), OperationProvider::Github) => github_target
                .as_ref()
                .expect("resolved GitHub target")
                .coordination_key
                .clone(),
            (None, Some(repository), OperationProvider::Git) => repository.clone(),
            (None, None, OperationProvider::Git) => {
                format!("local:{}", self.main_root().display())
            }
            (Some(_), _, OperationProvider::Github) => unreachable!("validated above"),
            (None, None, OperationProvider::Github) => unreachable!("validated above"),
        };
        let scope_was_declared = request.scope.is_some();
        let scope = request.scope.unwrap_or_else(|| "repository".into());
        validate_scope(&scope)?;
        if inferred.is_none() && !scope_was_declared {
            return Err(BrokerOpError::InvalidCoordinatedOperation {
                reason: "ambiguous operation requires an explicit --scope as well as --effect"
                    .into(),
            });
        }

        let is_remote_write = effect != OperationEffect::Read
            && (resolved_target.is_some() || github_target.is_some());
        let _lock = if effect == OperationEffect::Read {
            None
        } else {
            Some(RepositoryWriteLock::acquire(self.main_root(), &repository)?)
        };
        if effect != OperationEffect::Read {
            let unresolved = self
                .store()
                .unresolved_coordinated_operations(&repository)?;
            for operation in unresolved {
                match operation.status {
                    OperationStatus::Prepared => {
                        self.store().transition_coordinated_operation(
                            operation.id,
                            OperationStatus::Failed,
                            None,
                            Some(r#"{"reason":"abandoned_before_start"}"#),
                        )?;
                    }
                    OperationStatus::Running => {
                        let operation = self.store().transition_coordinated_operation(
                            operation.id,
                            OperationStatus::OutcomeUnknown,
                            operation.exit_code,
                            Some(r#"{"reason":"process_ended_without_outcome"}"#),
                        )?;
                        return Err(BrokerOpError::CoordinatedOperationBlocked {
                            repository,
                            operation_id: operation.id,
                            recovery: UnknownOutcomeRecovery::from_operation(&operation),
                        });
                    }
                    OperationStatus::OutcomeUnknown => {
                        let recovery = UnknownOutcomeRecovery::from_operation(&operation);
                        return Err(BrokerOpError::CoordinatedOperationBlocked {
                            repository,
                            operation_id: operation.id,
                            recovery,
                        });
                    }
                    _ => {}
                }
            }
        }
        let mut host_guard = if is_remote_write {
            Some(crate::HostOperationGuard::begin(
                &self.host_operation_database_path()?,
                &repository,
                request.provider,
                effect,
            )?)
        } else {
            None
        };
        pre_execute().map_err(|reason| BrokerOpError::InvalidCoordinatedOperation { reason })?;
        let push_planning = if request.provider == OperationProvider::Git {
            plan_exact_push(cwd, &request.args, resolved_target.as_ref())
        } else {
            PushPlanning::NotApplicable
        };

        let command_json = redacted_command(request.provider, &request.args)?;
        let operation = self
            .store()
            .create_coordinated_operation(&NewCoordinatedOperation {
                session_id: request.session_id,
                provider: request.provider,
                repository: repository.clone(),
                scope,
                effect,
                authorization_reason,
                command_json,
                pid: i64::from(std::process::id()),
                host_operation_id: host_guard
                    .as_ref()
                    .map(|guard| guard.operation().operation_id.clone()),
                identity_provenance: if resolved_target.is_some() || github_target.is_some() {
                    OperationIdentityProvenance::VerifiedCanonical
                } else {
                    OperationIdentityProvenance::LocalRepository
                },
            })?;
        if let Some(guard) = &mut host_guard {
            guard.mark_running()?;
        }
        self.store().transition_coordinated_operation(
            operation.id,
            OperationStatus::Running,
            None,
            Some(
                &journal_details(
                    classification,
                    resolved_target.as_ref(),
                    github_target.as_ref(),
                    with_push_planning(json!({}), &push_planning),
                )
                .to_string(),
            ),
        )?;

        let executable = match request.provider {
            OperationProvider::Git => "git",
            OperationProvider::Github => "gh",
        };
        let mut command = Command::new(executable);
        command
            .args(&request.args)
            .current_dir(cwd)
            .stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("AETHYME_BROKER_SESSION_ID", request.session_id.to_string())
            .env("AETHYME_BROKER_OPERATION_ID", operation.id.to_string());
        if request.provider == OperationProvider::Github {
            command.env(
                "GH_REPO",
                &github_target
                    .as_ref()
                    .expect("resolved GitHub target")
                    .display_slug,
            );
        }
        let output = match command.output() {
            Ok(output) => output,
            Err(source) => {
                let operation = self.store().transition_coordinated_operation(
                    operation.id,
                    OperationStatus::Failed,
                    None,
                    Some(
                        &journal_details(
                            classification,
                            resolved_target.as_ref(),
                            github_target.as_ref(),
                            with_push_planning(json!({ "reason": "spawn_failed" }), &push_planning),
                        )
                        .to_string(),
                    ),
                )?;
                if let Some(guard) = &mut host_guard {
                    guard.finish(operation.status)?;
                }
                return Err(BrokerOpError::OperationSpawn {
                    executable: executable.into(),
                    source,
                });
            }
        };
        let exit_code = output.status.code().map(i64::from);
        let (status, details) = if output.status.success() {
            match on_success(&output.stdout, operation.id) {
                Ok(Some(result)) => (
                    OperationStatus::Succeeded,
                    journal_details(
                        classification,
                        resolved_target.as_ref(),
                        github_target.as_ref(),
                        with_push_planning(json!({ "result": result }), &push_planning),
                    ),
                ),
                Ok(None) => (
                    OperationStatus::Succeeded,
                    journal_details(
                        classification,
                        resolved_target.as_ref(),
                        github_target.as_ref(),
                        with_push_planning(json!({}), &push_planning),
                    ),
                ),
                Err(reason) => (
                    OperationStatus::OutcomeUnknown,
                    journal_details(
                        classification,
                        resolved_target.as_ref(),
                        github_target.as_ref(),
                        with_push_planning(
                            json!({
                                "reason": "success_result_not_recorded",
                                "diagnosis": reason,
                            }),
                            &push_planning,
                        ),
                    ),
                ),
            }
        } else if effect == OperationEffect::Read {
            (
                OperationStatus::Failed,
                journal_details(
                    classification,
                    resolved_target.as_ref(),
                    github_target.as_ref(),
                    json!({}),
                ),
            )
        } else if let Some((status, push_reconciliation)) =
            reconcile_failed_push(cwd, &push_planning)
        {
            (
                status,
                journal_details(
                    classification,
                    resolved_target.as_ref(),
                    github_target.as_ref(),
                    json!({ "push_reconciliation": push_reconciliation }),
                ),
            )
        } else {
            // A mutating command may have applied a subset of its effects
            // before returning non-zero. Treating that as safely failed would
            // make a blind retry possible, so require external inspection.
            (
                OperationStatus::OutcomeUnknown,
                journal_details(
                    classification,
                    resolved_target.as_ref(),
                    github_target.as_ref(),
                    json!({}),
                ),
            )
        };
        let operation = self.store().transition_coordinated_operation(
            operation.id,
            status,
            exit_code,
            Some(&details.to_string()),
        )?;
        if let Some(guard) = &mut host_guard {
            guard.finish(operation.status)?;
        }
        Ok(CoordinatedOperationReport {
            operation,
            classification,
            resolved_target,
            github_target,
            command_success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    pub fn reconcile_coordinated_operation(
        &mut self,
        operation_id: i64,
        succeeded: bool,
        reason: &str,
    ) -> Result<OperationReconcileReport, BrokerOpError> {
        if reason.trim().is_empty() {
            return Err(BrokerOpError::InvalidCoordinatedOperation {
                reason: "operation reconciliation requires a non-empty --reason".into(),
            });
        }
        let operation = self.store().coordinated_operation(operation_id)?.ok_or(
            crate::BrokerError::CoordinatedOperationNotFound(operation_id),
        )?;
        if operation.host_operation_id.is_none()
            && operation.status != OperationStatus::OutcomeUnknown
        {
            return Err(BrokerOpError::InvalidCoordinatedOperation {
                reason: format!(
                    "operation {} is {}, not outcome_unknown",
                    operation_id,
                    operation.status.as_str()
                ),
            });
        }
        let status = if succeeded {
            OperationStatus::ReconciledSucceeded
        } else {
            OperationStatus::ReconciledFailed
        };
        if let Some(host_operation_id) = &operation.host_operation_id {
            crate::reconcile_host_operation(
                &self.host_operation_database_path()?,
                host_operation_id,
                succeeded,
            )?;
        }
        let mut details = operation
            .details_json
            .as_deref()
            .and_then(|details| serde_json::from_str::<serde_json::Value>(details).ok())
            .filter(serde_json::Value::is_object)
            .unwrap_or_else(|| json!({}));
        details["reconciliation"] = json!({
            "operator_reason": reason,
            "outcome": status.as_str(),
        });
        let operation = self.store().transition_coordinated_operation(
            operation_id,
            status,
            operation.exit_code,
            Some(&details.to_string()),
        )?;
        Ok(OperationReconcileReport {
            operation,
            reason: reason.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).into()).collect()
    }

    #[test]
    fn classifiers_fail_closed_and_detect_destructive_operations() {
        assert_eq!(
            classify_git(&args(&["status"])),
            Some(OperationEffect::Read)
        );
        assert_eq!(classify_git(&args(&["push"])), Some(OperationEffect::Write));
        assert_eq!(
            classify_git(&args(&["push", "--force-with-lease"])),
            Some(OperationEffect::Destructive)
        );
        assert_eq!(classify_git(&args(&["unknown-extension"])), None);
        assert_eq!(
            classify_git(&args(&[
                "-C",
                "/tmp/linked-worktree",
                "merge",
                "--ff-only",
                "abc123"
            ])),
            Some(OperationEffect::Write)
        );
        assert_eq!(classify_git(&args(&["-C"])), None);

        assert_eq!(
            classify_gh(&args(&["pr", "view", "12"])),
            Some(OperationEffect::Read)
        );
        assert_eq!(
            classify_gh(&args(&["pr", "merge", "12"])),
            Some(OperationEffect::Write)
        );
        assert_eq!(
            classify_gh(&args(&["api", "repos/o/r", "--method", "DELETE"])),
            Some(OperationEffect::Destructive)
        );
        assert_eq!(classify_gh(&args(&["extension", "exec", "x"])), None);
    }

    #[test]
    fn redaction_keeps_audit_shape_without_secret_values() {
        let value = redacted_command(
            OperationProvider::Github,
            &args(&["secret", "set", "TOKEN", "--body", "super-secret"]),
        )
        .unwrap();
        assert!(value.contains("[REDACTED]"));
        assert!(!value.contains("super-secret"));
    }

    #[test]
    fn github_target_cannot_be_overridden_after_the_broker_boundary() {
        let err = crate::resolve_github_target(
            "owner/repo",
            &args(&["pr", "merge", "12", "--repo", "other/repo"]),
        )
        .unwrap_err();
        assert!(err.to_string().contains("second repository target"));
    }
}
