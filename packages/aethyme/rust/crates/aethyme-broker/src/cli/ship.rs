//! `broker ship` and `integration`: publishing the integration branch.

use super::*;

/// Ship plans list every promoted entry in the prefix; show the boundary only.
pub(super) const SHIP_ENTRY_CAP: usize = 6;

/// Print at most `cap` items, then say how many were withheld.
///
/// Agent-facing output is charged per token. A plan that enumerates every
/// finding costs the reader far more than the decision it supports: this
/// repository's `gc plan` reached ~319 KB, of which 93% was one list. The
/// counts and the digest are what a reader acts on; the enumeration is what
/// they page past. `--detail` restores it when someone genuinely wants to audit.
/// "Ready" means this exact prefix is safe to push. It does not mean the prefix
/// represents every piece of work in the repository, and an operator asking to
/// "publish everything" reasonably reads it that way (issue #141).
pub(super) fn repository_wide_publication_lines(
    assessment: &crate::ship::ShipLocalMainSyncAssessment,
    local_default_branch_ref: &str,
    publication_sha: &str,
    local_default_branch_sha: &str,
) -> Vec<String> {
    if !assessment.current_branch_matches {
        // The primary checkout is on another branch, so local main says nothing
        // about completeness here.
        return Vec::new();
    }
    let excluded_commits = !assessment.fast_forward;
    let dirty = assessment.tracked_dirty_paths.len();
    if !excluded_commits && dirty == 0 {
        return vec![
            "Repository-wide publication: complete (this prefix represents local main)".into(),
        ];
    }
    let mut lines = vec!["Repository-wide publication: INCOMPLETE".into()];
    if excluded_commits {
        lines.push(format!(
            "  excluded: local {} carries commits this prefix does not contain; list them with `git log --oneline {}..{}`",
            local_default_branch_ref, publication_sha, local_default_branch_sha,
        ));
    }
    if dirty > 0 {
        lines.push(format!(
            "  excluded: {dirty} uncommitted tracked path(s): {}",
            assessment.tracked_dirty_paths.join(", ")
        ));
    }
    lines.push("  publishing now is safe, but omits the work listed above".into());
    lines
}

pub(super) fn upstream_relation(local_only: u64, upstream_only: u64) -> String {
    match (local_only, upstream_only) {
        (0, 0) => "fetched upstream matches local main".into(),
        (local, 0) => format!(
            "local main ahead by {local} {}",
            plural(local as usize, "commit", "commits")
        ),
        (0, upstream) => format!(
            "local main behind by {upstream} {}",
            plural(upstream as usize, "commit", "commits")
        ),
        (local, upstream) => format!(
            "diverged: {local} local-only {}, {upstream} upstream-only {}",
            plural(local as usize, "commit", "commits"),
            plural(upstream as usize, "commit", "commits")
        ),
    }
}

pub(super) fn render_integration_status(
    report: &crate::IntegrationStatusView,
    json: bool,
) -> Result<(), UsageError> {
    if json {
        out!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }

    out!(
        "Integration: {} @ {}",
        report.branch,
        short_commit(&report.head)
    );
    let main_relation = if report.head == report.main_head {
        "current with integration".to_string()
    } else if report.main_is_ancestor {
        format!(
            "{} {} behind integration",
            report.commits_ahead_main,
            plural(report.commits_ahead_main as usize, "commit", "commits")
        )
    } else {
        "diverged from integration".into()
    };
    out!(
        "Main:        {} ({main_relation})",
        short_commit(&report.main_head)
    );
    if let (Some(upstream_ref), Some(upstream_head)) = (&report.upstream_ref, &report.upstream_head)
    {
        let relation = upstream_relation(
            report.main_ahead_upstream_commits,
            report.main_behind_upstream_commits,
        );
        out!(
            "Upstream:    {} @ {} ({relation})",
            upstream_ref,
            short_commit(upstream_head)
        );
    }
    out!();

    if report.promoted_entries.is_empty() && report.changed_files.is_empty() {
        out!("Pending layer: none");
    } else {
        out!(
            "Pending layer: {} promoted {}, {} {} changed, {} {} ahead of main",
            report.promoted_entries.len(),
            plural(report.promoted_entries.len(), "entry", "entries"),
            report.changed_files.len(),
            plural(report.changed_files.len(), "file", "files"),
            report.commits_ahead_main,
            plural(report.commits_ahead_main as usize, "commit", "commits"),
        );
    }

    if report.promoted_entries.is_empty() {
        out!("Promoted entries: none");
    } else {
        out!("Promoted entries:");
        for entry in report.promoted_entries.iter().take(10) {
            let label = entry
                .task
                .as_deref()
                .or(entry.branch.as_deref())
                .unwrap_or("-");
            out!(
                "  q{} session {} {} -> {}  {}",
                entry.queue_entry_id,
                entry.session_id,
                short_commit(&entry.head_commit),
                short_commit(&entry.merge_commit),
                label
            );
            if !entry.files.is_empty() {
                out!("    files: {}", capped_join(&entry.files, 5));
            }
        }
        if report.promoted_entries.len() > 10 {
            out!(
                "  and {} more promoted {}",
                report.promoted_entries.len() - 10,
                plural(report.promoted_entries.len() - 10, "entry", "entries")
            );
        }
    }

    if report.changed_files.is_empty() {
        out!("Changed files: none");
    } else {
        out!("Changed files:");
        for path in report.changed_files.iter().take(12) {
            out!("  - {path}");
        }
        if report.changed_files.len() > 12 {
            out!(
                "  and {} more {}",
                report.changed_files.len() - 12,
                plural(report.changed_files.len() - 12, "file", "files")
            );
        }
    }

    if report.conflicts.is_empty() {
        out!("Conflicts with pending layer: none");
    } else {
        out!("Conflicts with pending layer:");
        for conflict in report.conflicts.iter().take(12) {
            out!(
                "  session {}: {} (session {}, integration {})",
                conflict.session_id,
                conflict.path,
                conflict.session_path,
                conflict.promoted_path
            );
        }
        if report.conflicts.len() > 12 {
            out!(
                "  and {} more {}",
                report.conflicts.len() - 12,
                plural(report.conflicts.len() - 12, "conflict", "conflicts")
            );
        }
    }

    if let Some(reconciliation) = &report.reconciliation {
        out!(
            "Reconciliation evidence: {} landed, {} ambiguous, {} unresolved, {} unrecorded",
            reconciliation.landed_entry_count,
            reconciliation.ambiguous_entry_count,
            reconciliation.unresolved_entry_count,
            reconciliation.unrecorded_commits.len()
        );
        out!("  {}", reconciliation.explanation);
    }

    out!("Delivery state: {}", report.next_action.state.as_str());
    out!("Next action: {}", report.next_action.summary);
    for command in &report.next_action.commands {
        out!("  run: {command}");
    }
    Ok(())
}

pub(super) fn render_ship_plan(
    report: &crate::ShipPlan,
    json: bool,
    detail: bool,
) -> Result<(), UsageError> {
    if json {
        out!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    out!(
        "Ship plan q{} (session {})",
        report.queue_entry.id,
        report.originating_session.id
    );
    out!(
        "Integration: {} @ {}",
        report.integration_ref,
        report.integration_sha
    );
    out!("Publication prefix: {}", report.publication_sha);
    // One line per promoted entry ever included is unbounded and grows with
    // the repository's history; the count plus the boundary entries is what a
    // reviewer checks.
    // Most of an included prefix is already on the remote; enumerating it buries
    // the handful of entries this push actually publishes (issue #141).
    let label = |entry: &crate::ship::ShipPromotedEntry| {
        format!("q{}@{}", entry.queue_entry_id, entry.promotion_sha)
    };
    let newly = report
        .included_entries
        .iter()
        .filter(|entry| entry.newly_published)
        .map(label)
        .collect::<Vec<_>>();
    let already_published = report.included_entries.len() - newly.len();
    out!(
        "Included entries: {} total, {} already on the remote default branch",
        report.included_entries.len(),
        already_published
    );
    if newly.is_empty() {
        out!("Newly published by this push: none — the remote already contains this prefix");
    } else {
        out!(
            "Newly published by this push: {} ({})",
            newly.len(),
            if detail || newly.len() <= SHIP_ENTRY_CAP {
                newly.join(", ")
            } else {
                format!(
                    "{}, ... , {} — rerun with --detail for all",
                    newly[..2].join(", "),
                    newly[newly.len() - 1]
                )
            }
        );
    }
    if detail {
        let included = report
            .included_entries
            .iter()
            .map(label)
            .collect::<Vec<_>>();
        out!("Included prefix in full: {}", included.join(", "));
    }
    if !report.excluded_entries.is_empty() {
        out!(
            "Excluded later entries: {}",
            report
                .excluded_entries
                .iter()
                .map(|entry| format!("q{}@{}", entry.queue_entry_id, entry.promotion_sha))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    out!(
        "Local default:  {} @ {}",
        report.local_default_branch_ref,
        report.local_default_branch_sha
    );
    out!(
        "Remote default: {}/{} @ {}",
        report.target.remote_name,
        report.remote_default_branch_ref,
        report.remote_default_branch_sha
    );
    out!(
        "Target: {} ({})",
        report.target.display_slug,
        report.target.normalized_host
    );
    out!(
        "Delivery: {} (source {})",
        report.delivery.mode.as_str(),
        report.delivery.source.as_str()
    );
    if let Some(configured) = report.delivery.configured_mode {
        out!("Configured delivery: {}", configured.as_str());
    } else {
        out!("Configured delivery: none (legacy default is local_main_merge)");
    }
    if let Some(recommended) = report.delivery.recommended_mode {
        out!("Recommended delivery: {}", recommended.as_str());
    }
    if report.delivery.divergence_reasons.is_empty() {
        out!("Delivery divergence: none");
    } else {
        out!(
            "Delivery divergence: {}",
            report.delivery.divergence_reasons.join(", ")
        );
    }
    out!("Delivery selection: {}", report.delivery.reason);
    out!(
        "Trusted delivery config: {} @ {} (digest {})",
        report.delivery.trusted_config_ref,
        report.delivery.trusted_config_commit,
        report
            .delivery
            .trusted_config_digest
            .as_deref()
            .unwrap_or("absent")
    );
    out!("Delivery plan digest: {}", report.plan_digest);
    out!("Freshness: {:?}", report.freshness.result);
    out!("Proposed push: {}", report.proposed_push.command.join(" "));
    out!(
        "Publication policy: {:?} (evidence {})",
        report.publication_policy.policy.mode,
        if report.publication_policy.satisfied {
            "satisfied"
        } else {
            "missing or stale"
        }
    );
    for evidence in &report.publication_policy.evidence {
        out!(
            "  q{} session {}: {} ({})",
            evidence.queue_entry_id,
            evidence.session_id,
            if evidence.covered {
                "covered"
            } else {
                "not covered"
            },
            evidence.reason
        );
    }
    if let Some(remediation) = &report.publication_policy.remediation {
        out!("Publication remediation: {remediation}");
    }
    for line in repository_wide_publication_lines(
        &report.local_main_sync_assessment,
        &report.local_default_branch_ref,
        &report.publication_sha,
        &report.local_default_branch_sha,
    ) {
        out!("{line}");
    }
    let assessment = &report.local_main_sync_assessment;
    out!(
        "Local-main synchronization safe now: {}",
        if report.local_main_sync_safe {
            "yes"
        } else {
            "no"
        }
    );
    if !assessment.tracked_dirty_paths.is_empty() {
        out!(
            "Blocking tracked paths: {}",
            assessment.tracked_dirty_paths.join(", ")
        );
    }
    if !assessment.conflicting_untracked_paths.is_empty() {
        out!(
            "Blocking untracked collisions: {}",
            assessment.conflicting_untracked_paths.join(", ")
        );
    } else if !assessment.untracked_paths.is_empty() {
        out!(
            "Unrelated untracked paths preserved: {}",
            assessment.untracked_paths.join(", ")
        );
    }
    if report.delivery.requires_explicit_selection {
        out!(
            "Re-plan with an explicit route: aethyme broker advanced ship plan --entry {} --delivery <local_main_merge|pull_request>",
            report.queue_entry.id,
        );
    } else if report.delivery.source == crate::RepositoryDeliveryModeSource::LegacyDefault {
        out!(
            "Confirm with: aethyme broker advanced ship execute --entry {} --confirm {}",
            report.queue_entry.id,
            report.publication_sha
        );
    } else if report.delivery.source == crate::RepositoryDeliveryModeSource::CliOverride {
        out!(
            "Confirm with: aethyme broker advanced ship execute --entry {} --confirm {} --delivery {} --plan {}",
            report.queue_entry.id,
            report.publication_sha,
            report.delivery.mode.as_str(),
            report.plan_digest
        );
    } else {
        out!(
            "Confirm with: aethyme broker advanced ship execute --entry {} --confirm {} --plan {}",
            report.queue_entry.id,
            report.publication_sha,
            report.plan_digest
        );
    }
    Ok(())
}

pub(super) fn render_ship_execution(
    report: &crate::ShipExecutionReport,
    json: bool,
) -> Result<(), UsageError> {
    if json {
        out!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    out!(
        "Published {} to {}/{}.",
        report.published_sha,
        report.plan.target.remote_name,
        report.plan.remote_default_branch_ref
    );
    out!("Verified remote SHA: {}", report.verified_remote_sha);
    out!(
        "Publication authorization: {:?}",
        report.publication_authorization.kind
    );
    if let Some(digest) = &report.publication_authorization.reason_digest {
        out!("Break-glass reason SHA-256: {digest}");
    }
    out!(
        "Operations: fetch {}, push {}, verify {}",
        report.fetch_operation.id,
        report.push_operation.id,
        report.verify_operation.id
    );
    if report.local_main_sync.synchronized {
        out!(
            "Local main synchronized: {} -> {}",
            report.local_main_sync.before_sha,
            report.local_main_sync.after_sha
        );
    } else if let Some(command) = &report.local_main_sync.follow_up_command {
        out!("Local main unchanged. To synchronize it explicitly:");
        out!("  {command}");
    }
    Ok(())
}

pub(super) fn render_delivery_execution(
    report: &crate::DeliveryExecutionReport,
    json: bool,
) -> Result<(), UsageError> {
    // Keep the wire shape of the established no-policy command stable. The
    // new wrapper is only needed when delivery has an explicit policy or
    // route-specific operations to report.
    if let crate::DeliveryExecutionReport::LocalMainMerge {
        ship,
        preservation_operation: None,
        merge_operation: None,
        ..
    } = report
    {
        return render_ship_execution(ship, json);
    }
    if json {
        out!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }

    match report {
        crate::DeliveryExecutionReport::LocalMainMerge {
            ship,
            preservation_operation,
            merge_operation,
            plan_digest,
        } => {
            out!("Delivery mode: local_main_merge");
            out!("Delivery plan digest: {plan_digest}");
            if let Some(operation) = preservation_operation {
                out!("Preservation ref operation: {}", operation.id);
            }
            if let Some(operation) = merge_operation {
                out!("Local-main merge operation: {}", operation.id);
            }
            render_ship_execution(ship, false)?;
        }
        crate::DeliveryExecutionReport::PullRequest(report) => {
            out!("Delivery mode: pull_request");
            out!("Delivery branch: {}", report.branch);
            out!("Proposed SHA: {}", report.proposed_sha);
            out!(
                "Pull request #{}: {} ({})",
                report.pull_request.number,
                report.pull_request.url,
                report.pull_request.state
            );
            out!(
                "Base: {} @ {}",
                report.pull_request.base_branch,
                report.pull_request.base_sha
            );
            let checks = &report.pull_request.checks;
            out!(
                "Checks: {} total, {} pending, {} failed, {} passed, {} unknown",
                checks.total,
                checks.pending,
                checks.failed,
                checks.passed,
                checks.unknown
            );
            out!("Delivery state: {}", report.delivery_state.as_str());
            out!(
                "Operations: fetch {}, push {}, verify {}",
                report.fetch_operation.id,
                report.push_operation.id,
                report.verify_operation.id
            );
            if let Some(operation) = &report.target_verification_operation {
                out!("Target-branch verification operation: {}", operation.id);
            }
            if let Some(sha) = &report.target_remote_sha {
                out!("Observed target branch SHA: {sha}");
            }
            if let Some(operation) = &report.branch_operation {
                out!("Branch operation: {}", operation.id);
            }
            if let Some(operation) = &report.create_operation {
                out!("Pull-request create operation: {}", operation.id);
            }
            out!(
                "Publication exposures resolved: {}",
                report.resolved_exposures.len()
            );
            out!(
                "Publication advisories resolved: {}",
                report.resolved_advisories.len()
            );
            if report.delivery_state == crate::DeliveryExecutionState::Published {
                out!("Exact delivery head is verified on the target default branch.");
            } else if report.delivery_state == crate::DeliveryExecutionState::PullRequestMerged {
                out!(
                    "The pull request merge is confirmed, but the exact delivery head is not yet verified on the target default branch."
                );
            } else {
                out!(
                    "Next: merge and verify this pull request on the target default branch before resolving publication exposures."
                );
            }
        }
    }
    Ok(())
}

pub(super) fn render_integration_stability(
    report: &crate::IntegrationStabilityReport,
    json: bool,
) -> Result<(), UsageError> {
    if json {
        out!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }

    out!(
        "Integration: {} {} -> {}",
        report.branch,
        short_commit(&report.start_head),
        short_commit(&report.end_head)
    );
    out!(
        "Window:      {}s (observed {}ms)",
        report.requested_seconds,
        report.observed_ms
    );
    out!(
        "Result:      {}",
        if report.stable { "stable" } else { "moved" }
    );
    out!("{}", report.message);
    if report.live_sessions.is_empty() {
        out!("Live sessions: none");
    } else {
        out!("Live sessions:");
        for session in report.live_sessions.iter().take(10) {
            out!(
                "  session {} {} {} {}",
                session.id,
                session.status.as_str(),
                session.branch,
                session.task.as_deref().unwrap_or("-")
            );
        }
        if report.live_sessions.len() > 10 {
            out!(
                "  and {} more {}",
                report.live_sessions.len() - 10,
                plural(report.live_sessions.len() - 10, "session", "sessions")
            );
        }
    }
    for command in &report.commands {
        out!("run: {command}");
    }
    Ok(())
}

pub(super) fn render_integration_reconcile(
    report: &crate::IntegrationReconcileReport,
    json: bool,
) -> Result<(), UsageError> {
    if json {
        out!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }

    out!("Local main:  {}", short_commit(&report.local_main));
    out!(
        "Upstream:    {} @ {}",
        report.upstream_ref,
        short_commit(&report.upstream_head)
    );
    out!(
        "Integration: {} -> {}",
        short_commit(&report.old_integration),
        short_commit(&report.new_integration)
    );
    if let Some(path) = &report.resolution_file {
        out!("Resolution:  {path}");
    }
    if let Some(digest) = &report.plan_digest {
        out!("Plan digest: {digest}");
    }
    out!(
        "Result:      {}",
        if report.applied {
            "applied"
        } else if report.safe {
            "safe dry-run"
        } else {
            "blocked"
        }
    );
    for entry in &report.entries {
        out!(
            "  q{} session {}: {} — {}",
            entry.queue_entry_id,
            entry.session_id,
            entry.classification.as_str(),
            entry.evidence
        );
        if !entry.conflicts.is_empty() {
            out!("    conflicts: {}", capped_join(&entry.conflicts, 5));
        }
    }
    if let Some(template) = &report.resolution_template {
        out!(
            "Resolution template: {} recorded, {} unrecorded ({})",
            template.document.resolutions.len(),
            template.document.unrecorded_resolutions.len(),
            if template.complete {
                "complete"
            } else {
                "operator input required"
            }
        );
        out!(
            "  recorded classification: {}",
            template
                .field_contract
                .recorded_classification_allowed_values
                .join(", ")
        );
        for rule in &template.field_contract.unrecorded_dispositions {
            out!(
                "  {}: upstream_commit {}; {}",
                rule.value,
                rule.upstream_commit,
                rule.condition
            );
        }
        out!("  operator: {}", template.field_contract.operator);
        out!("  reason: {}", template.field_contract.reason);
    }
    for warning in &report.warnings {
        out!("Warning: {warning}");
    }
    out!("Next action: {}", report.next_action);
    Ok(())
}

pub(super) fn write_reconciliation_resolution_template(
    path: &std::path::Path,
    document: &crate::IntegrationReconcileResolutionTemplateDocument,
) -> Result<(), UsageError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let parent = parent.unwrap_or_else(|| std::path::Path::new("."));
    if !parent.is_dir() {
        return Err(UsageError::Message(format!(
            "resolution template parent directory does not exist: {}",
            parent.display()
        )));
    }
    let mut bytes = serde_json::to_vec_pretty(document)?;
    bytes.push(b'\n');
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|error| {
        UsageError::Message(format!(
            "cannot create resolution template beside {}: {error}",
            path.display()
        ))
    })?;
    temporary.write_all(&bytes).map_err(|error| {
        UsageError::Message(format!(
            "cannot write resolution template {}: {error}",
            path.display()
        ))
    })?;
    temporary.as_file().sync_all().map_err(|error| {
        UsageError::Message(format!(
            "cannot sync resolution template {}: {error}",
            path.display()
        ))
    })?;
    temporary.persist_noclobber(path).map_err(|error| {
        UsageError::Message(format!(
            "refusing to overwrite resolution template {}: {}",
            path.display(),
            error.error
        ))
    })?;
    Ok(())
}

/// `broker ship`.
pub(super) fn run_ship(parsed: Parsed) -> Result<(), UsageError> {
    let action = parsed
        .positional
        .first()
        .map(String::as_str)
        .ok_or(UsageError::Message("ship requires an action: plan".into()))?;
    match action {
        "plan" => {
            let entry = parsed.entry.ok_or(UsageError::Message(
                "ship plan requires --entry <id>".into(),
            ))?;
            let delivery = parse_repository_delivery_mode(parsed.delivery_mode.as_deref())?;
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let report = broker.ship_plan_with_delivery(entry, delivery)?;
            render_ship_plan(&report, parsed.json, parsed.detail)?;
        }
        "execute" => {
            let entry = parsed.entry.ok_or(UsageError::Message(
                "ship execute requires --entry <id>".into(),
            ))?;
            let confirm = parsed.confirm.as_deref().ok_or(UsageError::Message(
                "ship execute requires --confirm <full-publication-sha>".into(),
            ))?;
            let delivery = parse_repository_delivery_mode(parsed.delivery_mode.as_deref())?;
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            // Preserve the established command contract for callers
            // that do not opt into delivery routing. A configured
            // repository policy still refuses this compatibility path
            // inside `ship_execute_with_policy`; an unconfigured
            // repository keeps its historical direct-ship behavior.
            if delivery.is_none() && parsed.delivery_plan.is_none() {
                let report = broker.ship_execute_with_policy(
                    entry,
                    confirm,
                    parsed.sync_main,
                    parsed.break_glass,
                    parsed.reason.as_deref(),
                )?;
                render_ship_execution(&report, parsed.json)?;
                return Ok(());
            }
            let report = broker.ship_execute_delivery(
                entry,
                confirm,
                delivery,
                parsed.delivery_plan.as_deref(),
                parsed.sync_main,
                parsed.break_glass,
                parsed.reason.as_deref(),
            )?;
            render_delivery_execution(&report, parsed.json)?;
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown ship action {other:?} — expected plan or execute"
            )));
        }
    }
    Ok(())
}

/// `broker integration`.
pub(super) fn run_integration(parsed: Parsed) -> Result<(), UsageError> {
    let action = parsed
        .positional
        .first()
        .map(String::as_str)
        .ok_or(UsageError::Message(
            "integration requires an action: status or wait-stable".into(),
        ))?;
    match action {
        "status" => {
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let report = if parsed.read_only_snapshot {
                broker.integration_status_snapshot()?
            } else {
                broker.integration_status(now_ms())?
            };
            render_integration_status(&report, parsed.json)?;
        }
        "wait-stable" => {
            let seconds = parsed.seconds.unwrap_or(30);
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let report = broker.wait_integration_stable(seconds)?;
            render_integration_stability(&report, parsed.json)?;
            if !report.stable {
                return Err(UsageError::Message(
                    "integration moved during wait-stable window".into(),
                ));
            }
        }
        "reconcile" => {
            if parsed.apply && parsed.dry_run {
                return Err(UsageError::Message(format!(
                    "choose either --dry-run or --apply, not both; {INTEGRATION_RECONCILE_USAGE}"
                )));
            }
            let upstream = parsed
                .upstream
                .clone()
                .ok_or(UsageError::Message(INTEGRATION_RECONCILE_USAGE.into()))?;
            if parsed.apply && parsed.confirm.is_none() {
                return Err(UsageError::Message(INTEGRATION_RECONCILE_USAGE.into()));
            }
            if parsed.apply && parsed.write_resolution_template.is_some() {
                return Err(UsageError::Message(format!(
                    "--write-resolution-template is a dry-run aid and cannot be combined with --apply; {INTEGRATION_RECONCILE_USAGE}"
                )));
            }
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let report = broker.reconcile_integration(crate::IntegrationReconcileOptions {
                upstream,
                apply: parsed.apply,
                resolution_file: parsed.resolution_file.clone(),
                confirm: parsed.confirm.clone(),
            })?;
            if let Some(path) = parsed.write_resolution_template.as_deref() {
                let template = report.resolution_template.as_ref().ok_or_else(|| {
                    UsageError::Message(
                        "no reconciliation resolution template is required for this plan".into(),
                    )
                })?;
                write_reconciliation_resolution_template(path, &template.document)?;
                eprintln!("Wrote resolution template to {}", path.display());
            }
            render_integration_reconcile(&report, parsed.json)?;
            if !report.safe {
                return Err(UsageError::Message(
                    "integration reconciliation is ambiguous or conflicting; no state changed"
                        .into(),
                ));
            }
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown integration action {other:?} — expected status, wait-stable, or reconcile"
            )));
        }
    }
    Ok(())
}
