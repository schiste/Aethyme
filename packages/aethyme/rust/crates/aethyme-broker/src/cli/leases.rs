//! `broker leases`, `note`, `advisories` and `exposures`: what sessions claim and tell each other.

use super::*;

pub(super) fn render_lease_plan(report: &crate::LeasePlan, json: bool) -> Result<(), UsageError> {
    if json {
        out!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }

    for path in &report.paths {
        out!(
            "{} — {}",
            path.path,
            if path.would_conflict {
                "would conflict"
            } else {
                "clear"
            }
        );
        for (label, overlaps) in [("owned", &path.owned), ("conflict", &path.conflicts)] {
            for overlap in overlaps {
                let owner = match &overlap.owner_context {
                    Some(context) => format!("{} [{}]", overlap.owner_status.as_str(), context),
                    None => overlap.owner_status.as_str().to_string(),
                };
                out!(
                    "  {label:<8} {:<9} session {:<4} {:<9} {} (expires {}; owner {} at {})",
                    match overlap.relation {
                        crate::LeaseOverlapRelation::Exact => "exact",
                        crate::LeaseOverlapRelation::Directory => "directory",
                    },
                    overlap.session_id,
                    overlap.kind.as_str(),
                    overlap.path,
                    overlap
                        .expires_at
                        .map(|expiry| expiry.to_string())
                        .unwrap_or_else(|| "never".to_string()),
                    owner,
                    overlap.owner_worktree,
                );
                if label == "conflict" {
                    for action in &overlap.safe_next_actions {
                        out!("    next: {action}");
                    }
                }
            }
        }
        if path.owned.is_empty() && path.conflicts.is_empty() {
            out!("  no active overlaps");
        }
    }
    Ok(())
}

pub(super) fn render_planned_explicit_leases(leases: &[crate::Lease]) {
    if leases.is_empty() {
        return;
    }
    out!("Planned explicit leases:");
    for lease in leases {
        out!("  {}", lease.path);
    }
}

pub(super) fn advisory_text(value: &str) -> String {
    serde_json::to_string(value).expect("serializing advisory text cannot fail")
}

pub(super) fn render_advisory(advisory: &crate::Advisory) {
    out!(
        "Advisory {}: {} [{} / {}]",
        advisory.id,
        advisory_text(&advisory.identity),
        advisory.severity.as_str(),
        advisory.resolution_state.as_str(),
    );
    out!(
        "Session: {}",
        advisory
            .session_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "none".into())
    );
    out!(
        "Queue entry: {}",
        advisory
            .queue_entry_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "none".into())
    );
    out!(
        "Integration SHA: {}",
        advisory.integration_sha.as_deref().unwrap_or("none")
    );
    out!("Created: {}", advisory.created_at);
    out!(
        "Acknowledged: {}",
        advisory
            .acknowledged_at
            .map(|time| time.to_string())
            .unwrap_or_else(|| "no".into())
    );
    out!(
        "Suppressed: {}",
        advisory
            .suppressed_at
            .map(|time| time.to_string())
            .unwrap_or_else(|| "no".into())
    );
    out!(
        "Resolved: {}",
        advisory
            .resolved_at
            .map(|time| time.to_string())
            .unwrap_or_else(|| "no".into())
    );
    if let Some(evidence) = advisory.resolution_evidence.as_deref() {
        out!("Resolution evidence: {}", advisory_text(evidence));
    }
    if !advisory.paths.is_empty() {
        out!("Paths:");
        for path in &advisory.paths {
            out!("  - {}", advisory_text(path));
        }
    }
    if !advisory.evidence.is_empty() {
        out!("Evidence:");
        for evidence in &advisory.evidence {
            out!(
                "  - {}: {}",
                advisory_text(&evidence.kind),
                advisory_text(&evidence.summary)
            );
        }
    }
    if advisory.resolution_state == crate::AdvisoryResolutionState::Outstanding {
        out!(
            "Acknowledge: aethyme broker advanced advisories ack {}",
            advisory.id
        );
        if advisory.audience == crate::AdvisoryAudience::Maintainer {
            out!(
                "Suppress: aethyme broker advanced advisories suppress {}",
                advisory.id
            );
        }
    }
}

pub(super) fn parse_advisory_id(value: Option<&String>, usage: &str) -> Result<i64, UsageError> {
    let id = value
        .ok_or_else(|| UsageError::Message(usage.into()))?
        .parse::<i64>()
        .map_err(|_| {
            UsageError::Message(format!("advisory id must be a positive integer; {usage}"))
        })?;
    if id <= 0 {
        return Err(UsageError::Message(format!(
            "advisory id must be a positive integer; {usage}"
        )));
    }
    Ok(id)
}

/// `broker leases`.
pub(super) fn run_leases(parsed: Parsed) -> Result<(), UsageError> {
    let export = parsed.positional.first().map(String::as_str) == Some("export");
    let mut broker = open_broker(parsed.read_only_snapshot || export)?;
    match parsed.positional.first().map(String::as_str) {
        None => {
            let overlaps = if parsed.read_only_snapshot {
                broker.lease_overlaps_snapshot()?
            } else {
                broker.refresh_leases()?
            };
            let leases = broker.store().active_leases()?;
            if parsed.json {
                out!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "leases": leases,
                        "overlaps": overlaps,
                    }))?
                );
            } else if leases.is_empty() {
                out!("No active leases.");
            } else {
                out!("{:<4} {:<9} PATH", "SID", "KIND");
                for lease in leases {
                    out!(
                        "{:<4} {:<9} {}",
                        lease.session_id,
                        lease.kind.as_str(),
                        lease.path
                    );
                }
                print_overlap_warnings(&overlaps);
            }
        }
        Some("claim") => {
            let path = parsed
                .positional
                .get(1)
                .ok_or(UsageError::Message("claim requires a path".into()))?;
            let session = parsed
                .session
                .ok_or(UsageError::Message("claim requires --session <id>".into()))?;
            let report = broker.claim_lease(session, path, parsed.ttl_seconds.map(|s| s * 1000))?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                out!("Session {session} claimed {path}.");
            }
        }
        Some("plan") => {
            let paths = parsed.positional.get(1..).unwrap_or_default();
            if paths.is_empty() {
                return Err(UsageError::Message(
                    "plan requires at least one path".into(),
                ));
            }
            let report = broker.plan_leases(paths, parsed.session)?;
            render_lease_plan(&report, parsed.json)?;
        }
        Some("export") => {
            let limit = parsed
                .limit
                .map(|value| value as usize)
                .unwrap_or(crate::DEFAULT_LEASE_ROUTING_EXPORT_LIMIT);
            let report = broker.export_lease_routing(
                crate::LeaseRoutingExportOptions {
                    session_id: parsed.session,
                    queue_entry_id: parsed.entry,
                    limit,
                },
                now_ms(),
            )?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                out!(
                    "Lease routing for {} (session {}, {} of {} rows):",
                    report.repository.display_slug,
                    report.selector.session_id,
                    report.leases.len(),
                    report.total_matching
                );
                for lease in &report.leases {
                    let routes = if lease.routing_categories.is_empty() {
                        "unrouted".into()
                    } else {
                        lease.routing_categories.join(",")
                    };
                    out!(
                        "  {} [{} / {} / {}] routes={}{}",
                        lease.path,
                        lease.path_kind.as_str(),
                        lease.lease_kind.as_str(),
                        lease.state.as_str(),
                        routes,
                        if lease.conflicting_session_ids.is_empty() {
                            String::new()
                        } else {
                            format!(
                                " conflicts=s{}",
                                lease
                                    .conflicting_session_ids
                                    .iter()
                                    .map(i64::to_string)
                                    .collect::<Vec<_>>()
                                    .join(",s")
                            )
                        }
                    );
                }
                if report.truncated {
                    out!(
                        "  truncated: increase --limit up to {}",
                        crate::MAX_LEASE_ROUTING_EXPORT_LIMIT
                    );
                }
            }
        }
        Some("release") => {
            let path = parsed
                .positional
                .get(1)
                .ok_or(UsageError::Message("release requires a path".into()))?;
            let session = parsed.session.ok_or(UsageError::Message(
                "release requires --session <id>".into(),
            ))?;
            broker.store().release_lease(session, path)?;
            if parsed.json {
                out!("{{\"released\":{}}}", serde_json::to_string(path)?);
            } else {
                out!("Session {session} released {path}.");
            }
        }
        Some(other) => {
            return Err(UsageError::Message(format!(
                "unknown leases action {other:?} — expected claim, plan, export, or release"
            )));
        }
    }
    Ok(())
}

/// `broker advisories`.
pub(super) fn run_advisories(parsed: Parsed) -> Result<(), UsageError> {
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    match parsed.positional.first().map(String::as_str) {
        Some("list") => {
            if parsed.positional.len() != 1 {
                return Err(UsageError::Message(
                    "usage: aethyme broker advanced advisories list [--all] [--json]".into(),
                ));
            }
            if !parsed.read_only_snapshot {
                broker.refresh_maintainer_recommendations()?;
            }
            let report = broker.advisory_list(parsed.all)?;
            if !parsed.read_only_snapshot {
                broker.record_advisories_shown(
                    &report.advisories,
                    crate::AdvisoryDeliverySurface::Inventory,
                )?;
            }
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else if report.advisories.is_empty() {
                out!("No outstanding advisories.");
            } else {
                out!(
                    "{:<5} {:<11} {:<9} {:<14} IDENTITY",
                    "ID",
                    "AUDIENCE",
                    "SEVERITY",
                    "STATE"
                );
                for advisory in &report.advisories {
                    out!(
                        "{:<5} {:<11} {:<9} {:<14} {}",
                        advisory.id,
                        advisory.audience.as_str(),
                        advisory.severity.as_str(),
                        advisory.resolution_state.as_str(),
                        advisory_text(&advisory.identity),
                    );
                }
                out!("Outstanding: {}", report.outstanding_count);
            }
        }
        Some("show") => {
            if parsed.positional.len() != 2 {
                return Err(UsageError::Message(ADVISORIES_SHOW_USAGE.into()));
            }
            let id = parse_advisory_id(parsed.positional.get(1), ADVISORIES_SHOW_USAGE)?;
            let advisory = broker.advisory(id)?;
            if !parsed.read_only_snapshot {
                broker.record_advisories_shown(
                    std::slice::from_ref(&advisory),
                    crate::AdvisoryDeliverySurface::Inventory,
                )?;
            }
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&advisory)?);
            } else {
                render_advisory(&advisory);
            }
        }
        Some("ack") => {
            if parsed.positional.len() != 2 {
                return Err(UsageError::Message(ADVISORIES_ACK_USAGE.into()));
            }
            let id = parse_advisory_id(parsed.positional.get(1), ADVISORIES_ACK_USAGE)?;
            let advisory = broker.acknowledge_advisory(id)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&advisory)?);
            } else {
                out!(
                    "Acknowledged advisory {}: {}",
                    advisory.id,
                    advisory_text(&advisory.identity)
                );
                out!("Projection refreshed: {}", crate::BROKER_ADVISORY_RELPATH);
            }
        }
        Some("suppress") => {
            if parsed.positional.len() != 2 {
                return Err(UsageError::Message(ADVISORIES_SUPPRESS_USAGE.into()));
            }
            let id = parse_advisory_id(parsed.positional.get(1), ADVISORIES_SUPPRESS_USAGE)?;
            let advisory = broker.suppress_maintainer_advisory(id)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&advisory)?);
            } else {
                out!(
                    "Suppressed maintainer advisory {}: {}",
                    advisory.id,
                    advisory_text(&advisory.identity)
                );
            }
        }
        Some("metrics") => {
            if parsed.positional.len() != 1 {
                return Err(UsageError::Message(
                    "usage: aethyme broker advanced advisories metrics [--json]".into(),
                ));
            }
            let summary = broker.advisory_delivery_summary()?;
            let metrics = broker.advisory_delivery_metrics()?;
            if parsed.json {
                out!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "schema_version": 1,
                        "summary": summary,
                        "metrics": metrics,
                    }))?
                );
            } else {
                out!(
                    "Advisory delivery: {} shown, {} actioned, {} displays across {} surfaces.",
                    summary.shown_advisories,
                    summary.actioned_advisories,
                    summary.total_shows,
                    summary.surface_rows,
                );
                for metric in metrics {
                    out!(
                        "  advisory {} / {}: {} display{}{}",
                        metric.advisory_id,
                        metric.surface.as_str(),
                        metric.show_count,
                        if metric.show_count == 1 { "" } else { "s" },
                        metric
                            .action
                            .map(|action| format!("; {}", action.as_str()))
                            .unwrap_or_default(),
                    );
                }
            }
        }
        Some(other) => {
            return Err(UsageError::Message(format!(
                "unknown advisories action {other:?} — expected list, show, ack, suppress, or metrics"
            )));
        }
        None => {
            return Err(UsageError::Message(
                "advisories requires an action: list, show, ack, suppress, or metrics".into(),
            ));
        }
    }
    Ok(())
}

/// `broker exposures`.
pub(super) fn run_exposures(parsed: Parsed) -> Result<(), UsageError> {
    let action = parsed
        .positional
        .first()
        .map(String::as_str)
        .ok_or_else(|| UsageError::Message("exposures requires an action: plan or apply".into()))?;
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    match action {
        "plan" => {
            let plan = broker.exposure_reconciliation_plan()?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&plan)?);
            } else {
                out!(
                    "Remote: {} @ {}",
                    plan.remote_default_branch_ref,
                    short_commit(&plan.remote_default_branch_sha)
                );
                out!(
                    "Tracking: {} @ {} ({})",
                    plan.tracking_ref,
                    plan.tracking_sha
                        .as_deref()
                        .map(short_commit)
                        .unwrap_or("missing"),
                    if plan.tracking_matches_remote {
                        "current"
                    } else {
                        "stale or missing"
                    }
                );
                out!(
                    "Exposures: {} contained, {} remaining",
                    plan.contained_exposures.len(),
                    plan.remaining_exposures.len()
                );
                let eligible = plan
                    .advisories
                    .iter()
                    .filter(|advisory| advisory.eligible)
                    .count();
                out!(
                    "Advisories: {} eligible, {} blocked by live leases",
                    eligible,
                    plan.advisories.len().saturating_sub(eligible)
                );
                for refusal in &plan.refusals {
                    out!("Refusal: {refusal}");
                }
                out!("Plan digest: {}", plan.digest);
                if plan.safe {
                    out!(
                        "Apply with: aethyme broker advanced exposures apply --session <id> --confirm {}",
                        plan.digest
                    );
                }
            }
        }
        "apply" => {
            let session = parsed.session.ok_or_else(|| {
                UsageError::Message("exposures apply requires --session <id>".into())
            })?;
            let confirm = parsed.confirm.as_deref().ok_or_else(|| {
                UsageError::Message("exposures apply requires --confirm <sha256>".into())
            })?;
            let report = broker.apply_exposure_reconciliation(session, confirm)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                out!(
                    "Verified {} at {} via operation {}.",
                    report.plan.remote_default_branch_ref,
                    report.plan.remote_default_branch_sha,
                    report.verification_operation.id
                );
                out!(
                    "Resolved {} exposure(s) and {} advisory record(s).",
                    report.resolved_exposures.len(),
                    report.resolved_advisories.len()
                );
            }
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown exposures action {other:?} — expected plan or apply"
            )));
        }
    }
    Ok(())
}

/// `broker note`.
pub(super) fn run_note(parsed: Parsed) -> Result<(), UsageError> {
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    match parsed.positional.first().map(String::as_str) {
        Some("send") if parsed.positional.len() == 1 => {
            let sender = parsed.session.ok_or_else(|| {
                UsageError::Message("note send requires --session <sender>".into())
            })?;
            let recipient = parsed.to_session.ok_or_else(|| {
                UsageError::Message("note send requires --to-session <recipient>".into())
            })?;
            let message = parsed
                .message
                .as_deref()
                .ok_or_else(|| UsageError::Message("note send requires --message <text>".into()))?;
            let note = broker.send_session_note(sender, recipient, message)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&note)?);
            } else {
                out!(
                    "Sent broker note {} from session {} to session {}.",
                    note.id,
                    note.sender_session_id,
                    note.recipient_session_id
                );
            }
        }
        Some("list") if parsed.positional.len() == 1 => {
            let recipient = parsed.session.ok_or_else(|| {
                UsageError::Message("note list requires --session <recipient>".into())
            })?;
            let list = broker.session_note_list(recipient)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&list)?);
            } else if list.notes.is_empty() {
                out!("No broker notes for session {recipient}.");
            } else {
                out!("{:<5} {:<8} {:<14} MESSAGE", "ID", "FROM", "STATE");
                for note in &list.notes {
                    out!(
                        "{:<5} {:<8} {:<14} {}",
                        note.id,
                        note.sender_session_id,
                        if note.acknowledged_at.is_some() {
                            "acknowledged"
                        } else {
                            "unread"
                        },
                        note.message
                    );
                }
                out!("Unread: {}", list.unread_count);
            }
        }
        Some("ack") if parsed.positional.len() == 1 => {
            let recipient = parsed.session.ok_or_else(|| {
                UsageError::Message("note ack requires --session <recipient>".into())
            })?;
            let note_id = parsed
                .note_id
                .ok_or_else(|| UsageError::Message("note ack requires --id <note-id>".into()))?;
            let note = broker.acknowledge_session_note(recipient, note_id)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&note)?);
            } else {
                out!("Acknowledged broker note {}.", note.id);
            }
        }
        Some(other) => {
            return Err(UsageError::Message(format!(
                "unknown note action {other:?} — expected send, list, or ack"
            )));
        }
        None => {
            return Err(UsageError::Message(
                "note requires an action: send, list, or ack".into(),
            ));
        }
    }
    Ok(())
}
