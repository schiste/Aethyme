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

/// The exit code for a submission, text or `--json`.
///
/// A deferred entry is `Submitted`, so `for_submission` alone would call it a
/// success. It is not: the change was never judged, and exit 0 would tell an
/// agent its work was verified. ENVIRONMENT already means "free the resource
/// and retry without changing code".
fn submission_exit_code(outcome: &crate::SubmitOutcome) -> u8 {
    if outcome.gate_verification.status == crate::SubmissionGateVerificationStatus::Deferred {
        crate::exit_status::ENVIRONMENT
    } else {
        crate::exit_status::for_submission(outcome.entry.status, &outcome.gate_outcomes)
    }
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
        out!("Next: aethyme broker advanced queue history --before {before}");
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
            "  apply: aethyme broker submit promotion-record apply --confirm {}",
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
            if plan.strategy == crate::MainReconcileStrategy::FastForward {
                out!(
                    "  strategy: fast-forward {} to {} (merge --ff-only; nothing is left behind, so no preservation ref)",
                    plan.default_branch,
                    &plan.integration_sha[..12.min(plan.integration_sha.len())]
                );
            } else {
                out!("  strategy: reset onto integration");
                out!("  preservation ref: {}", plan.preservation_ref);
            }
            out!(
                "  apply: aethyme broker advanced main reconcile apply --session <id> --confirm {}",
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
                "  next: aethyme broker advanced representation record --session {} --confirm {}",
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
                "  next: aethyme broker advanced representation record --session {} --confirm {}",
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
        "Planning session {} — HEAD {} against base {}",
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
/// Refuse a submission whose tracked cross-process removals carry no contract
/// decision, before any gate starts building (#418).
///
/// Only a refusal is fatal. When the session cannot be inspected, the
/// preflight steps aside with a warning: submit reports that failure in its
/// own terms, and the contract gate on the merged tree stays authoritative.
fn contract_preflight(broker: &mut crate::Broker, session: i64) -> Result<(), UsageError> {
    let inspected = (|| -> Result<Result<(), String>, String> {
        let info = broker
            .store()
            .session(session)
            .map_err(|error| format!("cannot load session {session}: {error}"))?;
        let repo_root = std::path::PathBuf::from(&info.worktree_path);
        let checkout = crate::GitRepo::discover(&repo_root)
            .map_err(|error| format!("cannot inspect the session worktree: {error}"))?;
        let plan = broker
            .submission_plan(session)
            .map_err(|error| format!("cannot plan the submission: {error}"))?;
        let base = checkout
            .merge_base("HEAD", &plan.integration_head)
            .map_err(|error| format!("cannot find the submission base: {error}"))?;
        let messages = plan
            .pending_owned_commit_ids()
            .iter()
            .map(|commit| checkout.commit_message(commit))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("cannot read the pending commit messages: {error}"))?
            .join("\n");
        crate::contract_check::preflight_submit_decision(&repo_root, &base, &messages)
    })();
    match inspected {
        Ok(verdict) => verdict.map_err(UsageError::Message),
        Err(reason) => {
            eprintln!(
                "warning: skipped the contract-decision preflight ({reason}); \
                 the contract gate still checks the merged tree."
            );
            Ok(())
        }
    }
}

pub(super) fn run_submit(parsed: Parsed) -> Result<(), UsageError> {
    let session = parsed
        .session
        .ok_or(UsageError::Message("submit requires --session <id>".into()))?;
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    contract_preflight(&mut broker, session)?;
    // Preflight (dogfood feedback 2026-07-14): show exactly what
    // will be submitted before anything runs — and warn about
    // uncommitted work, which never integrates.
    if !parsed.json
        && let Ok(info) = broker.store().session(session)
        && let Ok(checkout) = crate::GitRepo::discover(std::path::Path::new(&info.worktree_path))
    {
        let plan = broker.submission_plan(session)?;
        render_submission_plan(&plan, &checkout);
        if let Ok(uncommitted) = checkout.uncommitted_summary()
            && !uncommitted.is_empty()
        {
            out!(
                "  ⚠ uncommitted changes NOT included (only committed work integrates): {}",
                uncommitted.describe(5)
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
            if let Some(advice) = graph.advice() {
                out!("  advice: {advice}");
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
                "verification: no gate matched this diff ({} configured, 0 selected); review triggers with `aethyme broker advanced gates affected --session {}`",
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
            // Deliberately not phrased as a failure. The gate never judged the
            // change, so the next action is to free the host resource and
            // resubmit -- not to edit code that has not been shown to be wrong.
            crate::SubmissionGateVerificationStatus::Deferred => out!(
                "verification: deferred — a selected gate could not run on this host \
                 (resources, disk, or its first timeout); the change was not judged. Free \
                 the resource, then `aethyme broker submit --session {}` again without \
                 changing code",
                outcome.entry.session_id
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
        if let Some(base) = &outcome.verified_against {
            if base.source == crate::VERIFIED_AGAINST_UPSTREAM {
                out!(
                    "  verified against {} at {} (verify-only: integration is not used)",
                    base.reference,
                    short_commit(&base.commit)
                );
            }
            if let Some(reason) = &base.fallback_reason {
                eprintln!("⚠ {reason}");
            }
        }
        for warning in &outcome.lease_warnings {
            eprintln!(
                "⚠ lease overlap ({}): session {} holds {}; {}",
                warning.severity.as_deref().unwrap_or("low"),
                warning.session_id,
                warning.path,
                warning.reason.as_deref().unwrap_or("not blocking")
            );
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
        if outcome.entry.status.as_str() == "rejected"
            || outcome.gate_verification.status == crate::SubmissionGateVerificationStatus::Deferred
        {
            // "Fix forward" only follows a verdict. A deferred change was never
            // judged, so there is nothing yet to fix.
            if outcome.entry.status.as_str() == "rejected"
                && let Ok(info) = broker.store().session(outcome.entry.session_id)
                && std::path::Path::new(&info.worktree_path) == broker.main_root()
            {
                eprintln!(
                    "note: this work is already on main (main-checkout session) — \
                     the broker cannot hold it back. Fix forward on main and resubmit."
                );
            }
            let code = submission_exit_code(&outcome);
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
                 a follow-up with `aethyme broker start --reuse --task \"...\" --short-name \"<short name>\"`, or \
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
                 Promote with `aethyme broker submit promote --entry {}`.",
                outcome.entry.id,
                outcome.entry.id,
            );
        }
    }
    // `--json` used to exit 0 for a rejected or conflicted entry, so a
    // caller reading only the exit code saw a failed gate as success.
    let code = submission_exit_code(&outcome);
    if code != crate::exit_status::SUCCESS {
        return Err(UsageError::SilentExit(code));
    }
    Ok(())
}

/// `broker repair`.
/// `broker push`: publish the session's own branch, optionally with a draft PR.
/// `broker sync`.
pub(super) fn run_sync(parsed: Parsed) -> Result<(), UsageError> {
    let session = parsed
        .session
        .ok_or(UsageError::Message("sync requires --session <id>".into()))?;
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    let report = broker.sync_session(session)?;
    let conflict = report.outcome == crate::SyncOutcome::Conflict;
    if parsed.json {
        out!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        let short = |oid: &str| oid[..oid.len().min(12)].to_string();
        match report.outcome {
            crate::SyncOutcome::AlreadyCurrent => out!(
                "Session {} is current: {} contains {} at {}",
                report.session_id,
                report.branch,
                report.default_ref,
                short(&report.default_commit)
            ),
            crate::SyncOutcome::Synced => {
                // Name the direction and say nothing left this machine: "merged
                // into published branch" read as "my branch is published"
                // (2026-10-03), when only the local session branch moved.
                let (action, remote_state) = match report.strategy {
                    crate::SyncStrategy::Rebase => (
                        format!("rebased {} onto {}", report.branch, report.default_ref),
                        "the branch is not on the remote yet",
                    ),
                    // `None` never accompanies `Synced`.
                    crate::SyncStrategy::Merge | crate::SyncStrategy::None => (
                        format!("merged {} into {}", report.default_ref, report.branch),
                        "the remote branch is unchanged",
                    ),
                };
                out!(
                    "Synced session {} locally: {action}, bringing in {} commit(s): {} -> {}",
                    report.session_id,
                    report.behind_before,
                    short(&report.before),
                    short(&report.after)
                );
                out!("  Nothing was pushed: {remote_state}.");
                if let Some(next) = &report.next_action {
                    out!("  Next: `{next}` to publish it.");
                }
            }
            crate::SyncOutcome::Conflict => {
                eprintln!(
                    "✗ not synced: catching up with {} ({} commit(s) behind) would conflict; \
                     nothing was changed",
                    report.default_ref, report.behind_before
                );
                for path in &report.conflicts {
                    eprintln!("  conflict: {path}");
                }
                eprintln!("  resolve by hand:");
                for command in &report.manual_commands {
                    eprintln!("    {command}");
                }
            }
        }
        if let Some(note) = &report.fetch_note {
            out!("  note: {note}");
        }
    }
    if conflict {
        return Err(UsageError::SilentExit(crate::exit_status::REFUSED));
    }
    Ok(())
}

pub(super) fn run_push(parsed: Parsed) -> Result<(), UsageError> {
    let session = parsed
        .session
        .ok_or(UsageError::Message("push requires --session <id>".into()))?;
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    let mut report = match broker.push_session(session, parsed.open_pr) {
        Ok(report) => report,
        Err(crate::BrokerOpError::CoordinatedOperationBlocked { recovery, .. }) => {
            return Err(UsageError::Exit {
                message: recovery.to_string(),
                code: crate::exit_status::OUTCOME_UNKNOWN,
            });
        }
        Err(crate::BrokerOpError::InvalidCoordinatedOperation { reason }) => {
            return Err(crate::BrokerOpError::InvalidCoordinatedOperation {
                reason: crate::session_push::with_pre_push_path_hint(&reason),
            }
            .into());
        }
        Err(error) => return Err(error.into()),
    };
    report.review_run = review_on_push(broker.main_root(), &report);
    if parsed.json {
        out!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    let previous = report
        .previous_remote_oid
        .as_deref()
        .map_or("new branch".to_string(), |oid| {
            format!("was {}", &oid[..oid.len().min(12)])
        });
    out!(
        "Pushed {} to {}/{} at {} ({previous}; {} commit(s) not on the remote before)",
        report.branch,
        report.remote,
        report.branch,
        &report.pushed_oid[..report.pushed_oid.len().min(12)],
        report.commits_pushed
    );
    if report.uncommitted_files > 0 {
        let counts = &report.uncommitted;
        let mut parts = Vec::new();
        if counts.modified > 0 {
            parts.push(format!("{} modified", counts.modified));
        }
        if counts.untracked_entries > 0 {
            parts.push(format!("{} untracked", counts.untracked_entries));
        }
        let more = report
            .uncommitted_files
            .saturating_sub(u32::try_from(counts.sample.len()).unwrap_or(u32::MAX));
        let more = if more > 0 {
            format!(", +{more} more")
        } else {
            String::new()
        };
        out!(
            "  Not pushed: {} ({}{more}); commit them and push again",
            parts.join(", "),
            counts.sample.join(", ")
        );
    }
    if let Some(drift) = report
        .default_branch
        .as_ref()
        .filter(|drift| drift.behind > 0)
    {
        if drift.would_conflict {
            out!(
                "  Warning: {} moved {} commit(s) past this branch, and merging it would \
                 conflict in: {}",
                drift.reference,
                drift.behind,
                drift.conflicting_paths.join(", ")
            );
        } else {
            out!(
                "  {} moved {} commit(s) past this branch; it still merges cleanly",
                drift.reference,
                drift.behind
            );
        }
        if let Some(command) = &drift.suggested_command {
            out!("  Catch up with: {command}");
        }
    }
    if let Some(note) = &report.default_branch_note {
        out!("  Note: {note}");
    }
    for overlap in &report.pr_overlaps {
        out!(
            "  {} open PR #{} {}: {}",
            if overlap.conflicting_hunks {
                "Warning: changes the same lines as"
            } else {
                "Touches the same files as"
            },
            overlap.pr,
            overlap.url,
            overlap.files.join(", ")
        );
    }
    for duplicate in &report.duplicate_work {
        out!(
            "  Warning: session {} ({}) also works on {}: {}",
            duplicate.session_id,
            duplicate.status.as_str(),
            match (duplicate.reason, duplicate.pull_request) {
                (crate::DuplicateWorkReason::SameBranch, _) => "the same branch".to_string(),
                (_, Some(pr)) => format!("PR #{pr}"),
                (_, None) => "the same pull request".to_string(),
            },
            duplicate.task.as_deref().unwrap_or("(no task)")
        );
    }
    if !report.pr_overlaps_unknown.is_empty() {
        out!(
            "  Overlap unknown for {} open PR(s) whose change could not be read",
            report.pr_overlaps_unknown.len()
        );
    }
    match &report.pr {
        Some(pr) if pr.created => {
            out!("  Opened draft pull request #{} {}", pr.number, pr.url);
            if pr.ci_skips_drafts {
                out!(
                    "  Note: this repository's CI skips draft pull requests; checks run once \
                     it is marked ready (`gh pr ready {}`, through `aethyme broker advanced gh`)",
                    pr.number
                );
            }
        }
        Some(pr) => out!("  Pull request #{} {} ({})", pr.number, pr.url, pr.state),
        None => out!(
            "  Open a draft pull request with: aethyme broker push --session {} --pr",
            report.session_id
        ),
    }
    if let Some(size) = &report.pr_size
        && size.over_threshold
    {
        out!(
            "  Warning: this change is large ({} files, {} changed lines; [review] pr_size is {} \
             files, {} lines). Consider splitting it; the push was not refused.",
            size.files,
            size.changed_lines,
            size.max_files,
            size.max_changed_lines
        );
    }
    if let Some(run) = &report.review_run
        && run["performed"].as_bool() == Some(true)
    {
        let actions = run["actions"].as_array().map_or(0, Vec::len);
        out!(
            "  Review run on push: {actions} GitHub action(s) for pull request #{}",
            run["pull_request"]
        );
    }
    Ok(())
}

/// `[review] run_on_push`: run one review for the pushed pull request.
///
/// The review is the same `broker advanced review run --from-provider` a tick
/// performs, started as its own process so a slow provider cannot hold the
/// push: past `run_on_push_budget_secs` the push returns and the run carries
/// on in the background, logging to `.aethyme/logs/`. It is never killed,
/// because a coordinated write cut in half would leave an outcome nobody can
/// vouch for. Failure is reported, never fatal: the push already happened.
fn review_on_push(
    main_root: &std::path::Path,
    report: &crate::SessionPushReport,
) -> Option<serde_json::Value> {
    let program = std::env::var_os("AETHYME_BIN")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_exe().ok());
    review_on_push_with(program, main_root, report)
}

fn review_on_push_with(
    program: Option<std::path::PathBuf>,
    main_root: &std::path::Path,
    report: &crate::SessionPushReport,
) -> Option<serde_json::Value> {
    let policy = match crate::ReviewPolicy::load(main_root) {
        Ok(policy) if policy.run_on_push => policy,
        Ok(_) => return None,
        Err(error) => {
            return Some(review_on_push_failure(
                report,
                None,
                &format!("cannot read [review]: {error}"),
            ));
        }
    };
    let Some(pr) = &report.pr else {
        return Some(serde_json::json!({
            "performed": false,
            "skipped": "no open pull request for this branch",
        }));
    };
    if pr.draft {
        return Some(serde_json::json!({
            "performed": false,
            "pull_request": pr.number,
            "skipped": "the pull request is a draft",
        }));
    }
    let Some(program) = program else {
        return Some(review_on_push_failure(
            report,
            Some(pr.number),
            "cannot locate the aethyme executable",
        ));
    };
    let logs = main_root.join(".aethyme/logs");
    let log_path = logs.join(format!("review-on-push-pr{}.log", pr.number));
    let log = std::fs::create_dir_all(&logs)
        .and_then(|()| std::fs::File::create(&log_path))
        .and_then(|file| Ok((file.try_clone()?, file)));
    let Ok((stdout, stderr)) = log else {
        return Some(review_on_push_failure(
            report,
            Some(pr.number),
            &format!("cannot write {}", log_path.display()),
        ));
    };
    let child = std::process::Command::new(&program)
        .args(review_run_args(report, pr.number))
        .current_dir(main_root)
        .stdin(std::process::Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(error) => {
            return Some(review_on_push_failure(
                report,
                Some(pr.number),
                &format!("cannot start the review run: {error}"),
            ));
        }
    };
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(policy.run_on_push_budget_secs);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Ok(None) => break None,
            Err(error) => {
                return Some(review_on_push_failure(
                    report,
                    Some(pr.number),
                    &format!("cannot wait for the review run: {error}"),
                ));
            }
        }
    };
    let output = std::fs::read_to_string(&log_path).unwrap_or_default();
    match status {
        None => Some(review_on_push_failure(
            report,
            Some(pr.number),
            &format!(
                "the review run is still going after {}s; it continues in the background, \
                 logging to {}",
                policy.run_on_push_budget_secs,
                log_path.display()
            ),
        )),
        Some(status) if status.success() => {
            let run = serde_json::from_str::<serde_json::Value>(&output).unwrap_or_default();
            Some(serde_json::json!({
                "performed": true,
                "pull_request": pr.number,
                "actions": run["performed"].clone(),
                "rule_comments": run["rule_comments"].clone(),
                "decisions": run["decisions"].clone(),
            }))
        }
        Some(status) => {
            let tail: Vec<&str> = output.lines().rev().take(5).collect();
            let tail: Vec<&str> = tail.into_iter().rev().collect();
            Some(review_on_push_failure(
                report,
                Some(pr.number),
                &format!("the review run exited with {status}: {}", tail.join(" | ")),
            ))
        }
    }
}

fn review_run_args(report: &crate::SessionPushReport, pull_request: i64) -> Vec<String> {
    vec![
        "broker".into(),
        "advanced".into(),
        "review".into(),
        "run".into(),
        "--session".into(),
        report.session_id.to_string(),
        "--repo".into(),
        report.repository.clone(),
        "--pr".into(),
        pull_request.to_string(),
        "--from-provider".into(),
        "--json".into(),
    ]
}

/// A failed or unfinished review run on push: printed as a warning with the
/// exact command to rerun it, and returned for `push --json`.
fn review_on_push_failure(
    report: &crate::SessionPushReport,
    pull_request: Option<i64>,
    error: &str,
) -> serde_json::Value {
    let rerun = pull_request.map(|pr| {
        let args = review_run_args(report, pr)
            .iter()
            .filter(|arg| *arg != "--json")
            .map(|arg| crate::broker::shell_quote(arg))
            .collect::<Vec<_>>()
            .join(" ");
        format!("aethyme {args}")
    });
    eprintln!(
        "warning: review run on push did not complete: {error}{}",
        rerun
            .as_deref()
            .map(|command| format!(". Rerun: {command}"))
            .unwrap_or_default()
    );
    serde_json::json!({
        "performed": false,
        "pull_request": pull_request,
        "error": error,
        "rerun": rerun,
    })
}

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
                match &report.preservation_ref {
                    Some(preservation_ref) => {
                        out!("  preserved pre-move tip: {preservation_ref}");
                        out!(
                            "  {} represented commit(s) left behind, recoverable from that ref",
                            report.represented_commits
                        );
                    }
                    None => out!("  fast-forward: no commits left behind"),
                }
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
                        "Apply with: aethyme broker advanced checkpoint apply --session {} --confirm {}",
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
        out!("Next: aethyme broker advanced ship plan --entry {entry}");
    }
    Ok(())
}

#[cfg(test)]
mod review_on_push_tests {
    use super::{review_on_push_with, review_run_args};
    use std::os::unix::fs::PermissionsExt;

    fn report(draft: bool) -> crate::SessionPushReport {
        crate::SessionPushReport {
            session_id: 12,
            branch: "agent/x".into(),
            remote: "origin".into(),
            pushed_oid: "a".repeat(40),
            previous_remote_oid: None,
            commits_pushed: 1,
            uncommitted_files: 0,
            uncommitted: crate::UncommittedCounts::default(),
            pr: Some(crate::SessionPullRequest {
                url: "https://github.com/acme/product/pull/7".into(),
                number: 7,
                state: "OPEN".into(),
                created: false,
                ci_skips_drafts: false,
                draft,
            }),
            pr_overlaps: Vec::new(),
            pr_overlaps_unknown: Vec::new(),
            duplicate_work: Vec::new(),
            default_branch: None,
            default_branch_note: None,
            pr_size: None,
            review_run: None,
            repository: "acme/product".into(),
        }
    }

    /// A repository with `[review]` set as given, and a stand-in for
    /// `aethyme` that records its arguments and plays `script`.
    fn fixture(review: &str, script: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join(".aethyme")).unwrap();
        std::fs::write(
            root.path().join(".aethyme/config.toml"),
            format!("[review]\n{review}\n"),
        )
        .unwrap();
        let program = root.path().join("aethyme-stub");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"{}\"\n{script}\n",
                root.path().join("args").display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        (root, program)
    }

    #[test]
    fn nothing_runs_unless_run_on_push_is_set() {
        let (root, program) = fixture("schema_version = 1", "exit 0");
        assert_eq!(
            review_on_push_with(Some(program), root.path(), &report(false)),
            None
        );
        assert!(!root.path().join("args").exists());
    }

    #[test]
    fn an_open_pull_request_gets_one_review_run_from_the_provider() {
        let (root, program) = fixture(
            "run_on_push = true",
            r#"printf '{"performed":[{"purpose":"post rule comment"}],"rule_comments":[{"decision":"create"}]}'"#,
        );
        let run = review_on_push_with(Some(program), root.path(), &report(false)).unwrap();
        assert_eq!(run["performed"], true, "{run:#}");
        assert_eq!(run["pull_request"], 7);
        assert_eq!(run["rule_comments"][0]["decision"], "create");
        let args = std::fs::read_to_string(root.path().join("args")).unwrap();
        assert_eq!(
            args.lines().collect::<Vec<_>>(),
            review_run_args(&report(false), 7),
            "the idempotent `review run --from-provider` path, for exactly this PR"
        );
    }

    #[test]
    fn a_draft_or_a_missing_pull_request_is_skipped() {
        let (root, program) = fixture("run_on_push = true", "exit 0");
        let run = review_on_push_with(Some(program.clone()), root.path(), &report(true)).unwrap();
        assert_eq!(run["skipped"], "the pull request is a draft");
        let mut none = report(false);
        none.pr = None;
        let run = review_on_push_with(Some(program), root.path(), &none).unwrap();
        assert_eq!(run["performed"], false);
        assert!(!root.path().join("args").exists());
    }

    #[test]
    fn a_failed_review_run_is_reported_with_the_command_to_rerun_it() {
        let (root, program) = fixture("run_on_push = true", "echo 'provider down' >&2\nexit 3");
        let run = review_on_push_with(Some(program), root.path(), &report(false)).unwrap();
        assert_eq!(run["performed"], false, "{run:#}");
        assert!(
            run["error"].as_str().unwrap().contains("provider down"),
            "{run:#}"
        );
        assert_eq!(
            run["rerun"],
            "aethyme broker advanced review run --session 12 --repo acme/product --pr 7 --from-provider"
        );
    }

    #[test]
    fn a_slow_review_run_returns_at_the_budget_and_keeps_running() {
        let (root, program) = fixture(
            "run_on_push = true\nrun_on_push_budget_secs = 1",
            "sleep 3\nprintf '{}'",
        );
        let started = std::time::Instant::now();
        let run = review_on_push_with(Some(program), root.path(), &report(false)).unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(3));
        assert!(
            run["error"]
                .as_str()
                .unwrap()
                .contains("continues in the background"),
            "{run:#}"
        );
    }

    #[test]
    fn an_out_of_range_budget_is_rejected() {
        let (root, program) = fixture("run_on_push = true\nrun_on_push_budget_secs = 0", "exit 0");
        let run = review_on_push_with(Some(program), root.path(), &report(false)).unwrap();
        assert!(
            run["error"]
                .as_str()
                .unwrap()
                .contains("run_on_push_budget_secs"),
            "{run:#}"
        );
    }

    #[test]
    fn an_unknown_review_key_is_rejected() {
        let (root, program) = fixture("run_on_pus = true", "exit 0");
        let run = review_on_push_with(Some(program), root.path(), &report(false)).unwrap();
        assert!(
            run["error"].as_str().unwrap().contains("run_on_pus"),
            "{run:#}"
        );
    }
}
