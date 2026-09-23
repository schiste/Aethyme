//! `broker submit`, `repair`, `promote`, `queue`, `checkpoint`, `promotion-record`, `main reconcile` and `representation`: integrating a session's work.

use super::*;

pub(super) fn queue_status_is_current(status: crate::MergeStatus) -> bool {
    matches!(
        status,
        crate::MergeStatus::Submitted
            | crate::MergeStatus::Simulating
            | crate::MergeStatus::Conflict
            | crate::MergeStatus::Verified
    )
}

pub(super) fn render_queue_history(page: &crate::MergeQueueHistoryPage) {
    if page.entries.is_empty() {
        out!("No terminal merge-queue entries in this page.");
    } else {
        out!("{:<4} {:<4} {:<17} HEAD", "ID", "SID", "STATUS");
        for entry in &page.entries {
            out!(
                "{:<4} {:<4} {:<17} {}",
                entry.id,
                entry.session_id,
                entry.status.as_str(),
                short_commit(&entry.head_commit)
            );
        }
    }
    let summary = page
        .terminal_counts
        .iter()
        .map(|item| format!("{} {}", item.status.as_str(), item.count))
        .collect::<Vec<_>>()
        .join(", ");
    out!(
        "Terminal totals: {}",
        if summary.is_empty() { "none" } else { &summary }
    );
    if let Some(before) = page.next_before_id {
        out!("Next: aethyme broker queue history --before {before}");
    }
}

pub(super) fn render_repair_report(report: &crate::RepairReport) {
    out!(
        "Repair session {}: {}",
        report.session_id,
        report.action.as_str()
    );
    out!("  source: {}", report.source.as_str());
    if let Some(base) = &report.base {
        out!("  base: {}", &base[..12.min(base.len())]);
    }
    if report.pending_commits.is_empty() {
        out!("  pending commits: none");
    } else {
        out!("  pending commits:");
        for commit in &report.pending_commits {
            out!("    - {commit}");
        }
    }
    out!(
        "  leases refreshed: {}",
        if report.leases_refreshed { "yes" } else { "no" }
    );
    if report.affected_gates.is_empty() {
        out!("  affected gates: none");
    } else {
        out!("  affected gates:");
        for gate in &report.affected_gates {
            match &gate.triggered_by {
                Some(path) => out!("    - {} (triggered by {})", gate.gate, path),
                None => out!("    - {} (always runs)", gate.gate),
            }
        }
    }
    out!("  next: {}", report.next_command);
}

pub(super) fn render_promotion_record_plan(plan: &crate::PromotionRecordPlan) {
    let recoverable = plan.recoverable().count();
    out!(
        "Promotion record plan {}: {} unrecorded commit(s), {} recoverable",
        plan.digest,
        plan.candidates.len(),
        recoverable
    );
    out!(
        "  integration: {} @ {}",
        plan.integration_ref,
        plan.integration_tip
    );
    for candidate in &plan.candidates {
        match (&candidate.entry_id, &candidate.blocker) {
            (Some(entry), None) => {
                out!(
                    "  {} -> entry {} (session {}), currently {}",
                    candidate.commit,
                    entry,
                    candidate.session_id.unwrap_or_default(),
                    candidate.current_status.as_deref().unwrap_or("unknown")
                );
                for line in &candidate.evidence {
                    out!("      evidence: {line}");
                }
            }
            _ => {
                out!("  {} -> not recoverable", candidate.commit);
                if let Some(blocker) = &candidate.blocker {
                    out!("      blocked: {blocker}");
                }
            }
        }
    }
    if recoverable == 0 {
        out!("  apply: nothing recoverable");
    } else {
        out!(
            "  apply: aethyme broker promotion-record apply --confirm {}",
            plan.digest
        );
    }
}

pub(super) fn load_main_reconcile_resolutions(
    parsed: &Parsed,
) -> Result<Option<crate::MainReconcileResolutionDocument>, UsageError> {
    let Some(path) = parsed.resolution_file.as_deref() else {
        return Ok(None);
    };
    let text = std::fs::read_to_string(path).map_err(|source| {
        UsageError::Message(format!("cannot read {}: {source}", path.display()))
    })?;
    let document: crate::MainReconcileResolutionDocument = serde_json::from_str(&text)
        .map_err(|source| UsageError::Message(format!("invalid {}: {source}", path.display())))?;
    Ok(Some(document))
}

pub(super) fn render_main_reconcile_plan(plan: &crate::MainReconcilePlan, detail: bool) {
    out!(
        "Main reconcile plan {}: {} local-only commit(s) on {}",
        plan.digest,
        plan.commits.len(),
        plan.default_branch
    );
    out!("  local:       {} @ {}", plan.local_ref, plan.local_sha);
    out!(
        "  integration: {} @ {}",
        plan.integration_ref,
        plan.integration_sha
    );
    let unrepresented = plan.unrepresented().count();
    out!(
        "  {} already represented, {} unrepresented",
        plan.commits.len() - unrepresented,
        unrepresented
    );
    if !plan.dirty_tracked_paths.is_empty() {
        out!(
            "  uncommitted tracked path(s): {}",
            plan.dirty_tracked_paths.join(", ")
        );
    }
    // Unrepresented commits are the decision; represented ones are the evidence
    // that moving the branch is safe, and are summarised unless asked for.
    for commit in plan.unrepresented() {
        out!(
            "  unrepresented {} {} — {}{}",
            &commit.commit[..12.min(commit.commit.len())],
            commit.subject,
            commit.evidence,
            match commit.resolution {
                Some(resolution) => format!(" [{}]", resolution.as_str()),
                None => " [no decision recorded]".into(),
            }
        );
    }
    if detail {
        for commit in plan
            .commits
            .iter()
            .filter(|item| item.disposition == crate::MainReconcileDisposition::AlreadyRepresented)
        {
            out!(
                "  represented   {} {} — {}",
                &commit.commit[..12.min(commit.commit.len())],
                commit.subject,
                commit.evidence
            );
        }
    }
    match &plan.refusal {
        Some(refusal) => out!("  refusal: {refusal}"),
        None => {
            out!("  preservation ref: {}", plan.preservation_ref);
            out!(
                "  apply: aethyme broker main reconcile apply --session <id> --confirm {}",
                plan.digest
            );
        }
    }
}

/// `representation scan|status|record` -- the lane for work that reached the
/// default branch through a provider-side merge instead of through submit.
/// Scanning is inspection; recording is the deliberate, digest-bound write that
/// lets such a session close (#152).
pub(super) fn run_representation(parsed: Parsed) -> Result<(), UsageError> {
    let action = parsed
        .positional
        .first()
        .map(String::as_str)
        .unwrap_or("scan");
    let session = parsed.session.ok_or(UsageError::Message(
        "representation requires --session <id>".into(),
    ))?;
    // `record` is the only writer; scanning must not take the write lock.
    let mut broker = open_broker(action != "record")?;
    match action {
        "scan" | "status" => {
            let scan = broker.scan_session_representation(session)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&scan)?);
            } else {
                render_representation_scan(&scan);
            }
        }
        "record" => {
            let confirm = parsed.confirm.as_deref().ok_or(UsageError::Message(
                "representation record requires --confirm <sha256>".into(),
            ))?;
            let scan = broker.record_session_representation(session, confirm)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&scan)?);
            } else {
                render_representation_scan(&scan);
            }
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown representation action {other:?}; expected scan, status, or record"
            )));
        }
    }
    Ok(())
}

pub(super) fn render_representation_scan(scan: &crate::RepresentationScan) {
    out!(
        "Session {} head {} ({} changed path(s) since {})",
        scan.session_id,
        short(&scan.session_head),
        scan.paths(),
        short(&scan.base)
    );
    if let Some(record) = &scan.existing {
        match record.representing_commit.as_deref() {
            Some(commit) => out!(
                "  recorded: represented by {} on {} ({})",
                short(commit),
                record.representing_ref,
                record.discovery.as_str()
            ),
            None => out!(
                "  recorded: nothing to represent on {} ({})",
                record.representing_ref,
                record.discovery.as_str()
            ),
        }
        return;
    }
    match &scan.search.outcome {
        crate::LandingOutcome::NothingToRepresent => {
            out!(
                "  nothing to represent: this head adds no net change to {}",
                scan.branch
            );
            out!(
                "  next: aethyme broker representation record --session {} --confirm {}",
                scan.session_id,
                scan.digest
            );
        }
        crate::LandingOutcome::Landed(landing) => {
            out!(
                "  landed on {} as {} ({})",
                scan.branch,
                short(&landing.commit),
                landing.subject
            );
            out!(
                "  {} of {} path(s) matched; {} commit(s) examined",
                landing.paths,
                scan.paths(),
                scan.search.examined
            );
            out!(
                "  next: aethyme broker representation record --session {} --confirm {}",
                scan.session_id,
                scan.digest
            );
        }
        crate::LandingOutcome::NotFound { closest } => {
            out!(
                "  NOT represented on {} ({} commit(s) examined{})",
                scan.branch,
                scan.search.examined,
                if scan.search.truncated {
                    ", search truncated"
                } else {
                    ""
                }
            );
            match closest {
                Some(closest) => out!(
                    "  closest was {} ({}): matched {} path(s), missing {}",
                    short(&closest.commit),
                    closest.subject,
                    closest.matched_paths,
                    closest.missing_path
                ),
                None => out!("  no commit on {} carried any of this work", scan.branch),
            }
            out!(
                "  next: aethyme broker submit --session {}",
                scan.session_id
            );
        }
    }
}

pub(super) fn render_submission_plan(plan: &crate::SubmissionPlan, checkout: &crate::GitRepo) {
    out!(
        "Submitting session {} — HEAD {} onto integration {}",
        plan.session_id,
        short_sha(&plan.session_head),
        short_sha(&plan.integration_head)
    );
    out!(
        "  recorded baseline: {}",
        plan.recorded_baseline
            .as_deref()
            .map(short_sha)
            .unwrap_or("missing")
    );

    render_submission_group(
        "session-owned commits",
        plan.commits
            .iter()
            .filter(|commit| commit.ownership == crate::SubmissionCommitOwnership::SessionOwned),
        checkout,
    );
    render_submission_group(
        "inherited baseline history (not replayed)",
        plan.commits.iter().filter(|commit| {
            commit.ownership == crate::SubmissionCommitOwnership::InheritedFromRecordedBaseline
        }),
        checkout,
    );
    render_submission_group(
        "ambiguous commits (submission refused)",
        plan.commits.iter().filter(|commit| {
            commit.ownership == crate::SubmissionCommitOwnership::Ambiguous
                || commit.integration_state == crate::SubmissionIntegrationState::Ambiguous
        }),
        checkout,
    );

    out!(
        "  merged-tree delta: {} file(s)",
        plan.merged_tree_paths.len()
    );
    for path in plan.merged_tree_paths.iter().take(10) {
        out!("    {path}");
    }
    if plan.merged_tree_paths.len() > 10 {
        out!("    ... and {} more", plan.merged_tree_paths.len() - 10);
    }
    for warning in &plan.warnings {
        out!("  warning: {warning}");
    }
}

pub(super) fn render_submission_group<'a>(
    label: &str,
    commits: impl Iterator<Item = &'a crate::SubmissionCommitProvenance>,
    checkout: &crate::GitRepo,
) {
    let commits = commits.collect::<Vec<_>>();
    out!("  {label}: {}", commits.len());
    for commit in commits.iter().take(10) {
        let subject = checkout
            .commit_message(&commit.commit)
            .ok()
            .and_then(|message| message.lines().next().map(str::to_string))
            .unwrap_or_else(|| "<subject unavailable>".into());
        let state = match commit.integration_state {
            crate::SubmissionIntegrationState::Pending => "pending replay",
            crate::SubmissionIntegrationState::AlreadyIntegratedByAncestry => {
                "already integrated by ancestry"
            }
            crate::SubmissionIntegrationState::AlreadyIntegratedByStablePatchIdentity => {
                "already integrated by patch identity"
            }
            crate::SubmissionIntegrationState::Ambiguous => "ambiguous integration identity",
        };
        out!("    {} {subject} [{state}]", short_sha(&commit.commit));
    }
    if commits.len() > 10 {
        out!("    ... and {} more", commits.len() - 10);
    }
}

/// `broker submit`.
pub(super) fn run_submit(parsed: Parsed) -> Result<(), UsageError> {
    let session = parsed
        .session
        .ok_or(UsageError::Message("submit requires --session <id>".into()))?;
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    // Preflight (dogfood feedback 2026-07-14): show exactly what
    // will be submitted before anything runs — and warn about
    // uncommitted work, which never integrates.
    if !parsed.json
        && let Ok(info) = broker.store().session(session)
        && let Ok(checkout) = crate::GitRepo::discover(std::path::Path::new(&info.worktree_path))
    {
        let plan = broker.submission_plan(session)?;
        render_submission_plan(&plan, &checkout);
        if let Ok(dirty) = checkout.dirty_paths()
            && !dirty.is_empty()
        {
            out!(
                "  ⚠ {} uncommitted change(s) NOT included \
                 (only committed work integrates), e.g. {}",
                dirty.len(),
                dirty.first().map(String::as_str).unwrap_or("")
            );
        }
    }
    let outcome = broker.submit_with_intent(
        session,
        if parsed.no_cache {
            crate::CachePolicy::Bypass
        } else {
            crate::CachePolicy::Use
        },
        if parsed.verify_only {
            crate::PromotionIntent::VerifyOnly
        } else {
            crate::PromotionIntent::Configured
        },
    )?;
    if parsed.json {
        out!("{}", serde_json::to_string_pretty(&outcome)?);
    } else if !outcome.conflicts.is_empty() {
        eprintln!("✗ conflict — rejected before any gate ran. Conflicting files:");
        for conflict in &outcome.conflict_details {
            eprintln!(
                "  - {} from session commit {} ({})",
                conflict.path,
                conflict.originating_commit,
                conflict.ownership.as_str()
            );
            if !conflict.integration_side_commits.is_empty() {
                eprintln!(
                    "    integration side: {}",
                    conflict.integration_side_commits.join(", ")
                );
            }
        }
        eprintln!(
            "Instructions written to the session worktree at {}",
            crate::ACTION_REQUIRED_RELPATH
        );
        eprintln!(
            "Quick start: git fetch . {base} && git rebase {base}   (then resubmit)",
            base = outcome.entry.base_commit
        );
        return Err(UsageError::Exit {
            message: "submission conflicted".into(),
            code: crate::exit_status::REFUSED,
        });
    } else {
        if let Some(graph) = &outcome.graph_integrity
            && graph.enforced
        {
            out!(
                "graph integrity: {:?} (tree {}, policy {}) — {}",
                graph.status,
                short_commit(&graph.tree_hash),
                short_commit(&graph.policy_digest),
                graph.reason
            );
            if !graph.changed_paths.is_empty() {
                out!("  stale graph paths: {}", graph.changed_paths.join(", "));
            }
        }
        let gate_wall_ms: i64 = outcome
            .gate_outcomes
            .iter()
            .filter(|gate| !gate.cached)
            .filter_map(|gate| gate.duration_ms)
            .sum();
        for gate in &outcome.gate_outcomes {
            if gate.cached {
                out!(
                    "gate {:<20} {} (cached, tree {}, saved {})",
                    gate.gate,
                    gate_status_label(gate.status, gate.failure_class),
                    short_commit(&gate.tree_hash),
                    duration_label(gate.duration_ms)
                );
            } else {
                out!(
                    "gate {:<20} {} in {} (tree {})",
                    gate.gate,
                    gate_status_label(gate.status, gate.failure_class),
                    duration_label(gate.duration_ms),
                    short_commit(&gate.tree_hash),
                );
            }
            render_gate_failure_tail(gate);
        }
        match outcome.gate_verification.status {
            crate::SubmissionGateVerificationStatus::NotRun => {}
            crate::SubmissionGateVerificationStatus::NoConfiguration => out!(
                "verification: conflict-only — the base has no .aethyme/gates.toml; 0 gates selected"
            ),
            crate::SubmissionGateVerificationStatus::NoGatesTriggered => out!(
                "verification: no gate matched this diff ({} configured, 0 selected); review triggers with `aethyme broker gates affected --session {}`",
                outcome.gate_verification.configured_gates,
                outcome.entry.session_id
            ),
            crate::SubmissionGateVerificationStatus::Passed => out!(
                "verification: {} selected gate(s) passed ({} executed, {} cached)",
                outcome.gate_verification.selected_gates,
                outcome.gate_verification.executed_gates,
                outcome.gate_verification.cached_gates
            ),
            crate::SubmissionGateVerificationStatus::Failed => out!(
                "verification: {} selected gate(s) did not all pass",
                outcome.gate_verification.selected_gates
            ),
        }
        if !outcome.no_changes {
            out!("gate wall time: {}ms", gate_wall_ms);
        }
        if outcome.entry.status.as_str() == "verified"
            && matches!(
                outcome.gate_verification.status,
                crate::SubmissionGateVerificationStatus::NoConfiguration
                    | crate::SubmissionGateVerificationStatus::NoGatesTriggered
            )
        {
            out!(
                "entry {} → conflict-checked (eligible for manual promotion; no gate verification)",
                outcome.entry.id
            );
        } else {
            out!(
                "entry {} → {}{}",
                outcome.entry.id,
                outcome.entry.status.as_str(),
                if outcome.promoted {
                    " (auto-promoted)"
                } else {
                    ""
                }
            );
            // A verified entry that did not move is the normal outcome
            // where promotion is off, and reads as a silent failure
            // without the reason (#290 phase 2.2).
            if let Some(reason) = &outcome.promotion_suppressed {
                out!("  {reason}");
            }
        }
        if outcome.no_changes {
            // "Nothing pending" is the right summary only when nothing was
            // set aside. Commits that predate the recorded baseline are not
            // session-owned, and saying so here is what saves the reader
            // from reading the plan JSON to find out why (issue #144).
            let inherited = outcome
                .submission_plan
                .commits
                .iter()
                .filter(|commit| {
                    commit.ownership
                        == crate::SubmissionCommitOwnership::InheritedFromRecordedBaseline
                })
                .count();
            if inherited > 0 {
                out!(
                    "What now: no pending session-owned content remains to integrate, but \
                     {inherited} commit(s) on this branch predate the recorded baseline{} \
                     and are not session-owned, so they were not replayed. To submit them, \
                     re-adopt the worktree from a base that precedes them.",
                    outcome
                        .submission_plan
                        .recorded_baseline
                        .as_deref()
                        .map(|baseline| format!(" ({})", &baseline[..12.min(baseline.len())]))
                        .unwrap_or_default(),
                );
            } else {
                out!(
                    "What now: no pending session-owned content remains to integrate; \
                     aethyme/integration was not moved and no gates ran."
                );
            }
            return Ok(());
        }
        if outcome.entry.status.as_str() == "rejected" {
            if let Ok(info) = broker.store().session(outcome.entry.session_id)
                && std::path::Path::new(&info.worktree_path) == broker.main_root()
            {
                eprintln!(
                    "note: this work is already on main (main-checkout session) — \
                     the broker cannot hold it back. Fix forward on main and resubmit."
                );
            }
            let code =
                crate::exit_status::for_submission(outcome.entry.status, &outcome.gate_outcomes);
            return Err(UsageError::Exit {
                message: if code == crate::exit_status::ENVIRONMENT {
                    "gates could not run on this host (resource contention or \
                     environment); the code was not judged. Free the resource and resubmit"
                        .into()
                } else {
                    "gates failed on the merged tree".into()
                },
                code,
            });
        }
        // "What now?" — the next expected human action was
        // implicit (dogfood feedback 2026-07-14).
        if outcome.promoted {
            let integration = broker
                .integration_head()
                .map(|(_, commit)| commit[..12.min(commit.len())].to_string())
                .unwrap_or_else(|_| "?".into());
            out!(
                "What now: aethyme/integration is at {integration} and contains this work. \
                 Your checkout and branches are untouched — keep working, or start \
                 a follow-up with `aethyme broker adopt --reuse --task \"...\"`, or \
                 finish safely with `aethyme broker finish --session {}`.",
                outcome.entry.session_id,
            );
        } else if crate::PromoteConfig::load(broker.main_root()).mode
            == crate::PromoteMode::VerifyOnly
        {
            // Telling an operator to promote in a repository that has
            // opted out contradicts the line printed directly above it,
            // and names a command whose whole point is that it is not
            // wanted here (#290 phase 2.2).
            out!(
                "What now: entry {} is verified and nothing moved, which is what this \
                 repository is configured for. Ship the work its usual way, or finish \
                 with `aethyme broker finish --session {}`.",
                outcome.entry.id,
                outcome.entry.session_id,
            );
        } else {
            out!(
                "What now: entry {} is verified but not promoted (manual mode). \
                 Promote with `aethyme broker promote --entry {}`.",
                outcome.entry.id,
                outcome.entry.id,
            );
        }
    }
    // `--json` used to exit 0 for a rejected or conflicted entry, so a
    // caller reading only the exit code saw a failed gate as success.
    let code = crate::exit_status::for_submission(outcome.entry.status, &outcome.gate_outcomes);
    if code != crate::exit_status::SUCCESS {
        return Err(UsageError::SilentExit(code));
    }
    Ok(())
}

/// `broker repair`.
pub(super) fn run_repair(parsed: Parsed) -> Result<(), UsageError> {
    let session = parsed
        .session
        .ok_or(UsageError::Message("repair requires --session <id>".into()))?;
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    let report = broker.repair(session)?;
    if parsed.json {
        out!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        render_repair_report(&report);
    }
    Ok(())
}

/// `broker main`.
pub(super) fn run_main_reconcile(parsed: Parsed) -> Result<(), UsageError> {
    let action = parsed.positional.first().map(String::as_str);
    let step = parsed.positional.get(1).map(String::as_str);
    if action != Some("reconcile") {
        return Err(UsageError::Message(
            "main requires reconcile plan or reconcile apply".into(),
        ));
    }
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    match step {
        Some("plan") => {
            if let Some(path) = parsed.write_resolution_template.as_deref() {
                let template = broker.main_reconcile_resolution_template()?;
                std::fs::write(path, serde_json::to_string_pretty(&template)?).map_err(
                    |source| {
                        UsageError::Message(format!("cannot write {}: {source}", path.display()))
                    },
                )?;
                out!(
                    "Wrote {} resolution(s) needing a decision to {}",
                    template.resolutions.len(),
                    path.display()
                );
                return Ok(());
            }
            let document = load_main_reconcile_resolutions(&parsed)?;
            let plan = broker.main_reconcile_plan_with(document.as_ref())?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&plan)?);
            } else {
                render_main_reconcile_plan(&plan, parsed.detail);
            }
        }
        Some("apply") => {
            let session = parsed.session.ok_or(UsageError::Message(
                "main reconcile apply requires --session <id>".into(),
            ))?;
            let confirm = parsed.confirm.as_deref().ok_or(UsageError::Message(
                "main reconcile apply requires --confirm <sha256>".into(),
            ))?;
            let document = load_main_reconcile_resolutions(&parsed)?;
            let report = broker.main_reconcile_apply_with(session, confirm, document.as_ref())?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                out!(
                    "Main reconciled: {} moved {} -> {}",
                    report.default_branch,
                    &report.moved_from[..12.min(report.moved_from.len())],
                    &report.moved_to[..12.min(report.moved_to.len())],
                );
                out!("  preserved pre-move tip: {}", report.preservation_ref);
                out!(
                    "  {} represented commit(s) left behind, recoverable from that ref",
                    report.represented_commits
                );
            }
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown main reconcile step {other:?}; expected plan or apply"
            )));
        }
    }
    Ok(())
}

/// `broker promotion-record`.
pub(super) fn run_promotion_record(parsed: Parsed) -> Result<(), UsageError> {
    let action = parsed
        .positional
        .first()
        .map(String::as_str)
        .ok_or(UsageError::Message(
            "promotion-record requires plan or apply".into(),
        ))?;
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    match action {
        "plan" => {
            let plan = broker.promotion_record_plan()?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&plan)?);
            } else {
                render_promotion_record_plan(&plan);
            }
        }
        "apply" => {
            let confirm = parsed.confirm.as_deref().ok_or(UsageError::Message(
                "promotion-record apply requires --confirm <sha256>".into(),
            ))?;
            let report = broker.promotion_record_apply(confirm)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                out!(
                    "Promotion record recovery: {} restored",
                    report.restored.len()
                );
                for id in &report.restored {
                    out!("  entry {id} recorded as promoted");
                }
                for skip in &report.skipped {
                    out!("  skipped: {skip}");
                }
            }
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown promotion-record action {other:?}; expected plan or apply"
            )));
        }
    }
    Ok(())
}

/// `broker checkpoint`.
pub(super) fn run_checkpoint(parsed: Parsed) -> Result<(), UsageError> {
    let action = parsed
        .positional
        .first()
        .map(String::as_str)
        .ok_or(UsageError::Message(
            "checkpoint requires plan or apply".into(),
        ))?;
    let session = parsed.session.ok_or(UsageError::Message(
        "checkpoint plan/apply requires --session <id>".into(),
    ))?;
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    match action {
        "plan" => {
            let report = broker.plan_session_checkpoint_recovery(session)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                out!(
                    "Checkpoint recovery for session {}: {}",
                    session,
                    if report.safe { "safe" } else { "refused" }
                );
                out!(
                    "  old: {}",
                    report.old_checkpoint.as_deref().unwrap_or("missing")
                );
                out!(
                    "  proposed: {}",
                    report.proposed_checkpoint.as_deref().unwrap_or("missing")
                );
                out!(
                    "  session HEAD: {} ({}; {} ahead, {} behind)",
                    report.session_head,
                    report
                        .integration_relation
                        .map(|relation| relation.as_str())
                        .unwrap_or("unknown"),
                    report.ahead_commits,
                    report.behind_commits
                );
                out!("  pending commits: {}", report.pending_commits.len());
                out!("  preservation branch: {}", report.preservation_branch);
                for refusal in &report.refusals {
                    out!("  refusal: {refusal}");
                }
                if !report.next_actions.is_empty() {
                    out!("  recovery actions:");
                    for action in &report.next_actions {
                        out!("    {}: {}", action.kind, action.command);
                        out!("      {}", action.description);
                    }
                }
                out!("Plan digest: {}", report.digest);
                if report.safe {
                    out!(
                        "Apply with: aethyme broker checkpoint apply --session {} --confirm {}",
                        session,
                        report.digest
                    );
                }
            }
        }
        "apply" => {
            let confirm = parsed.confirm.as_deref().ok_or(UsageError::Message(
                "checkpoint apply requires --confirm <sha256>".into(),
            ))?;
            let report = broker.apply_session_checkpoint_recovery(session, confirm)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                out!(
                    "Re-anchored session {} at {} after preserving {}.",
                    session,
                    report.accepted_session_head,
                    report.preservation_ref
                );
                out!("Next: aethyme broker submit --session {session}");
            }
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown checkpoint action {other:?} — expected plan or apply"
            )));
        }
    }
    Ok(())
}

/// `broker queue`.
pub(super) fn run_queue(parsed: Parsed) -> Result<(), UsageError> {
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    if parsed.positional.first().map(String::as_str) == Some("history") {
        if parsed.positional.len() != 1 {
            return Err(UsageError::Message(
                "queue history accepts no positional arguments".into(),
            ));
        }
        let page = broker
            .store()
            .merge_queue_history_page(parsed.limit.unwrap_or(50), parsed.before)?;
        if parsed.json {
            out!("{}", serde_json::to_string_pretty(&page)?);
        } else {
            render_queue_history(&page);
        }
        return Ok(());
    }
    if !parsed.positional.is_empty() || parsed.limit.is_some() || parsed.before.is_some() {
        return Err(UsageError::Message(
            "queue accepts no selectors; use `queue history [--limit <n>] [--before <id>]`".into(),
        ));
    }
    let mut entries = broker.store().merge_queue()?;
    // Watching a submit in flight is the one polling loop agents run,
    // and the bare inventory grows without bound — it reached 13 KB
    // here, paid on every poll. `--active` answers "is it done yet" in
    // the few entries that can still change. The bare command keeps its
    // documented compatibility-inventory shape.
    if parsed.active {
        entries.retain(|entry| {
            matches!(
                entry.status,
                crate::MergeStatus::Submitted
                    | crate::MergeStatus::Simulating
                    | crate::MergeStatus::Verified
                    | crate::MergeStatus::Conflict
            )
        });
    }
    if parsed.json {
        out!("{}", serde_json::to_string_pretty(&entries)?);
    } else if entries.is_empty() {
        out!(
            "{}",
            if parsed.active {
                "No queue entry is in flight."
            } else {
                "Merge queue is empty."
            }
        );
    } else {
        out!("{:<4} {:<4} {:<11} HEAD", "ID", "SID", "STATUS");
        for entry in entries {
            out!(
                "{:<4} {:<4} {:<11} {}",
                entry.id,
                entry.session_id,
                entry.status.as_str(),
                &entry.head_commit[..12.min(entry.head_commit.len())]
            );
        }
    }
    Ok(())
}

/// `broker promote`.
pub(super) fn run_promote(parsed: Parsed) -> Result<(), UsageError> {
    let entry = parsed
        .entry
        .ok_or(UsageError::Message("promote requires --entry <id>".into()))?;
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    broker.promote(entry)?;
    if parsed.json {
        out!("{{\"promoted\":{entry}}}");
    } else {
        out!("Promoted entry {entry} to the local integration branch.");
        out!("Next: aethyme broker ship plan --entry {entry}");
    }
    Ok(())
}
