//! `broker gc`, `storage` and `reclaim`: disk the broker can give back.

use super::*;

/// Default number of items any one plan list prints before summarising.
pub(super) const GC_LIST_CAP: usize = 5;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum ReclaimAction {
    Plan,
    Apply { confirm: String },
}

/// Validate the complete reclaim selector before the CLI opens broker state.
/// The no-action form remains an alias for `plan` for compatibility.
pub(super) fn parse_reclaim_action(parsed: &Parsed) -> Result<ReclaimAction, UsageError> {
    match parsed.positional.as_slice() {
        [] => Ok(ReclaimAction::Plan),
        [action] if action.as_str() == "plan" => Ok(ReclaimAction::Plan),
        [action] if action.as_str() == "apply" => {
            let confirm = parsed.confirm.as_deref().ok_or_else(|| {
                UsageError::Message("reclaim apply requires --confirm <sha256>".into())
            })?;
            if !crate::reclaim::is_reclaim_plan_digest(confirm) {
                return Err(UsageError::Message(
                    "reclaim apply --confirm must be a 64-character hexadecimal SHA-256".into(),
                ));
            }
            Ok(ReclaimAction::Apply {
                confirm: confirm.to_string(),
            })
        }
        [action] => Err(UsageError::Message(format!(
            "unknown reclaim action {action:?}; expected plan or apply"
        ))),
        _ => Err(UsageError::Message(
            "reclaim accepts exactly one action: `plan` or `apply --confirm <sha256>`".into(),
        )),
    }
}

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
        if detail && root.retired_count > 0 {
            out!(
                "    retired: {} worktree(s) removed by finished sessions",
                root.retired_count
            );
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
    for group in &plan.recovery_archives {
        out!(
            "  recovery archives: {} ({} archive(s), {} by the broker, {}{}; `aethyme broker gc plan` in the owning repository proposes expired broker archives)",
            group.path.display(),
            group.archive_count,
            group.broker_archive_count,
            group
                .estimated_bytes
                .map(human_bytes)
                .unwrap_or_else(|| "unknown bytes".into()),
            group
                .oldest_created_at_ms
                .map(|oldest| format!(
                    ", oldest {} days",
                    plan.evaluated_at.saturating_sub(oldest) / 86_400_000
                ))
                .unwrap_or_default(),
        );
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
            "  apply: aethyme broker gc storage apply --confirm {}",
            plan.digest
        );
    }
}

pub(super) fn render_storage_attribution(report: &crate::StorageAttributionReport) {
    let attributable = report.roots.iter().filter(|root| root.attributable).count();
    out!(
        "Unmarked worktree roots under {}: {} ({} provably {})",
        report.storage_root.display(),
        report.roots.len(),
        attributable,
        report.repository_key
    );
    for root in &report.roots {
        let state = match (root.attributable, root.marked) {
            (true, true) => "marked",
            (true, false) if report.applied => "unchanged (a marker appeared meanwhile)",
            (true, false) => "attributable",
            (false, _) => "not attributable",
        };
        out!("  {state}: {}", root.root.display());
        out!("      {}", root.reason);
    }
    if !report.applied && attributable > 0 {
        out!(
            "  next: aethyme broker gc storage attribute --apply  (writes {attributable} ownership {}; removes nothing)",
            crate::broker::plural_word(attributable, "marker", "markers")
        );
    } else if report.applied && report.roots.iter().any(|root| root.marked) {
        out!("  next: aethyme broker gc storage plan  (review what the newly owned roots hold)");
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

pub(super) fn render_gc_plan(plan: &crate::GcPlan, detail: bool, full_scan: bool) {
    let digest_status = if plan.digest.is_empty() {
        "incomplete; no apply digest"
    } else {
        plan.digest.as_str()
    };
    out!(
        "GC plan {}: {} rows, {} files, {} represented worktrees, {} build caches, {} gate cache entries, {} orphaned roots, {} reclaimable",
        digest_status,
        plan.rows.len(),
        plan.files.len(),
        plan.worktrees.len(),
        plan.artifacts.len(),
        plan.gate_caches.len(),
        plan.orphans.len(),
        human_bytes(plan.estimated_reclaimable_bytes),
    );
    out!(
        "  build output reclaimable: {} (finished sessions' build caches and gate cache entries)",
        human_bytes(plan.estimated_build_output_reclaimable_bytes),
    );
    // Printed whether or not anything is proposed: #295 was an operator
    // reading a near-zero total as "the disk is unreclaimable" while the gate
    // cache was the largest thing on it and simply not listed.
    if let Some(cache) = &plan.gate_cache {
        out!(
            "  gate cache: {} in {} {} at {}; {} kept (active), {} held by running gates, {} reclaimable; budget for older entries {}",
            human_bytes(cache.total_bytes),
            cache.entries.len(),
            crate::broker::plural_word(cache.entries.len(), "entry", "entries"),
            cache.root,
            human_bytes(cache.active_bytes),
            human_bytes(cache.held_bytes),
            human_bytes(cache.reclaimable_bytes),
            human_bytes(cache.budget_bytes),
        );
        if cache.include_active {
            out!(
                "    --include-active-gate-cache: the most recently used cache of each kind is proposed; the next gate will rebuild from scratch"
            );
        } else if cache.active_bytes > 0 {
            out!(
                "    the active cache is kept because the next gate reuses it; `gc plan --include-active-gate-cache` proposes it too (the next gate will rebuild from scratch)"
            );
        }
        for holder in &cache.holders {
            out!("    held: {holder}");
        }
        render_capped(&cache.entries, GC_LIST_CAP, detail, |entry| {
            out!(
                "    {} {} ({}, {}) — {}",
                match entry.disposition {
                    crate::GcGateCacheDisposition::Reclaimable => "reclaimable",
                    crate::GcGateCacheDisposition::Held => "held",
                    crate::GcGateCacheDisposition::Active => "kept (active)",
                    crate::GcGateCacheDisposition::WithinBudget => "kept",
                    crate::GcGateCacheDisposition::Unmeasured => "unmeasured",
                },
                entry.entry,
                entry
                    .estimated_bytes
                    .map(human_bytes)
                    .unwrap_or_else(|| "unknown bytes".into()),
                entry
                    .age_days
                    .map(|days| format!("used {days}d ago"))
                    .unwrap_or_else(|| "last use unknown".into()),
                entry.reason,
            );
        });
    }
    // The host environment build cache, this repository's part (#1275).
    if let Some(cache) = &plan.environment_cache {
        if cache.complete {
            if let (
                Some(build_bytes),
                Some(build_entries),
                Some(in_use_bytes),
                Some(reclaimable_bytes),
                Some(template_entries),
                Some(host_total_bytes),
            ) = (
                cache.build_bytes,
                cache.build_entries,
                cache.in_use_bytes,
                cache.reclaimable_bytes,
                cache.template_entries,
                cache.host_total_bytes,
            ) {
                out!(
                    "  environment cache: {} in {} {} at {}; {} in use, {} reclaimable; {} {}; budget {} for the whole cache ({} used)",
                    human_bytes(build_bytes),
                    build_entries,
                    crate::broker::plural_word(build_entries, "build", "builds"),
                    cache.root,
                    human_bytes(in_use_bytes),
                    human_bytes(reclaimable_bytes),
                    template_entries,
                    crate::broker::plural_word(template_entries, "template", "templates"),
                    human_bytes(cache.budget_bytes),
                    human_bytes(host_total_bytes),
                );
            } else {
                out!(
                    "  environment cache: inventory status is invalid at {}",
                    cache.root
                );
            }
        } else {
            out!(
                "  environment cache: incomplete inventory at {}; totals withheld (budget {})",
                cache.root,
                human_bytes(cache.budget_bytes),
            );
        }
        if let Some(code) = &cache.refusal_code {
            out!("    inventory refusal ({code})");
        }
        if let Some(reason) = &cache.refused {
            out!("    nothing proposed: {reason}");
        }
        render_capped(&plan.environment_caches, GC_LIST_CAP, detail, |entry| {
            out!(
                "    reclaimable {}/{} ({}) — {}",
                entry.component,
                entry.digest,
                human_bytes(entry.estimated_bytes),
                entry.reason,
            );
        });
    }
    for warning in &plan.retention_config_warnings {
        out!("  retention warning: {warning}");
    }
    if !plan.deferred_checks.is_empty() {
        out!("  deferred checks: {}", plan.deferred_checks.join(", "));
        if !full_scan
            && plan
                .deferred_checks
                .iter()
                .any(|check| check == "recovery_archives")
        {
            out!("  recovery archives: nonempty archive inventory is verified only by --full-scan");
        }
        if full_scan {
            out!("  next: resolve the deferred checks and review a new plan");
        } else {
            out!(
                "  next: use `aethyme broker gc reclaim plan` for build caches or repeat with `aethyme broker gc plan --full-scan` for an exhaustive inventory"
            );
        }
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
    // The ordering line is not decoration: `Broker::gc_apply_bounded` drains
    // these lists in the order printed and stops at its deadline, so this says
    // which end of the backlog a bounded apply actually reaches (#176).
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
    render_capped(&plan.recovery_archives, GC_LIST_CAP, detail, |archive| {
        out!(
            "  recovery archive: {} (session {}, {} days old, {}) — {}",
            archive.path,
            archive.session_id,
            plan.evaluated_at.saturating_sub(archive.created_at_ms) / 86_400_000,
            human_bytes(archive.estimated_bytes),
            archive.reason,
        );
    });
    if let Some(inventory) = &plan.recovery_archive_inventory {
        out!(
            "  recovery archives: {} in {} ({}), {} proposed{}",
            inventory.count,
            inventory.root,
            human_bytes(inventory.estimated_bytes),
            plan.recovery_archives.len(),
            inventory
                .oldest_created_at_ms
                .map(|oldest| format!(
                    ", oldest {} days",
                    plan.evaluated_at.saturating_sub(oldest) / 86_400_000
                ))
                .unwrap_or_default(),
        );
        render_capped(&inventory.unowned, GC_LIST_CAP, detail, |unowned| {
            out!("    kept, not the broker's to remove: {unowned}");
        });
    }
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
    if !plan.deferred_checks.is_empty() || plan.digest.is_empty() {
        out!("  apply: unavailable until the plan is complete");
    } else if plan.rows.is_empty()
        && plan.files.is_empty()
        && plan.worktrees.is_empty()
        && plan.artifacts.is_empty()
        && plan.orphans.is_empty()
        && plan.gate_caches.is_empty()
        && plan.environment_caches.is_empty()
        && plan.checkpoint_pin_releases.is_empty()
        && plan.publication_exposure_expiries.is_empty()
        && plan.recovery_archives.is_empty()
    {
        out!("  apply: nothing eligible");
    } else {
        out!(
            "  apply: aethyme broker gc apply --confirm {}{}{}",
            plan.digest,
            if plan
                .gate_cache
                .as_ref()
                .is_some_and(|cache| cache.include_active)
            {
                " --include-active-gate-cache"
            } else {
                ""
            },
            if full_scan { " --full-scan" } else { "" }
        );
    }
}

pub(super) fn render_gc_apply(report: &crate::GcApplyReport) {
    out!(
        "GC apply {}: {} rows, {} files, {} worktrees, {} build caches, {} gate cache entries, {} orphaned roots, {} checkpoint pins released, {} exposures expired, {} reclaimed",
        if report.complete {
            "complete"
        } else {
            "paused"
        },
        report.rows_removed,
        report.files_completed.len(),
        report.sessions_cleaned.len(),
        report.artifacts_reclaimed.len(),
        report.gate_caches_reclaimed.len(),
        report.orphans_removed.len(),
        report.checkpoint_pins_released.len(),
        report.publication_exposures_expired.len(),
        human_bytes(report.reclaimed_bytes),
    );
    for archive in &report.recovery_archives_removed {
        out!("  recovery archive removed: {archive}");
    }
    for entry in &report.environment_caches_reclaimed {
        out!("  environment cache entry evicted: {entry}");
    }
    for failure in &report.failures {
        out!("  retained: {failure}");
    }
    if let Some(action) = &report.recovery_action {
        out!("  recovery: {action}");
    }
}

pub(super) fn run_reclaim(parsed: Parsed, action: ReclaimAction) -> Result<(), UsageError> {
    let mut broker = open_broker(true)?;
    let root = broker.require_bound_repository_worktree_root("worktree cache reclamation")?;
    let retention_policy = crate::load_retention_policy(broker.main_root())?;
    // This repository's own worktree directory, not the shared container:
    // reclaiming another repository's build output from here would be a
    // surprise, and that repository's broker knows which of its sessions are
    // live while this one does not.
    // Only sessions actually working are protected. An idle or stale session
    // may be one that simply cannot be closed, and those are precisely the
    // worktrees whose build output accumulates.
    let now = now_ms();
    // This command opened a read-only snapshot. Use its non-mutating liveness
    // view and fail closed if that inventory cannot be read; a transition
    // write through `agents()` would fail here and must not erase all guards.
    let active: Vec<std::path::PathBuf> = broker
        .agents_snapshot(now)?
        .into_iter()
        .filter(|agent| agent.derived_status == crate::SessionStatus::Active)
        .map(|agent| std::path::PathBuf::from(agent.session.worktree_path.clone()))
        .collect();
    let recorded_sessions: Vec<_> = broker
        .store()
        .live_sessions()?
        .into_iter()
        .chain(broker.store().cleaned_sessions()?)
        .collect();
    // One session's worktree, so a plan can be reviewed and applied without
    // waiting on every other worktree in the root to hold still -- and
    // without walking them: only that worktree is scanned.
    let scope = match parsed.session {
        None => None,
        Some(session_id) => {
            let session = recorded_sessions
                .iter()
                .find(|session| session.id == session_id)
                .ok_or_else(|| {
                    UsageError::Message(format!(
                        "session {session_id} is not a session of this repository"
                    ))
                })?;
            let worktree = std::path::PathBuf::from(&session.worktree_path);
            match crate::reclaim::worktree_in_root(&root, &worktree) {
                Some(base) => Some((base, true)),
                None => {
                    // Preserve an empty scoped plan for a worktree that has
                    // already been removed from this root. An existing
                    // worktree at another root is an unsupported scope, not
                    // a successful scan with zero candidates.
                    let parent_is_bound_root = worktree.parent().is_some_and(|parent| {
                        parent == root.as_path()
                            || match (parent.canonicalize(), root.canonicalize()) {
                                (Ok(parent), Ok(root)) => parent == root,
                                _ => false,
                            }
                    });
                    if !worktree.is_dir() && parent_is_bound_root {
                        eprintln!(
                            "Note: session {session_id}'s worktree {} is no longer present under {}; nothing to reclaim.",
                            worktree.display(),
                            root.display()
                        );
                        Some((worktree, false))
                    } else {
                        return Err(UsageError::Exit {
                            message: format!(
                                "session {session_id}'s recorded worktree {} is not an available direct child of the repository's bound worktree root {}. Session-scoped reclaim supports only worktrees directly under the current bound root; no plan or cleanup was performed. Inspect the binding with `aethyme broker advanced worktree-root --json`; legacy-root reclamation is unsupported.",
                                worktree.display(),
                                root.display()
                            ),
                            code: crate::exit_status::REFUSED,
                        });
                    }
                }
            }
        }
    };
    // An apply credits what each removal freed and never shows the planned
    // sizes, so it skips measuring every file under every candidate.
    let sizing = if matches!(&action, ReclaimAction::Apply { .. }) {
        crate::reclaim::Sizing::Skip
    } else {
        crate::reclaim::Sizing::Measure
    };
    let mut candidates = match &scope {
        Some((_, false)) => Vec::new(),
        Some((worktree, true)) => crate::reclaim::scan_worktree(
            worktree,
            &active,
            &retention_policy.artefact_directories,
            sizing,
        ),
        None => crate::reclaim::scan_root(
            &root,
            &active,
            &retention_policy.artefact_directories,
            sizing,
        ),
    };
    // A worktree no session records has no owner who reviewed it, so its
    // artefacts are reported but kept rather than treated as abandoned.
    let recorded: Vec<std::path::PathBuf> = recorded_sessions
        .into_iter()
        .map(|session| std::path::PathBuf::from(session.worktree_path))
        .collect();
    crate::reclaim::protect_unrecorded_worktrees(&mut candidates, &recorded);
    // Session state cannot see a dev server or a build started from a plain
    // shell, nor someone working without the hook; both keep using the build
    // output a quiet session's row would release. Checked at plan and again
    // at apply, so a worktree that came back into use in between keeps it.
    match crate::reclaim::open_file_snapshot_under(&root) {
        crate::reclaim::OpenFileSnapshot::Complete(open_files) => {
            crate::reclaim::protect_worktrees_with_open_file_uses(&mut candidates, &open_files);
        }
        crate::reclaim::OpenFileSnapshot::Incomplete { reason, observed } => {
            crate::reclaim::protect_worktrees_when_open_file_snapshot_is_incomplete(
                &mut candidates,
                &reason,
                &observed,
            );
            eprintln!(
                "Warning: process-use inspection is incomplete; affected cleanup candidates \
                 are kept for safety: {reason}"
            );
        }
    }
    crate::reclaim::protect_recently_worked_worktrees(
        &mut candidates,
        now,
        crate::reclaim::RECENT_WORK_WINDOW_MS,
    );
    let session_flag = parsed
        .session
        .map(|id| format!(" --session {id}"))
        .unwrap_or_default();
    let scope = scope.map(|(worktree, _)| worktree);
    let digest = crate::reclaim::plan_digest(&root, scope.as_deref(), &candidates);
    let plan = crate::ReclaimPlan {
        digest: digest.clone(),
        root: root.clone(),
        scope: scope.clone(),
        reclaimable_bytes: crate::reclaimable_bytes(&candidates),
        total_bytes: candidates.iter().map(|c| c.bytes).sum(),
        candidates,
    };
    let gib = |bytes: u64| format!("{:.1} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0));
    match action {
        ReclaimAction::Plan => {
            if let Err(error) = crate::reclaim::save_snapshot(
                &root,
                scope.as_deref(),
                &plan.digest,
                &plan.candidates,
            ) {
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
                    let kept = if candidate.reclaimable {
                        String::new()
                    } else {
                        format!("  (kept: {})", candidate.reason)
                    };
                    out!(
                        "  {:>10}  {}{kept}",
                        gib(candidate.bytes),
                        candidate.path.display()
                    );
                }
                if plan.reclaimable_bytes == 0 {
                    out!("Nothing to reclaim.");
                } else {
                    out!(
                        "Apply with: aethyme broker gc reclaim apply --confirm {}{session_flag}",
                        plan.digest
                    );
                }
            }
        }
        ReclaimAction::Apply { confirm } => {
            // The confirmation authorizes what was reviewed. When the fresh
            // scan decides exactly the same, apply it as is. Otherwise the
            // verified saved review names the authorized paths, and each is
            // removed only if the fresh scan still finds it reclaimable; a
            // change anywhere else in the root no longer voids the review.
            let mut not_reviewed = Vec::new();
            let mut withdrawn = Vec::new();
            let plan = if confirm == plan.digest {
                plan
            } else {
                match crate::reclaim::load_snapshot(&root, &confirm) {
                    // A review covers what it scanned: a one-session review
                    // says nothing about the other worktrees, and a
                    // whole-root review was not made about one session.
                    Ok(Some(reviewed)) if reviewed.scope != scope => {
                        let describe = |scope: &Option<std::path::PathBuf>| match scope {
                            Some(worktree) => format!("worktree {}", worktree.display()),
                            None => "the whole worktree root".to_string(),
                        };
                        return Err(UsageError::Message(format!(
                            "confirmation {confirm} reviewed {}, but this apply covers {}; apply it with the same --session it was planned with, or re-run `aethyme broker gc reclaim plan{session_flag}` and review that",
                            describe(&reviewed.scope),
                            describe(&scope)
                        )));
                    }
                    Ok(Some(reviewed)) => {
                        let narrowed = crate::reclaim::restrict_to_review(
                            &plan.candidates,
                            &reviewed.decisions,
                        );
                        not_reviewed = narrowed.not_reviewed;
                        withdrawn = narrowed.withdrawn;
                        crate::ReclaimPlan {
                            digest: confirm.to_string(),
                            root: plan.root,
                            scope: plan.scope,
                            reclaimable_bytes: crate::reclaimable_bytes(&narrowed.candidates),
                            total_bytes: plan.total_bytes,
                            candidates: narrowed.candidates,
                        }
                    }
                    // Without a verified review there is nothing to narrow
                    // to, so only an exact match authorizes deletion.
                    unreadable => {
                        let detail = match unreadable {
                            Err(error) => format!(
                                "the saved review could not be read ({error}); no reviewed decision set can be established"
                            ),
                            _ => "the saved review is unavailable; no reviewed decision set can be established".into(),
                        };
                        return Err(UsageError::Message(format!(
                            "confirmation does not match the current plan; re-run `aethyme broker gc reclaim plan{session_flag}` and review it again (reviewed {}, current {}); {}",
                            confirm, plan.digest, detail
                        )));
                    }
                }
            };
            let apply_root = broker
                .require_bound_repository_worktree_root("worktree cache reclamation apply")?;
            if apply_root != root {
                return Err(UsageError::Message(
                    "the bound worktree root changed while preparing this reclaim; re-run the plan and review it again".into(),
                ));
            }
            let mut outcome = crate::apply_reclaim(&plan);
            withdrawn.append(&mut outcome.skipped);
            outcome.skipped = withdrawn;
            outcome.not_reviewed = not_reviewed;
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
                for path in &outcome.not_reviewed {
                    out!(
                        "  not reviewed, left in place: {} (re-run `aethyme broker gc reclaim plan{session_flag}` to include it)",
                        path.display()
                    );
                }
            }
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
            UsageError::Message(
                "gc requires `plan`, `apply --confirm <sha256>`, `abandon --confirm <sha256>`, `forget-abandoned --confirm <sha256>`, `artifact-use reconcile plan <session-id>`, `artifact-use reconcile apply <session-id> --confirm <sha256>`, or `sweep`".into(),
            )
        })?;
    if action == "service" {
        return run_gc_sweep_service(parsed);
    }
    if action == "__continue-artifacts" {
        return run_artifact_sweep_continuation_worker(parsed);
    }
    if action == "artifact-use" {
        return run_gc_artifact_use_reconcile(parsed);
    }
    if parsed.positional.len() != 1 {
        return Err(UsageError::Message(
            "gc accepts one action: `plan`, `apply --confirm <sha256>`, `abandon --confirm <sha256>`, `forget-abandoned --confirm <sha256>`, `artifact-use reconcile <plan|apply> <session-id>`, or `sweep`".into(),
        ));
    }
    if matches!(action, "abandon" | "forget-abandoned") && parsed.include_active_gate_cache {
        return Err(UsageError::Message(format!(
            "gc {action} does not accept --include-active-gate-cache"
        )));
    }
    if matches!(action, "abandon" | "forget-abandoned") {
        if parsed.read_only_snapshot {
            return Err(UsageError::Message(format!(
                "gc {action} changes the repository journal and does not accept --read-only-snapshot"
            )));
        }
        if parsed.confirm.is_none() {
            let required = if action == "abandon" {
                "pending-digest"
            } else {
                "archived-plan-digest"
            };
            return Err(UsageError::Message(format!(
                "gc {action} requires --confirm <{required}>"
            )));
        }
    }
    #[cfg(target_os = "macos")]
    let managed_tick = {
        if action != "sweep" && crate::gc_sweep_service::managed_tick_requested() {
            return Err(UsageError::Message(
                "managed GC service markers are valid only for the fixed `gc sweep --json` tick"
                    .into(),
            ));
        }
        if action == "sweep" {
            preflight_managed_gc_tick(&parsed)?
        } else {
            None
        }
    };
    #[cfg(not(target_os = "macos"))]
    {
        let requested = std::env::var_os("AETHYME_GC_SWEEP_SERVICE_UUID").is_some()
            || std::env::var_os("AETHYME_GC_SWEEP_SERVICE_GENERATION").is_some();
        if requested {
            return Err(UsageError::Message(
                "managed GC service ticks are supported only on macOS".into(),
            ));
        }
    }
    // A normal Broker::open resumes confirmed GC maintenance at startup. The
    // journal-recovery commands must operate on the pending journal before
    // any startup maintenance can consume it, so use its read-only DB snapshot
    // while the command itself takes the repository GC lock for file changes.
    let broker_result = if matches!(action, "abandon" | "forget-abandoned") {
        open_broker(true)
    } else {
        open_broker(parsed.read_only_snapshot)
    };
    let mut broker = match broker_result {
        Ok(broker) => broker,
        Err(error) => {
            #[cfg(target_os = "macos")]
            if let Some(tick) = managed_tick.as_ref() {
                crate::gc_sweep_service::record_managed_tick(
                    tick,
                    None,
                    Some(&format!("broker open failed: {error:?}")),
                )?;
            }
            return Err(error);
        }
    };
    match action {
        "plan" => {
            if parsed.confirm.is_some() {
                return Err(UsageError::Message(
                    "gc plan does not accept --confirm; review its emitted digest".into(),
                ));
            }
            let plan =
                broker.gc_plan_for_cli(parsed.include_active_gate_cache, parsed.full_scan)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&plan)?);
            } else {
                render_gc_plan(&plan, parsed.detail, parsed.full_scan);
            }
        }
        "apply" => {
            let confirm = parsed.confirm.as_deref().ok_or_else(|| {
                UsageError::Message("gc apply requires --confirm <sha256>".into())
            })?;
            let report = broker.gc_apply_for_cli(
                confirm,
                parsed.include_active_gate_cache,
                parsed.full_scan,
            )?;
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
        "abandon" => {
            let confirm = parsed.confirm.as_deref().ok_or_else(|| {
                UsageError::Message("gc abandon requires --confirm <pending-digest>".into())
            })?;
            let report = broker.gc_abandon_pending(confirm)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                out!(
                    "Archived the pending GC journal {} ({} bytes) at {}.",
                    report.digest,
                    report.bytes_archived,
                    report.archive_path
                );
                out!("  no retained rows, files, worktrees or artifacts were removed");
                out!("  next: review a fresh plan with `aethyme broker gc plan`");
            }
        }
        "forget-abandoned" => {
            let confirm = parsed.confirm.as_deref().ok_or_else(|| {
                UsageError::Message(
                    "gc forget-abandoned requires --confirm <archived-plan-digest>".into(),
                )
            })?;
            let report = broker.gc_forget_abandoned(confirm)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                out!(
                    "Forgot GC archive or staging files for {} ({} bytes). No retained data was removed.",
                    report.digest,
                    report.bytes_forgotten
                );
            }
        }
        "sweep" => {
            let sweep = broker.gc_sweep_artifacts();
            #[cfg(target_os = "macos")]
            if let Err(error) = &sweep {
                if let Some(tick) = managed_tick.as_ref() {
                    crate::gc_sweep_service::record_managed_tick(tick, None, Some(&error.to_string()))
                        .map_err(|receipt_error| {
                            UsageError::Message(format!(
                                "GC sweep failed ({error}); writing its managed tick receipt failed: {receipt_error}"
                            ))
                        })?;
                }
            }
            let report = sweep?;
            #[cfg(target_os = "macos")]
            if let Some(tick) = managed_tick.as_ref() {
                crate::gc_sweep_service::record_managed_tick(tick, Some(&report), None)?;
            }
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                render_gc_sweep(&report);
            }
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown gc action {other:?}; expected `plan`, `apply`, `abandon`, `forget-abandoned` or `sweep`"
            )));
        }
    }
    Ok(())
}

fn run_gc_artifact_use_reconcile(parsed: Parsed) -> Result<(), UsageError> {
    if parsed.positional.len() != 4 || parsed.positional[1] != "reconcile" {
        return Err(UsageError::Message(
            "usage: aethyme broker gc artifact-use reconcile <plan|apply> <exact-session-id> [--confirm <sha256>] [--json]".into(),
        ));
    }
    let action = parsed.positional[2].as_str();
    let session_id = parsed.positional[3]
        .parse::<i64>()
        .ok()
        .filter(|session_id| *session_id > 0)
        .ok_or_else(|| {
            UsageError::Message(
                "artifact-use reconciliation requires a positive exact session id".into(),
            )
        })?;
    let confirmed_digest = match action {
        "plan" if parsed.confirm.is_none() => None,
        "plan" => {
            return Err(UsageError::Message(
                "artifact-use reconciliation plan does not accept --confirm".into(),
            ));
        }
        "apply" => {
            let confirm = parsed.confirm.as_deref().ok_or_else(|| {
                UsageError::Message(
                    "artifact-use reconciliation apply requires --confirm <sha256>".into(),
                )
            })?;
            if !crate::reclaim::is_reclaim_plan_digest(confirm) {
                return Err(UsageError::Message(
                    "artifact-use reconciliation --confirm must be a 64-character hexadecimal SHA-256".into(),
                ));
            }
            Some(confirm.to_owned())
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown artifact-use reconciliation action {other:?}; expected plan or apply"
            )));
        }
    };

    let mut broker = open_broker(true)?;
    let _lifecycle_claim =
        if action == "apply" {
            Some(broker.try_claim_worktree_lifecycle()?.ok_or_else(|| {
                UsageError::Message("worktree lifecycle is busy; retry later".into())
            })?)
        } else {
            None
        };
    let session = broker.store().session(session_id)?;
    let checkout_root = PathBuf::from(&session.worktree_path);
    let cohort = broker
        .closed_artifact_checkout_cohort_for_session_readonly(
            session_id,
            &checkout_root,
            None,
        )?
        .ok_or_else(|| {
            UsageError::Message(
                "the selected session does not have a complete, closed checkout cohort; no markers were changed".into(),
            )
        })?;

    let cwd = std::env::current_dir()
        .map_err(|error| UsageError::Message(format!("cannot resolve cwd: {error}")))?;
    let repository = crate::GitRepo::discover(&cwd)?;
    let primary_root = repository.main_root()?;
    let git_common_dir = repository.git_common_dir()?;
    let repository_key = crate::host_state::repository_key(&primary_root, Some(&git_common_dir));
    let host_database = broker.host_operation_database_path()?;
    let registry = crate::worktree_registry::artifact_use_primary_root_witness_guard(
        &host_database,
        &primary_root,
        &repository_key,
    )?;

    match action {
        "plan" => {
            let plan = crate::file_lock::artifact_use::plan_artifact_use_marker_reconciliation(
                &primary_root,
                &checkout_root,
                session_id,
                &cohort,
                &registry,
            )?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&plan)?);
            } else {
                render_artifact_use_reconcile_plan(&plan);
            }
        }
        "apply" => {
            let report = crate::file_lock::artifact_use::apply_artifact_use_marker_reconciliation(
                &primary_root,
                &checkout_root,
                session_id,
                &cohort,
                &registry,
                confirmed_digest.as_deref().expect("validated above"),
            )?;
            let stopped = report.stopped.as_ref().map(|stop| {
                format!(
                    "artifact-use reconciliation stopped at {} ({:?}): {}",
                    stop.marker_filename, stop.kind, stop.reason
                )
            });
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                render_artifact_use_reconcile_report(&report);
            }
            if let Some(message) = stopped {
                return Err(UsageError::Message(format!(
                    "{message}; per-marker outcomes were emitted above"
                )));
            }
        }
        _ => unreachable!("action was validated above"),
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn run_gc_sweep_service(parsed: Parsed) -> Result<(), UsageError> {
    if parsed.read_only_snapshot {
        return Err(UsageError::Message(
            "GC service controls do not accept --read-only-snapshot".into(),
        ));
    }
    if parsed.positional.len() != 2 {
        return Err(UsageError::Message(
            "gc service accepts exactly one action: plan, install, status, activate, deactivate, or uninstall".into(),
        ));
    }
    let action = parsed.positional[1].as_str();
    match action {
        "plan" => {
            if parsed.confirm.is_some() {
                return Err(UsageError::Message(
                    "gc service plan does not accept --confirm".into(),
                ));
            }
            let plan = crate::gc_sweep_service::service_plan()?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&plan)?);
            } else {
                out!(
                    "GC sweep service plan {} (generation {})",
                    plan.digest,
                    plan.generation
                );
                out!(
                    "  repository: {} ({})",
                    plan.repository.display(),
                    plan.repository_uuid
                );
                out!(
                    "  immutable runtime: {} + {} ({}, {})",
                    plan.router.display(),
                    plan.engine.display(),
                    plan.runtime_version,
                    plan.runtime_build_identity
                );
                out!("  cadence: every {} seconds", plan.interval_seconds);
                out!(
                    "  unit: {} (install remains inactive)",
                    plan.unit_path.display()
                );
                out!(
                    "  next: aethyme broker gc service install --confirm {}",
                    plan.digest
                );
            }
        }
        "install" => {
            let confirm = service_confirmation(&parsed)?;
            let plan = crate::gc_sweep_service::service_plan()?;
            let installed = crate::gc_sweep_service::install(&plan, confirm)?;
            let result = serde_json::json!({
                "schema": "aethyme.gc-sweep-service-install/v1",
                "action": "install",
                "plan_digest": plan.digest,
                "generation": plan.generation,
                "created_paths": installed,
                "activated": false
            });
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                out!(
                    "Installed the exact GC sweep service files; scheduler activation was not performed."
                );
                out!("  plan: {}", plan.digest);
                for path in installed {
                    out!("  created: {}", path.display());
                }
            }
        }
        "status" | "activate" | "deactivate" | "uninstall" => {
            let confirmation = if action == "status" {
                if parsed.confirm.is_some() {
                    return Err(UsageError::Message(
                        "gc service status does not accept --confirm".into(),
                    ));
                }
                None
            } else {
                Some(service_confirmation(&parsed)?)
            };
            let home = crate::gc_sweep_service::control_home()?;
            let uuid = crate::gc_sweep_service::uuid_for_current_repository()?;
            let result = crate::gc_sweep_service::control(&home, &uuid, action, confirmation)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                out!("GC sweep service {action}: {}", result);
            }
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown GC service action {other:?}; expected plan, install, status, activate, deactivate, or uninstall"
            )));
        }
    }
    Ok(())
}

fn render_artifact_use_reconcile_plan(
    plan: &crate::file_lock::artifact_use::ArtifactUseReconcilePlan,
) {
    out!(
        "Artifact-use marker reconciliation plan {} for session {}:",
        plan.digest,
        plan.selected_session_id
    );
    out!("  checkout: {}", plan.checkout_root.display());
    out!(
        "  closed aliases: {}",
        plan.cohort
            .session_ids()
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    );
    for marker in &plan.markers {
        match &marker.disposition {
            crate::file_lock::artifact_use::ArtifactUseReconcileDisposition::RemoveIfConfirmed => {
                out!(
                    "  candidate: {} (session {}, bytes SHA-256 {})",
                    marker.filename,
                    marker.session_id,
                    marker.bytes_sha256
                );
            }
            crate::file_lock::artifact_use::ArtifactUseReconcileDisposition::Retain { reason } => {
                out!("  retained: {} ({reason})", marker.filename);
            }
            crate::file_lock::artifact_use::ArtifactUseReconcileDisposition::Unknown { reason } => {
                out!("  unknown: {} ({reason})", marker.filename);
            }
        }
    }
    out!(
        "  apply with: aethyme broker gc artifact-use reconcile apply {} --confirm {}",
        plan.selected_session_id,
        plan.digest
    );
}

fn render_artifact_use_reconcile_report(
    report: &crate::file_lock::artifact_use::ArtifactUseReconcileReport,
) {
    out!(
        "Artifact-use reconciliation apply for plan {}:",
        report.plan_digest
    );
    for outcome in &report.outcomes {
        match &outcome.outcome {
            crate::file_lock::artifact_use::ArtifactUseReconcileOutcomeKind::Removed {
                directory_sync:
                    crate::file_lock::artifact_use::ArtifactUseReconcileDirectorySync::Synced,
            } => out!("  removed: {} (directory sync confirmed)", outcome.filename),
            crate::file_lock::artifact_use::ArtifactUseReconcileOutcomeKind::Removed {
                directory_sync:
                    crate::file_lock::artifact_use::ArtifactUseReconcileDirectorySync::Unconfirmed,
            } => out!(
                "  removed: {} (directory sync unconfirmed)",
                outcome.filename
            ),
            crate::file_lock::artifact_use::ArtifactUseReconcileOutcomeKind::Retained {
                reason,
            } => {
                out!("  retained: {} ({reason})", outcome.filename);
            }
            crate::file_lock::artifact_use::ArtifactUseReconcileOutcomeKind::Unknown { reason } => {
                out!("  unknown: {} ({reason})", outcome.filename);
            }
        }
    }
    if let Some(stop) = &report.stopped {
        out!(
            "  stopped at {} ({:?}): {}",
            stop.marker_filename,
            stop.kind,
            stop.reason
        );
    }
    out!("  checkout and broker records were not removed");
}

#[cfg(not(target_os = "macos"))]
fn run_gc_sweep_service(_parsed: Parsed) -> Result<(), UsageError> {
    Err(UsageError::Message(
        "per-repository scheduled GC services are supported only on macOS".into(),
    ))
}

#[cfg(any(target_os = "macos", test))]
pub(super) fn service_confirmation(parsed: &Parsed) -> Result<&str, UsageError> {
    let confirm = parsed.confirm.as_deref().ok_or_else(|| {
        UsageError::Message("GC service state changes require --confirm <plan-sha256>".into())
    })?;
    if confirm.len() != 64
        || !confirm
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(UsageError::Message(
            "GC service --confirm must be a 64-character lowercase SHA-256".into(),
        ));
    }
    Ok(confirm)
}

#[cfg(target_os = "macos")]
fn preflight_managed_gc_tick(
    parsed: &Parsed,
) -> Result<Option<crate::gc_sweep_service::ManagedTick>, UsageError> {
    if !crate::gc_sweep_service::managed_tick_requested() {
        return Ok(None);
    }
    if parsed.positional.as_slice() != ["sweep"]
        || !parsed.json
        || parsed.read_only_snapshot
        || parsed.confirm.is_some()
        || parsed.include_active_gate_cache
        || parsed.full_scan
        || parsed.detail
    {
        return Err(UsageError::Message(
            "managed GC service accepts only the fixed `broker gc sweep --json` invocation".into(),
        ));
    }
    let cwd = std::env::current_dir()
        .map_err(|error| UsageError::Message(format!("cannot resolve cwd: {error}")))?;
    let snapshot = Broker::open_snapshot(&cwd)?;
    crate::gc_sweep_service::preflight_managed_tick(&snapshot)
        .map_err(|error| UsageError::Message(format!("managed GC tick preflight refused: {error}")))
}

/// Private detached-worker entry point. It is absent from help and refuses to
/// open a broker unless the exact installed worker environment, repository,
/// runtime pair, and singleton lock all agree.
fn run_artifact_sweep_continuation_worker(parsed: Parsed) -> Result<(), UsageError> {
    validate_artifact_sweep_worker_invocation(
        std::env::var(crate::gc::ARTIFACT_SWEEP_WORKER_ENV)
            .ok()
            .as_deref(),
        parsed.read_only_snapshot,
        parsed.positional.len(),
        artifact_worker_test_or_build_environment(),
    )?;
    let requested_root = std::fs::canonicalize(&parsed.positional[1]).map_err(|error| {
        UsageError::Message(format!(
            "cannot resolve automatic artifact continuation repository: {error}"
        ))
    })?;
    let discovered = crate::GitRepo::discover(&requested_root)?;
    let repository_root = std::fs::canonicalize(discovered.main_root()?)?;
    if repository_root != requested_root {
        return Err(UsageError::Message(
            "automatic artifact continuation repository does not match its main checkout".into(),
        ));
    }
    if crate::host_state::path_is_ephemeral(&repository_root)
        || !crate::gc::artifact_worker_runtime_is_aligned()
    {
        return Err(UsageError::Message(
            "automatic artifact continuation runtime or repository is not eligible".into(),
        ));
    }

    // Hold the OS lock before Broker::open. The parent drops it only after
    // this process has been spawned, so no second detached worker can enter.
    let mut worker_lock = None;
    for _ in 0..40 {
        if let Some(lock) = crate::gc::try_artifact_sweep_worker_lock(&repository_root)? {
            worker_lock = Some(lock);
            break;
        }
        // The launcher still owns the lock while the child starts. Wait for
        // that handoff, but do not queue another worker behind an active one.
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let Some(_worker_lock) = worker_lock else {
        return Err(UsageError::Message(
            "automatic artifact continuation worker is already active".into(),
        ));
    };
    let mut broker = Broker::open(&repository_root)?;
    broker.run_artifact_sweep_recovery_worker()?;
    Ok(())
}

fn validate_artifact_sweep_worker_invocation(
    worker_environment: Option<&str>,
    read_only_snapshot: bool,
    positional_count: usize,
    test_or_build_environment: bool,
) -> Result<(), UsageError> {
    if worker_environment != Some("1") {
        return Err(UsageError::Message(
            "automatic artifact continuation is an internal worker command".into(),
        ));
    }
    if read_only_snapshot || positional_count != 2 {
        return Err(UsageError::Message(
            "automatic artifact continuation requires one repository path and writable broker state".into(),
        ));
    }
    if test_or_build_environment {
        return Err(UsageError::Message(
            "automatic artifact continuation cannot run from a test or build environment".into(),
        ));
    }
    Ok(())
}

const ARTIFACT_WORKER_TEST_ENVIRONMENT_KEYS: &[&str] = &[
    crate::BROKER_DB_ENV,
    "AETHYME_TEST_DB_SUFFIX",
    crate::disk_headroom::TEST_AVAILABLE_BYTES_ENV,
    crate::disk_headroom::TEST_AVAILABLE_INODES_ENV,
    "CARGO_MANIFEST_DIR",
    "CARGO_TARGET_DIR",
    "CARGO_BIN_EXE_aethyme",
];

fn artifact_worker_test_or_build_environment() -> bool {
    cfg!(test) || artifact_worker_environment_has(|key| std::env::var_os(key).is_some())
}

fn artifact_worker_environment_has(mut present: impl FnMut(&str) -> bool) -> bool {
    ARTIFACT_WORKER_TEST_ENVIRONMENT_KEYS
        .iter()
        .any(|key| present(key))
}

fn render_gc_sweep(report: &crate::ArtifactSweepReport) {
    let noun = if report.directories_reclaimed == 1 {
        "directory"
    } else {
        "directories"
    };
    out!(
        "artifact sweep: reclaimed {} build-output {noun} within {} ms{}",
        report.directories_reclaimed,
        report.budget_ms,
        if report.disk_pressured {
            " (disk below the gate headroom: pressured budget)"
        } else {
            ""
        }
    );
    if report.complete {
        out!("  complete: every eligible session was visited");
    } else {
        out!("  paused at the budget; run `aethyme broker gc sweep` again to continue");
    }
    match report.legacy_unscoped_state {
        crate::ArtifactSweepLegacyUnscopedState::Clear => {
            out!(
                "  legacy unscoped artifact-sweep state: clear; this lap covers only the current storage context"
            );
        }
        crate::ArtifactSweepLegacyUnscopedState::UnknownPending => {
            out!(
                "  legacy unscoped artifact-sweep state: unknown and still pending; this lap covers only the current storage context"
            );
        }
    }
}

/// `broker storage`.
pub(super) fn run_storage(parsed: Parsed) -> Result<(), UsageError> {
    if parsed.positional.first().map(String::as_str) == Some("identity") {
        return run_storage_identity(parsed);
    }
    let action = parsed.positional.first().map(String::as_str);
    if parsed.positional.len() > 1 {
        return Err(UsageError::Message(
            "storage accepts at most one action: `plan`, `apply --confirm <sha256>` or `attribute [--apply]`".into(),
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
                    || "review a new plan with `aethyme broker gc storage plan`".into(),
                )));
            }
        }
        Some("attribute") => {
            if parsed.confirm.is_some() {
                return Err(UsageError::Message(
                    "storage attribute does not take --confirm; it writes only ownership markers it can prove, with --apply".into(),
                ));
            }
            let report = crate::storage_attribute(&cwd, parsed.apply)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                render_storage_attribution(&report);
            }
        }
        Some(other) => {
            return Err(UsageError::Message(format!(
                "unknown storage action {other:?}; expected `plan`, `apply` or `attribute`"
            )));
        }
    }
    Ok(())
}

fn render_root_identity_recovery(plan: &crate::worktree_registry::RootIdentityRecoveryPlan) {
    out!(
        "Worktree root identity recovery {}",
        if plan.applied { "applied" } else { "plan" }
    );
    out!(
        "  host operation database: {}",
        plan.host_operation_database.display()
    );
    out!("  exact root: {}", plan.root.display());
    out!(
        "  repository owner: {} ({})",
        plan.repository_root.display(),
        plan.repository_key
    );
    out!(
        "  root inode / current volume UUID: {} / {}",
        plan.root_inode,
        plan.root_volume_uuid_hex
    );
    out!(
        "  origin inode / current volume UUID: {} / {}",
        plan.origin_inode,
        plan.origin_volume_uuid_hex
    );
    out!(
        "  historical UUID proof: none; review the existing inode, owner marker, and repository binding"
    );
    out!("  old payload SHA-256: {}", plan.old_payload_sha256);
    out!("  new payload SHA-256: {}", plan.new_payload_sha256);
    out!("  owner marker bytes SHA-256: {}", plan.owner_marker_sha256);
    out!("  confirmation: {}", plan.confirmation);
}

fn run_storage_identity(parsed: Parsed) -> Result<(), UsageError> {
    if parsed.positional.len() != 4 {
        return Err(UsageError::Message(
            "usage: aethyme broker gc storage identity <plan|apply> <host-operation-db> <exact-root> [--confirm <old-sha256:new-sha256:owner-marker-sha256>] [--json]".into(),
        ));
    }
    let action = parsed.positional[1].as_str();
    let host_database = Path::new(&parsed.positional[2]);
    let root = Path::new(&parsed.positional[3]);
    let cwd = std::env::current_dir()
        .map_err(|error| UsageError::Message(format!("cannot resolve cwd: {error}")))?;
    let repository = crate::GitRepo::discover(&cwd)?;
    let repository_root = repository.main_root()?;
    let git_common_dir = repository.git_common_dir()?;
    let repository_key = crate::host_state::repository_key(&repository_root, Some(&git_common_dir));

    match action {
        "plan" => {
            if parsed.confirm.is_some() {
                return Err(UsageError::Message(
                    "storage identity plan does not accept --confirm".into(),
                ));
            }
            let plan = crate::worktree_registry::plan_root_identity_recovery(
                host_database,
                root,
                &repository_root,
                &repository_key,
            )?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&plan)?);
            } else {
                render_root_identity_recovery(&plan);
            }
        }
        "apply" => {
            let confirm = parsed.confirm.as_deref().ok_or_else(|| {
                UsageError::Message(
                    "storage identity apply requires --confirm <old-sha256:new-sha256:owner-marker-sha256>".into(),
                )
            })?;
            let report = crate::worktree_registry::apply_root_identity_recovery(
                host_database,
                root,
                &repository_root,
                &repository_key,
                confirm,
            )?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                render_root_identity_recovery(&report);
            }
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown storage identity action {other:?}; expected `plan` or `apply`"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod artifact_worker_tests {
    use super::*;

    fn worker_environment_fixture() -> (tempfile::TempDir, crate::test_storage::TestStorage, Broker)
    {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        std::fs::write(root.join("README.md"), "worker environment fixture\n").unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["add", "README.md"],
            vec![
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-qm",
                "fixture",
            ],
        ] {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let storage = crate::test_storage::TestStorage::new();
        let broker = storage.open(&root).unwrap();
        (owner, storage, broker)
    }

    #[test]
    fn produced_worker_environment_passes_consumer_without_test_database_override() {
        let (_owner, _storage, mut broker) = worker_environment_fixture();
        let root = broker.main_root().to_path_buf();
        let database = broker.store().db_path().to_path_buf();
        let gate_scope = crate::gate_database::for_child(&root, &database, None).unwrap();
        for scope in [None, Some(std::ffi::OsStr::new(&gate_scope))] {
            let command = broker
                .artifact_sweep_worker_command(std::path::Path::new("unused-router"), &root, scope)
                .unwrap();
            let environment: std::collections::BTreeMap<_, _> = command
                .get_envs()
                .map(|(key, value)| (key.to_os_string(), value.map(std::ffi::OsStr::to_os_string)))
                .collect();
            assert_eq!(
                environment.get(std::ffi::OsStr::new(crate::BROKER_DB_ENV)),
                Some(&None)
            );
            assert!(
                environment
                    .get(std::ffi::OsStr::new("AETHYME_BROKER_STORAGE_CONTEXT_V1"))
                    .is_some_and(Option::is_some)
            );
            assert_eq!(
                environment
                    .get(std::ffi::OsStr::new(crate::gc::ARTIFACT_SWEEP_WORKER_ENV))
                    .and_then(Option::as_deref),
                Some(std::ffi::OsStr::new("1"))
            );
            assert_eq!(
                environment.get(std::ffi::OsStr::new(crate::gate_database::SCOPE_ENV)),
                Some(&scope.map(std::ffi::OsStr::to_os_string)),
                "the emitted scope must be the exact snapshot used for the database proof"
            );
            let forbidden = artifact_worker_environment_has(|key| {
                environment
                    .get(std::ffi::OsStr::new(key))
                    .is_some_and(Option::is_some)
            });
            assert!(
                validate_artifact_sweep_worker_invocation(Some("1"), false, 2, forbidden).is_ok()
            );
            assert_eq!(
                crate::gate_database::resolve(&root, None, scope).unwrap(),
                database,
                "removing the pin must preserve the exact database the child opens"
            );
            // Test the produced environment through the real consumer predicate
            // without inheriting the Cargo runner's own ambient test markers.
            for marker in ARTIFACT_WORKER_TEST_ENVIRONMENT_KEYS {
                assert!(
                    validate_artifact_sweep_worker_invocation(
                        Some("1"),
                        false,
                        2,
                        artifact_worker_environment_has(|key| key == *marker
                            || environment
                                .get(std::ffi::OsStr::new(key))
                                .is_some_and(Option::is_some)),
                    )
                    .is_err(),
                    "test marker {marker} must still refuse"
                );
            }
        }
    }

    #[test]
    fn worker_command_refuses_database_resolution_mismatch_or_invalid_gate_scope() {
        let (_owner, storage, mut broker) = worker_environment_fixture();
        let root = broker.main_root().to_path_buf();
        let other_database = storage.root().join("different-worker.db");
        let different_scope =
            crate::gate_database::for_child(&root, &other_database, None).unwrap();
        for scope in [
            std::ffi::OsStr::new(&different_scope),
            std::ffi::OsStr::new("malformed-scope"),
        ] {
            assert!(
                broker
                    .artifact_sweep_worker_command(
                        std::path::Path::new("unused-router"),
                        &root,
                        Some(scope)
                    )
                    .is_err()
            );
        }
        let other_root = tempfile::tempdir().unwrap();
        assert!(
            broker
                .artifact_sweep_worker_command(
                    std::path::Path::new("unused-router"),
                    other_root.path(),
                    None
                )
                .is_err()
        );
        assert!(broker.store().db_path().is_file());
    }

    #[test]
    fn hidden_worker_cli_rejects_untrusted_invocations_before_repository_open() {
        assert!(validate_artifact_sweep_worker_invocation(None, false, 2, false).is_err());
        assert!(validate_artifact_sweep_worker_invocation(Some("wrong"), false, 2, false).is_err());
        assert!(validate_artifact_sweep_worker_invocation(Some("1"), true, 2, false).is_err());
        assert!(validate_artifact_sweep_worker_invocation(Some("1"), false, 1, false).is_err());
        assert!(validate_artifact_sweep_worker_invocation(Some("1"), false, 2, true).is_err());
        assert!(validate_artifact_sweep_worker_invocation(Some("1"), false, 2, false).is_ok());
        assert!(artifact_worker_environment_has(
            |key| key == crate::BROKER_DB_ENV
        ));
        assert!(artifact_worker_environment_has(
            |key| key == "AETHYME_TEST_DB_SUFFIX"
        ));
        assert!(!artifact_worker_environment_has(|_| false));
    }
}
