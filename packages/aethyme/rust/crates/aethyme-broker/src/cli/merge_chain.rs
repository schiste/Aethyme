//! `broker advanced merge-chain`: argument handling and output. The chain
//! itself lives in `crate::merge_chain`.

use std::time::Duration;

use super::*;
use crate::merge_chain::{
    ChainWriter, GhChainReader, MergeChainOptions, MergeChainReport, MergeMethod, SystemClock,
    WriteOutcome,
};

const DEFAULT_POLL_SECONDS: u64 = 30;
const DEFAULT_CHECKS_TIMEOUT_SECONDS: u64 = 90 * 60;
const DEFAULT_MAIN_TIMEOUT_SECONDS: u64 = 60 * 60;
const DEFAULT_DISPATCH_AFTER_SECONDS: u64 = 3 * 60;

/// The flags only `merge-chain` reads, kept together so the shared parser
/// carries one field for them.
#[derive(Clone, Default)]
pub(super) struct MergeChainFlags {
    merge_method: Option<MergeMethod>,
    poll_seconds: Option<u64>,
    checks_timeout_seconds: Option<u64>,
    main_timeout_seconds: Option<u64>,
    dispatch_after_seconds: Option<u64>,
    gates_workflow: Option<String>,
}

impl MergeChainFlags {
    pub(super) fn set(&mut self, flag: &str, value: &str) -> Result<(), UsageError> {
        let seconds = || {
            value
                .parse::<u64>()
                .map_err(|_| UsageError::Message(format!("{flag} must be a non-negative integer")))
        };
        match flag {
            "--merge-method" => {
                self.merge_method = Some(MergeMethod::parse(value).ok_or_else(|| {
                    UsageError::Message("--merge-method must be merge, squash or rebase".into())
                })?);
            }
            "--poll-seconds" => self.poll_seconds = Some(seconds()?),
            "--checks-timeout" => self.checks_timeout_seconds = Some(seconds()?),
            "--main-timeout" => self.main_timeout_seconds = Some(seconds()?),
            "--dispatch-after" => self.dispatch_after_seconds = Some(seconds()?),
            "--gates-workflow" => self.gates_workflow = Some(value.to_string()),
            other => {
                return Err(UsageError::Message(format!(
                    "unknown merge-chain flag {other}"
                )));
            }
        }
        Ok(())
    }
}

/// Backs every chain write with a coordinated `gh` operation, so each one is
/// authorized by `--reason`, serialized and journaled like `broker advanced gh`.
struct CoordinatedWriter<'a> {
    broker: &'a mut Broker,
    session: i64,
    repository: String,
    reason: String,
}

impl ChainWriter for CoordinatedWriter<'_> {
    fn write(&mut self, args: Vec<String>) -> Result<WriteOutcome, String> {
        let request = crate::CoordinatedCommand {
            session_id: self.session,
            provider: crate::OperationProvider::Github,
            repository: Some(self.repository.clone()),
            resolved_target: None,
            scope: None,
            declared_effect: None,
            destructive_confirmed: false,
            authorization_reason: Some(self.reason.clone()),
            args,
        };
        let report = self
            .broker
            .run_coordinated_operation_with_wait(request, crate::QueueWait::Forever)
            .map_err(|error| error.to_string())?;
        let operation = report.operation.id;
        if report.ok() {
            return Ok(WriteOutcome::Succeeded { operation });
        }
        let detail = report
            .stderr
            .lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .unwrap_or("no provider output")
            .to_string();
        if let Some(recovery) = report.unknown_outcome_recovery() {
            return Ok(WriteOutcome::Unknown {
                operation,
                recovery: recovery.to_string(),
                detail,
            });
        }
        Ok(WriteOutcome::Failed { operation, detail })
    }
}

pub(super) fn run_merge_chain(parsed: Parsed) -> Result<(), UsageError> {
    let session = parsed.session.ok_or(UsageError::Message(
        "merge-chain requires --session <id>".into(),
    ))?;
    let repository = parsed.repository.clone().ok_or(UsageError::Message(
        "merge-chain requires --repo <owner/name>".into(),
    ))?;
    let reason = match parsed.reason.clone() {
        Some(reason) if !reason.trim().is_empty() => reason,
        _ if parsed.dry_run => String::new(),
        _ => {
            return Err(UsageError::Message(
                "merge-chain requires --reason <text>: it merges pull requests".into(),
            ));
        }
    };
    if parsed.positional.is_empty() {
        return Err(UsageError::Message(
            "merge-chain requires at least one pull request number".into(),
        ));
    }
    let mut pull_requests = Vec::new();
    for value in &parsed.positional {
        let number: u64 = value
            .trim_start_matches('#')
            .parse()
            .map_err(|_| UsageError::Message(format!("not a pull request number: {value}")))?;
        if pull_requests.contains(&number) {
            return Err(UsageError::Message(format!(
                "pull request #{number} is listed twice"
            )));
        }
        pull_requests.push(number);
    }
    let flags = &parsed.merge_chain;
    if flags.dispatch_after_seconds.is_some() && flags.gates_workflow.is_none() {
        return Err(UsageError::Message(
            "--dispatch-after needs --gates-workflow <file>: there is nothing to dispatch".into(),
        ));
    }
    let options = MergeChainOptions {
        repository: repository.clone(),
        pull_requests,
        merge_method: flags.merge_method.unwrap_or(MergeMethod::Merge),
        poll_interval: Duration::from_secs(flags.poll_seconds.unwrap_or(DEFAULT_POLL_SECONDS)),
        checks_timeout: Duration::from_secs(
            flags
                .checks_timeout_seconds
                .unwrap_or(DEFAULT_CHECKS_TIMEOUT_SECONDS),
        ),
        main_timeout: Duration::from_secs(
            flags
                .main_timeout_seconds
                .unwrap_or(DEFAULT_MAIN_TIMEOUT_SECONDS),
        ),
        dispatch_after: Duration::from_secs(
            flags
                .dispatch_after_seconds
                .unwrap_or(DEFAULT_DISPATCH_AFTER_SECONDS),
        ),
        gates_workflow: flags.gates_workflow.clone(),
        dry_run: parsed.dry_run,
    };
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    // Refuse a closed or foreign session before the first provider call,
    // not at the first write after minutes of waiting on checks.
    let record = broker.store().session(session)?;
    if record.status.is_closed() {
        return Err(crate::BrokerOpError::ClosedSessionOperation {
            session_id: session,
            repository_root: broker.main_root().display().to_string(),
        }
        .into());
    }
    crate::operations::refuse_session_repository_mismatch(
        session,
        Path::new(&record.worktree_path),
        &repository,
    )?;
    let reader = GhChainReader {
        repository: repository.clone(),
    };
    let mut writer = CoordinatedWriter {
        broker: &mut broker,
        session,
        repository,
        reason,
    };
    let json = parsed.json;
    // Progress goes to stderr under --json so stdout stays one document.
    let mut progress = |line: String| {
        if json {
            eprintln!("{line}");
        } else {
            out!("{line}");
        }
    };
    let report = crate::merge_chain::run_merge_chain(
        &options,
        &reader,
        &mut writer,
        &SystemClock,
        &mut progress,
    );
    render_merge_chain(&report, json)?;
    match &report.stopped {
        None => Ok(()),
        Some(stop) => Err(UsageError::Exit {
            message: format!(
                "merge-chain stopped at #{} ({}): {}",
                stop.pull_request,
                serde_json::to_value(stop.stage)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_string))
                    .unwrap_or_default(),
                stop.reason
            ),
            code: stop.exit_code,
        }),
    }
}

fn render_merge_chain(report: &MergeChainReport, json: bool) -> Result<(), UsageError> {
    if json {
        out!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    if report.dry_run {
        out!(
            "merge-chain plan for {} (dry run, nothing written):",
            report.repository
        );
        for planned in &report.plan {
            out!("  #{}", planned.pull_request);
            for step in &planned.steps {
                out!("    - {step}");
            }
        }
    } else {
        for merged in &report.merged {
            out!(
                "#{} {} as {}; base checks green: {}",
                merged.number,
                if merged.already_merged {
                    "already merged"
                } else {
                    "merged"
                },
                merged.merge_commit,
                merged.main_checks.join(", ")
            );
        }
    }
    if let Some(stop) = &report.stopped {
        out!("stopped at #{}: {}", stop.pull_request, stop.reason);
        if let Some(head) = &stop.head {
            out!("  head: {head}");
        }
        if let Some(commit) = &stop.merge_commit {
            out!("  merge commit: {commit}");
        }
        out!("  next: {}", stop.next_action);
    } else if !report.dry_run {
        out!(
            "merge-chain complete: {} pull request(s)",
            report.merged.len()
        );
    }
    Ok(())
}
