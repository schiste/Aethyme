//! `broker exec`, `git`, `gh`, `operations`, `blockers` and `unblock`: coordinated operations and their recovery.

use super::*;

pub(super) fn coordination_wait_summary(operation: &crate::CoordinatedOperation) -> Option<String> {
    if operation.status != crate::OperationStatus::Prepared {
        return None;
    }
    let details = operation
        .details_json
        .as_deref()
        .and_then(|details| serde_json::from_str::<serde_json::Value>(details).ok())?;
    let wait = details.get("coordination_wait")?;
    let holder = wait.get("holder")?;
    let holder_name = match (
        holder
            .get("operation_id")
            .and_then(serde_json::Value::as_i64),
        holder.get("session_id").and_then(serde_json::Value::as_i64),
    ) {
        (Some(operation_id), Some(session_id)) => {
            format!("operation {operation_id} (session {session_id})")
        }
        (Some(operation_id), None) => format!("operation {operation_id}"),
        _ => wait
            .get("holder_description")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("an unrecorded holder")
            .to_string(),
    };
    let waiting_started_at = wait
        .get("waiting_started_at")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(operation.created_at);
    let waited_seconds = now_ms().saturating_sub(waiting_started_at).max(0) as u64 / 1_000;
    Some(format!(
        "waiting for {holder_name} for {}",
        crate::operations::humanize_duration(waited_seconds)
    ))
}

pub(super) const UNBLOCK_USAGE: &str = "usage: aethyme broker unblock <id> [--outcome <succeeded|failed>] [--reason <text>] [--confirm <generation>] [--json]";

pub(super) fn render_blockers(report: &crate::BlockerReport) {
    if report.blockers.is_empty() {
        out!("blockers: none");
    }
    for blocker in &report.blockers {
        out!(
            "blocker {} [{} {}{}]: {}",
            blocker.id,
            blocker.kind.as_str(),
            match blocker.scope {
                crate::BlockerScope::Repo => "repo",
                crate::BlockerScope::Host => "host",
            },
            if blocker.safe_to_clear_automatically {
                ", safe to clear"
            } else {
                ""
            },
            blocker.cause
        );
        if let Some(inspect) = &blocker.inspect {
            out!("  inspect: {inspect}");
        }
        out!("  clear: {}", blocker.clear);
    }
    if let Some(pair) = &report.paired_recovery {
        out!(
            "recovery order: {} are the halves of one killed write; clearing one alone is not a recovery",
            pair.blockers.join(" + ")
        );
        for (index, step) in pair.steps.iter().enumerate() {
            out!("  {}. {step}", index + 1);
        }
    }
    for source in &report.unavailable {
        out!(
            "blockers: could not read {}: {} -- the list above is incomplete",
            source.source,
            source.error
        );
    }
}

pub(super) fn run_unblock(parsed: Parsed) -> Result<(), UsageError> {
    let [id] = parsed.positional.as_slice() else {
        return Err(UsageError::Message(UNBLOCK_USAGE.into()));
    };
    crate::BlockerRef::parse(id)
        .map_err(|reason| UsageError::Message(format!("{reason}\n{UNBLOCK_USAGE}")))?;
    let outcome = match parsed.outcome.as_deref() {
        None => None,
        Some("succeeded") => Some(true),
        Some("failed") => Some(false),
        Some(_) => {
            return Err(UsageError::Message(format!(
                "--outcome must be succeeded or failed\n{UNBLOCK_USAGE}"
            )));
        }
    };
    let mut broker = open_broker(false)?;
    let request = crate::UnblockRequest {
        id: id.clone(),
        outcome,
        reason: parsed.reason.clone(),
        confirm: parsed.confirm.clone(),
    };
    match broker.unblock(&request)? {
        crate::UnblockOutcome::Cleared(report) => {
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                out!("unblocked {}: {}", report.id, report.action);
            }
            Ok(())
        }
        crate::UnblockOutcome::Refused(refusal) => {
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&refusal)?);
                return Err(UsageError::SilentExit(crate::exit_status::REFUSED));
            }
            let flags = if refusal.required_flags.is_empty() {
                String::new()
            } else {
                format!("; required: {}", refusal.required_flags.join(" "))
            };
            Err(UsageError::Exit {
                message: format!("unblock {} refused: {}{flags}", refusal.id, refusal.reason),
                code: crate::exit_status::REFUSED,
            })
        }
    }
}

pub(super) fn parse_operation_effect(
    value: Option<&str>,
) -> Result<Option<crate::OperationEffect>, UsageError> {
    value
        .map(|value| {
            crate::OperationEffect::parse(value).map_err(|_| {
                UsageError::Message("--effect must be read, write, or destructive".into())
            })
        })
        .transpose()
}

pub(super) fn operation_history_query(
    parsed: &Parsed,
) -> Result<crate::OperationHistoryQuery, UsageError> {
    let limit = parsed
        .limit
        .unwrap_or(crate::DEFAULT_OPERATION_HISTORY_LIMIT);
    if limit == 0 || limit > crate::MAX_OPERATION_HISTORY_LIMIT {
        return Err(UsageError::Message(format!(
            "--limit must be between 1 and {}",
            crate::MAX_OPERATION_HISTORY_LIMIT
        )));
    }
    if parsed.before.is_some_and(|id| id <= 0) {
        return Err(UsageError::Message(
            "--before must be a positive operation id".into(),
        ));
    }
    let status = parsed
        .status
        .as_deref()
        .map(|value| {
            crate::OperationStatus::parse(value).map_err(|_| {
                UsageError::Message(
                    "--status must be prepared, running, succeeded, failed, outcome_unknown, reconciled_succeeded, or reconciled_failed".into(),
                )
            })
        })
        .transpose()?;
    let provider = parsed
        .provider
        .as_deref()
        .map(|value| {
            crate::OperationProvider::parse(value)
                .map_err(|_| UsageError::Message("--provider must be git or github".into()))
        })
        .transpose()?;
    Ok(crate::OperationHistoryQuery {
        limit,
        before_id: parsed.before,
        session_id: parsed.session,
        status,
        repository: parsed.repository.clone(),
        provider,
    })
}

pub(super) fn operations_reconcile_error(detail: impl std::fmt::Display) -> UsageError {
    UsageError::Message(format!(
        "{detail}\noperations reconcile requires every field: --operation <id|host-operation-id>, --outcome <succeeded|failed>, and --reason <text>.\n{OPERATIONS_RECONCILE_USAGE}"
    ))
}

pub(super) fn render_operation_show(report: &crate::OperationShowReport) {
    let operation = &report.operation;
    out!("Operation:      {}", operation.id);
    out!("Session:        {}", operation.session_id);
    out!("Provider:       {}", operation.provider.as_str());
    out!("Repository:     {}", operation.repository);
    out!("Scope:          {}", operation.scope);
    out!("Effect:         {}", operation.effect.as_str());
    out!("Status:         {}", operation.status.as_str());
    if operation.status == crate::OperationStatus::Running {
        out!(
            "Liveness:       {}",
            crate::operations::operation_liveness_summary(operation)
        );
    }
    if let Some(waiting) = coordination_wait_summary(operation) {
        out!("Queue wait:     {waiting}");
    }
    out!("Identity:       {}", operation.identity_provenance.as_str());
    if let Some(provenance) = &operation.agent_provenance {
        let program = |key: &str| {
            provenance
                .get(key)
                .and_then(|process| process.get("program"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unidentified")
                .to_string()
        };
        out!(
            "Provenance:     caller {}, holder {}, agent {}",
            program("caller"),
            program("holder"),
            provenance
                .get("session_agent_identity")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("-")
        );
    }
    out!("Command:        {}", operation.command_json);
    out!(
        "Host operation: {}",
        operation.host_operation_id.as_deref().unwrap_or("none")
    );
    out!("Reconciliation: {}", report.reconciliation.state.as_str());
    out!(
        "Write blocked:  {}",
        if report.reconciliation.write_blocked {
            "yes"
        } else {
            "no"
        }
    );
    out!("Automatic retry: forbidden");
    if let Some(evidence) = &report.reconciliation.evidence {
        out!("Evidence:       {evidence}");
    }
    if let Some(reason) = &report.reconciliation.operator_reason {
        out!("Operator reason: {reason}");
    }
    if let Some(recovery) = &report.reconciliation.recovery {
        out!("Inspect:        {}", recovery.inspection);
        out!("If succeeded:   {}", recovery.succeeded_command);
        out!("If failed:      {}", recovery.failed_command);
        out!("Blind retry is forbidden until reconciliation is recorded.");
    }
}

pub(super) fn render_operation_stats(report: &crate::OperationStats) {
    let repository = report.repository.as_deref().unwrap_or("all repositories");
    out!("Coordination statistics for {repository}:");
    out!(
        "  observed: {} (measured: {}, unmeasured: {}, history truncated: {})",
        report.observed_operations,
        report.measured_operations,
        report.unmeasured_operations,
        if report.history_truncated {
            "yes"
        } else {
            "no"
        },
    );
    render_timing_distribution("lock hold", &report.lock_hold_ms);
    render_timing_distribution("queue wait", &report.queue_wait_ms);
    out!(
        "  queue depth: {} samples, p50 {}, p99 {}, max {}",
        report.queue_depth.sample_count,
        format_optional_usize(report.queue_depth.p50),
        format_optional_usize(report.queue_depth.p99),
        format_optional_usize(report.queue_depth.max),
    );
    out!(
        "  known unrelated contention: {} waits, {}ms total, max {}ms",
        report.unrelated_contention.sample_count,
        report.unrelated_contention.total_queue_wait_ms,
        format_optional_ms(report.unrelated_contention.max_queue_wait_ms),
    );
    out!(
        "  hooks outside lock: {} samples",
        report.hooks_outside_lock.sample_count
    );
    render_timing_distribution(
        "    hook lock hold",
        &report.hooks_outside_lock.lock_hold_ms,
    );
    render_timing_distribution(
        "    hook queue wait",
        &report.hooks_outside_lock.queue_wait_ms,
    );
    let ref_stats = &report.pr_merge_ref_determination;
    out!(
        "  pr merge ref determination: {} measured ({} succeeded, {} failed), {} unmeasured",
        ref_stats.measured_count,
        ref_stats.succeeded_count,
        ref_stats.failed_count,
        ref_stats.unmeasured_count,
    );
    render_timing_distribution("    ref determination", &ref_stats.duration_ms);
    if !report.by_kind.is_empty() {
        out!("  by operation kind:");
        for kind in &report.by_kind {
            out!(
                "    {} ({} samples): hold p50 {}, p99 {}; wait p50 {}, p99 {}",
                kind.kind,
                kind.sample_count,
                format_optional_ms(kind.lock_hold_ms.p50_ms),
                format_optional_ms(kind.lock_hold_ms.p99_ms),
                format_optional_ms(kind.queue_wait_ms.p50_ms),
                format_optional_ms(kind.queue_wait_ms.p99_ms),
            );
        }
    }
    out!("  note: {}", report.interpretation);
}

pub(super) fn render_timing_distribution(
    label: &str,
    distribution: &crate::OperationTimingDistribution,
) {
    out!(
        "  {label}: {} samples, total {}ms, p50 {}, p99 {}, max {}",
        distribution.sample_count,
        distribution.total_ms,
        format_optional_ms(distribution.p50_ms),
        format_optional_ms(distribution.p99_ms),
        format_optional_ms(distribution.max_ms),
    );
}

pub(super) fn format_optional_ms(value: Option<i64>) -> String {
    value
        .map(|value| format!("{value}ms"))
        .unwrap_or_else(|| "n/a".into())
}

pub(super) fn format_optional_usize(value: Option<usize>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "n/a".into())
}

pub(super) fn render_coordinated_operation(
    report: &crate::CoordinatedOperationReport,
    session_worktree: Option<&str>,
    json: bool,
) -> Result<(), UsageError> {
    if json {
        out!("{}", serde_json::to_string_pretty(report)?);
    } else {
        if !report.stdout.is_empty() {
            print!("{}", report.stdout);
            if !report.stdout.ends_with('\n') {
                out!();
            }
        }
        if !report.stderr.is_empty() {
            eprint!("{}", report.stderr);
            if !report.stderr.ends_with('\n') {
                eprintln!();
            }
        }
        out!(
            "operation {}: {} {} on {} ({} effect)",
            report.operation.id,
            report.operation.provider.as_str(),
            report.operation.status.as_str(),
            report.operation.repository,
            report.classification,
        );
        // Session ids are numbered per repository and resolved against the
        // broker of the caller's directory, so naming the worktree the id
        // resolved to is what makes a session of the wrong repository visible
        // at the point of the command rather than later from its effects.
        if let Some(worktree) = session_worktree {
            out!(
                "  session {} worktree: {worktree}",
                report.operation.session_id
            );
        }
        // What the push actually sent. The planner resolved this before the
        // command ran; printing it is what makes a refspec that resolved to an
        // unintended commit visible at the point of the push rather than later
        // from CI metadata (#269).
        for pushed in &report.pushed_refs {
            out!(
                "  pushed {} -> {}",
                &pushed.proposed_sha[..pushed.proposed_sha.len().min(12)],
                pushed.destination_ref,
            );
        }
        // A create that exited non-zero has already been reconciled against the
        // repository by now, so the operator reads the answer here rather than
        // going to look for the issue by hand (#184).
        if let Some(outcome) = report.create_outcome() {
            out!("{outcome}");
        }
        // The PR is linkable the moment it exists; starting the watch is left
        // to the caller because it polls the provider, and this command may
        // still be inside the repository write lock (#150, and #138 for why).
        if let Some(cleanup) = &report.post_merge_cleanup {
            out!(
                "post-merge integration cleanup: {} — {}",
                cleanup.state.as_str(),
                cleanup.explanation
            );
            if let Some(operation_id) = cleanup.fetch_operation_id {
                out!("  upstream refresh operation: {operation_id}");
            }
            if let Some(command) = &cleanup.next_action {
                out!("  next: {command}");
            }
        }
    }
    Ok(())
}

fn changed_paths_remaining(report: &crate::GuardedExecReport) -> Vec<String> {
    let mut paths = report.touched_paths.clone();
    paths.extend(report.new_untracked_paths.iter().cloned());
    paths.sort();
    paths.dedup();
    paths
}

/// `broker exec`.
pub(super) fn run_exec(parsed: Parsed) -> Result<(), UsageError> {
    let session = parsed
        .session
        .ok_or(UsageError::Message("exec requires --session <id>".into()))?;
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    let report = broker.guarded_exec(session, &parsed.exec_command)?;
    let changed_paths = changed_paths_remaining(&report);
    if parsed.json {
        let mut value = serde_json::to_value(&report)?;
        let object = value
            .as_object_mut()
            .expect("guarded exec report is an object");
        object.insert("command_ran".into(), serde_json::Value::Bool(true));
        object.insert(
            "edits_remain".into(),
            serde_json::Value::Bool(!changed_paths.is_empty()),
        );
        object.insert(
            "changed_paths_remaining".into(),
            serde_json::to_value(&changed_paths)?,
        );
        out!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        out!(
            "exec session {}: command {}{}",
            session,
            if report.command_success {
                "passed"
            } else {
                "failed"
            },
            report
                .exit_code
                .map(|code| format!(" ({code})"))
                .unwrap_or_default()
        );
        out!("  command ran: yes");
        out!(
            "  edits remain after command: {}",
            if changed_paths.is_empty() {
                "no"
            } else {
                "yes"
            }
        );
        // JSON carries the whole list; a codemod can change thousands of
        // files, so the human summary stays capped like the lists below.
        if changed_paths.is_empty() {
            out!("  changed paths: none");
        } else {
            out!("  changed paths: {}", capped_join(&changed_paths, 8));
        }
        if !report.newly_dirty_paths.is_empty() {
            out!(
                "  newly dirty: {}",
                capped_join(&report.newly_dirty_paths, 8)
            );
        }
        if !report.modified_preexisting_dirty_paths.is_empty() {
            out!(
                "  changed while already dirty: {}",
                capped_join(&report.modified_preexisting_dirty_paths, 8)
            );
        }
        if !report.outside_lease_paths.is_empty() {
            out!(
                "  outside explicit leases: {}",
                capped_join(&report.outside_lease_paths, 8)
            );
        }
        if !report.foreign_paths.is_empty() {
            out!(
                "  adoption-time foreign paths: {}",
                capped_join(&report.foreign_paths, 8)
            );
        }
    }
    if !report.ok {
        // `ok` is `command_success && audit.ok`, so the two causes are
        // already separable. Reporting both as an ownership failure
        // sends the reader to debug leases when the wrapped command
        // simply exited non-zero for its own reasons.
        let guard_refused = !report.outside_lease_paths.is_empty()
            || !report.foreign_paths.is_empty()
            || !report.modified_preexisting_dirty_paths.is_empty();
        return Err(UsageError::Message(
            match (report.command_success, guard_refused) {
                (true, _) => {
                    "guarded exec refused: the command ran and left changed paths outside \
                          this session's ownership (listed above)"
                        .to_string()
                }
                (false, false) => format!(
                    "guarded exec: the command ran and exited {} — the guard found no ownership \
                 violation, so this is the command's own failure",
                    report
                        .exit_code
                        .map(|code| code.to_string())
                        .unwrap_or_else(|| "by signal".into())
                ),
                (false, true) => format!(
                    "guarded exec: the command ran and exited {}, and it left changed paths outside \
                 this session's ownership (listed above)",
                    report
                        .exit_code
                        .map(|code| code.to_string())
                        .unwrap_or_else(|| "by signal".into())
                ),
            },
        ));
    }
    Ok(())
}

/// The `broker advanced git|gh` invocation as typed, kept to print the
/// corrected command when the broker refuses it for a missing flag.
struct OperationEcho {
    subcommand: String,
    session: i64,
    repository: Option<String>,
    effect: Option<String>,
    scope: Option<String>,
    destructive: bool,
    cross_session: Option<i64>,
    ref_write_acknowledged: bool,
    reason: Option<String>,
    no_wait: bool,
    queue_timeout_seconds: Option<u64>,
    json: bool,
    args: Vec<String>,
}

impl OperationEcho {
    /// The same command with the missing flags added before `--`. A
    /// repository that cannot be inferred stays a visible placeholder.
    fn corrected(&self, missing: &crate::MissingOperationFlags) -> String {
        let quote = crate::broker::shell_quote;
        let mut words = vec![
            "aethyme".to_string(),
            "broker".into(),
            "advanced".into(),
            self.subcommand.clone(),
            "--session".into(),
            self.session.to_string(),
        ];
        let repository = match (&self.repository, &missing.repository) {
            (Some(repository), _) => Some(quote(repository)),
            (None, Some(Some(inferred))) => Some(quote(inferred)),
            (None, Some(None)) => Some("<owner/name>".to_string()),
            (None, None) => None,
        };
        if let Some(repository) = repository {
            words.extend(["--repo".to_string(), repository]);
        }
        if let Some(effect) = &self.effect {
            words.extend(["--effect".to_string(), quote(effect)]);
        }
        if let Some(scope) = &self.scope {
            words.extend(["--scope".to_string(), quote(scope)]);
        }
        if self.destructive || missing.destructive {
            words.push("--destructive".into());
        }
        if let Some(session) = self.cross_session {
            words.extend(["--cross-session".to_string(), session.to_string()]);
        }
        if self.ref_write_acknowledged {
            words.push("--ref-write-acknowledged".into());
        }
        match &self.reason {
            Some(reason) => words.extend(["--reason".to_string(), quote(reason)]),
            None if missing.reason_required => {
                words.extend(["--reason".to_string(), "'<authorization>'".to_string()]);
            }
            None => {}
        }
        if self.no_wait {
            words.push("--no-wait".into());
        }
        if let Some(seconds) = self.queue_timeout_seconds {
            words.extend(["--queue-timeout".to_string(), seconds.to_string()]);
        }
        if self.json {
            words.push("--json".into());
        }
        words.push("--".into());
        words.extend(self.args.iter().map(|arg| quote(arg)));
        words.join(" ")
    }
}

/// `broker git` | `broker gh`.
pub(super) fn run_git_gh(parsed: Parsed, subcommand: &str) -> Result<(), UsageError> {
    let session = parsed.session.ok_or(UsageError::Message(format!(
        "{subcommand} requires --session <id>"
    )))?;
    let provider = if subcommand == "git" {
        crate::OperationProvider::Git
    } else {
        crate::OperationProvider::Github
    };
    let declared_effect = parse_operation_effect(parsed.effect.as_deref())?;
    let echo = OperationEcho {
        subcommand: subcommand.to_string(),
        session,
        repository: parsed.repository.clone(),
        effect: parsed.effect.clone(),
        scope: parsed.scope.clone(),
        destructive: parsed.destructive,
        cross_session: parsed.cross_session,
        ref_write_acknowledged: parsed.ref_write_acknowledged,
        reason: parsed.reason.clone(),
        no_wait: parsed.no_wait,
        queue_timeout_seconds: parsed.queue_timeout_seconds,
        json: parsed.json,
        args: parsed.exec_command.clone(),
    };
    let request = crate::CoordinatedCommand {
        session_id: session,
        provider,
        repository: parsed.repository,
        resolved_target: None,
        scope: parsed.scope,
        declared_effect,
        destructive_confirmed: parsed.destructive,
        cross_session: parsed.cross_session,
        ref_write_acknowledged: parsed.ref_write_acknowledged,
        authorization_reason: parsed.reason,
        args: parsed.exec_command,
    };
    let queue_wait = match (parsed.no_wait, parsed.queue_timeout_seconds) {
        (true, Some(_)) => {
            return Err(UsageError::Message(
                "--no-wait and --queue-timeout are mutually exclusive".into(),
            ));
        }
        (true, None) => crate::QueueWait::Refuse,
        (false, Some(seconds)) => crate::QueueWait::Seconds(seconds),
        (false, None) => crate::QueueWait::Forever,
    };
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    // `HEAD` is per-worktree state, and the coordinated command runs
    // inside the *session* worktree rather than wherever the operator
    // stood. Resolving it there publishes that session's commit under
    // the branch name typed here, and the push reports success (#269).
    // The check lives at this layer because "the caller's directory" is
    // a property of this invocation, not of the broker: an in-process
    // caller has no meaningful cwd, and asking the process for one
    // would make library behaviour depend on ambient state.
    if request.provider == crate::OperationProvider::Git {
        let symbolic = crate::worktree_relative_push_sources(&request.args);
        if !symbolic.is_empty()
            && let Ok(record) = broker.store().session(request.session_id)
            && let Ok(caller) = std::env::current_dir()
            && !crate::is_within(&caller, std::path::Path::new(&record.worktree_path))
        {
            return Err(UsageError::Message(format!(
                "refusing a worktree-relative push source from outside the session \
                 worktree: {}\n  caller cwd:         {}\n  session {} worktree: {}\n\
                 `HEAD` resolves in the session worktree, not where you are. Push an \
                 explicit commit (`git rev-parse HEAD`) or run from the session worktree.",
                symbolic.join(", "),
                caller.display(),
                record.id,
                record.worktree_path,
            )));
        }
    }
    let session_worktree = broker
        .store()
        .session(session)
        .ok()
        .map(|record| record.worktree_path);
    let report = match broker.run_coordinated_operation_with_wait(request, queue_wait) {
        Ok(report) => report,
        Err(crate::BrokerOpError::InvalidCoordinatedOperation { reason }) => {
            // A refusal for a missing flag is only useful with the command
            // that would pass, so print it whole instead of making the reader
            // rebuild a long argv by hand.
            let repo = crate::GitRepo::discover(broker.main_root()).ok();
            let missing = repo.as_ref().and_then(|repo| {
                crate::missing_operation_flags(
                    &reason,
                    provider,
                    &echo.args,
                    declared_effect,
                    echo.repository.is_some(),
                    repo,
                )
            });
            let reason = match missing {
                Some(missing) => format!("{reason}\n  run instead: {}", echo.corrected(&missing)),
                None => reason,
            };
            let reason = crate::session_push::with_pre_push_path_hint(&reason);
            return Err(crate::BrokerOpError::InvalidCoordinatedOperation { reason }.into());
        }
        Err(error) => return Err(error.into()),
    };
    render_coordinated_operation(&report, session_worktree.as_deref(), parsed.json)?;
    // After the coordinated operation returned, so the repository
    // write lock is released. Starting the watch inside it would hold
    // that lock across a provider call (#138). Opt-in only: a fleet
    // that watched every PR it opens would deliver interruptions to
    // agents that never asked for them.
    if let Some(number) = report.created_pull_request {
        let session = report.operation.session_id;
        let repository = report
            .github_target
            .as_ref()
            .map(|target| target.display_slug.clone())
            .unwrap_or_else(|| report.operation.repository.clone());
        let root = broker.main_root().to_path_buf();
        if crate::pr_monitoring_is_active(&root, session) {
            // Comments and reviews, not checks: this exists to route
            // human review back to the agent, and check churn on a
            // busy PR would bury it. An empty list is rejected as
            // meaning nothing, so the default must be explicit.
            match broker.start_pull_request_watch(
                session,
                &repository,
                number,
                vec![
                    crate::PullRequestActivityKind::Comment,
                    crate::PullRequestActivityKind::Review,
                ],
                300,
                &crate::GithubCliPullRequestWatchProvider,
                now_ms(),
            ) {
                Ok(watch) => out!(
                    "Watching pull request {number} for session {session} (watch {})",
                    watch.id
                ),
                // Monitoring is an addition to the work, never a reason
                // to report the pull request itself as failed.
                Err(error) => {
                    out!("Pull request {number} opened; watch not started: {error}")
                }
            }
        } else {
            out!(
                "Pull request {number} opened. PR monitoring is off for session {session}; enable with:"
            );
            out!("  aethyme broker advanced watch pr monitoring activate --session {session}");
        }
    }
    if !report.ok() {
        if let Some(recovery) = report.unknown_outcome_recovery() {
            return Err(UsageError::Exit {
                message: recovery.to_string(),
                code: crate::exit_status::OUTCOME_UNKNOWN,
            });
        }
        return Err(UsageError::Message(coordinated_failure_message(
            subcommand, &report,
        )));
    }
    Ok(())
}

/// Lines of provider stderr carried into a coordinated failure's message.
const COORDINATED_FAILURE_STDERR_LINES: usize = 3;

/// The failure line for a coordinated operation, with the end of the
/// provider's stderr: that is where git and gh say why (`index.lock exists`,
/// `HTTP 422`), and without it the recorded failure names only an id.
fn coordinated_failure_message(
    subcommand: &str,
    report: &crate::CoordinatedOperationReport,
) -> String {
    let mut message = format!(
        "coordinated {subcommand} operation {} failed",
        report.operation.id
    );
    if let Some(code) = report.operation.exit_code {
        message.push_str(&format!(" (exit {code})"));
    }
    let lines: Vec<&str> = report
        .stderr
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let tail = &lines[lines.len().saturating_sub(COORDINATED_FAILURE_STDERR_LINES)..];
    if !tail.is_empty() {
        message.push_str(": ");
        message.push_str(&tail.join(" | "));
    }
    if let Some(hint) =
        crate::session_push::missing_executable_path_hint(&report.stdout, &report.stderr)
    {
        message.push('\n');
        message.push_str(hint);
    }
    message
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OperationIdentifier {
    Coordinated(i64),
    Host(String),
}

fn parse_operation_identifier(value: &str) -> Result<OperationIdentifier, String> {
    let host_id = value.strip_prefix("hostop:").unwrap_or(value);
    if host_id.len() == 32 && host_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(OperationIdentifier::Host(host_id.to_ascii_lowercase()));
    }
    if value.starts_with("hostop:") {
        return Err("host operation id must contain exactly 32 hexadecimal characters".into());
    }
    if value.bytes().all(|byte| byte.is_ascii_digit()) {
        return value
            .parse::<i64>()
            .ok()
            .filter(|id| *id > 0)
            .map(OperationIdentifier::Coordinated)
            .ok_or_else(|| "operation id must be a positive integer".into());
    }
    Err(
        "operation id must be a positive integer or a 32-character hexadecimal host operation id"
            .into(),
    )
}

fn render_host_operation_show(operation: &crate::HostOperation) {
    out!("Host operation: {}", operation.operation_id);
    out!("Provider:       {}", operation.provider.as_str());
    out!("Remote:         {}", operation.remote_key);
    out!("Effect:         {}", operation.effect.as_str());
    out!("Status:         {}", operation.status.as_str());
    out!("Holder PID:     {}", operation.holder_pid);
    out!("Created at:     {}", operation.created_at);
    out!("Updated at:     {}", operation.updated_at);
    if let Some(finished_at) = operation.finished_at {
        out!("Finished at:    {finished_at}");
    }
}

/// Broker operations.
pub(super) fn run_operations(parsed: Parsed) -> Result<(), UsageError> {
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    match parsed.positional.first().map(String::as_str) {
        None | Some("list") => {
            if parsed.positional.len() > 1 {
                return Err(UsageError::Message(
                    "operations list does not accept positional arguments".into(),
                ));
            }
            let query = operation_history_query(&parsed)?;
            let page = broker.store().operation_history(&query)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&page)?);
            } else if page.operations.is_empty() {
                out!("No coordinated operations recorded.");
            } else {
                out!(
                    "{:<5} {:<8} {:<21} {:<22} SCOPE / WAIT",
                    "ID",
                    "TOOL",
                    "STATUS",
                    "REPOSITORY"
                );
                for operation in page.operations {
                    let waiting = coordination_wait_summary(&operation)
                        .map(|summary| format!("  {summary}"))
                        .unwrap_or_default();
                    let liveness = if operation.status == crate::OperationStatus::Running {
                        format!(
                            "  liveness: {}",
                            crate::operations::operation_liveness_summary(&operation)
                        )
                    } else {
                        String::new()
                    };
                    out!(
                        "{:<5} {:<8} {:<21} {:<22} {}{}",
                        operation.id,
                        operation.provider.as_str(),
                        operation.status.as_str(),
                        operation.repository,
                        operation.scope,
                        format!("{waiting}{liveness}"),
                    );
                }
                if let Some(before_id) = page.next_before_id {
                    out!("More operations: pass --before {before_id}.");
                }
            }
        }
        Some("show") => {
            if parsed.positional.len() != 2 {
                return Err(UsageError::Message(OPERATIONS_SHOW_USAGE.into()));
            }
            let identifier =
                parse_operation_identifier(&parsed.positional[1]).map_err(|error| {
                    UsageError::Message(format!("{error}; {OPERATIONS_SHOW_USAGE}"))
                })?;
            let coordinated_id = match &identifier {
                OperationIdentifier::Coordinated(id) => Some(*id),
                OperationIdentifier::Host(host_id) => broker
                    .store()
                    .coordinated_operation_id_for_host_operation(host_id)?,
            };
            if let Some(operation_id) = coordinated_id {
                let report = broker.show_coordinated_operation(operation_id)?;
                if parsed.json {
                    out!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    render_operation_show(&report);
                }
            } else if let OperationIdentifier::Host(host_id) = identifier {
                let database = broker
                    .host_operation_database_path()
                    .map_err(|error| UsageError::Message(error.to_string()))?;
                let operation = crate::host_operation(&database, &host_id)
                    .map_err(|error| UsageError::Message(error.to_string()))?
                    .ok_or_else(|| {
                        UsageError::Message(format!("host operation {host_id} was not found"))
                    })?;
                if parsed.json {
                    out!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "host_operation": operation
                        }))?
                    );
                } else {
                    render_host_operation_show(&operation);
                }
            }
        }
        Some("stats") => {
            if parsed.positional.len() != 1 {
                return Err(UsageError::Message(OPERATIONS_STATS_USAGE.into()));
            }
            let limit = parsed.limit.unwrap_or(crate::DEFAULT_OPERATION_STATS_LIMIT);
            if limit == 0 || limit > crate::MAX_OPERATION_STATS_LIMIT {
                return Err(UsageError::Message(format!(
                    "--limit must be between 1 and {}\n{OPERATIONS_STATS_USAGE}",
                    crate::MAX_OPERATION_STATS_LIMIT
                )));
            }
            let report = broker.coordinated_operation_stats(parsed.repository.as_deref(), limit)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                render_operation_stats(&report);
            }
        }
        Some("reconcile") => {
            if parsed.operation.is_none()
                || parsed.outcome.is_none()
                || parsed.reason.as_deref().is_none_or(str::is_empty)
            {
                return Err(operations_reconcile_error(
                    "incomplete operation reconciliation request",
                ));
            }
            let identifier = parse_operation_identifier(
                parsed.operation.as_deref().expect("validated operation id"),
            )
            .map_err(operations_reconcile_error)?;
            let outcome = parsed.outcome.as_deref().expect("validated outcome");
            let succeeded = match outcome {
                "succeeded" => true,
                "failed" => false,
                _ => {
                    return Err(operations_reconcile_error(
                        "--outcome must be succeeded or failed",
                    ));
                }
            };
            let reason = parsed.reason.as_deref().expect("validated reason");
            let operation_id = match identifier {
                OperationIdentifier::Coordinated(id) => Some(id),
                OperationIdentifier::Host(host_id) => {
                    if let Some(id) = broker
                        .store()
                        .coordinated_operation_id_for_host_operation(&host_id)?
                    {
                        Some(id)
                    } else {
                        let outcome = broker.unblock(&crate::UnblockRequest {
                            id: format!("hostop:{host_id}"),
                            outcome: Some(succeeded),
                            reason: Some(reason.to_string()),
                            confirm: None,
                        })?;
                        match outcome {
                            crate::UnblockOutcome::Cleared(report) => {
                                if parsed.json {
                                    out!("{}", serde_json::to_string_pretty(&report)?);
                                } else {
                                    out!("host operation {host_id} reconciled: {}", report.action);
                                }
                                return Ok(());
                            }
                            crate::UnblockOutcome::Refused(refusal) => {
                                return Err(operations_reconcile_error(refusal.reason));
                            }
                        }
                    }
                }
            };
            let report = broker
                .reconcile_coordinated_operation(
                    operation_id.expect("coordinated operation id resolved"),
                    succeeded,
                    reason,
                )
                .map_err(operations_reconcile_error)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                out!(
                    "operation {} reconciled as {}: {}",
                    report.operation.id,
                    report.operation.status.as_str(),
                    report.reason,
                );
            }
        }
        Some(other) => {
            return Err(UsageError::Message(format!(
                "unknown operations action {other:?} — expected list, show, stats, or reconcile"
            )));
        }
    }
    Ok(())
}

/// `broker blockers`.
pub(super) fn run_blockers(parsed: Parsed) -> Result<(), UsageError> {
    if !parsed.positional.is_empty() {
        return Err(UsageError::Message(
            "blockers does not accept positional arguments; usage: aethyme broker unblock [--json]"
                .into(),
        ));
    }
    let broker = open_broker(parsed.read_only_snapshot)?;
    let report = broker.blockers();
    if parsed.json {
        out!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        render_blockers(&report);
    }
    Ok(())
}

#[cfg(test)]
mod exec_report_tests {
    use super::*;

    #[test]
    fn remaining_changed_paths_include_untracked_and_tracked_edits_once() {
        let report = crate::GuardedExecReport {
            session_id: 1,
            command: vec!["test".into()],
            exit_code: Some(0),
            command_success: true,
            before_dirty_paths: Vec::new(),
            after_dirty_paths: Vec::new(),
            newly_dirty_paths: vec!["src/changed.rs".into()],
            new_untracked_paths: vec!["notes.txt".into(), "src/changed.rs".into()],
            modified_preexisting_dirty_paths: Vec::new(),
            touched_paths: vec!["src/changed.rs".into()],
            outside_lease_paths: Vec::new(),
            foreign_paths: Vec::new(),
            ok: true,
        };

        assert_eq!(
            changed_paths_remaining(&report),
            vec!["notes.txt".to_string(), "src/changed.rs".to_string()]
        );
    }
}
