//! `broker gc`, `storage` and `reclaim`: disk the broker can give back.

use super::*;

/// Default number of items any one plan list prints before summarising.
pub(super) const GC_LIST_CAP: usize = 5;

pub(super) fn render_storage_plan(plan: &crate::StoragePlan, detail: bool) {
    out!(
        "Host storage plan {}: {} root(s), {} on-disk directory entries, {} host candidate(s), {} reclaimable; {} enrolled primary checkout(s), {} artifact(s), {} artifact candidate(s), {} reclaimable",
        plan.digest,
        plan.summary.root_count,
        plan.summary.on_disk_directory_count,
        plan.summary.candidate_count,
        plan.summary
            .reclaimable_bytes
            .map(human_bytes)
            .unwrap_or_else(|| "unknown bytes".into()),
        plan.summary.primary_checkout_count,
        plan.summary.primary_artifact_count,
        plan.summary.primary_candidate_count,
        plan.summary
            .primary_reclaimable_bytes
            .map(human_bytes)
            .unwrap_or_else(|| "unknown bytes".into()),
    );
    out!(
        "  storage root: {} (orphan grace {} day(s))",
        plan.storage_root.display(),
        plan.orphan_worktree_roots_days
    );
    for warning in &plan.warnings {
        out!("  warning: {warning}");
    }
    for root in &plan.roots {
        out!(
            "  root: {} ({:?}, marker {:?}, owner {}, {}, {})",
            root.path.display(),
            root.filesystem_kind,
            root.marker_status,
            match root.owner_exists {
                Some(true) => "present",
                Some(false) => "missing",
                None => "unknown",
            },
            root.worktree_count,
            root.estimated_bytes
                .map(human_bytes)
                .unwrap_or_else(|| "unknown bytes".into()),
        );
        for blocker in &root.blockers {
            out!("    blocker: {blocker}");
        }
        if detail {
            for entry in &root.reconciliation.entries {
                if !entry.missing_from.is_empty() {
                    out!(
                        "    drift: {} (missing {:?}{})",
                        entry.path.display(),
                        entry.missing_from,
                        if entry.git_marker { "; git marker" } else { "" },
                    );
                }
            }
        }
    }
    for checkout in &plan.primary_checkouts {
        out!(
            "  primary checkout: {} ({}, {} artifact(s))",
            checkout.path.display(),
            if checkout.clean {
                "clean"
            } else {
                "refused: dirty"
            },
            checkout.artifacts.len(),
        );
        for blocker in &checkout.blockers {
            out!("    blocker: {blocker}");
        }
        if detail {
            for artifact in &checkout.artifacts {
                out!(
                    "    artifact: {} ({}, ignored {}, tracked {}) — {}",
                    artifact.path.display(),
                    artifact
                        .estimated_bytes
                        .map(human_bytes)
                        .unwrap_or_else(|| "unknown bytes".into()),
                    artifact.ignored,
                    artifact.tracked,
                    artifact.reason,
                );
            }
        }
    }
    render_capped(&plan.candidates, GC_LIST_CAP, detail, |candidate| {
        out!(
            "  candidate: {:?} {} ({}) — {}",
            candidate.kind,
            candidate.path.display(),
            candidate
                .estimated_bytes
                .map(human_bytes)
                .unwrap_or_else(|| "unknown bytes".into()),
            candidate.reason,
        );
    });
    render_capped(&plan.primary_candidates, GC_LIST_CAP, detail, |candidate| {
        out!(
            "  primary candidate: {:?} {} ({}) — {}",
            crate::StorageCandidateKind::PrimaryArtifact,
            candidate.path.display(),
            candidate
                .estimated_bytes
                .map(human_bytes)
                .unwrap_or_else(|| "unknown bytes".into()),
            candidate.reason,
        );
    });
    if plan.candidates.is_empty() && plan.primary_candidates.is_empty() {
        out!("  apply: nothing eligible");
    } else {
        out!(
            "  apply: aethyme broker storage apply --confirm {}",
            plan.digest
        );
    }
}

pub(super) fn render_storage_apply(report: &crate::StorageApplyReport) {
    out!(
        "Host storage apply {}: {} removed, {} reclaimed",
        if report.complete {
            "complete"
        } else {
            "paused"
        },
        report.applied.len(),
        human_bytes(report.reclaimed_bytes),
    );
    for item in &report.applied {
        out!(
            "  removed: {:?} {} ({})",
            item.kind,
            item.path.display(),
            human_bytes(item.reclaimed_bytes),
        );
    }
    for failure in &report.failures {
        out!(
            "  retained: {:?} {} — {}",
            failure.kind,
            failure.path.display(),
            failure.reason,
        );
    }
    if let Some(action) = &report.recovery_action {
        out!("  recovery: {action}");
    }
}

pub(super) fn render_gc_plan(plan: &crate::GcPlan, detail: bool) {
    out!(
        "GC plan {}: {} rows, {} files, {} represented worktrees, {} build caches, {} orphaned roots, {} reclaimable",
        plan.digest,
        plan.rows.len(),
        plan.files.len(),
        plan.worktrees.len(),
        plan.artifacts.len(),
        plan.orphans.len(),
        human_bytes(plan.estimated_reclaimable_bytes),
    );
    for warning in &plan.retention_config_warnings {
        out!("  retention warning: {warning}");
    }
    if !plan.declined_artifacts.is_empty() {
        out!(
            "  ignored but unclassified: {} {}, {}; reported only, not reclaimable",
            plan.declined_artifacts.len(),
            crate::broker::plural_word(plan.declined_artifacts.len(), "directory", "directories"),
            human_bytes(plan.estimated_declined_artifact_bytes),
        );
    }
    out!(
        "  retained: {}; blocked by policy or provenance: {}",
        human_bytes(plan.estimated_retained_bytes),
        human_bytes(plan.estimated_blocked_bytes),
    );
    // The ordering line is not decoration: `gc apply --budget-ms` drains these
    // lists in the order printed and stops at its deadline, so this says which
    // end of the backlog a bounded sweep actually reaches (#176).
    let ordered = plan.reclaim_order.as_str().replace('_', " ");
    match plan.budget_verdict {
        crate::BudgetVerdict::Over => out!(
            "  over budget by {}: candidates ordered {}{}",
            human_bytes(plan.retained_bytes_deficit),
            ordered,
            if plan.clears_retained_bytes_budget {
                "; applying this plan clears the budget"
            } else {
                "; applying all of this plan still leaves retention over budget"
            },
        ),
        crate::BudgetVerdict::Within => {
            out!("  within budget: candidates ordered {ordered}")
        }
        // Distinguished from "within budget" on purpose. The bytes this
        // total skipped are exactly the ones that would have decided the
        // question, so silence here would read as a pass (#176).
        crate::BudgetVerdict::Unknown => {
            out!("  budget undecided, retained total is a floor: candidates ordered {ordered}")
        }
        crate::BudgetVerdict::Unset => {
            out!("  no retained bytes budget configured: candidates ordered {ordered}")
        }
    }
    // Says which of the byte figures above are measurements. A plan that
    // reports a floor as a total lets the budget read as satisfied because
    // nobody looked (#176).
    if plan.unmeasured_directory_count > 0 {
        out!(
            "  size measurement: {} retained {} never been sized, so byte totals are a floor",
            plan.unmeasured_directory_count,
            crate::broker::plural_word(
                plan.unmeasured_directory_count,
                "directory has",
                "directories have",
            ),
        );
    }
    // Listed apart from the candidate sections above because it is not a
    // candidate list: `gc apply` will not touch any of these, and printing
    // them among things the digest authorizes would imply otherwise (#176).
    if let Some(sweep) = &plan.reconciliation
        && sweep.unclaimed_count > 0
    {
        out!(
            "  unclaimed by any session ({} of {} {} under {} broker worktree {}, {}): reported only, `gc apply` does not remove these",
            sweep.unclaimed_count,
            sweep.directory_count,
            crate::broker::plural_word(sweep.directory_count, "directory", "directories"),
            sweep.scanned_root_count,
            crate::broker::plural_word(sweep.scanned_root_count, "root", "roots"),
            if sweep.sized {
                human_bytes(sweep.unclaimed_bytes)
            } else {
                "unsized".to_string()
            },
        );
        render_capped(&sweep.unclaimed, GC_LIST_CAP, detail, |entry| {
            out!(
                "  unclaimed: {} ({}{})",
                entry.path,
                entry.kind,
                entry
                    .estimated_bytes
                    .map(|bytes| format!(", {}", human_bytes(bytes)))
                    .unwrap_or_default()
            );
        });
    }
    render_capped(&plan.rows, GC_LIST_CAP, detail, |row| {
        out!(
            "  row: {:?} {} at {} ({} bytes)",
            row.kind,
            row.id,
            row.recorded_at,
            row.estimated_bytes
        );
    });
    render_capped(&plan.files, GC_LIST_CAP, detail, |file| {
        out!(
            "  file: {:?} {} ({} -> {} bytes; before {})",
            file.action,
            file.path,
            file.bytes_before,
            file.bytes_after,
            file.before_sha256
        );
    });
    render_capped(&plan.worktrees, GC_LIST_CAP, detail, |worktree| {
        out!(
            "  worktree: session {} {} ({} bytes)",
            worktree.session_id,
            worktree.worktree_path,
            worktree.estimated_bytes
        );
        out!(
            "    ref: {} at {}",
            worktree.branch_ref,
            worktree.branch_tip.as_deref().unwrap_or("missing")
        );
    });
    render_capped(&plan.artifacts, GC_LIST_CAP, detail, |artifact| {
        out!(
            "  build cache: session {} {}/{} ({}, idle {} days)",
            artifact.session_id,
            artifact.worktree_path,
            artifact.relative_dir,
            human_bytes(artifact.estimated_bytes),
            artifact.idle_days,
        );
    });
    render_capped(&plan.declined_artifacts, GC_LIST_CAP, detail, |artifact| {
        out!(
            "  ignored but unclassified: session {} {}/{} ({}) — {}",
            artifact.session_id,
            artifact.worktree_path,
            artifact.relative_dir,
            human_bytes(artifact.estimated_bytes),
            artifact.reason,
        );
    });
    render_capped(&plan.orphans, GC_LIST_CAP, detail, |orphan| {
        out!(
            "  orphaned root: {} ({}) — {}",
            orphan.worktree_root,
            human_bytes(orphan.estimated_bytes),
            orphan.reason,
        );
        out!(
            "    owning repository: {} (missing)",
            orphan.repository_root
        );
    });
    render_capped(&plan.checkpoint_pin_releases, GC_LIST_CAP, detail, |pin| {
        out!(
            "  checkpoint pin: session {} queue {} ({}; releasing broker metadata does not remove committed work)",
            pin.session_id,
            pin.queue_entry_id,
            pin.reason,
        );
    });
    render_capped(
        &plan.publication_exposure_expiries,
        GC_LIST_CAP,
        detail,
        |expiry| {
            out!(
                "  publication expiry: exposure {} queue {} ({} days old; {})",
                expiry.exposure_id,
                expiry.queue_entry_id,
                expiry.age_days,
                expiry.reason,
            );
        },
    );
    if !plan.blocker_summary.is_empty() {
        out!("  protections by kind:");
        for summary in &plan.blocker_summary {
            let age = summary
                .oldest_age_days
                .map(|days| {
                    let member = summary
                        .oldest_id
                        .map(|id| format!(", oldest member {id}"))
                        .unwrap_or_default();
                    let policy = summary
                        .age_policy_days
                        .map(|policy| format!(", policy {policy}d"))
                        .unwrap_or_default();
                    format!(
                        ", oldest {days}d{member}{policy}{}",
                        if summary.age_exceeded {
                            ", age exceeded"
                        } else {
                            ""
                        }
                    )
                })
                .unwrap_or_default();
            out!(
                "    {}: {} {}, {} retained{}",
                summary.kind,
                summary.count,
                crate::broker::plural_word(summary.count, "blocker", "blockers"),
                human_bytes(summary.retained_bytes),
                age,
            );
        }
    }
    if !plan.worktree_blocker_summary.is_empty() {
        out!("  blocked retained bytes by kind:");
        for summary in &plan.worktree_blocker_summary {
            out!(
                "    {}: {} {}, {} retained",
                summary.kind,
                summary.count,
                crate::broker::plural_word(summary.count, "blocker", "blockers"),
                human_bytes(summary.retained_bytes),
            );
        }
    }
    render_capped(&plan.blockers, GC_LIST_CAP, detail, |blocker| {
        out!(
            "  protected: {}{} — {}",
            blocker.kind,
            blocker.id.map(|id| format!(" {id}")).unwrap_or_default(),
            blocker.reason
        );
    });
    if plan.rows.is_empty()
        && plan.files.is_empty()
        && plan.worktrees.is_empty()
        && plan.artifacts.is_empty()
        && plan.orphans.is_empty()
        && plan.checkpoint_pin_releases.is_empty()
        && plan.publication_exposure_expiries.is_empty()
    {
        out!("  apply: nothing eligible");
    } else {
        out!("  apply: aethyme broker gc apply --confirm {}", plan.digest);
    }
}

pub(super) fn render_gc_apply(report: &crate::GcApplyReport) {
    out!(
        "GC apply {}: {} rows, {} files, {} worktrees, {} build caches, {} orphaned roots, {} checkpoint pins released, {} exposures expired, {} reclaimed",
        if report.complete {
            "complete"
        } else {
            "paused"
        },
        report.rows_removed,
        report.files_completed.len(),
        report.sessions_cleaned.len(),
        report.artifacts_reclaimed.len(),
        report.orphans_removed.len(),
        report.checkpoint_pins_released.len(),
        report.publication_exposures_expired.len(),
        human_bytes(report.reclaimed_bytes),
    );
    for failure in &report.failures {
        out!("  retained: {failure}");
    }
    if let Some(action) = &report.recovery_action {
        out!("  recovery: {action}");
    }
}

pub(super) fn run_reclaim(parsed: Parsed) -> Result<(), UsageError> {
    let action = parsed
        .positional
        .first()
        .map(String::as_str)
        .unwrap_or("plan");
    let mut broker = open_broker(true)?;
    let retention_policy = crate::load_retention_policy(broker.main_root())?;
    // This repository's own worktree directory, not the shared container:
    // reclaiming another repository's build output from here would be a
    // surprise, and that repository's broker knows which of its sessions are
    // live while this one does not.
    let Some(root) = broker.worktree_root_plan()?.preferred_root else {
        return Err(UsageError::Message(
            "this repository has no broker worktree root to reclaim from".into(),
        ));
    };
    // Only sessions actually working are protected. An idle or stale session
    // may be one that simply cannot be closed, and those are precisely the
    // worktrees whose build output accumulates.
    let now = now_ms();
    let active: Vec<std::path::PathBuf> = broker
        .agents(now)
        .unwrap_or_default()
        .into_iter()
        .filter(|agent| agent.derived_status == crate::SessionStatus::Active)
        .map(|agent| std::path::PathBuf::from(agent.session.worktree_path.clone()))
        .collect();
    let candidates = crate::scan_reclaim_with_extra_directories(
        &root,
        &active,
        &retention_policy.artefact_directories,
    );
    let digest = crate::reclaim::plan_digest(&root, &candidates);
    let plan = crate::ReclaimPlan {
        digest: digest.clone(),
        root: root.clone(),
        reclaimable_bytes: crate::reclaimable_bytes(&candidates),
        total_bytes: candidates.iter().map(|c| c.bytes).sum(),
        candidates,
    };
    let gib = |bytes: u64| format!("{:.1} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0));
    match action {
        "plan" => {
            if let Err(error) = crate::reclaim::save_snapshot(&root, &plan.digest, &plan.candidates)
            {
                eprintln!(
                    "Warning: cannot save reclaim plan review snapshot; continuing with the \
                     digest-bound plan: {error}"
                );
            }
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&plan)?);
            } else {
                out!(
                    "Reclaim plan {}: {} reclaimable of {} found",
                    plan.digest,
                    gib(plan.reclaimable_bytes),
                    gib(plan.total_bytes)
                );
                for candidate in plan.candidates.iter().take(20) {
                    out!(
                        "  {:>10}  {}{}",
                        gib(candidate.bytes),
                        candidate.path.display(),
                        if candidate.reclaimable {
                            ""
                        } else {
                            "  (active session; kept)"
                        }
                    );
                }
                if plan.reclaimable_bytes == 0 {
                    out!("Nothing to reclaim.");
                } else {
                    out!(
                        "Apply with: aethyme broker reclaim apply --confirm {}",
                        plan.digest
                    );
                }
            }
        }
        "apply" => {
            let confirm = parsed.confirm.as_deref().ok_or_else(|| {
                UsageError::Message("reclaim apply requires --confirm <sha256>".into())
            })?;
            // Re-derived from a fresh scan, so a plan whose decision set moved
            // is refused rather than applied to a different set than was reviewed.
            if confirm != plan.digest {
                let (changes, snapshot_error) = match crate::reclaim::load_snapshot(&root, confirm)
                {
                    Ok(Some((_, reviewed))) => (
                        crate::reclaim::decision_changes(
                            &reviewed,
                            &crate::reclaim::decisions(&plan.candidates),
                        ),
                        None,
                    ),
                    Ok(None) => (Vec::new(), None),
                    Err(error) => (Vec::new(), Some(error.to_string())),
                };
                let detail = if changes.is_empty() {
                    match snapshot_error {
                        Some(error) => format!(
                            "the saved review could not be read ({error}); no decision diff can be established"
                        ),
                        None => {
                            "the saved review is unavailable; no decision diff can be established"
                                .into()
                        }
                    }
                } else {
                    format!("changes since review: {}", capped_join(&changes, 8))
                };
                return Err(UsageError::Message(format!(
                    "confirmation does not match the current plan; re-run `aethyme broker reclaim plan` and review it again (reviewed {}, current {}); {}",
                    confirm, plan.digest, detail
                )));
            }
            let outcome = crate::apply_reclaim(&plan);
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&outcome)?);
            } else {
                out!(
                    "Reclaimed {} from {} director{}",
                    gib(outcome.reclaimed_bytes),
                    outcome.removed.len(),
                    if outcome.removed.len() == 1 {
                        "y"
                    } else {
                        "ies"
                    }
                );
                for skipped in &outcome.skipped {
                    out!("  skipped: {skipped}");
                }
            }
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown reclaim action {other:?}; expected plan or apply"
            )));
        }
    }
    Ok(())
}

/// `broker gc`.
pub(super) fn run_gc(parsed: Parsed) -> Result<(), UsageError> {
    let action = parsed
        .positional
        .first()
        .map(String::as_str)
        .ok_or_else(|| {
            UsageError::Message("gc requires `plan` or `apply --confirm <sha256>`".into())
        })?;
    if parsed.positional.len() != 1 {
        return Err(UsageError::Message(
            "gc accepts exactly one action: `plan` or `apply --confirm <sha256>`".into(),
        ));
    }
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    match action {
        "plan" => {
            if parsed.confirm.is_some() {
                return Err(UsageError::Message(
                    "gc plan does not accept --confirm; review its emitted digest".into(),
                ));
            }
            let plan = broker.gc_plan()?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&plan)?);
            } else {
                render_gc_plan(&plan, parsed.detail);
            }
        }
        "apply" => {
            let confirm = parsed.confirm.as_deref().ok_or_else(|| {
                UsageError::Message("gc apply requires --confirm <sha256>".into())
            })?;
            let report = broker.gc_apply(confirm)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                render_gc_apply(&report);
            }
            if !report.complete {
                return Err(UsageError::Message(report.recovery_action.unwrap_or_else(
                    || {
                        format!(
                            "GC paused; resume with `aethyme broker gc apply --confirm {confirm}`"
                        )
                    },
                )));
            }
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown gc action {other:?}; expected `plan` or `apply`"
            )));
        }
    }
    Ok(())
}

/// `broker storage`.
pub(super) fn run_storage(parsed: Parsed) -> Result<(), UsageError> {
    let action = parsed.positional.first().map(String::as_str);
    if parsed.positional.len() > 1 {
        return Err(UsageError::Message(
            "storage accepts at most one action: `plan` or `apply --confirm <sha256>`".into(),
        ));
    }
    let cwd = std::env::current_dir()
        .map_err(|error| UsageError::Message(format!("cannot resolve cwd: {error}")))?;
    match action {
        None | Some("plan") => {
            if parsed.confirm.is_some() {
                return Err(UsageError::Message(
                    "storage plan does not accept --confirm; review its emitted digest".into(),
                ));
            }
            let plan = crate::storage_plan(&cwd)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&plan)?);
            } else {
                render_storage_plan(&plan, parsed.detail);
            }
        }
        Some("apply") => {
            let confirm = parsed.confirm.as_deref().ok_or_else(|| {
                UsageError::Message("storage apply requires --confirm <sha256>".into())
            })?;
            let report = crate::storage_apply(&cwd, confirm)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                render_storage_apply(&report);
            }
            if !report.complete {
                return Err(UsageError::Message(report.recovery_action.unwrap_or_else(
                    || "review a new plan with `aethyme broker storage plan`".into(),
                )));
            }
        }
        Some(other) => {
            return Err(UsageError::Message(format!(
                "unknown storage action {other:?}; expected `plan` or `apply`"
            )));
        }
    }
    Ok(())
}
