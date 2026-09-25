//! `broker readiness`, `status`, `events`, `metrics`, `doctor`, `init`, `certify` and `scaffold`: reporting on the repository and the broker.

use super::*;

pub(super) fn render_readiness_report(
    report: &crate::ReadinessReport,
    json: bool,
) -> Result<(), UsageError> {
    if json {
        out!("{}", crate::render_readiness_json(report)?);
    } else {
        out!("{}", crate::render_readiness_text(report).trim_end());
    }
    Ok(())
}

pub(super) fn render_status_advice(advice: &[crate::StatusAdvice]) {
    out!("Next actions:");
    if advice.is_empty() {
        out!("  none");
        return;
    }
    for (index, item) in advice.iter().enumerate() {
        out!(
            "  {}. {:<7} {}",
            index + 1,
            item.severity.as_str().to_uppercase(),
            item.summary
        );
        if !item.evidence.is_empty() {
            out!("     evidence: {}", item.evidence.join("; "));
        }
        for command in &item.commands {
            out!("     run: {command}");
        }
    }
}

/// `broker readiness`.
pub(super) fn run_readiness(parsed: Parsed) -> Result<(), UsageError> {
    if !parsed.positional.is_empty() {
        return Err(UsageError::Message(
            "readiness does not accept positional arguments".into(),
        ));
    }
    let cwd = std::env::current_dir()
        .map_err(|error| UsageError::Message(format!("cannot resolve cwd: {error}")))?;
    let report = crate::inspect_repository_readiness(&cwd);
    render_readiness_report(&report, parsed.json)?;
    if let Some(required) = parsed.required_mode.as_deref() {
        let required =
            crate::RepositoryOperatingMode::parse_requirement(required).ok_or_else(|| {
                UsageError::Message(
                    "--require must be conflict-only, agent-ready, or parallel-ready".into(),
                )
            })?;
        if !report.meets(required) {
            return Err(UsageError::Exit {
                message: format!(
                    "repository mode {} does not meet required {}",
                    report.operating_mode.as_str(),
                    required.as_str()
                ),
                code: 1,
            });
        }
    }
    Ok(())
}

/// `broker status`.
pub(super) fn run_status(parsed: Parsed) -> Result<(), UsageError> {
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    // The mandated first step of every session, so its cost is a tax
    // on every agent. `--summary` skips the per-session diff that
    // dominates it and prints only what that step is read for (#182).
    if parsed.summary {
        let brief = broker.status_brief(now_ms())?;
        if parsed.json {
            out!("{}", serde_json::to_string_pretty(&brief)?);
        } else {
            out!("{}", brief.summary.message);
            render_status_advice(&brief.advice);
        }
        return Ok(());
    }
    let status = if parsed.read_only_snapshot {
        broker.status_snapshot(now_ms())?
    } else {
        broker.status(now_ms())?
    };
    if parsed.json {
        out!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        out!(
            "Integration: {} @ {}",
            status.integration_branch,
            &status.integration_head[..12.min(status.integration_head.len())]
        );
        // Name the ref the lead is counted against. Calling the
        // checkout "main" reported a 382-commit lead over an
        // eleven-day-old feature branch that happened to be checked
        // out, and an agent refused to publish on that number.
        out!(
            "Baseline:    {} @ {}",
            status.publication_baseline_ref,
            short_commit(&status.publication_baseline_head)
        );
        if status.main_head != status.publication_baseline_head {
            out!("Checkout:    {}", short_commit(&status.main_head));
        }
        if let (Some(upstream_ref), Some(upstream_head)) =
            (&status.upstream_ref, &status.upstream_head)
        {
            out!(
                "Upstream:    {} @ {} ({})",
                upstream_ref,
                short_commit(upstream_head),
                upstream_relation(
                    status.main_ahead_upstream_commits,
                    status.main_behind_upstream_commits,
                )
            );
        }
        out!("Summary: {}", status.summary.message);
        out!();
        render_status_advice(&status.advice);
        if !status.outstanding_advisories.is_empty() {
            out!();
            out!(
                "Outstanding advisories: {}",
                status.outstanding_advisories.len()
            );
            for advisory in status.outstanding_advisories.iter().take(10) {
                out!(
                    "  {} [{}]: {}",
                    advisory.id,
                    advisory.severity.as_str(),
                    advisory_text(&advisory.identity),
                );
                out!(
                    "    inspect: aethyme broker advanced advisories show {}",
                    advisory.id
                );
                out!(
                    "    acknowledge: aethyme broker advanced advisories ack {}",
                    advisory.id
                );
            }
            if status.outstanding_advisories.len() > 10 {
                out!(
                    "  and {} more; inspect: aethyme broker advanced advisories list",
                    status.outstanding_advisories.len() - 10
                );
            }
        }
        if !status.outstanding_entry_exposures.is_empty() {
            out!();
            out!(
                "Publication exposures: {} promoted {} not yet verified on remote main",
                status.outstanding_entry_exposures.len(),
                plural(status.outstanding_entry_exposures.len(), "entry", "entries")
            );
            for exposure in status.outstanding_entry_exposures.iter().take(10) {
                out!(
                    "  qid {} @ {}: {} {}",
                    exposure.queue_entry_id,
                    short_commit(&exposure.promotion_sha),
                    exposure.paths.len(),
                    plural(exposure.paths.len(), "path", "paths")
                );
            }
            if status.outstanding_entry_exposures.len() > 10 {
                out!(
                    "  and {} more",
                    status.outstanding_entry_exposures.len() - 10
                );
            }
            out!("  inspect: aethyme broker advanced exposures plan");
        }
        // A caller parked behind a wedged operation is inside a command
        // that never returns, so it cannot report its own wait. Status
        // is the out-of-band surface that can (issue #147).
        if !status.coordinated_operations.is_empty() {
            out!();
            let holders = status
                .coordinated_operations
                .iter()
                .filter(|operation| operation.holding_lock)
                .count();
            out!(
                "Coordinated operations: {} unresolved, {} holding a write lock",
                status.coordinated_operations.len(),
                holders
            );
            for operation in status.coordinated_operations.iter().take(10) {
                let role = if operation.holding_lock {
                    "holding".to_string()
                } else {
                    match operation.blocked_by {
                        Some(blocker) => format!("blocked by {blocker}"),
                        None => "queued".to_string(),
                    }
                };
                out!(
                    "  op {:<6} sess {:<4} {:<7} {:<28} {:<9} {:>8}  {} :: {}",
                    operation.id,
                    operation.session_id,
                    operation.provider,
                    operation.repository,
                    operation.status,
                    crate::operations::humanize_duration(operation.elapsed_seconds),
                    role,
                    crate::operations::operation_liveness_view_summary(&operation.liveness)
                );
            }
            if status.coordinated_operations.len() > 10 {
                out!("  and {} more", status.coordinated_operations.len() - 10);
            }
            out!("  inspect: aethyme broker advanced operations list");
        }
        out!();
        if status.agents.is_empty() {
            out!("No live sessions.");
        } else {
            out!(
                "{:<4} {:<8} {:<8} {:<30} {:<24} TASK",
                "ID",
                "STATUS",
                "ORIGIN",
                "REPO / TAB / PROVIDER",
                "BRANCH"
            );
            for view in &status.agents {
                let context = view.session.context_label().unwrap_or_else(|| "-".into());
                out!(
                    "{:<4} {:<8} {:<8} {:<30} {:<24} {}",
                    view.session.id,
                    view.derived_status.as_str(),
                    view.session.origin.as_str(),
                    context,
                    view.session.branch,
                    view.session.task.as_deref().unwrap_or("-"),
                );
            }
        }
        let explicit_leases = status
            .leases
            .iter()
            .filter(|lease| lease.kind == crate::LeaseKind::Explicit)
            .collect::<Vec<_>>();
        if !explicit_leases.is_empty() {
            out!();
            out!("Planned explicit leases:");
            for lease in explicit_leases {
                out!("  session {}: {}", lease.session_id, lease.path);
            }
        }
        let current_queue = status
            .queue
            .iter()
            .filter(|entry| queue_status_is_current(entry.status))
            .collect::<Vec<_>>();
        if !current_queue.is_empty() {
            out!();
            out!("Current merge queue:");
            out!("{:<4} {:<4} {:<11} HEAD", "QID", "SID", "QSTATUS");
            for entry in current_queue {
                out!(
                    "{:<4} {:<4} {:<11} {}",
                    entry.id,
                    entry.session_id,
                    entry.status.as_str(),
                    &entry.head_commit[..12.min(entry.head_commit.len())]
                );
            }
        }
        let terminal_counts = &status.queue_history.terminal_counts;
        if !terminal_counts.is_empty() {
            let total = terminal_counts.iter().map(|item| item.count).sum::<usize>();
            let summary = terminal_counts
                .iter()
                .map(|item| format!("{} {}", item.status.as_str(), item.count))
                .collect::<Vec<_>>()
                .join(", ");
            out!();
            out!(
                "Queue history: {total} terminal {} ({summary}).",
                plural(total, "entry", "entries")
            );
            out!("  inspect: {}", status.queue_history.command);
        }
        if status.advisory_delivery.shown_advisories > 0 {
            out!();
            out!(
                "Advisory delivery: {} shown, {} actioned, {} displays.",
                status.advisory_delivery.shown_advisories,
                status.advisory_delivery.actioned_advisories,
                status.advisory_delivery.total_shows,
            );
            out!("  inspect: aethyme broker advanced advisories metrics");
        }
        print_overlap_warnings(&status.overlaps);
        print_promoted_conflict_warnings(&status.promoted_conflicts);
    }
    Ok(())
}

/// `broker events`.
pub(super) fn run_events(parsed: Parsed) -> Result<(), UsageError> {
    if parsed.positional.first().map(String::as_str) == Some("prune") {
        let keep_days = parsed.keep_days.ok_or(UsageError::Message(
            "events prune requires --keep-days <n>".into(),
        ))?;
        let mut broker = open_broker(parsed.read_only_snapshot)?;
        let cutoff = now_ms() - keep_days * 24 * 60 * 60 * 1000;
        let removed = broker.store().prune_events_before(cutoff)?;
        if parsed.json {
            out!("{{\"pruned\":{removed}}}");
        } else {
            out!("Pruned {removed} event(s) older than {keep_days} day(s).");
        }
        return Ok(());
    }
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    let mut cursor = parsed.since.unwrap_or(0);
    // --follow survives transient read errors (e.g. a checkpoint
    // or a busy writer) with bounded retries instead of dying.
    let mut consecutive_errors = 0u32;
    loop {
        let events =
            match broker
                .store()
                .events_after_filtered(cursor, 1000, parsed.kind.as_deref())
            {
                Ok(events) => {
                    consecutive_errors = 0;
                    events
                }
                Err(err) if parsed.follow && consecutive_errors < 5 => {
                    consecutive_errors += 1;
                    eprintln!("events: transient read error ({err}); retrying");
                    std::thread::sleep(std::time::Duration::from_millis(700));
                    continue;
                }
                Err(err) => return Err(err.into()),
            };
        for event in &events {
            cursor = event.id;
            if parsed.json {
                out!("{}", serde_json::to_string(event)?);
            } else {
                out!(
                    "{:<6} {} {:<28} sid={} {}",
                    event.id,
                    event.ts,
                    event.kind,
                    event
                        .session_id
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "-".into()),
                    event.payload_json.as_deref().unwrap_or(""),
                );
            }
        }
        if !parsed.follow {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(700));
    }
    Ok(())
}

/// `broker metrics`.
pub(super) fn run_metrics(parsed: Parsed) -> Result<(), UsageError> {
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    // Gate executions (pass/fail) vs cache hits with saved time.
    let executed = broker.store().gate_execution_totals()?;
    let cached = broker
        .store()
        .events_after_filtered(0, i64::MAX, Some("gate.cached"))?;
    let saved_ms: i64 = cached
        .iter()
        .filter_map(|e| e.payload_json.as_deref())
        .filter_map(|p| serde_json::from_str::<serde_json::Value>(p).ok())
        .filter_map(|v| v.get("saved_ms").and_then(|s| s.as_i64()))
        .sum();
    let conflicts = broker
        .store()
        .events_after_filtered(0, i64::MAX, Some("merge.conflict"))?
        .len();
    let overlaps = broker
        .store()
        .events_after_filtered(0, i64::MAX, Some("lease.overlap"))?
        .len();

    // Command latency from the safe telemetry file.
    // (calls, total_ms, total_output_bytes, calls_that_recorded_bytes).
    // The last field matters because lines written before output
    // accounting existed carry no size; averaging over every call would
    // silently understate the cost of the ones that do.
    let mut commands: std::collections::BTreeMap<String, (i64, i64, i64, i64)> =
        std::collections::BTreeMap::new();
    let metrics_path = broker
        .main_root()
        .join(".aethyme/logs/command-metrics.jsonl");
    if let Ok(text) = std::fs::read_to_string(&metrics_path) {
        for line in text.lines() {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                let name = v
                    .get("command")
                    .and_then(|c| c.as_str())
                    .unwrap_or("?")
                    .to_string();
                let ms = v.get("duration_ms").and_then(|d| d.as_i64()).unwrap_or(0);
                let bytes = v.get("output_bytes").and_then(|b| b.as_i64());
                let entry = commands.entry(name).or_insert((0, 0, 0, 0));
                entry.0 += 1;
                entry.1 += ms;
                if let Some(bytes) = bytes {
                    entry.2 += bytes;
                    entry.3 += 1;
                }
            }
        }
    }

    if parsed.json {
        let out = serde_json::json!({
            "gates_executed": executed.iter().map(|(g, n, ms)| serde_json::json!({
                "gate": g, "runs": n, "total_ms": ms,
            })).collect::<Vec<_>>(),
            "gate_cache_hits": cached.len(),
            "gate_time_saved_ms": saved_ms,
            "conflicts_caught_pre_gate": conflicts,
            "overlaps_warned": overlaps,
            "commands": commands.iter().map(|(name, (count, ms, bytes, sized))| serde_json::json!({
                "command": name, "count": count, "total_ms": ms,
                "total_output_bytes": bytes, "output_sampled_calls": sized,
            })).collect::<Vec<_>>(),
        });
        out!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        out!("Gate executions:");
        for (gate, runs, ms) in &executed {
            out!("  {gate:<20} {runs} run(s), {ms}ms total");
        }
        out!(
            "Cache hits: {} (≈{}s of checks skipped)",
            cached.len(),
            saved_ms / 1000
        );
        out!("Conflicts caught before any gate ran: {conflicts}");
        out!("Overlap warnings: {overlaps}");
        out!("Broker command overhead:");
        for (name, (count, ms, bytes, sized)) in &commands {
            let output = if *sized == 0 {
                "output not sampled".to_string()
            } else {
                format!("{} per call", human_bytes((*bytes / sized.max(&1)) as u64))
            };
            out!(
                "  {name:<20} {count} call(s), {ms}ms total, {}ms avg, {output}",
                ms / count.max(&1),
            );
        }
        // Output size is what an agent pays per turn, so name the
        // worst offender rather than leaving it to be spotted in a table.
        if let Some((name, (_, _, bytes, sized))) = commands
            .iter()
            .filter(|(_, (_, _, _, sized))| *sized > 0)
            .max_by_key(|(_, (_, _, bytes, sized))| bytes / sized.max(&1))
        {
            out!(
                "Largest agent-facing output: {name} at {} per call over {sized} sampled call(s)",
                human_bytes((*bytes / sized.max(&1)) as u64)
            );
        }
    }
    Ok(())
}

/// `broker doctor`.
pub(super) fn run_doctor(parsed: Parsed) -> Result<(), UsageError> {
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    let report = if parsed.fix_version {
        broker.doctor_with_version_fix()?
    } else {
        broker.doctor()?
    };
    let blocker_report = broker.blockers();
    if parsed.json {
        let mut value = serde_json::to_value(&report)?;
        value["blockers"] = serde_json::to_value(&blocker_report.blockers)?;
        out!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        out!("integrity: {}", report.integrity);
        out!(
            "version: {} — {}",
            report.version.status.as_str(),
            report.version.message
        );
        if let Some(describe) = &report.version.binary.describe {
            out!(
                "  binary: aethyme {} ({describe})",
                report.version.binary.version
            );
        } else {
            out!("  binary: aethyme {}", report.version.binary.version);
        }
        if let Some(path) = &report.version.binary.path {
            out!("  path: {path}");
        }
        if report.version.repo_is_aethyme_source {
            let integration = report
                .version
                .integration_describe
                .as_deref()
                .or(report.version.integration_head.as_deref())
                .unwrap_or("unknown");
            out!(
                "  integration: {} {integration}",
                report.version.integration_branch
            );
        }
        if let Some(movement) = &report.integration_movement {
            out!("integration movement: {}", movement.message);
            out!(
                "  head: {} @ {}",
                movement.branch,
                short_commit(&movement.head)
            );
            for session in movement.live_sessions.iter().take(5) {
                out!(
                    "  live session {} {} {}",
                    session.id,
                    session.status.as_str(),
                    session.branch
                );
            }
            if movement.live_sessions.len() > 5 {
                out!(
                    "  and {} more live {}",
                    movement.live_sessions.len() - 5,
                    plural(movement.live_sessions.len() - 5, "session", "sessions")
                );
            }
            for command in &movement.commands {
                out!("  run: {command}");
            }
        }
        if let Some(repair) = &report.version_repair {
            out!(
                "version repair: {} — {}",
                repair.status.as_str(),
                repair.message
            );
            if repair.attempted {
                out!("  duration: {}ms", repair.duration_ms);
                if let Some(code) = repair.exit_code {
                    out!("  exit: {code}");
                }
                for step in &repair.steps {
                    out!(
                        "  {} {}: {}",
                        step.component,
                        step.action,
                        if step.success { "pass" } else { "fail" }
                    );
                    out!("    command: {}", step.command.join(" "));
                    if let Some(code) = step.exit_code {
                        out!("    exit: {code}");
                    }
                }
                if repair.steps.is_empty() {
                    out!("  command: {}", repair.command.join(" "));
                }
            }
            if !repair.stdout_tail.is_empty() {
                out!("  stdout tail:");
                for line in &repair.stdout_tail {
                    out!("    {line}");
                }
            }
            if !repair.stderr_tail.is_empty() {
                out!("  stderr tail:");
                for line in &repair.stderr_tail {
                    out!("    {line}");
                }
            }
        }
        if report.missing_worktrees.is_empty() {
            out!("worktrees: all live session worktrees exist");
        } else {
            for id in &report.missing_worktrees {
                out!("worktrees: session {id} worktree is missing (adopt gone stale?)");
            }
        }
        if report.orphaned_pidfiles.is_empty() {
            out!("gate runs: no orphaned pidfiles");
        } else {
            for name in &report.orphaned_pidfiles {
                out!("gate runs: orphaned pidfile removed: {name}");
            }
        }
        if blocker_report.blockers.is_empty() && blocker_report.unavailable.is_empty() {
            out!("blockers: none");
        } else {
            render_blockers(&blocker_report);
        }
        out!(
            "retention: {} rows, {} files, {} worktrees, {} retained, {} reclaimable; {} protected findings",
            report.retention.candidate_rows,
            report.retention.candidate_files,
            report.retention.candidate_worktrees,
            human_bytes(report.retention.estimated_retained_bytes),
            human_bytes(report.retention.estimated_reclaimable_bytes),
            report.retention.blockers,
        );
        for warning in &report.retention.retention_config_warnings {
            out!("  retention warning: {warning}");
        }
        // Doctor takes the recorded-size path, so its byte figures
        // can be floors. Say so before the budget line: a floor
        // under the budget is not a pass, it is an unanswered
        // question, and reporting only the pass is how the budget
        // stopped meaning anything (#176).
        if report.retention.unmeasured_directory_count > 0 {
            out!(
                "  retention: {} retained {} never been sized; byte totals above are a floor -- measure with `aethyme broker gc plan`",
                report.retention.unmeasured_directory_count,
                crate::broker::plural_word(
                    report.retention.unmeasured_directory_count,
                    "directory has",
                    "directories have",
                ),
            );
        }
        match report.retention.budget_verdict {
            crate::BudgetVerdict::Over => out!(
                "  warning: retained storage exceeds the configured {} budget; review `aethyme broker gc plan`",
                human_bytes(report.retention.policy.retained_bytes_budget)
            ),
            crate::BudgetVerdict::Unknown => out!(
                "  warning: cannot tell whether retained storage is within the configured {} budget; run `aethyme broker gc plan`",
                human_bytes(report.retention.policy.retained_bytes_budget)
            ),
            crate::BudgetVerdict::Within | crate::BudgetVerdict::Unset => {}
        }
        if let Some(digest) = &report.retention.pending_recovery_digest {
            out!("  recovery pending: aethyme broker gc apply --confirm {digest}");
        }
        if report.healthy() {
            out!("doctor: healthy");
        } else {
            return Err(UsageError::Message("doctor found problems".into()));
        }
    }
    Ok(())
}

/// `broker init`.
pub(super) fn run_init(parsed: Parsed) -> Result<(), UsageError> {
    let cwd = std::env::current_dir()
        .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?;
    let report = crate::init::guided_init(&cwd)?;
    if parsed.json {
        out!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        out!("Phase 1/3 — certify (read-only):");
        print_checks(&report.certify.checks);
        let Some(scaffold) = &report.scaffold else {
            out!();
            return Err(UsageError::Message(
                "certification failed — fix the FAIL items above, then re-run \
                 `aethyme init` (nothing was written)"
                    .into(),
            ));
        };
        out!();
        out!("Phase 2/3 — scaffold (deterministic, only-if-missing):");
        print_checks(&scaffold.checks);
        out!();
        out!("Phase 3/3 — gates draft (adaptive):");
        match &report.gates {
            Some(gates) => print_checks(&gates.checks),
            None => out!(
                "{:<8} {:<28} .aethyme/gates.toml already present — drafting skipped",
                "skip",
                "gates.draft"
            ),
        }
        out!();
        let write_checks: Vec<&crate::init::Check> = scaffold
            .checks
            .iter()
            .chain(report.gates.iter().flat_map(|g| g.checks.iter()))
            .collect();
        let existing: Vec<&str> = write_checks
            .iter()
            .filter(|c| c.status == crate::init::CheckStatus::Pass)
            .map(|c| c.id)
            .collect();
        if !existing.is_empty() {
            out!("Already existed (untouched): {}", existing.join(", "));
        }
        if report.changed {
            out!("Created this run:");
            for check in write_checks
                .iter()
                .filter(|c| c.status == crate::init::CheckStatus::Created)
            {
                out!("  - {} — {}", check.id, check.detail);
            }
        } else {
            out!(
                "Nothing created — this repository was already set up \
                 (init is idempotent)."
            );
        }
        out!();
        out!("Repository initialized.");
        out!();
        out!(
            "{}",
            crate::render_readiness_text(&report.readiness).trim_end()
        );
    }
    if !report.certified() {
        return Err(UsageError::Message("initialization failed".into()));
    }
    Ok(())
}

/// `broker certify` | `broker scaffold`.
pub(super) fn run_certify_scaffold(parsed: Parsed, subcommand: &str) -> Result<(), UsageError> {
    let cwd = std::env::current_dir()
        .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?;
    let report = if subcommand == "certify" {
        crate::init::certify(&cwd)?
    } else {
        crate::init::scaffold(&cwd)?
    };
    if parsed.json {
        out!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_checks(&report.checks);
        out!();
        if report.certified() {
            if subcommand == "certify" {
                out!("Certified (read-only — nothing written).");
            } else {
                out!("Scaffolding done — review the drafts, then run `aethyme certify`.");
            }
        } else {
            return Err(UsageError::Message("FAIL items above must be fixed".into()));
        }
    }
    if !report.certified() {
        return Err(UsageError::Message("certification failed".into()));
    }
    Ok(())
}
