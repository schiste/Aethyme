//! Durable coordination for Git and GitHub CLI operations.
//!
//! The broker does not reimplement either CLI. It fixes the executable,
//! classifies the requested argv, journals a redacted intent, serializes
//! repository writes with `flock`, and records the outcome. A process death
//! after the command starts becomes `outcome_unknown`; later writes fail
//! closed until an operator reconciles that journal row.

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::json;

use crate::broker::{Broker, BrokerOpError};
use crate::types::{
    CoordinatedOperation, NewCoordinatedOperation, OperationEffect, OperationProvider,
    OperationStatus,
};

#[derive(Debug, Clone)]
pub struct CoordinatedCommand {
    pub session_id: i64,
    pub provider: OperationProvider,
    /// Required for GitHub operations and remote Git operations. `owner/repo`.
    pub repository: Option<String>,
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
    pub command_success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl CoordinatedOperationReport {
    pub fn ok(&self) -> bool {
        self.command_success && self.operation.status == OperationStatus::Succeeded
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct OperationReconcileReport {
    pub operation: CoordinatedOperation,
    pub reason: String,
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

pub fn classify_git(args: &[String]) -> Option<OperationEffect> {
    let command = args.first()?.as_str();
    match command {
        "status" | "diff" | "log" | "show" | "rev-parse" | "ls-files" | "ls-tree" | "cat-file"
        | "grep" | "blame" | "describe" | "shortlog" | "whatchanged" | "merge-base"
        | "name-rev" | "for-each-ref" | "check-ignore" | "count-objects" | "fsck" | "help"
        | "version" | "--version" => Some(OperationEffect::Read),
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
    args.first().is_some_and(|arg| {
        matches!(
            arg.as_str(),
            "clone" | "fetch" | "pull" | "push" | "remote" | "ls-remote" | "submodule"
        )
    })
}

fn validate_gh_args(args: &[String]) -> Result<(), BrokerOpError> {
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "-R" | "--repo") || arg.starts_with("--repo="))
    {
        return Err(BrokerOpError::InvalidCoordinatedOperation {
            reason:
                "do not pass a second repository target after --; use broker gh --repo owner/name"
                    .into(),
        });
    }
    Ok(())
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
    pub fn run_coordinated_operation(
        &mut self,
        request: CoordinatedCommand,
    ) -> Result<CoordinatedOperationReport, BrokerOpError> {
        let session = self.store().session(request.session_id)?;
        self.run_coordinated_operation_at(request, Path::new(&session.worktree_path))
    }

    pub(crate) fn run_coordinated_operation_at(
        &mut self,
        request: CoordinatedCommand,
        cwd: &Path,
    ) -> Result<CoordinatedOperationReport, BrokerOpError> {
        if request.args.is_empty() {
            return Err(BrokerOpError::InvalidCoordinatedOperation {
                reason: format!(
                    "broker {} requires arguments after --",
                    request.provider.as_str()
                ),
            });
        }
        if request.provider == OperationProvider::Github {
            validate_gh_args(&request.args)?;
        }
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

        let repository = match (&request.repository, request.provider) {
            (Some(repository), _) => {
                validate_repository(repository)?;
                repository.clone()
            }
            (None, OperationProvider::Github) => {
                return Err(BrokerOpError::InvalidCoordinatedOperation {
                    reason: "broker gh requires --repo owner/name".into(),
                });
            }
            (None, OperationProvider::Git) if remote_git_operation(&request.args) => {
                return Err(BrokerOpError::InvalidCoordinatedOperation {
                    reason: "remote Git operation requires --repo owner/name".into(),
                });
            }
            (None, OperationProvider::Git) => {
                format!("local:{}", self.main_root().display())
            }
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
                        self.store().transition_coordinated_operation(
                            operation.id,
                            OperationStatus::OutcomeUnknown,
                            operation.exit_code,
                            Some(r#"{"reason":"process_ended_without_outcome"}"#),
                        )?;
                        return Err(BrokerOpError::CoordinatedOperationBlocked {
                            repository,
                            operation_id: operation.id,
                        });
                    }
                    OperationStatus::OutcomeUnknown => {
                        return Err(BrokerOpError::CoordinatedOperationBlocked {
                            repository,
                            operation_id: operation.id,
                        });
                    }
                    _ => {}
                }
            }
        }

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
            })?;
        self.store().transition_coordinated_operation(
            operation.id,
            OperationStatus::Running,
            None,
            Some(&json!({ "classification": classification }).to_string()),
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
            .env("AETHYME_BROKER_SESSION_ID", request.session_id.to_string());
        if request.provider == OperationProvider::Github {
            command.env("GH_REPO", &repository);
        }
        let output = match command.output() {
            Ok(output) => output,
            Err(source) => {
                self.store().transition_coordinated_operation(
                    operation.id,
                    OperationStatus::Failed,
                    None,
                    Some(r#"{"reason":"spawn_failed"}"#),
                )?;
                return Err(BrokerOpError::OperationSpawn {
                    executable: executable.into(),
                    source,
                });
            }
        };
        let exit_code = output.status.code().map(i64::from);
        let status = if output.status.success() {
            OperationStatus::Succeeded
        } else if effect == OperationEffect::Read {
            OperationStatus::Failed
        } else {
            // A mutating command may have applied a subset of its effects
            // before returning non-zero. Treating that as safely failed would
            // make a blind retry possible, so require external inspection.
            OperationStatus::OutcomeUnknown
        };
        let operation = self.store().transition_coordinated_operation(
            operation.id,
            status,
            exit_code,
            Some(&json!({ "classification": classification }).to_string()),
        )?;
        Ok(CoordinatedOperationReport {
            operation,
            classification,
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
        if operation.status != OperationStatus::OutcomeUnknown {
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
        let details = json!({ "operator_reason": reason }).to_string();
        let operation = self.store().transition_coordinated_operation(
            operation_id,
            status,
            operation.exit_code,
            Some(&details),
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
        let err =
            validate_gh_args(&args(&["pr", "merge", "12", "--repo", "other/repo"])).unwrap_err();
        assert!(err.to_string().contains("second repository target"));
    }
}
