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
        for entry in &checkout.git_storage {
            out!(
                "    Git {}: {} ({} logical bytes; retained by checkout cleanup)",
                entry.category,
                entry.path.display(),
                entry
                    .estimated_bytes
                    .map(|bytes| bytes.to_string())
                    .unwrap_or_else(|| "unknown".into()),
            );
            for blocker in &entry.blockers {
                out!("      blocker: {blocker}");
            }
            if detail {
                out!("      {}", entry.maintenance);
            }
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
        out!("  apply: no eligible checkout artifacts; shared Git storage is retained");
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
                "  next: use `aethyme broker gc reclaim plan --session <id>` for one session's build caches or repeat with `aethyme broker gc plan --full-scan` for an exhaustive inventory"
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
            "  apply: aethyme broker gc apply --confirm {}{}{}{}",
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
            if full_scan { " --full-scan" } else { "" },
            if plan
                .blockers
                .iter()
                .any(|blocker| blocker.kind == "orphans_excluded")
            {
                " --exclude-orphans"
            } else {
                ""
            }
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
    for skipped in &report.files_skipped {
        out!("  skipped file: {}: {}", skipped.path, skipped.reason);
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
    // A whole-root plan is what an agent reaches for when the disk is low.
    // On the busiest repository it walked every checkout and sized every
    // build tree serially for 8-16+ minutes, from scratch for each agent that
    // asked. It now discovers and sizes within budgets and shares what it
    // measures; a budgeted plan never sizes the root while another plan is
    // doing so, and `--full-scan` lifts both budgets and never waits.
    // Apply is untouched: it never sizes and re-proves every path itself.
    let whole_root_plan = scope.is_none() && matches!(&action, ReclaimAction::Plan);
    let bounded = whole_root_plan && !parsed.full_scan;
    let discovery_budget = bounded.then(reclaim_discovery_budget);
    let sizing_budget = bounded.then(reclaim_sizing_budget);
    let in_flight = if whole_root_plan {
        let alive = |holder: &crate::reclaim::InFlightPlan| {
            crate::reclaim::in_flight_holder_alive(holder, now_ms())
        };
        let me = crate::reclaim::InFlightPlan::current(now);
        match crate::reclaim::claim_in_flight(&root, me, &alive) {
            Ok(claim) => Some(claim),
            Err(error) => {
                eprintln!(
                    "Warning: cannot mark this whole-root reclaim plan as in flight; \
                     measuring without that coordination: {error}"
                );
                None
            }
        }
    } else {
        None
    };
    let discovery_started = std::time::Instant::now();
    let (mut candidates, discovery) = match &scope {
        Some((_, false)) => (Vec::new(), None),
        Some((worktree, true)) => (
            crate::reclaim::scan_worktree(
                worktree,
                &active,
                &retention_policy.artefact_directories,
                sizing,
            ),
            None,
        ),
        None => {
            // A whole-root plan sizes after the protections below, so its
            // sizing budget goes first to what may actually be reclaimed.
            let sizing = if whole_root_plan {
                crate::reclaim::Sizing::Skip
            } else {
                sizing
            };
            let deadline = discovery_budget.map_or_else(
                crate::reclaim::Deadline::never,
                crate::reclaim::Deadline::after,
            );
            let (found, mut discovery) = crate::reclaim::scan_root_within(
                &root,
                &active,
                &retention_policy.artefact_directories,
                sizing,
                &mut || deadline.passed(),
            );
            discovery.budget_ms = discovery_budget.map(duration_ms);
            discovery.elapsed_ms = elapsed_ms(discovery_started);
            (found, Some(discovery))
        }
    };
    let discovery_ms = elapsed_ms(discovery_started);
    let protection_started = std::time::Instant::now();
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
    let protection_ms = elapsed_ms(protection_started);
    let (sizing_report, sizing_phases) = if whole_root_plan {
        let (report, phases) =
            size_whole_root_plan(&root, &mut candidates, in_flight, sizing_budget);
        (Some(report), phases)
    } else {
        (None, SizingPhases::default())
    };
    let session_flag = parsed
        .session
        .map(|id| format!(" --session {id}"))
        .unwrap_or_default();
    let scope = scope.map(|(worktree, _)| worktree);
    // A plan whose discovery stopped early lists only part of the root. It is
    // shown, but it is not a review: its digest matches no complete scan and
    // no snapshot is saved for it, so apply refuses it.
    let confirmable = discovery.as_ref().is_none_or(|found| found.complete);
    let digest = if confirmable {
        crate::reclaim::plan_digest(&root, scope.as_deref(), &candidates)
    } else {
        crate::reclaim::partial_plan_digest(&root, &candidates)
    };
    let plan = crate::ReclaimPlan {
        digest: digest.clone(),
        root: root.clone(),
        scope: scope.clone(),
        reclaimable_bytes: crate::reclaimable_bytes(&candidates),
        total_bytes: crate::reclaim::measured_bytes(&candidates),
        candidates,
    };
    let gib = |bytes: u64| format!("{:.1} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0));
    match action {
        ReclaimAction::Plan => {
            let details = reclaim_plan_metric_details(
                &plan,
                ReclaimPlanPhases {
                    discovery: discovery.as_ref(),
                    sizing: sizing_report.as_ref(),
                    discovery_ms,
                    protection_ms,
                    sizing_phases,
                    full_scan: parsed.full_scan,
                    confirmable,
                },
            );
            super::telemetry::set_command_metric_details(details);
            let snapshot = if confirmable {
                crate::reclaim::save_snapshot(
                    &root,
                    scope.as_deref(),
                    &plan.digest,
                    &plan.candidates,
                )
            } else {
                Ok(())
            };
            if let Err(error) = snapshot {
                eprintln!(
                    "Warning: cannot save reclaim plan review snapshot; continuing with the \
                     digest-bound plan: {error}"
                );
            }
            if parsed.json {
                let view = ReclaimPlanView {
                    plan: &plan,
                    confirmable,
                    discovery: discovery.as_ref(),
                    sizing: sizing_report.as_ref(),
                };
                out!("{}", serde_json::to_string_pretty(&view)?);
            } else {
                // A partial plan's digest confirms nothing, and an agent that
                // copied it into apply would wait out a full apply scan only
                // to be refused. So it is not printed at all; `--json` still
                // carries it beside `"confirmable": false`.
                let title = if confirmable {
                    format!("Reclaim plan {}", plan.digest)
                } else {
                    "Partial reclaim plan (no digest: it cannot be applied)".to_string()
                };
                out!(
                    "{title}: {} reclaimable of {} found",
                    gib(plan.reclaimable_bytes),
                    gib(plan.total_bytes)
                );
                for candidate in plan.candidates.iter().take(20) {
                    let kept = if candidate.reclaimable {
                        String::new()
                    } else {
                        format!("  (kept: {})", candidate.reason)
                    };
                    let size = candidate.bytes.map_or_else(|| "unmeasured".into(), gib);
                    out!("  {size:>10}  {}{kept}", candidate.path.display());
                }
                let notes =
                    reclaim_plan_notes(discovery.as_ref(), sizing_report.as_ref(), now_ms());
                for line in notes {
                    out!("  {line}");
                }
                let unmeasured_reclaimable = plan
                    .candidates
                    .iter()
                    .any(|candidate| candidate.reclaimable && candidate.bytes.is_none());
                if !confirmable {
                    out!(
                        "Not applicable: this plan does not cover the whole root, so it has no digest to confirm; apply accepts only a complete plan's digest."
                    );
                } else if plan.reclaimable_bytes == 0 && !unmeasured_reclaimable {
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
            //
            // Every apply that got this far leaves a `reclaim.apply` metric
            // row, refusals included: it is the destructive half.
            let review_started = std::time::Instant::now();
            let mut metric = ReclaimApplyMetric {
                session_scoped: scope.is_some(),
                exact_match: confirm == plan.digest,
                scan_ms: discovery_ms,
                protection_ms,
                review_ms: 0,
                removal_ms: 0,
                candidates_scanned: plan.candidates.len(),
                reviewed_reclaimable: 0,
                withdrawn: 0,
            };
            let mut not_reviewed = Vec::new();
            let mut withdrawn = Vec::new();
            let plan = if confirm == plan.digest {
                metric.reviewed_reclaimable = count_reclaimable(&plan.candidates);
                plan
            } else {
                match crate::reclaim::load_snapshot(&root, &confirm) {
                    // A review covers what it scanned: a one-session review
                    // says nothing about the other worktrees, and a
                    // whole-root review was not made about one session.
                    Ok(Some(reviewed)) if reviewed.scope != scope => {
                        metric.review_ms = elapsed_ms(review_started);
                        metric.record(&plan.candidates, None, Some("scope_mismatch"));
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
                        metric.reviewed_reclaimable = reviewed
                            .decisions
                            .iter()
                            .filter(|decision| decision.reclaimable)
                            .count();
                        let narrowed = crate::reclaim::restrict_to_review(
                            &plan.candidates,
                            &reviewed.decisions,
                        );
                        not_reviewed = narrowed.not_reviewed;
                        withdrawn = narrowed.withdrawn;
                        metric.withdrawn = withdrawn.len();
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
                        metric.review_ms = elapsed_ms(review_started);
                        let (refusal, detail) = match unreadable {
                            Err(error) => (
                                "review_unreadable",
                                format!(
                                    "the saved review could not be read ({error}); no reviewed decision set can be established"
                                ),
                            ),
                            _ => (
                                "review_unavailable",
                                "the saved review is unavailable; no reviewed decision set can be established".into(),
                            ),
                        };
                        metric.record(&plan.candidates, None, Some(refusal));
                        return Err(UsageError::Message(format!(
                            "confirmation does not match the current plan; re-run `aethyme broker gc reclaim plan{session_flag}` and review it again (reviewed {}, current {}); {}",
                            confirm, plan.digest, detail
                        )));
                    }
                }
            };
            metric.review_ms = elapsed_ms(review_started);
            let apply_root = match broker
                .require_bound_repository_worktree_root("worktree cache reclamation apply")
            {
                Ok(apply_root) => apply_root,
                Err(error) => {
                    metric.record(&plan.candidates, None, Some("root_unbound"));
                    return Err(error.into());
                }
            };
            if apply_root != root {
                metric.record(&plan.candidates, None, Some("root_changed"));
                return Err(UsageError::Message(
                    "the bound worktree root changed while preparing this reclaim; re-run the plan and review it again".into(),
                ));
            }
            let removal_started = std::time::Instant::now();
            let mut outcome = crate::apply_reclaim(&plan);
            metric.removal_ms = elapsed_ms(removal_started);
            withdrawn.append(&mut outcome.skipped);
            outcome.skipped = withdrawn;
            outcome.not_reviewed = not_reviewed;
            metric.record(&plan.candidates, Some(&outcome), None);
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

/// Shortens the whole-root discovery budget to this many milliseconds.
const RECLAIM_DISCOVERY_BUDGET_VARIABLE: &str = "AETHYME_RECLAIM_PLAN_DISCOVERY_BUDGET_MS";
/// Shortens the whole-root sizing budget to this many milliseconds.
const RECLAIM_SIZING_BUDGET_VARIABLE: &str = "AETHYME_RECLAIM_PLAN_SIZING_BUDGET_MS";

fn reclaim_discovery_budget() -> std::time::Duration {
    whole_root_budget(
        crate::reclaim::WHOLE_ROOT_DISCOVERY_BUDGET,
        RECLAIM_DISCOVERY_BUDGET_VARIABLE,
    )
}

fn reclaim_sizing_budget() -> std::time::Duration {
    whole_root_budget(
        crate::reclaim::WHOLE_ROOT_SIZING_BUDGET,
        RECLAIM_SIZING_BUDGET_VARIABLE,
    )
}

/// A budgeted whole-root plan's budget: `default`, or shorter when
/// `variable` names fewer milliseconds. It is how the end-to-end tests make a
/// partial plan without a root large enough to exhaust 60 s, and an operator
/// may use it to get a quicker look.
fn whole_root_budget(default: std::time::Duration, variable: &str) -> std::time::Duration {
    shortened_budget(default, std::env::var(variable).ok().as_deref())
}

/// `default`, or `value` milliseconds when that parses and is shorter. A
/// value can only shorten a budget, never lengthen or lift it: a shorter
/// discovery budget yields a partial plan, which is never confirmable, and a
/// shorter sizing budget yields unmeasured sizes, which authorize nothing.
/// `--full-scan` is the only way to lift a budget.
fn shortened_budget(default: std::time::Duration, value: Option<&str>) -> std::time::Duration {
    value
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(std::time::Duration::from_millis)
        .map_or(default, |shorter| shorter.min(default))
}

fn duration_ms(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn elapsed_ms(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// How often a plan that found another whole-root plan in flight checks
/// whether it has finished.
const IN_FLIGHT_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// Size a whole-root plan's candidates within `budget` (`None`: unbounded,
/// for `--full-scan`).
///
/// Sizes another whole-root plan measured in the last
/// [`crate::reclaim::SIZE_REUSE_WINDOW_MS`] are reused, including those a
/// still-running plan has shared so far. When another plan holds the
/// in-flight marker, a budgeted plan waits for it within the same deadline
/// that bounds its walk, and walks only once it has finished; an unbounded
/// plan never waits. Fresh sizes are shared as they are measured. Sizes never
/// authorize a deletion, so reusing or skipping them leaves the plan's
/// decisions and digest exactly as they were. Returns the report and the
/// wait time and share count.
fn size_whole_root_plan(
    root: &Path,
    candidates: &mut [crate::ReclaimCandidate],
    claim: Option<crate::reclaim::InFlightClaim>,
    budget: Option<std::time::Duration>,
) -> (crate::reclaim::SizingReport, SizingPhases) {
    use crate::reclaim::{
        Deadline, InFlightClaim, InFlightPlan, SizeCheckpoints, in_flight_holder_alive,
        measure_within_sharing, recent_sizes, wait_for_in_flight,
    };
    let window = crate::reclaim::SIZE_REUSE_WINDOW_MS;
    let alive = |holder: &InFlightPlan| in_flight_holder_alive(holder, now_ms());
    // One deadline for the wait and the walk together, so a budgeted plan
    // never spends more than its sizing budget on sizes, however it spends it.
    let deadline = budget.map_or_else(Deadline::never, Deadline::after);
    let wait_started = std::time::Instant::now();
    let (walk, deferred_to, _guard) = match claim {
        Some(InFlightClaim::Claimed(guard)) => (true, None, Some(guard)),
        // `--full-scan` promises sizes for everything, so it never defers:
        // waiting for another plan could not measure what that plan skips.
        // It walks alongside, which costs I/O and never correctness.
        Some(InFlightClaim::HeldBy(_)) if budget.is_none() => (true, None, None),
        // A budgeted plan waits within its budget. Whatever the holder has
        // measured so far -- it shares sizes every few seconds -- is reused
        // below either way, so a minutes-long `--full-scan` holder no longer
        // leaves it with nothing.
        Some(InFlightClaim::HeldBy(holder)) => {
            let mut waited_out = || deadline.passed();
            let mut pause = || std::thread::sleep(IN_FLIGHT_POLL);
            let finished = wait_for_in_flight(root, &alive, &mut waited_out, &mut pause);
            (finished, (!finished).then_some(holder), None)
        }
        None => (true, None, None),
    };
    let wait_ms = elapsed_ms(wait_started);
    let started = std::time::Instant::now();
    let recent = recent_sizes(root, now_ms(), window);
    let mut checkpoints = SizeCheckpoints::new(
        root,
        window,
        crate::reclaim::SIZE_CHECKPOINT_INTERVAL_MS,
        now_ms(),
    );
    let (mut report, _) = measure_within_sharing(
        candidates,
        &recent,
        walk,
        &mut || deadline.passed(),
        &mut |path, bytes| checkpoints.measured(path, bytes, now_ms()),
    );
    let (size_shares, failure) = checkpoints.finish(now_ms());
    if let Some(error) = failure {
        eprintln!("Warning: cannot save reclaim plan sizes for the next plan to reuse: {error}");
    }
    report.budget_ms = budget.map(duration_ms);
    report.deferred_to = deferred_to;
    report.elapsed_ms = elapsed_ms(started);
    (
        report,
        SizingPhases {
            in_flight_wait_ms: wait_ms,
            size_shares,
        },
    )
}

/// Timings and counts of the sizing phase beyond its report.
#[derive(Debug, Clone, Copy, Default)]
struct SizingPhases {
    in_flight_wait_ms: u64,
    /// Writes of fresh sizes to the shared size cache.
    size_shares: usize,
}

fn count_reclaimable(candidates: &[crate::ReclaimCandidate]) -> usize {
    candidates
        .iter()
        .filter(|candidate| candidate.reclaimable)
        .count()
}

/// What one `reclaim apply` did, for its `reclaim.apply` metric row.
struct ReclaimApplyMetric {
    session_scoped: bool,
    /// The fresh scan hashed to the confirmed digest, so no saved review was
    /// needed.
    exact_match: bool,
    scan_ms: u64,
    /// The protections every candidate is re-proved against.
    protection_ms: u64,
    /// Matching the digest and loading and verifying the saved review.
    review_ms: u64,
    removal_ms: u64,
    candidates_scanned: usize,
    /// Paths the confirmed review marked reclaimable.
    reviewed_reclaimable: usize,
    /// Reviewed paths no longer present or now kept.
    withdrawn: usize,
}

impl ReclaimApplyMetric {
    /// Attach the row. `candidates` is what apply was handed (the fresh scan,
    /// narrowed to the review when one was used); `refused` names why apply
    /// stopped before removing anything.
    fn record(
        &self,
        candidates: &[crate::ReclaimCandidate],
        outcome: Option<&crate::ReclaimOutcome>,
        refused: Option<&str>,
    ) {
        super::telemetry::set_command_metric_details(self.details(candidates, outcome, refused));
    }

    fn details(
        &self,
        candidates: &[crate::ReclaimCandidate],
        outcome: Option<&crate::ReclaimOutcome>,
        refused: Option<&str>,
    ) -> serde_json::Value {
        let removed = outcome.map_or(0, |outcome| outcome.removed.len());
        let skipped = outcome.map_or(0, |outcome| outcome.skipped.len());
        let mut counts = serde_json::Map::new();
        counts.insert("candidates".into(), self.candidates_scanned.into());
        counts.insert("reviewed".into(), self.reviewed_reclaimable.into());
        counts.insert("authorized".into(), count_reclaimable(candidates).into());
        counts.insert("removed".into(), removed.into());
        counts.insert("withdrawn".into(), self.withdrawn.into());
        counts.insert(
            "removal_failed".into(),
            skipped.saturating_sub(self.withdrawn).into(),
        );
        counts.insert(
            "not_reviewed".into(),
            outcome
                .map_or(0, |outcome| outcome.not_reviewed.len())
                .into(),
        );
        // Kept candidates by which protection kept them; the reasons
        // themselves can name paths and processes and stay out of the row.
        let mut kept = std::collections::BTreeMap::<&str, usize>::new();
        for candidate in candidates.iter().filter(|candidate| !candidate.reclaimable) {
            let class = crate::reclaim::kept_reason_class(&candidate.reason);
            *kept.entry(class).or_default() += 1;
        }
        for (class, count) in kept {
            counts.insert(format!("kept_{class}"), count.into());
        }
        serde_json::json!({
            "phase_timings_ms": {
                "scan": self.scan_ms,
                "protection": self.protection_ms,
                "review": self.review_ms,
                "removal": self.removal_ms,
            },
            "phase_counts": counts,
            "reclaim_apply": {
                "scope": if self.session_scoped { "session" } else { "root" },
                "review": if self.exact_match { "exact" } else { "saved_review" },
                "refused": refused,
                "reclaimed_bytes": outcome.map_or(0, |outcome| outcome.reclaimed_bytes),
            },
        })
    }
}

/// The `reclaim plan --json` document: the plan, plus how complete it is.
#[derive(serde::Serialize)]
struct ReclaimPlanView<'a> {
    #[serde(flatten)]
    plan: &'a crate::ReclaimPlan,
    /// False when discovery stopped early: the digest then authorizes nothing.
    confirmable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    discovery: Option<&'a crate::reclaim::Discovery>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sizing: Option<&'a crate::reclaim::SizingReport>,
}

/// What a plan's metric row records beyond the plan itself.
struct ReclaimPlanPhases<'a> {
    discovery: Option<&'a crate::reclaim::Discovery>,
    sizing: Option<&'a crate::reclaim::SizingReport>,
    discovery_ms: u64,
    protection_ms: u64,
    sizing_phases: SizingPhases,
    full_scan: bool,
    confirmable: bool,
}

/// The command-metric fields of one `reclaim plan`, so a slow or cut-off
/// plan leaves phase timings and counts in `command-metrics.jsonl`.
fn reclaim_plan_metric_details(
    plan: &crate::ReclaimPlan,
    phases: ReclaimPlanPhases<'_>,
) -> serde_json::Value {
    let discovery = phases.discovery.cloned().unwrap_or_default();
    let sizing = phases.sizing.cloned().unwrap_or_default();
    let measured = if phases.sizing.is_some() {
        sizing.measured
    } else {
        plan.candidates
            .iter()
            .filter(|candidate| candidate.bytes.is_some())
            .count()
    };
    let unmeasured = plan
        .candidates
        .iter()
        .filter(|candidate| candidate.bytes.is_none())
        .count();
    serde_json::json!({
        "phase_timings_ms": {
            "discovery": phases.discovery_ms,
            "protection": phases.protection_ms,
            "in_flight_wait": phases.sizing_phases.in_flight_wait_ms,
            "sizing": sizing.elapsed_ms,
        },
        "phase_counts": {
            "worktrees_scanned": discovery.worktrees_scanned,
            "worktrees_not_scanned": discovery.worktrees_not_scanned,
            "candidates": plan.candidates.len(),
            "measured": measured,
            "reused": sizing.reused,
            "unmeasured": unmeasured,
            "size_shares": phases.sizing_phases.size_shares,
        },
        "reclaim_plan": {
            "scope": if plan.scope.is_some() { "session" } else { "root" },
            "full_scan": phases.full_scan,
            "discovery_complete": phases.discovery.is_none_or(|discovery| discovery.complete),
            "budget_exhausted": sizing.budget_exhausted,
            "reused": sizing.reused > 0,
            "deferred_to_pid": sizing.deferred_to.as_ref().map(|holder| holder.pid),
            "confirmable": phases.confirmable,
        },
    })
}

/// The lines that explain an incomplete whole-root plan and how to finish it.
fn reclaim_plan_notes(
    discovery: Option<&crate::reclaim::Discovery>,
    sizing: Option<&crate::reclaim::SizingReport>,
    now_ms: i64,
) -> Vec<String> {
    let mut notes = Vec::new();
    let mut incomplete = false;
    if let Some(sizing) = sizing {
        if sizing.reused > 0 {
            notes.push(format!(
                "reused: {} size(s) another whole-root plan measured in the last {} s",
                sizing.reused,
                crate::reclaim::SIZE_REUSE_WINDOW_MS / 1000
            ));
        }
        if sizing.unmeasured > 0 {
            incomplete = true;
            let why = match (&sizing.deferred_to, sizing.budget_exhausted) {
                (Some(holder), _) => format!(
                    "another whole-root plan (pid {}, started {} s ago) is still measuring this root; re-run when it finishes to reuse its sizes",
                    holder.pid,
                    now_ms.saturating_sub(holder.started_at_ms).max(0) / 1000
                ),
                (None, true) => format!(
                    "the {} s sizing budget ran out",
                    sizing.budget_ms.unwrap_or_default() / 1000
                ),
                (None, false) => "they were not measured".into(),
            };
            notes.push(format!(
                "not measured: {} candidate(s), listed as unmeasured and not counted in the totals; {why}",
                sizing.unmeasured
            ));
        }
    }
    if let Some(discovery) = discovery.filter(|discovery| !discovery.complete) {
        incomplete = true;
        notes.push(format!(
            "partial: discovery stopped after its {} s budget with {} of {} worktree(s) scanned; this plan cannot be applied",
            discovery.budget_ms.unwrap_or_default() / 1000,
            discovery.worktrees_scanned,
            discovery.worktrees_scanned + discovery.worktrees_not_scanned
        ));
    }
    if incomplete {
        notes.push(
            "next: `aethyme broker gc reclaim plan --session <id>` for one session, or `aethyme broker gc reclaim plan --full-scan` for the whole root without budgets"
                .into(),
        );
    }
    notes
}

#[cfg(unix)]
fn run_shared_gc_plan(
    parsed: &Parsed,
    directory: &Path,
    key: &str,
    deadline_ms: i64,
) -> Result<(), UsageError> {
    crate::service::client::deadline_remaining(deadline_ms)?;
    let client = crate::service::client::Client::connect(directory, &std::env::current_dir()?)?;
    let (plan, json_plan) = client.gc_plan(
        key,
        deadline_ms,
        parsed.include_active_gate_cache,
        parsed.full_scan,
    )?;
    if parsed.json {
        out!("{}", serde_json::to_string_pretty(&json_plan)?);
    } else {
        render_gc_plan(&plan, parsed.detail, parsed.full_scan);
    }
    Ok(())
}

#[cfg(not(unix))]
fn run_shared_gc_plan(
    _parsed: &Parsed,
    _directory: &Path,
    _key: &str,
    _deadline_ms: i64,
) -> Result<(), UsageError> {
    Err(UsageError::Message(
        "shared planning requires Unix sockets".into(),
    ))
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
    if action == crate::gc::TRASH_CATCHUP_ACTION {
        return run_trash_catchup_lane_process(parsed);
    }
    if action == crate::gc::RETIRE_LANE_ACTION {
        return run_checkout_retirement_lane_process(parsed);
    }
    // A managed sweep tick is a detached worker: its starved background lanes
    // may queue for the lifecycle lock (see `DetachedBackgroundWorker`).
    #[cfg(target_os = "macos")]
    let _detached_worker = (action == "sweep" && crate::gc_sweep_service::managed_tick_requested())
        .then(crate::file_lock::DetachedBackgroundWorker::enter);
    if action == "artifact-use" {
        return run_gc_artifact_use_reconcile(parsed);
    }
    if action == "remote-branches" {
        return run_gc_remote_branches(parsed);
    }
    if action == "trash" {
        return run_gc_trash(parsed);
    }
    if action == "storage" {
        if parsed.positional.get(1).map(String::as_str) != Some("maintenance-lock") {
            return Err(UsageError::Message(
                "gc storage accepts `maintenance-lock plan` or `maintenance-lock apply --confirm <sha256>`".into(),
            ));
        }
        let mut storage = parsed;
        storage.positional.remove(0);
        return run_storage(storage);
    }
    if parsed.positional.len() != 1 {
        return Err(UsageError::Message(
            "gc accepts one action: `plan`, `apply --confirm <sha256>`, `abandon --confirm <sha256>`, `forget-abandoned --confirm <sha256>`, `artifact-use reconcile <plan|apply> <session-id>`, or `sweep`".into(),
        ));
    }
    if parsed.service_directory.is_some()
        || parsed.request_key.is_some()
        || parsed.deadline_ms.is_some()
    {
        if parsed.exclude_orphans {
            return Err(UsageError::Message(
                "--exclude-orphans is not supported by shared planning".into(),
            ));
        }
        if action != "plan" || parsed.confirm.is_some() {
            return Err(UsageError::Message(
                "shared service options require gc plan without --confirm".into(),
            ));
        }
        let (Some(directory), Some(key), Some(deadline_ms)) = (
            parsed.service_directory.as_deref(),
            parsed.request_key.as_deref(),
            parsed.deadline_ms,
        ) else {
            return Err(UsageError::Message("shared planning requires --service-directory, --request-key and --deadline-ms together".into()));
        };
        return run_shared_gc_plan(&parsed, directory, key, deadline_ms);
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
    // Refuse a malformed action before any writable open below: that open
    // resumes GC maintenance and runs the bounded sweep, which may remove an
    // eligible checkout, and a refused command must change nothing.
    match action {
        "plan" if parsed.confirm.is_some() => {
            return Err(UsageError::Message(
                "gc plan does not accept --confirm; review its emitted digest".into(),
            ));
        }
        "apply" if parsed.confirm.is_none() => {
            return Err(UsageError::Message(
                "gc apply requires --confirm <sha256>".into(),
            ));
        }
        "plan" | "apply" | "abandon" | "forget-abandoned" | "sweep" => {}
        other => {
            return Err(UsageError::Message(format!(
                "unknown gc action {other:?}; expected `plan`, `apply`, `abandon`, `forget-abandoned` or `sweep`"
            )));
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
            let plan = broker.gc_plan_for_cli(
                parsed.include_active_gate_cache,
                parsed.full_scan,
                parsed.exclude_orphans,
            )?;
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
                parsed.exclude_orphans,
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
            if let Err(error) = &sweep
                && let Some(tick) = managed_tick.as_ref()
            {
                crate::gc_sweep_service::record_managed_tick(tick, None, Some(&error.to_string()))
                    .map_err(|receipt_error| {
                        UsageError::Message(format!(
                            "GC sweep failed ({error}); writing its managed tick receipt failed: {receipt_error}"
                        ))
                    })?;
            }
            let report = sweep?;
            #[cfg(target_os = "macos")]
            if let Some(tick) = managed_tick.as_ref() {
                crate::gc_sweep_service::record_managed_tick(tick, Some(&report), None)?;
            }
            // The background worker's remote passes: re-read the parents of
            // open stacked sessions, then the opt-in, bounded deletion of
            // merged session branches. Neither runs from a read-only snapshot.
            let (stack_failures, remote_branches) = if parsed.read_only_snapshot {
                (Vec::new(), None)
            } else {
                (
                    broker.reconcile_open_session_stacks(
                        &crate::session_stack::GhCliPullRequestReader,
                    ),
                    broker.run_remote_branch_cleanup(),
                )
            };
            if parsed.json {
                let mut value = serde_json::to_value(&report)?;
                if let Some(remote_branches) = &remote_branches {
                    value["remote_branch_cleanup"] = serde_json::to_value(remote_branches)?;
                }
                if !stack_failures.is_empty() {
                    value["session_stack_failures"] = serde_json::json!(stack_failures);
                }
                out!("{}", serde_json::to_string_pretty(&value)?);
            } else {
                render_gc_sweep(&report);
                for failure in &stack_failures {
                    out!("Stacked session parent not re-read: {failure}");
                }
                if let Some(remote_branches) = &remote_branches {
                    render_remote_branch_cleanup(remote_branches);
                }
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

/// Default and largest `gc trash drain --max-minutes`.
const GC_TRASH_DRAIN_DEFAULT_MINUTES: u32 = 20;
const GC_TRASH_DRAIN_MAX_MINUTES: u32 = 60;

/// `gc trash drain`: run the trash catch-up lane now, in the foreground.
fn run_gc_trash(parsed: Parsed) -> Result<(), UsageError> {
    if parsed.positional.len() != 2 || parsed.positional[1] != "drain" {
        return Err(UsageError::Message(
            "gc trash accepts `drain [--max-minutes <n>] [--json]`".into(),
        ));
    }
    let minutes = parsed.max_minutes.unwrap_or(GC_TRASH_DRAIN_DEFAULT_MINUTES);
    if minutes == 0 || minutes > GC_TRASH_DRAIN_MAX_MINUTES {
        return Err(UsageError::Message(format!(
            "--max-minutes must be between 1 and {GC_TRASH_DRAIN_MAX_MINUTES}"
        )));
    }
    let broker = open_broker(false)?;
    let host_dir = crate::host_state::resolve_host_state_dir().map(|(dir, _)| dir);
    let outcome = gc_trash_drain(
        &broker,
        host_dir.as_deref(),
        std::time::Duration::from_secs(u64::from(minutes) * 60),
        crate::checkout_trash::sample_host_pressure,
    )?;
    if parsed.json {
        out!("{}", serde_json::to_string_pretty(&outcome)?);
    } else {
        out!(
            "Checkout trash drain: {} entr(y/ies) removed ({} files, {} files/s over {} lap(s)); ended: {}.",
            outcome["entries_removed"],
            outcome["unlinks"],
            outcome["files_per_second"],
            outcome["laps"],
            outcome["end_reason"].as_str().unwrap_or("unknown"),
        );
        out!(
            "  remaining: {} entr(y/ies){}",
            outcome["entries_remaining"],
            outcome["oldest_remaining_age_hours"]
                .as_u64()
                .map(|hours| format!(", the oldest moved in {hours} hour(s) ago"))
                .unwrap_or_default()
        );
    }
    Ok(())
}

/// The lane `gc trash drain` runs, and what it reports; a refusal when
/// another catch-up lane is live on this host.
fn gc_trash_drain(
    broker: &Broker,
    host_dir: Option<&std::path::Path>,
    total: std::time::Duration,
    sample: fn() -> crate::checkout_trash::HostPressure,
) -> Result<serde_json::Value, UsageError> {
    let Some(host_dir) = host_dir else {
        return Err(UsageError::Message(
            "gc trash drain needs the host state directory, which could not be resolved".into(),
        ));
    };
    let laps = broker.run_checkout_trash_catchup_laps(
        Some(host_dir),
        crate::broker::TrashCatchupBounds {
            total,
            lap_max: crate::checkout_trash::TRASH_CATCHUP_MAX,
            max_laps: crate::broker::TRASH_CATCHUP_MAX_LAPS,
            until_empty: true,
            // The operator is told at once that another lane runs.
            lane_retry: None,
        },
        sample,
    );
    if laps
        .first()
        .is_some_and(|lap| lap.end == crate::broker::TrashCatchupEnd::LaneBusy)
    {
        return Err(UsageError::Message(GC_TRASH_DRAIN_LANE_BUSY.into()));
    }
    let drain_secs: f64 = laps.iter().map(|lap| lap.drain_time.as_secs_f64()).sum();
    let unlinks: usize = laps.iter().map(|lap| lap.unlinks).sum();
    let rows = broker.checkout_trash_rows();
    let remaining = crate::checkout_trash::TrashBacklog::of(&rows);
    Ok(serde_json::json!({
        "end_reason": laps.last().map_or("none", |lap| lap.end.as_str()),
        "laps": laps.len(),
        "slices": laps.iter().map(|lap| u64::from(lap.slices)).sum::<u64>(),
        "entries_removed": laps.iter().map(|lap| lap.entries_removed).sum::<usize>(),
        "unlinks": unlinks,
        "bytes": laps.iter().map(|lap| lap.bytes).sum::<u64>(),
        "drain_ms": (drain_secs * 1_000.0).round() as u64,
        "files_per_second": if drain_secs > 0.0 {
            (unlinks as f64 / drain_secs).round() as u64
        } else {
            0
        },
        "entries_remaining": rows.values().map(std::collections::BTreeMap::len).sum::<usize>(),
        "io_policy": crate::gc::current_io_policy(),
        "oldest_remaining_age_hours": remaining
            .oldest_age_ms(crate::clock::epoch_ms())
            .map(|age| age / 3_600_000),
    }))
}

/// The detached worker's trash catch-up lane process (`gc __trash-catchup
/// <root>`): it raises its own disk I/O out of the worker's throttled
/// policy, then runs the lane's laps for this repository.
fn run_trash_catchup_lane_process(parsed: Parsed) -> Result<(), UsageError> {
    // The same invocation checks as the worker entry, before anything opens.
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
            "cannot resolve the trash catch-up repository: {error}"
        ))
    })?;
    let discovered = crate::GitRepo::discover(&requested_root)?;
    let repository_root = std::fs::canonicalize(discovered.main_root()?)?;
    if repository_root != requested_root {
        return Err(UsageError::Message(
            "the trash catch-up repository does not match its main checkout".into(),
        ));
    }
    if crate::host_state::path_is_ephemeral(&repository_root)
        || !crate::gc::artifact_worker_runtime_is_aligned()
    {
        return Err(UsageError::Message(
            "the trash catch-up runtime or repository is not eligible".into(),
        ));
    }
    let priority = crate::gc::raise_trash_deleter_priority();
    // What took effect, read back: a raise the platform ignored stays
    // visible in the first reading.
    let effective = crate::gc::current_io_policy();
    if effective == "throttle" {
        crate::phase_telemetry::add(
            "checkout_trash_catchup_priority_not_raised",
            std::time::Duration::ZERO,
        );
    }
    if priority.io_policy == "important" && effective != "throttle" {
        // Below the headroom the drain no longer has the OS throttle as a
        // bound: it is held to the catch-up rate instead of running unpaced.
        crate::checkout_trash::mark_priority_raised();
    }
    crate::phase_telemetry::attach(
        "checkout_trash_catchup_priority",
        serde_json::json!({
            "io_policy": effective,
            "requested": priority.io_policy,
            "darwin_bg_cleared": priority.darwin_bg_cleared,
            "cpu_niced": priority.cpu_niced,
        }),
    );
    // No unrelated startup GC maintenance: this process only drains.
    let broker = Broker::open_for_cleanup_resolve(&repository_root)?;
    let host_dir = crate::host_state::resolve_host_state_dir().map(|(dir, _)| dir);
    let worker_started_ms = std::env::var(crate::gc::TRASH_CATCHUP_WORKER_STARTED_ENV)
        .ok()
        .and_then(|value| value.parse().ok());
    broker.run_checkout_trash_catchup_process(
        host_dir.as_deref(),
        worker_started_ms,
        effective,
        crate::checkout_trash::sample_host_pressure,
    );
    Ok(())
}

/// The detached worker's checkout retirement lane process (`gc
/// __retire-lane <root>`): normal disk priority, reduced CPU priority, then
/// the lane's slices for this repository (see `broker/retire/lane.rs`).
fn run_checkout_retirement_lane_process(parsed: Parsed) -> Result<(), UsageError> {
    // The same invocation checks as the worker entry, before anything opens.
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
            "cannot resolve the checkout retirement repository: {error}"
        ))
    })?;
    let discovered = crate::GitRepo::discover(&requested_root)?;
    let repository_root = std::fs::canonicalize(discovered.main_root()?)?;
    if repository_root != requested_root {
        return Err(UsageError::Message(
            "the checkout retirement repository does not match its main checkout".into(),
        ));
    }
    if crate::host_state::path_is_ephemeral(&repository_root)
        || !crate::gc::artifact_worker_runtime_is_aligned()
    {
        return Err(UsageError::Message(
            "the checkout retirement runtime or repository is not eligible".into(),
        ));
    }
    let Some((host_dir, _)) = crate::host_state::resolve_host_state_dir() else {
        return Err(UsageError::Message(
            "the checkout retirement lane needs the host state directory".into(),
        ));
    };
    let priority = crate::gc::raise_retirement_lane_priority();
    crate::phase_telemetry::attach(
        "checkout_retirement_lane_priority",
        serde_json::json!({
            "io_policy": crate::gc::current_io_policy(),
            "requested": priority.io_policy,
            "darwin_bg_cleared": priority.darwin_bg_cleared,
            "cpu_niced": priority.cpu_niced,
        }),
    );
    // No unrelated startup GC maintenance: this process only retires.
    let mut broker = Broker::open_for_cleanup_resolve(&repository_root)?;
    broker.run_checkout_retirement_lane_process(
        &host_dir,
        crate::checkout_trash::sample_host_pressure,
    );
    Ok(())
}

/// The refusal of `gc trash drain` while another lane is live.
const GC_TRASH_DRAIN_LANE_BUSY: &str = "another checkout trash catch-up lane is running on this host (a background worker's, or another `gc trash drain`); it is the one deleter allowed, so this run removed nothing. Try again once it ends.";

/// `gc remote-branches plan`: preview the remote session-branch cleanup.
fn run_gc_remote_branches(parsed: Parsed) -> Result<(), UsageError> {
    if parsed.positional.len() != 2 || parsed.positional[1] != "plan" {
        return Err(UsageError::Message(
            "gc remote-branches accepts `plan [--limit <n>] [--json]`; deletion runs only in the \
             background worker (`aethyme broker gc sweep`) when `[remote_branches] \
             delete_merged = true`"
                .into(),
        ));
    }
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    let limit = parsed.limit.map(|limit| limit as usize);
    let report = broker.plan_remote_branch_cleanup(limit);
    if parsed.json {
        out!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        render_remote_branch_cleanup(&report);
    }
    Ok(())
}

fn render_remote_branch_cleanup(report: &crate::remote_branch_cleanup::RemoteBranchCleanupReport) {
    out!(
        "Remote session branches ({}): {} awaiting a decision; deletion {}.",
        report.repository.as_deref().unwrap_or("unknown repository"),
        report.backlog,
        if report.enabled {
            "enabled"
        } else {
            "disabled"
        }
    );
    if let Some(skipped) = &report.skipped {
        out!("  skipped: {skipped}");
    }
    for item in &report.items {
        let pr = item
            .pr_number
            .map(|number| format!(" (#{number})"))
            .unwrap_or_default();
        out!("  {} {}{pr}: {}", item.outcome, item.branch, item.reason);
        for retarget in &item.retargets {
            out!(
                "    retarget #{} to {}: {}",
                retarget.pull_request,
                retarget.to,
                retarget.outcome
            );
        }
    }
    if let Some(stopped) = &report.stopped {
        out!("  stopped: {stopped}");
    }
    if let Some(next) = &report.next_action {
        out!("  next: {next}");
    }
}

/// A refusal says what it means for this session and that waiting is safe, so
/// an agent does not treat a correct refusal as something to work around.
fn artifact_use_refusal_message(
    reason: crate::auto_cleanup::ClosedArtifactCheckoutCohortUnavailableReason,
    id: i64,
    session_open: bool,
    session_cleaned: bool,
) -> String {
    use crate::auto_cleanup::ClosedArtifactCheckoutCohortUnavailableReason as Reason;
    let guidance = match reason {
        Reason::SessionChanged if session_open => format!(
            "Session {id} is still open, so its markers may belong to live work. Finish it, then plan again."
        ),
        Reason::SessionChanged if session_cleaned => {
            format!("Session {id} has already been cleaned, so this command no longer covers it.")
        }
        Reason::NonClosedAlias => format!(
            "Another session using the checkout of session {id} is still open. Finish it, then plan again."
        ),
        Reason::IndexMissing | Reason::IndexIncomplete | Reason::IndexChanged | Reason::WitnessChanged => {
            "The cleanup index is catching up with recent session changes, and later broker commands advance it. Plan again later.".to_owned()
        }
        Reason::SpawnedWitnessMismatch => format!(
            "The checkout of session {id} cannot prove it is the one recorded at spawn: the record has no spawn generation, or the checkout was replaced. Planning again will not change that."
        ),
        Reason::SpawnedOwnerMissing => format!(
            "Only adopted sessions use the checkout of session {id}, and an adopted session cannot prove who created it. Planning again will not change that."
        ),
        Reason::CheckoutMissing => {
            format!("The checkout of session {id} no longer exists, so it holds no disk space.")
        }
        Reason::AliasOverlapOutsideCohort => format!(
            "Another session's recorded path overlaps the checkout of session {id}, with no proof that session's checkout is gone. Planning again will not change that."
        ),
        _ => "Inspect the selected session and `aethyme broker gc plan --json` for the missing checkout proof.".to_owned(),
    };
    format!(
        "artifact-use reconciliation refused: {}; no markers were changed. {guidance} Leaving the marker retained is safe: it only holds back automatic cleanup of this checkout. Do not remove markers manually",
        reason.code()
    )
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
    // An operator's explicit apply is foreground work: it does not yield to
    // queued lifecycle waiters, it only refuses while the lock is held.
    let _foreground = crate::file_lock::ForegroundLifecycleTryClaim::enter();
    let _lifecycle_claim = if action == "apply" {
        Some(
            broker
                .try_claim_worktree_lifecycle("artifact_use_reconcile")?
                .ok_or_else(|| {
                    UsageError::Message("worktree lifecycle is busy; retry later".into())
                })?,
        )
    } else {
        None
    };
    let session = broker.store().session(session_id)?;
    let checkout_root = PathBuf::from(&session.worktree_path);
    let cohort = match broker.closed_artifact_checkout_cohort_for_session_readonly(
        session_id,
        &checkout_root,
        None,
    )? {
        crate::auto_cleanup::ClosedArtifactCheckoutCohortResolution::Available(cohort) => cohort,
        crate::auto_cleanup::ClosedArtifactCheckoutCohortResolution::Unavailable(reason) => {
            return Err(UsageError::Message(artifact_use_refusal_message(
                reason,
                session_id,
                !session.status.is_closed(),
                session.cleanup_state == crate::types::SessionCleanupState::Cleaned,
            )));
        }
    };

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
    // Nobody waits on this process: its starved background lanes may queue
    // for the lifecycle lock (see `DetachedBackgroundWorker`).
    let _detached_worker = crate::file_lock::DetachedBackgroundWorker::enter();
    // Nobody waits on this process: drop to background CPU and I/O priority
    // before anything else runs, so every later child inherits it.
    let priority = crate::gc::lower_artifact_worker_priority();
    // This worker removes checkouts unattended: its Git is probed here, never
    // taken from the per-user probe cache.
    crate::git::require_fresh_git_probe();
    let requested_root = std::fs::canonicalize(&parsed.positional[1]).map_err(|error| {
        UsageError::Message(format!(
            "cannot resolve automatic artifact continuation repository: {error}"
        ))
    })?;

    // Hold the OS lock before anything slow (the fresh Git probe the first
    // Git call runs takes seconds on a loaded machine) and before
    // Broker::open. The parent drops it only after this process has been
    // spawned, so no second detached worker can enter. Taken while the probe
    // ran, the gap let a broker open in between launch a second worker, and
    // whichever lost the lock exited 1 with nothing done.
    // The launcher still owns the lock while the child starts: wait for that
    // handoff (40 tries, 50 ms apart), but never queue behind an active
    // worker, which serves this repository's backlog: nothing failed.
    let Some(worker_lock) = crate::gc::take_artifact_sweep_worker_lock_after_handoff(
        &requested_root,
        40,
        std::time::Duration::from_millis(50),
    )?
    else {
        return Ok(());
    };
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
    // Hold the singleton briefly before reading the backlog: `finish` calls
    // arriving in a burst find this worker alive and are served by this run.
    std::thread::sleep(crate::gc::ARTIFACT_WORKER_COALESCE_WINDOW);
    let _run_scopes = enter_worker_run_scopes(&repository_root, priority);
    let mut broker = Broker::open(&repository_root)?;
    // The read-only cache measurement may walk for minutes: the worker
    // releases this repository's lock before it, so the next deletion run
    // can start.
    let result = broker.run_artifact_sweep_recovery_worker(move || drop(worker_lock));
    // Opt-in host lane: archive closed agent transcripts at this worker's
    // background priority. A no-op unless the host enabled it.
    crate::transcripts::run_worker_lane();
    result?;
    Ok(())
}

/// The detached worker's run-wide scopes, in order. Where this run reads
/// `[cleanup]` from is resolved first, once, and is visible to every thread;
/// then every unlink the process makes is paced, at a rate the pacing reads
/// from that `[cleanup]`.
fn enter_worker_run_scopes(
    repository_root: &std::path::Path,
    priority: crate::gc::BackgroundPriority,
) -> (
    crate::cleanup_config::RunScope,
    crate::gc::unattended_pacing::UnattendedPacingScope,
) {
    let cleanup_config = crate::cleanup_config::RunScope::enter(repository_root);
    let pacing = crate::gc::install_unattended_deletion_pacing(repository_root, priority);
    (cleanup_config, pacing)
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
        "artifact sweep: reclaimed {} build-output {noun} and {} filesystem entries within {} ms{}",
        report.directories_reclaimed,
        report.entries_reclaimed,
        report.budget_ms,
        if report.disk_pressured {
            " (below configured proactive headroom; pressure-scaled budget)"
        } else {
            ""
        }
    );
    out!("  scope: {} at {}", report.scope, report.at_ms);
    out!(
        "  logical file bytes observed during traversal for later-unlinked entries: {} (sizes can change before unlink; physical space recovered is not measured)",
        human_bytes(report.observed_logical_file_bytes)
    );
    if report.preparation_cache_bytes_reclaimed > 0 {
        out!(
            "  preparation-cache bytes reported: {}",
            human_bytes(report.preparation_cache_bytes_reclaimed)
        );
    }
    if report.disk_pressured {
        out!(
            "  proactive headroom threshold: {}",
            human_bytes(report.headroom_threshold_bytes)
        );
    }
    if !report.bytes_accounting_complete {
        out!("  byte accounting: incomplete after a tree-removal error");
    }
    if report.blockers.total > 0 {
        let by_reason = report
            .blockers
            .by_reason
            .iter()
            .map(|(reason, count)| format!("{reason}={count}"))
            .collect::<Vec<_>>()
            .join(", ");
        out!(
            "  blockers: {} total ({by_reason}); {} examples omitted",
            report.blockers.total,
            report.blockers.omitted_examples
        );
        for example in report.blockers.examples.iter().take(5) {
            out!(
                "    {}{}",
                example.reason,
                example
                    .path
                    .as_deref()
                    .map(|path| format!(": {path}"))
                    .unwrap_or_default()
            );
        }
    }
    if report.complete {
        out!("  complete: every eligible artifact cache was visited");
    } else {
        out!("  artifact cache pass paused; run `aethyme broker gc sweep` again to continue");
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
    if parsed.positional.first().map(String::as_str) == Some("maintenance-lock") {
        return run_storage_maintenance_lock(parsed);
    }
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

fn run_storage_maintenance_lock(parsed: Parsed) -> Result<(), UsageError> {
    if parsed.positional.len() != 2 {
        return Err(UsageError::Message(
            "storage maintenance-lock requires exactly one action: `plan` or `apply --confirm <sha256>`".into(),
        ));
    }
    if let Some(flag) = parsed.given_flags.iter().find(|flag| {
        !matches!(
            flag.as_str(),
            "--json" | "--json-output" | "--confirm" | "--no-quiet"
        )
    }) {
        return Err(UsageError::Message(format!(
            "{flag} is not supported by storage maintenance-lock"
        )));
    }
    if parsed
        .given_flags
        .iter()
        .filter(|flag| flag.as_str() == "--confirm")
        .count()
        > 1
    {
        return Err(UsageError::Message(
            "storage maintenance-lock accepts --confirm at most once".into(),
        ));
    }
    if parsed.apply {
        return Err(UsageError::Message(
            "storage maintenance-lock uses explicit `apply --confirm <sha256>`; --apply is not supported".into(),
        ));
    }
    let action = parsed.positional[1].as_str();
    let cwd = std::env::current_dir()
        .map_err(|error| UsageError::Message(format!("cannot resolve cwd: {error}")))?;
    match action {
        "plan" => {
            if parsed.confirm.is_some() {
                return Err(UsageError::Message(
                    "maintenance-lock plan does not accept --confirm".into(),
                ));
            }
            let plan = crate::git_maintenance_lock::plan(&cwd);
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&plan)?);
            } else {
                out!(
                    "Git maintenance-lock recovery plan: {}",
                    if plan.eligible { "eligible" } else { "refused" }
                );
                if let Some(target) = &plan.target {
                    out!("  exact target: {target}");
                }
                if let Some(digest) = &plan.confirmation_sha256 {
                    out!("  confirmation: {digest}");
                }
                for refusal in &plan.refusals {
                    out!("  refusal [{}]: {}", refusal.code, refusal.message);
                }
            }
        }
        "apply" => {
            let confirm = parsed.confirm.as_deref().ok_or_else(|| {
                UsageError::Message("maintenance-lock apply requires --confirm <sha256>".into())
            })?;
            let report = crate::git_maintenance_lock::apply(&cwd, confirm);
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                out!("Git maintenance-lock recovery apply: {:?}", report.status);
                if let Some(target) = &report.target {
                    out!("  exact target: {target}");
                }
                out!("  target unlinked: {}", report.target_unlinked);
                out!("  receipt written: {}", report.receipt_written);
                for refusal in &report.refusals {
                    out!("  refusal [{}]: {}", refusal.code, refusal.message);
                }
                if let Some(next_action) = &report.next_action {
                    out!("  next action: {next_action}");
                }
            }
            if !report.status.is_complete() {
                return Err(UsageError::Message(
                    report.next_action.clone().unwrap_or_else(|| {
                        format!(
                            "maintenance-lock apply did not complete: {:?}",
                            report.status
                        )
                    }),
                ));
            }
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown maintenance-lock action {other:?}; expected `plan` or `apply`"
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
mod artifact_use_refusal_tests {
    use super::artifact_use_refusal_message;
    use crate::auto_cleanup::ClosedArtifactCheckoutCohortUnavailableReason as Reason;

    #[test]
    fn every_refusal_keeps_its_code_and_says_retaining_is_safe() {
        for (reason, expected) in [
            (Reason::IndexIncomplete, "Plan again later"),
            (Reason::SpawnedWitnessMismatch, "Planning again will not"),
            (Reason::SpawnedOwnerMissing, "adopted session"),
            (Reason::CheckoutMissing, "no longer exists"),
            (Reason::AliasOverlapOutsideCohort, "overlaps"),
            (Reason::NonClosedAlias, "still open"),
            (Reason::SessionChanged, "gc plan --json"),
        ] {
            let message = artifact_use_refusal_message(reason, 7, false, false);
            assert!(
                message.starts_with(&format!(
                    "artifact-use reconciliation refused: {}; no markers were changed.",
                    reason.code()
                )),
                "{message}"
            );
            assert!(message.contains(expected), "{message}");
            assert!(message.contains("retained is safe"), "{message}");
            assert!(
                message.contains("Do not remove markers manually"),
                "{message}"
            );
        }
    }

    #[test]
    fn a_changed_session_says_whether_it_is_open_or_already_cleaned() {
        let open = artifact_use_refusal_message(Reason::SessionChanged, 7, true, false);
        assert!(open.contains("Session 7 is still open"), "{open}");
        let cleaned = artifact_use_refusal_message(Reason::SessionChanged, 7, false, true);
        assert!(
            cleaned.contains("Session 7 has already been cleaned"),
            "{cleaned}"
        );
    }
}

#[cfg(test)]
mod reclaim_plan_tests {
    use super::*;
    use crate::reclaim::{InFlightClaim, InFlightPlan, claim_in_flight, classify, record_sizes};
    use std::time::Duration;

    #[test]
    fn a_budget_override_only_ever_shortens() {
        let default = Duration::from_secs(60);
        assert_eq!(shortened_budget(default, None), default);
        assert_eq!(shortened_budget(default, Some("0")), Duration::ZERO);
        assert_eq!(
            shortened_budget(default, Some(" 1500 ")),
            Duration::from_millis(1500)
        );
        assert_eq!(shortened_budget(default, Some("600000")), default);
        assert_eq!(shortened_budget(default, Some("-1")), default);
        assert_eq!(shortened_budget(default, Some("soon")), default);
    }

    /// One reclaimable candidate holding a 32-byte file, and a marker naming
    /// this live process as another whole-root plan in flight.
    fn root_with_a_live_holder() -> (
        tempfile::TempDir,
        Vec<crate::ReclaimCandidate>,
        InFlightPlan,
        crate::reclaim::InFlightGuard,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("idle/target")).unwrap();
        std::fs::write(root.join("idle/target/x"), [b'x'; 32]).unwrap();
        let candidates = vec![classify(
            &root.join("idle/target"),
            &root.join("idle"),
            0,
            &[],
        )];
        let holder = InFlightPlan::current(now_ms());
        let guard = match claim_in_flight(root, holder.clone(), &|_| true).unwrap() {
            InFlightClaim::Claimed(guard) => guard,
            InFlightClaim::HeldBy(other) => panic!("fresh root already held by {other:?}"),
        };
        (tmp, candidates, holder, guard)
    }

    /// `--full-scan` used to wait 20 s for a budgeted holder and then list
    /// everything unmeasured. Unbounded, it must measure at once: with a
    /// live holder and no deadline, any wait would never end.
    #[test]
    fn a_full_scan_never_defers_to_a_plan_in_flight() {
        let (tmp, mut candidates, holder, _guard) = root_with_a_live_holder();

        let (report, phases) = size_whole_root_plan(
            tmp.path(),
            &mut candidates,
            Some(InFlightClaim::HeldBy(holder)),
            None,
        );

        assert_eq!(report.deferred_to, None);
        assert_eq!((report.measured, report.unmeasured), (1, 0));
        assert_eq!(report.budget_ms, None);
        assert_eq!(candidates[0].bytes, Some(32));
        assert_eq!(phases.size_shares, 1, "a full scan shares what it measured");
    }

    /// A budgeted plan that finds a holder waits only within its one sizing
    /// deadline, then reuses whatever the holder has shared so far.
    #[test]
    fn a_budgeted_plan_reuses_what_a_running_holder_has_shared() {
        let (tmp, mut candidates, holder, _guard) = root_with_a_live_holder();
        let shared = [(tmp.path().join("idle/target"), 7_u64)];
        record_sizes(
            tmp.path(),
            now_ms(),
            crate::reclaim::SIZE_REUSE_WINDOW_MS,
            &shared,
        )
        .unwrap();

        let (report, _) = size_whole_root_plan(
            tmp.path(),
            &mut candidates,
            Some(InFlightClaim::HeldBy(holder.clone())),
            Some(Duration::ZERO),
        );

        assert_eq!(report.deferred_to, Some(holder));
        assert_eq!((report.reused, report.measured), (1, 0));
        assert_eq!(candidates[0].bytes, Some(7));
        assert_eq!(report.budget_ms, Some(0));
    }

    /// The `reclaim.apply` row counts kept candidates by protection, never
    /// by their free-text reasons, which can name paths and processes.
    #[test]
    fn the_apply_metric_counts_kept_candidates_by_protection_only() {
        let root = std::path::Path::new("/r");
        let active = classify(
            &root.join("a/target"),
            &root.join("a"),
            0,
            &[root.join("a")],
        );
        let idle = classify(&root.join("b/target"), &root.join("b"), 0, &[]);
        let metric = ReclaimApplyMetric {
            session_scoped: false,
            exact_match: false,
            scan_ms: 1,
            protection_ms: 2,
            review_ms: 3,
            removal_ms: 0,
            candidates_scanned: 2,
            reviewed_reclaimable: 1,
            withdrawn: 0,
        };

        let row = metric.details(&[active, idle], None, Some("review_unavailable"));

        assert_eq!(row["phase_counts"]["kept_active_session"], 1);
        assert_eq!(row["phase_counts"]["authorized"], 1);
        assert_eq!(row["phase_counts"]["removed"], 0);
        assert_eq!(row["reclaim_apply"]["refused"], "review_unavailable");
        assert_eq!(row["reclaim_apply"]["review"], "saved_review");
        assert_eq!(row["phase_timings_ms"]["review"], 3);
        assert!(!row.to_string().contains("/r/"), "{row}");
    }
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

    /// The lane process's body records `checkout_trash_catchup` (with the
    /// I/O policy) into the command's recorder, which the command metric
    /// row is built from; the hidden action is refused outside the worker.
    #[test]
    fn the_trash_catchup_lane_records_its_attachment_and_is_refused_in_tests() {
        let (owner, _storage, broker) = worker_environment_fixture();
        let host = owner.path().join("host-state");
        std::fs::create_dir_all(&host).unwrap();
        let recorder = crate::phase_telemetry::Recorder::default();
        {
            let _installed = crate::phase_telemetry::install(recorder.clone());
            broker.run_checkout_trash_catchup_process(
                Some(&host),
                Some(crate::clock::epoch_ms()),
                "important",
                crate::checkout_trash::HostPressure::default,
            );
        }
        let attachments = recorder.attachments();
        let lane = attachments
            .get("checkout_trash_catchup")
            .expect("the lane attaches its record");
        assert_eq!(lane["io_policy"], "important", "{lane}");
        assert_eq!(lane["end_reason"], "caught_up", "{lane}");
        // The hidden action runs the worker entry's invocation checks: in a
        // test (or build) environment it is refused before anything opens.
        assert!(
            validate_artifact_sweep_worker_invocation(
                Some("1"),
                false,
                2,
                artifact_worker_test_or_build_environment(),
            )
            .is_err()
        );
    }

    /// `gc trash drain` refuses, removing nothing, while another catch-up
    /// lane holds the host lock, and runs once it is free.
    #[test]
    fn gc_trash_drain_refuses_while_another_lane_is_live() {
        let (owner, _storage, broker) = worker_environment_fixture();
        let host = owner.path().join("host-state");
        std::fs::create_dir_all(&host).unwrap();
        let lane = crate::checkout_trash::try_deleter_lock(
            &host.join(crate::broker::TRASH_CATCHUP_LOCK_FILE),
        )
        .unwrap()
        .unwrap();
        let no_pressure = crate::checkout_trash::HostPressure::default;
        let refused = gc_trash_drain(
            &broker,
            Some(&host),
            std::time::Duration::from_secs(60),
            no_pressure,
        );
        match refused {
            Err(UsageError::Message(message)) => {
                assert_eq!(message, GC_TRASH_DRAIN_LANE_BUSY);
            }
            other => panic!("expected the lane-busy refusal, got {other:?}"),
        }
        drop(lane);
        let ran = gc_trash_drain(
            &broker,
            Some(&host),
            std::time::Duration::from_secs(60),
            no_pressure,
        )
        .unwrap();
        assert_eq!(ran["end_reason"], "caught_up", "{ran}");
        assert_eq!(ran["entries_remaining"], 0, "{ran}");
        assert!(
            gc_trash_drain(
                &broker,
                None,
                std::time::Duration::from_secs(60),
                no_pressure
            )
            .is_err()
        );
    }

    /// The worker entry point resolves the `[cleanup]` source before the
    /// pacing reads it, and the worker run records it for `broker status`.
    #[test]
    fn the_worker_run_resolves_and_records_its_cleanup_config_source() {
        let (_owner, _storage, mut broker) = worker_environment_fixture();
        let root = broker.main_root().to_path_buf();
        {
            let _scopes = enter_worker_run_scopes(&root, crate::gc::BackgroundPriority::default());
            assert!(crate::cleanup_config::installed_for(&root).is_some());
            let _ = broker.run_artifact_sweep_recovery_worker(|| {});
        }
        assert!(crate::cleanup_config::installed_for(&root).is_none());
        let recorded = broker
            .store_ref()
            .meta_get(crate::cleanup_config::SUMMARY_KEY)
            .unwrap()
            .expect("the worker run records its [cleanup] source");
        assert!(recorded.contains("no_default_branch"), "{recorded}");
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
