//! `broker worktree-root`, `start`, `start-agent`, `adopt`, `prepare`, `agents`, `handoff`, `finish`, `close`, `cleanup` and `worktrees`: a session's life cycle.

use super::*;

/// Entry point for the router. Returns a process exit code.
/// `--agent` when given, else `AETHYME_AGENT`. Resolved in the agent's own
/// process, because promotion can run from a different one (issue rescue: a
/// later `submit` or queue drain would otherwise credit whoever triggered it).
pub(super) fn session_agent_identity(explicit: Option<&str>) -> Option<String> {
    explicit
        .map(str::to_string)
        .or_else(|| crate::attribution::agent_from_env().map(|identity| identity.render()))
}

pub(super) fn session_context_value(
    explicit: Option<&String>,
    environment: &[&str],
) -> Option<String> {
    explicit
        .cloned()
        .or_else(|| environment.iter().find_map(|name| std::env::var(name).ok()))
}

pub(super) fn session_context(parsed: &Parsed) -> crate::SessionContext {
    crate::SessionContext::new(
        session_context_value(
            parsed.repo_name.as_ref(),
            &[
                "AETHYME_SESSION_REPO_NAME",
                "AETHYME_CHAU7_REPO_NAME",
                "AETHYME_REPO_NAME",
            ],
        ),
        session_context_value(
            parsed.tab_name.as_ref(),
            &["AETHYME_SESSION_TAB_NAME", "AETHYME_CHAU7_TAB_NAME"],
        ),
        session_context_value(
            parsed.ai_provider.as_ref(),
            &[
                "AETHYME_SESSION_AI_PROVIDER",
                "AETHYME_CHAU7_AI_PROVIDER",
                "AETHYME_AI_PROVIDER",
            ],
        ),
    )
}

/// The base a session's branch was cut from, and what that base carries
/// relative to the default branch in both directions.
///
/// Shared by `start` and `start-agent` because both select a base and both open
/// pull requests from it. `start-agent` is the detached case, where nobody is
/// watching the terminal -- which is exactly why it cannot be the one surface
/// that stays silent about an inherited gap (#290).
pub(super) fn render_start_base(base: &crate::SessionStartBase) {
    out!(
        "Start base: {} at {} ({})",
        base.ref_name,
        short_commit(&base.commit),
        base.evidence.as_str()
    );
    let default_ref = || base.default_ref.as_deref().unwrap_or("the default branch");
    // Integration is normally ahead of the default branch. Behind means it
    // stopped following, and every session cut from it inherits the gap —
    // silently, because the line above looks identical either way.
    if let Some(behind) = base.behind_default_commits
        && behind > 0
    {
        out!(
            "warning: this base is {behind} commit(s) behind {}; a branch cut \
             from it carries that gap into its pull request",
            default_ref()
        );
        out!(
            "         inspect with `aethyme broker advanced integration status`, or start \
             from the default branch if integration is not the base you want."
        );
    }
    // Ahead is the designed state: integration carries promoted work the default
    // branch has not published. It is also the state that puts other sessions'
    // commits into this session's pull request, and nothing printed above
    // separates 0 from 20.
    //
    // Deliberately a note rather than a warning. In a repository that promotes
    // this is true at almost every start, and a warning that always fires stops
    // being read. Escalating it belongs with the merge-path detection in #290
    // phase 1.2, which can tell whether pull requests are how work ships here;
    // until then the count is the signal.
    if let Some(ahead) = base.ahead_default_commits
        && ahead > 0
    {
        out!(
            "note: this base is {ahead} commit(s) ahead of {}; a pull request \
             opened from this branch carries them alongside your own work",
            default_ref()
        );
    }
}

pub(super) fn render_worktree_placement(placement: &crate::WorktreePlacement) {
    let boundary = if placement.outside_repository {
        "outside the repository"
    } else {
        "inside the repository fallback"
    };
    out!(
        "Worktree root: {} ({}, {boundary})",
        placement.root.display(),
        placement.source.as_str()
    );
    if let Some(reason) = &placement.fallback_reason {
        out!("Warning: external worktree placement was unavailable: {reason}");
    }
}

pub(super) fn render_preparation_status(
    status: &crate::PreparationStatus,
    json: bool,
) -> Result<(), UsageError> {
    if json {
        out!("{}", serde_json::to_string_pretty(status)?);
        return Ok(());
    }
    out!(
        "Preparation {:?} for session {}: {}",
        status.state,
        status.session_id,
        status.reason
    );
    if let Some(digest) = &status.expected_digest {
        out!("Expected digest: {}", short_sha(digest));
    }
    if !status.missing_outputs.is_empty() {
        out!("Missing outputs:");
        for path in &status.missing_outputs {
            out!("  {path}");
        }
    }
    if let Some(next_action) = &status.next_action {
        out!("Next: {next_action}");
    }
    Ok(())
}

pub(super) fn render_finish_report(report: &crate::FinishReport) {
    out!(
        "Finish session {}: {}",
        report.session_id,
        report.status.as_str()
    );
    out!("  {}", report.summary);
    out!("  worktree: {}", report.worktree_path);
    if let Some(entry_id) = report.latest_queue_entry_id {
        let status = report
            .latest_queue_status
            .map(|status| status.as_str())
            .unwrap_or("unknown");
        out!("  latest queue: qid {entry_id} ({status})");
    }
    out!(
        "  delivery: submitted={}, promoted={}, published={}",
        if report.delivery.submitted {
            "yes"
        } else {
            "no"
        },
        if report.delivery.promoted {
            "yes"
        } else {
            "no"
        },
        if report.delivery.published {
            "yes"
        } else {
            "no"
        },
    );
    if !report.dirty_paths.is_empty() {
        out!("  dirty paths: {}", capped_join(&report.dirty_paths, 8));
    }
    if report.unsubmitted_commits > 0 {
        out!("  unsubmitted commits: {}", report.unsubmitted_commits);
    }
    out!(
        "  pending work: {} ({} dirty paths, {} unsubmitted commits{})",
        if report.pending_work.present {
            "yes"
        } else {
            "no"
        },
        report.pending_work.dirty_path_count,
        report.pending_work.unsubmitted_commits,
        if report.pending_work.worktree_missing {
            ", worktree missing"
        } else {
            ""
        },
    );
    if report.leases_held.is_empty() {
        out!("  leases held: none recorded");
    } else {
        // Past tense once closed: closing released these, and the list is
        // handoff history rather than a claim of current ownership (#141).
        if report.cleanup.completed {
            out!("  leases at close:");
        } else {
            out!("  leases held:");
        }
        for lease in &report.leases_held {
            out!(
                "    {} {} {} (expires {}, released {})",
                lease.kind.as_str(),
                match lease.state {
                    crate::FinishLeaseState::Active => "active",
                    crate::FinishLeaseState::Released => "released",
                    crate::FinishLeaseState::Expired => "expired",
                },
                lease.path,
                lease
                    .expires_at
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "never".into()),
                lease
                    .released_at
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "never".into()),
            );
        }
    }
    match &report.last_gate {
        Some(gate) => out!(
            "  last gate: {} {} on tree {} at {} ({})",
            gate.gate,
            gate.status.as_str(),
            short_commit(&gate.tree_hash),
            gate.recorded_at,
            match gate.cache_source {
                crate::FinishGateCacheSource::Executed => "executed",
                crate::FinishGateCacheSource::CacheHit => "cache hit",
            }
        ),
        None => out!("  last gate: none recorded"),
    }
    match &report.last_graph_integrity {
        Some(graph) => out!(
            "  last graph integrity: {} on tree {} under policy {} at {}",
            graph.status.as_str(),
            short_commit(&graph.tree_hash),
            short_commit(&graph.policy_digest),
            graph.recorded_at,
        ),
        None => out!("  last graph integrity: none recorded"),
    }
    out!(
        "  cleanup safe: {}",
        if report.cleanup_safe { "yes" } else { "no" }
    );
    out!(
        "  physical cleanup: requested={}, kept={}, attempted={}, completed={}, reclaimed={} bytes",
        report.cleanup.requested,
        report.cleanup.kept,
        report.cleanup.attempted,
        report.cleanup.completed,
        report.cleanup.reclaimed_bytes,
    );
    out!(
        "    worktree: {} ({})",
        report.worktree_path,
        if report.cleanup.worktree_removed {
            "removed"
        } else {
            "retained"
        }
    );
    if let Some(branch) = &report.cleanup.branch_ref {
        out!(
            "    branch: {}{} ({})",
            branch,
            report
                .cleanup
                .branch_tip
                .as_deref()
                .map(|tip| format!(" at {tip}"))
                .unwrap_or_default(),
            if report.cleanup.branch_removed {
                "removed"
            } else {
                "retained"
            }
        );
    }
    if let Some(action) = &report.cleanup.recovery_action {
        out!("    recovery: {action}");
    }
    for warning in &report.warnings {
        out!("  warning: {warning}");
    }
    if report.next_commands.is_empty() {
        out!("  next: none");
    } else {
        out!("  next:");
        for command in &report.next_commands {
            out!("    run: {command}");
        }
    }
    out!(
        "  recommended next: {}",
        report.recommended_next_action.as_deref().unwrap_or("none")
    );
}

pub(super) fn render_cleanup_sweep_report(report: &crate::CleanupSweepReport, detail: bool) {
    out!(
        "Cleanup {}: {} retained broker-owned worktrees, {} eligible",
        if report.applied { "apply" } else { "plan" },
        report.plan.retained_worktree_count,
        report.plan.eligible_worktree_count,
    );
    out!(
        "  retained: {}; reclaimable now: {}; branches: {} retained, {} eligible",
        human_bytes(report.plan.estimated_retained_bytes),
        human_bytes(report.plan.estimated_reclaimable_bytes),
        report.plan.retained_branch_count,
        report.plan.eligible_branch_count,
    );
    out!("  reviewed plan digest: {}", report.plan.digest);
    render_capped(&report.plan.worktrees, GC_LIST_CAP, detail, |item| {
        out!(
            "  session {}: {} ({}) — {}",
            item.session_id,
            item.disposition.as_str(),
            item.estimated_bytes
                .map(human_bytes)
                .unwrap_or_else(|| "size unavailable".into()),
            item.reason,
        );
        out!("    {}", item.worktree_path);
        if let Some(branch_tip) = &item.branch_tip {
            out!("    {} at {}", item.branch_ref, branch_tip);
        }
        for command in &item.inspection_commands {
            out!("    inspect: {command}");
        }
        if !item.eligible() {
            out!("    explicit discard: {}", item.force_cleanup_command);
        }
    });
    if report.applied {
        out!(
            "  removed: {}",
            if report.removed_session_ids.is_empty() {
                "none".into()
            } else {
                report
                    .removed_session_ids
                    .iter()
                    .map(i64::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        );
        for failure in &report.failures {
            out!(
                "  retained session {} after revalidation: {}",
                failure.session_id,
                failure.reason
            );
        }
    } else if report.plan.eligible_worktree_count > 0 || report.plan.eligible_branch_count > 0 {
        out!(
            "  apply: aethyme broker finish cleanup --all-cleaned --apply --confirm {}",
            report.plan.digest
        );
    }
}

pub(super) fn render_handoff_report(report: &crate::SessionHandoffReport) {
    let handoff = &report.handoff;
    out!(
        "Session {} handoff: {} (event {} at {})",
        handoff.session_id,
        handoff.status.as_str(),
        report.event_id,
        report.recorded_at
    );
    if let Some(entry_id) = handoff.latest_queue_entry_id {
        let status = handoff
            .latest_queue_status
            .map(|status| status.as_str())
            .unwrap_or("unknown");
        out!("  latest queue: qid {entry_id} ({status})");
    }
    out!(
        "  delivery: submitted={}, promoted={}, published={}",
        if handoff.delivery.submitted {
            "yes"
        } else {
            "no"
        },
        if handoff.delivery.promoted {
            "yes"
        } else {
            "no"
        },
        if handoff.delivery.published {
            "yes"
        } else {
            "no"
        },
    );
    out!(
        "  pending work: {} ({} dirty paths, {} unsubmitted commits{})",
        if handoff.pending_work.present {
            "yes"
        } else {
            "no"
        },
        handoff.pending_work.dirty_path_count,
        handoff.pending_work.unsubmitted_commits,
        if handoff.pending_work.worktree_missing {
            ", worktree missing"
        } else {
            ""
        },
    );
    let active = handoff
        .leases_held
        .iter()
        .filter(|lease| lease.state == crate::FinishLeaseState::Active)
        .count();
    let released = handoff
        .leases_held
        .iter()
        .filter(|lease| lease.state == crate::FinishLeaseState::Released)
        .count();
    let expired = handoff
        .leases_held
        .iter()
        .filter(|lease| lease.state == crate::FinishLeaseState::Expired)
        .count();
    out!(
        "  leases: {} recorded ({} active, {} released, {} expired)",
        handoff.leases_held.len(),
        active,
        released,
        expired
    );
    match &handoff.last_gate {
        Some(gate) => out!(
            "  last gate: {} {} on tree {} at {} ({})",
            gate.gate,
            gate.status.as_str(),
            short_commit(&gate.tree_hash),
            gate.recorded_at,
            match gate.cache_source {
                crate::FinishGateCacheSource::Executed => "executed",
                crate::FinishGateCacheSource::CacheHit => "cache hit",
            }
        ),
        None => out!("  last gate: none recorded"),
    }
    match &handoff.last_graph_integrity {
        Some(graph) => out!(
            "  last graph integrity: {} on tree {} under policy {} at {}",
            graph.status.as_str(),
            short_commit(&graph.tree_hash),
            short_commit(&graph.policy_digest),
            graph.recorded_at,
        ),
        None => out!("  last graph integrity: none recorded"),
    }
    out!(
        "  cleanup safe: {}",
        if handoff.cleanup_safe { "yes" } else { "no" }
    );
    out!(
        "  next: {}",
        handoff.recommended_next_action.as_deref().unwrap_or("none")
    );
}

pub(super) fn resolve_handoff_worktree(path: &std::path::Path) -> Result<PathBuf, UsageError> {
    if path.exists() {
        return Ok(crate::GitRepo::discover(path)?.root().to_path_buf());
    }
    let mut existing = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| UsageError::Message(format!("cannot resolve cwd: {error}")))?
            .join(path)
    };
    let mut missing_tail = Vec::new();
    while !existing.exists() {
        let Some(name) = existing.file_name() else {
            return Ok(existing);
        };
        missing_tail.push(name.to_os_string());
        if !existing.pop() {
            return Ok(existing);
        }
    }
    let mut resolved = existing.canonicalize().unwrap_or(existing);
    for name in missing_tail.into_iter().rev() {
        resolved.push(name);
    }
    Ok(resolved)
}

/// Record what a new session says it will work on, and say what happened.
///
/// Parsed here so a malformed `--claim` fails before the session exists rather
/// than leaving one half-described. Reported in the same breath because a
/// silent capture would make "no collisions" and "nothing recorded"
/// indistinguishable.
pub(super) fn capture_declared_scopes(
    broker: &mut crate::Broker,
    session_id: i64,
    task: Option<&str>,
    declared: &[String],
    quiet: bool,
) -> Result<(), UsageError> {
    let mut parsed = Vec::with_capacity(declared.len());
    for text in declared {
        let (kind, value, operation) =
            crate::parse_scope_argument(text).map_err(UsageError::Message)?;
        parsed.push((kind, value, operation));
    }
    let report = broker.capture_session_scopes(session_id, &parsed, task)?;
    if quiet {
        return Ok(());
    }
    if report.declared > 0 || report.derived > 0 {
        out!(
            "Scope: {} declared, {} derived from the task",
            report.declared,
            report.derived
        );
        let overlaps = broker.scope_overlaps_snapshot()?;
        let mine: Vec<_> = overlaps
            .iter()
            .filter(|overlap| overlap.session_a == session_id || overlap.session_b == session_id)
            .collect();
        for overlap in &mine {
            let other = if overlap.session_a == session_id {
                overlap.session_b
            } else {
                overlap.session_a
            };
            out!(
                "  {} with session {}: {}",
                overlap.severity.as_str(),
                other,
                overlap.explanation
            );
            out!("    {}", overlap.suggestion);
        }
        if mine.is_empty() {
            out!("  no other live session names these targets");
        }
    } else if let Some(reason) = &report.degraded {
        // Never let an empty scope set read as a clean bill of health.
        out!("Scope: none recorded — {reason}");
    }
    Ok(())
}

/// Print the worktree report: what holds work that exists nowhere else, first.
///
/// The summary leads with bytes that cannot be reclaimed by any policy, because
/// that is the number a reader acts on -- every other figure in a storage
/// report is already answerable by `broker storage`.
pub(super) fn render_worktree_report(report: &crate::WorktreeReport) {
    if report.rows.is_empty() {
        out!("No worktrees on this host.");
        return;
    }
    out!(
        "{} worktree(s), {}. {} hold work that exists nowhere else ({}).",
        report.rows.len(),
        human_bytes(report.total_bytes),
        report.unique_work_count,
        human_bytes(report.unique_work_bytes),
    );
    if report.unique_work_count > 0 {
        out!("No cleanup can reclaim those; each needs a push-or-discard decision.");
    }
    out!();
    for row in &report.rows {
        let state = match &row.work {
            crate::WorkState::Uncommitted { files } => {
                format!("uncommitted ({files} file(s))")
            }
            crate::WorkState::Unpushed { commits } => {
                format!("unpushed ({commits} commit(s))")
            }
            crate::WorkState::Recoverable => "recoverable".to_string(),
            crate::WorkState::NotACheckout => "not a checkout".to_string(),
            crate::WorkState::PrunableRegistration => "prunable registration".to_string(),
        };
        let idle = row
            .idle_days
            .map(|days| format!("{days}d idle"))
            .unwrap_or_else(|| "-".to_string());
        let git = match (row.git.as_ref(), row.git_registered, row.git_error.as_ref()) {
            (Some(git), _, _) => {
                let mut flags = Vec::new();
                if git.detached {
                    flags.push("detached");
                }
                if git.locked {
                    flags.push("locked");
                }
                if git.prunable {
                    flags.push("prunable");
                }
                if flags.is_empty() {
                    String::new()
                } else {
                    format!("  [git: {}]", flags.join(", "))
                }
            }
            (None, Some(false), _) => "  [git: unregistered]".to_string(),
            (None, _, Some(error)) => {
                let reason = error.split_whitespace().collect::<Vec<_>>().join(" ");
                format!("  [git state unknown: {reason}]")
            }
            _ => String::new(),
        };
        out!(
            "  {:<22} {:>9}  {:<26} {:<10} {}{}{}",
            truncate(&row.repository, 22),
            human_bytes(row.bytes),
            state,
            idle,
            row.branch.as_deref().unwrap_or("-"),
            if row.live { "  [live session]" } else { "" },
            git,
        );
    }
}

/// `broker worktree-root`.
pub(super) fn run_worktree_root(parsed: Parsed) -> Result<(), UsageError> {
    if !parsed.positional.is_empty() {
        return Err(UsageError::Message(
            "worktree-root does not accept positional arguments".into(),
        ));
    }
    let broker = open_broker(parsed.read_only_snapshot)?;
    let plan = broker.worktree_root_plan()?;
    if parsed.json {
        out!("{}", serde_json::to_string_pretty(&plan)?);
    } else {
        out!("Repository: {}", plan.repository_root.display());
        out!("Repository key: {}", plan.repository_key);
        if let (Some(root), Some(source)) = (&plan.preferred_root, plan.preferred_source) {
            out!(
                "Preferred worktree root: {} ({})",
                root.display(),
                source.as_str()
            );
            out!(
                "Scanner boundary: {}",
                if plan.preferred_outside_repository {
                    "outside the repository"
                } else {
                    "invalid: preferred root resolves inside the repository"
                }
            );
        } else {
            out!("Preferred worktree root: unavailable");
        }
        out!(
            "Legacy fallback: {} (used only when host state is unavailable)",
            plan.legacy_fallback_root.display()
        );
    }
    Ok(())
}

/// `broker adopt`.
pub(super) fn run_adopt(parsed: Parsed) -> Result<(), UsageError> {
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    let path = parsed
        .positional
        .first()
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir().map_err(|e| UsageError::Message(e.to_string()))?);
    let mode = match (parsed.reuse, parsed.replace_stale) {
        (true, true) => {
            return Err(UsageError::Message(
                "--reuse and --replace-stale are mutually exclusive".into(),
            ));
        }
        (true, false) => crate::AdoptMode::Reuse,
        (false, true) => crate::AdoptMode::ReplaceStale,
        (false, false) => crate::AdoptMode::New,
    };
    if parsed.sync_integration && mode != crate::AdoptMode::Reuse {
        return Err(UsageError::Message(
            "--sync-integration requires --reuse".into(),
        ));
    }
    warn_stale_broker_binary(&broker);
    let agent_identity = session_agent_identity(parsed.agent.as_deref());
    let context = session_context(&parsed);
    let report = broker.adopt_with_options_and_context(
        &path,
        parsed.task.as_deref(),
        crate::AdoptOptions {
            mode,
            sync_integration: parsed.sync_integration,
            planned_paths: parsed.planned_paths,
        },
        agent_identity.as_deref(),
        context,
    )?;
    for renamed in &report.renamed_targets {
        out!(
            "Renamed target: {} is now {}{}",
            renamed.from,
            renamed.to,
            match (renamed.promoted_entry_id, renamed.promoted_session_id) {
                (Some(entry), Some(session)) =>
                    format!(" (queue entry {entry}, session {session})"),
                _ => String::new(),
            }
        );
        out!("  port this session's changes onto the new path before submitting");
    }
    let session = &report.session;
    if parsed.json {
        // Scope is recorded in both output modes: `--json` is the form
        // agents use, so skipping capture there drops it for most callers.
        capture_declared_scopes(
            &mut broker,
            session.id,
            parsed.task.as_deref(),
            &parsed.declared_scopes,
            true,
        )?;
        out!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        match report.outcome {
            crate::AdoptOutcome::Created => out!(
                "Created session {} on the existing worktree — {} on branch {}",
                session.id,
                session.worktree_path,
                session.branch
            ),
            crate::AdoptOutcome::Reused => out!(
                "Reusing session {} — worktree {} on branch {}",
                session.id,
                session.worktree_path,
                session.branch
            ),
            crate::AdoptOutcome::Replaced => out!(
                "Replaced the prior session with session {} on the existing worktree — {} on branch {}",
                session.id,
                session.worktree_path,
                session.branch
            ),
        }
        capture_declared_scopes(
            &mut broker,
            session.id,
            parsed.task.as_deref(),
            &parsed.declared_scopes,
            false,
        )?;
        if std::path::Path::new(&session.worktree_path) == broker.main_root() {
            out!(
                "note: main-checkout session — verification is advisory here \
                 (commits land on main before gates run); use a worktree \
                 session for enforced verification."
            );
        }
        // Pre-existing uncommitted work is not this session's, but the
        // repository's own pre-push gate validates the whole snapshot,
        // so it will fail the first push for reasons the session cannot
        // see. Saying so at adopt time is cheaper than discovering it
        // one rejected push at a time.
        if let Ok(repo) = crate::GitRepo::discover(std::path::Path::new(&session.worktree_path))
            && let Ok(dirty) = repo.dirty_paths()
            && !dirty.is_empty()
        {
            let shown = dirty.iter().take(5).cloned().collect::<Vec<_>>();
            out!(
                "warning: {} uncommitted path(s) already present in this checkout \
                 before the session began: {}{}",
                dirty.len(),
                shown.join(", "),
                if dirty.len() > shown.len() {
                    format!(", and {} more", dirty.len() - shown.len())
                } else {
                    String::new()
                }
            );
            out!(
                "         they are not owned by this session, and a repository \
                 pre-push gate validates the whole snapshot — commit or set them \
                 aside, or adopt an isolated worktree instead."
            );
        }
        if let Some(sync) = &report.integration_sync {
            let summary = match sync.outcome {
                crate::AdoptIntegrationSyncOutcome::AlreadyCurrent => "already current",
                crate::AdoptIntegrationSyncOutcome::FastForwarded => "fast-forwarded",
            };
            out!(
                "Integration synchronization: {summary} ({} -> {}, {} at {})",
                short_commit(&sync.before_head),
                short_commit(&sync.after_head),
                sync.integration_branch,
                short_commit(&sync.integration_head),
            );
        }
        if let Some(drift) = &report.integration_drift {
            out!(
                "Integration drift: {} (session HEAD {}, {} HEAD {}; {} ahead, {} behind)",
                drift.relation.as_str(),
                short_commit(&drift.session_head),
                drift.integration_branch,
                short_commit(&drift.integration_head),
                drift.ahead_commits,
                drift.behind_commits,
            );
            if !drift.overlapping_changed_paths.is_empty() {
                out!("Overlapping changed paths:");
                for path in &drift.overlapping_changed_paths {
                    out!("  {path}");
                }
            }
            if let Some(warning) = &drift.warning {
                out!("Warning: {warning}");
            }
            out!("Safe next action: {}", drift.safe_next_action);
        }
        render_planned_explicit_leases(&report.planned_explicit_leases);
        render_preparation_status(&report.preparation, false)?;
    }
    Ok(())
}

/// `broker start`.
pub(super) fn run_start(parsed: Parsed) -> Result<(), UsageError> {
    Broker::reject_integration_based_review_task(parsed.pull_request)?;
    let context = session_context(&parsed);
    let task = parsed
        .task
        .ok_or(UsageError::Message("start requires --task".into()))?;
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    warn_stale_broker_binary(&broker);
    let agent_identity = session_agent_identity(parsed.agent.as_deref());
    let report = broker.start_worktree_with_planned_paths_and_context(
        &task,
        &parsed.planned_paths,
        agent_identity.as_deref(),
        context,
        parsed.base.as_deref(),
    )?;
    let session = &report.session;
    if parsed.json {
        capture_declared_scopes(
            &mut broker,
            session.id,
            Some(task.as_str()),
            &parsed.declared_scopes,
            true,
        )?;
        out!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        out!(
            "Started session {} — worktree {} on branch {}",
            session.id,
            session.worktree_path,
            session.branch
        );
        capture_declared_scopes(
            &mut broker,
            session.id,
            Some(task.as_str()),
            &parsed.declared_scopes,
            false,
        )?;
        render_start_base(&report.start_base);
        render_worktree_placement(&report.worktree_placement);
        render_planned_explicit_leases(&report.planned_explicit_leases);
        render_preparation_status(&report.preparation, false)?;
        out!("Worktree: cd {}", session.worktree_path);
    }
    Ok(())
}

/// `broker start-agent`.
pub(super) fn run_start_agent(parsed: Parsed) -> Result<(), UsageError> {
    Broker::reject_integration_based_review_task(parsed.pull_request)?;
    let context = session_context(&parsed);
    let task = parsed
        .task
        .ok_or(UsageError::Message("start-agent requires --task".into()))?;
    let cmd = parsed
        .cmd
        .ok_or(UsageError::Message("start-agent requires --cmd".into()))?;
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    warn_stale_broker_binary(&broker);
    let agent_identity = session_agent_identity(parsed.agent.as_deref());
    let report = broker.start_agent_report_with_context(
        &task,
        &cmd,
        agent_identity.as_deref(),
        context,
        parsed.base.as_deref(),
    )?;
    let session = &report.session;
    if parsed.json {
        out!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        out!(
            "Started session {} (pid {}) — worktree {} on branch {}\nLog: {}",
            session.id,
            session.pid.unwrap_or(-1),
            session.worktree_path,
            session.branch,
            session.log_path.as_deref().unwrap_or("-"),
        );
        render_start_base(&report.start_base);
        render_worktree_placement(&report.worktree_placement);
    }
    Ok(())
}

/// `broker prepare`.
pub(super) fn run_prepare(parsed: Parsed) -> Result<(), UsageError> {
    let session_id = parsed
        .session
        .ok_or_else(|| UsageError::Message("prepare requires --session <id>".into()))?;
    match parsed.positional.first().map(String::as_str) {
        Some("status") => {
            if parsed.positional.len() != 1 {
                return Err(UsageError::Message(
                    "prepare status accepts no additional arguments".into(),
                ));
            }
            if parsed.offline || parsed.wait.is_some() {
                return Err(UsageError::Message(
                    "--offline and --wait apply only to preparation execution".into(),
                ));
            }
            let broker = open_broker(true)?;
            let status = broker.preparation_status(session_id)?;
            render_preparation_status(&status, parsed.json)?;
        }
        None => {
            let wait = parsed
                .wait
                .as_deref()
                .map(parse_resource_duration)
                .transpose()?
                .unwrap_or_default();
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let report = broker.prepare_session(session_id, parsed.offline, wait)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                out!(
                    "Preparation {:?} for session {} (digest {})",
                    report.state,
                    report.session_id,
                    short_sha(&report.digest)
                );
                for step in &report.steps {
                    out!(
                        "  {}: {} (exit {:?})",
                        step.name,
                        if step.succeeded { "passed" } else { "failed" },
                        step.exit_code
                    );
                }
                if report.shared_cache_coordinated {
                    out!("Shared cache: coordinated host-wide");
                }
                out!("Next: {}", report.next_action);
            }
        }
        Some(other) => {
            return Err(UsageError::Message(format!(
                "unknown prepare action {other:?}; expected status or no action"
            )));
        }
    }
    Ok(())
}

/// `broker agents`.
pub(super) fn run_agents(parsed: Parsed) -> Result<(), UsageError> {
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    let (overlaps, views) = if parsed.read_only_snapshot {
        (
            broker.lease_overlaps_snapshot()?,
            broker.agents_snapshot(now_ms())?,
        )
    } else {
        (broker.refresh_leases()?, broker.agents(now_ms())?)
    };
    if parsed.json {
        out!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "agents": views,
                "overlaps": overlaps,
            }))?
        );
    } else if views.is_empty() {
        out!("No live sessions. Start one with `aethyme broker start --task \"...\"`.");
    } else {
        out!(
            "{:<4} {:<8} {:<8} {:<24} TASK",
            "ID",
            "STATUS",
            "ORIGIN",
            "BRANCH"
        );
        for view in views {
            out!(
                "{:<4} {:<8} {:<8} {:<24} {}",
                view.session.id,
                view.derived_status.as_str(),
                view.session.origin.as_str(),
                view.session.branch,
                view.session.task.as_deref().unwrap_or("-"),
            );
        }
        print_overlap_warnings(&overlaps);
    }
    Ok(())
}

/// `broker handoff`.
pub(super) fn run_handoff(parsed: Parsed) -> Result<(), UsageError> {
    let broker = open_broker(parsed.read_only_snapshot)?;
    let report = match (parsed.session, parsed.worktree.as_deref()) {
        (Some(session), None) => broker.latest_handoff_for_session(session)?,
        (None, Some(worktree)) => {
            let worktree = resolve_handoff_worktree(worktree)?;
            broker.latest_handoff_for_worktree(&worktree)?
        }
        (Some(_), Some(_)) => {
            return Err(UsageError::Message(
                "handoff takes either --session <id> or --worktree <path>, not both".into(),
            ));
        }
        (None, None) => {
            return Err(UsageError::Message(
                "handoff requires --session <id> or --worktree <path>".into(),
            ));
        }
    };
    if parsed.json {
        out!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        render_handoff_report(&report);
    }
    Ok(())
}

/// `broker finish`.
pub(super) fn run_finish(parsed: Parsed) -> Result<(), UsageError> {
    let session = parsed
        .session
        .ok_or(UsageError::Message("finish requires --session <id>".into()))?;
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    let report = broker.finish_with_options(
        session,
        crate::FinishOptions {
            keep_worktree: parsed.keep_worktree,
        },
    )?;
    if parsed.json {
        out!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        render_finish_report(&report);
        if report.status == crate::FinishStatus::Blocked {
            return Err(UsageError::Message("session is not ready to finish".into()));
        }
    }
    Ok(())
}

/// `broker close`.
pub(super) fn run_close(parsed: Parsed) -> Result<(), UsageError> {
    let session = parsed
        .session
        .ok_or(UsageError::Message("close requires --session <id>".into()))?;
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    broker.close(session)?;
    if parsed.json {
        out!("{}", serde_json::json!({ "closed": session }));
    } else {
        out!(
            "Session {session} closed (state only — worktree untouched). \
             Next task on the same worktree: `aethyme broker start --adopt --task \"...\"`."
        );
    }
    Ok(())
}

/// `broker worktrees`.
pub(super) fn run_worktrees(parsed: Parsed) -> Result<(), UsageError> {
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    let report = broker.worktree_report()?;
    if parsed.json {
        out!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        render_worktree_report(&report);
    }
    Ok(())
}

/// `broker cleanup`.
pub(super) fn run_cleanup(parsed: Parsed) -> Result<(), UsageError> {
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    if parsed.all_cleaned {
        if !parsed.positional.is_empty() || parsed.force || parsed.dry_run {
            return Err(UsageError::Message(
                "cleanup --all-cleaned takes no session id, --force, or --dry-run; planning is already the default and --apply removes only revalidated eligible worktrees"
                    .into(),
            ));
        }
        if !parsed.apply && parsed.confirm.is_some() {
            return Err(UsageError::Message(
                "cleanup --all-cleaned --confirm requires --apply; review the current plan first"
                    .into(),
            ));
        }
        let report = broker.cleanup_cleaned_worktrees(parsed.apply, parsed.confirm.as_deref())?;
        if parsed.json {
            out!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            render_cleanup_sweep_report(&report, parsed.detail);
        }
    } else {
        if parsed.apply {
            return Err(UsageError::Message(
                "cleanup <session-id> does not take --apply; use the exact command after finish reports cleanup safe"
                    .into(),
            ));
        }
        // The table admits these for `--all-cleaned`; for one session they
        // would be dropped, and `--dry-run` would then clean for real.
        if let Some(flag) = [
            ("--dry-run", parsed.dry_run),
            ("--confirm", parsed.confirm.is_some()),
            ("--detail", parsed.detail),
        ]
        .into_iter()
        .find_map(|(flag, given)| given.then_some(flag))
        {
            return Err(UsageError::Exit {
                message: format!(
                    "cleanup <session-id> does not take {flag}; it applies to cleanup --all-cleaned"
                ),
                code: crate::exit_status::USAGE,
            });
        }
        let id: i64 = parsed
            .positional
            .first()
            .ok_or(UsageError::Message(
                "cleanup requires a session id or --all-cleaned".into(),
            ))?
            .parse()
            .map_err(|_| UsageError::Message("session id must be an integer".into()))?;
        broker.cleanup(id, parsed.force)?;
        if parsed.json {
            out!("{{\"cleaned\":{id}}}");
        } else {
            out!("Cleaned session {id}.");
        }
    }
    Ok(())
}
