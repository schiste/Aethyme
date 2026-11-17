//! Compact routine status output. This path starts from `StatusBrief` and does
//! not construct the full `StatusView` or inspect per-session worktrees.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::Serialize;

const SCHEMA: &str = "aethyme.broker-status-compact/v1";

#[derive(Debug, Serialize)]
pub(crate) struct CompactStatus {
    schema: &'static str,
    summary: CompactSummary,
    data_freshness: CompactFreshness,
    advice: CompactAdvice,
    waiters: CompactWaiters,
    session_contexts: BoundedRecords,
    #[serde(skip_serializing_if = "Option::is_none")]
    collaboration_fence: Option<crate::CollaborationFence>,
    lease_release_requests: BoundedRecords,
    coordinated_operations: CompactOperations,
    phase_timings_ms: BTreeMap<String, u64>,
    omitted_details: &'static [&'static str],
    drilldown: CompactDrilldown,
}

#[derive(Debug, Serialize)]
struct CompactSummary {
    message: String,
    live_sessions: usize,
    active_sessions: usize,
    idle_sessions: usize,
    stale_sessions: usize,
    dirty_sessions: Option<usize>,
    overlap_count: usize,
    promoted_conflict_count: Option<usize>,
    integration_relation: crate::StatusIntegrationRelation,
    integration_ahead_main_commits: Option<u64>,
    integration_head: Option<String>,
    baseline_ref: Option<String>,
    baseline_head: Option<String>,
    main_head: Option<String>,
    integration_ahead_local_main_commits: Option<u64>,
    may_move_integration: bool,
    commands: Vec<String>,
    unknown_fields: Vec<&'static str>,
}

impl CompactSummary {
    fn from_brief(summary: crate::StatusSummary) -> Self {
        let integration_checked =
            summary.integration_relation != crate::StatusIntegrationRelation::NotChecked;
        let integration_ahead_main_commits =
            integration_checked.then_some(summary.integration_ahead_main_commits);
        let integration_head = integration_checked.then_some(summary.integration_head);
        let baseline_ref = integration_checked.then_some(summary.baseline_ref);
        let baseline_head = integration_checked.then_some(summary.baseline_head);
        let main_head = integration_checked.then_some(summary.main_head);
        let integration_ahead_local_main_commits =
            integration_checked.then_some(summary.integration_ahead_local_main_commits);
        let mut unknown_fields = vec!["dirty_sessions", "promoted_conflict_count"];
        if integration_ahead_main_commits.is_none() {
            unknown_fields.push("integration_ahead_main_commits");
        }
        if integration_head.is_none() {
            unknown_fields.push("integration_head");
        }
        if baseline_ref.is_none() {
            unknown_fields.push("baseline_ref");
        }
        if baseline_head.is_none() {
            unknown_fields.push("baseline_head");
        }
        if main_head.is_none() {
            unknown_fields.push("main_head");
        }
        if integration_ahead_local_main_commits.is_none() {
            unknown_fields.push("integration_ahead_local_main_commits");
        }
        Self {
            message: "No current broker session in this checkout.".into(),
            live_sessions: summary.live_sessions,
            active_sessions: summary.active_sessions,
            idle_sessions: summary.idle_sessions,
            stale_sessions: summary.stale_sessions,
            dirty_sessions: None,
            overlap_count: summary.overlap_count,
            promoted_conflict_count: None,
            integration_relation: summary.integration_relation,
            integration_ahead_main_commits,
            integration_head,
            baseline_ref,
            baseline_head,
            main_head,
            integration_ahead_local_main_commits,
            may_move_integration: summary.may_move_integration,
            commands: vec!["aethyme broker start --task \"<task>\"".into()],
            unknown_fields,
        }
    }
}

#[derive(Debug, Serialize)]
struct CompactFreshness {
    leases_refreshed: bool,
    leases_refreshed_at_ms: Option<i64>,
    deferred_checks: Vec<String>,
    deferred_checks_are_unknown: bool,
}

#[derive(Debug, Serialize)]
struct CompactAdvice {
    category_count: usize,
    observation_count: usize,
    categories: Vec<CompactAdviceCategory>,
}

#[derive(Debug, Serialize)]
struct CompactAdviceCategory {
    id: String,
    severity: String,
    count: usize,
    duplicate_count: usize,
    summary: String,
    summary_variants_omitted: bool,
    commands: Vec<String>,
    commands_omitted_count: usize,
    details_omitted: bool,
}

#[derive(Debug, Serialize)]
struct CompactWaiters {
    count: usize,
    details_omitted: bool,
    omitted_count: usize,
    records: [(); 0],
}

#[derive(Debug, Serialize)]
struct BoundedRecords {
    count: usize,
    omitted_count: usize,
    records: [(); 0],
}

impl BoundedRecords {
    fn new(count: usize) -> Self {
        Self {
            count,
            omitted_count: count,
            records: [],
        }
    }
}

#[derive(Debug, Serialize)]
struct CompactOperations {
    count: usize,
    holding_lock_count: usize,
    waiting_count: usize,
    omitted_count: usize,
    records: [(); 0],
}

#[derive(Debug, Serialize)]
struct CompactDrilldown {
    full_status_json: &'static str,
    fresh_audit_json: &'static str,
}

/// Repository observations are counted by category. Their identities,
/// summaries and recovery instructions belong to the full report.
fn compact_advice(advice: Vec<crate::StatusAdvice>) -> CompactAdvice {
    let observation_count = advice.len();
    let mut groups = BTreeMap::<_, (usize, usize)>::new();
    for item in advice {
        let (count, omitted_commands) = groups
            .entry((item.id.to_string(), item.severity.as_str().to_string()))
            .or_default();
        *count += 1;
        *omitted_commands += item.commands.len();
    }
    let categories: Vec<_> = groups
        .into_iter()
        .map(
            |((id, severity), (count, commands_omitted_count))| CompactAdviceCategory {
                id,
                severity,
                count,
                duplicate_count: count.saturating_sub(1),
                summary: format!("{count} recorded observation(s)"),
                summary_variants_omitted: true,
                commands: Vec::new(),
                commands_omitted_count,
                details_omitted: true,
            },
        )
        .collect();
    CompactAdvice {
        category_count: categories.len(),
        observation_count,
        categories,
    }
}

impl CompactStatus {
    pub(crate) fn from_brief(
        brief: crate::StatusBrief,
        context: crate::broker::RoutineStatusContext,
    ) -> Self {
        let session_contexts = BoundedRecords::new(context.session_contexts.len());
        let lease_release_requests = BoundedRecords::new(context.lease_release_requests.len());
        let operations_count = context.coordinated_operations.len();
        let holding_lock_count = context
            .coordinated_operations
            .iter()
            .filter(|op| op.holding_lock)
            .count();
        let waiting_count = context
            .coordinated_operations
            .iter()
            .filter(|op| !op.holding_lock && op.status == "prepared")
            .count();
        let coordinated_operations = CompactOperations {
            count: operations_count,
            holding_lock_count,
            waiting_count,
            omitted_count: operations_count,
            records: [],
        };
        let waiter_count = brief.waiters.len();

        Self {
            schema: SCHEMA,
            summary: CompactSummary::from_brief(brief.summary),
            data_freshness: CompactFreshness {
                leases_refreshed: brief.leases_refreshed,
                leases_refreshed_at_ms: brief.leases_refreshed_at_ms,
                deferred_checks: brief.deferred_checks,
                deferred_checks_are_unknown: true,
            },
            advice: compact_advice(brief.advice),
            waiters: CompactWaiters {
                count: waiter_count,
                details_omitted: waiter_count > 0,
                omitted_count: waiter_count,
                records: [],
            },
            session_contexts,
            collaboration_fence: context.collaboration_fence,
            lease_release_requests,
            coordinated_operations,
            phase_timings_ms: brief.phase_timings_ms,
            omitted_details: &[
                "advice.reason",
                "advice.evidence",
                "advice.session_id",
                "advice.queue_entry_id",
                "per-observation summaries and commands",
                "session contexts, pending lease requests, operations and waiters",
                "lease request reasons, operation liveness details and other per-session metadata",
            ],
            drilldown: CompactDrilldown {
                full_status_json: "aethyme broker status --full --json",
                fresh_audit_json: "aethyme broker status --refresh --json",
            },
        }
    }

    pub(crate) fn render_text(&self) -> String {
        let mut output = String::new();
        macro_rules! line {
            ($($arg:tt)*) => {
                writeln!(output, $($arg)*).expect("writing to String cannot fail");
            };
        }

        line!("Broker status (compact)");
        line!("{}", self.summary.message);
        line!(
            "Sessions: {} live ({} active, {} idle, {} stale); {} recorded lease-overlap observations.",
            self.summary.live_sessions,
            self.summary.active_sessions,
            self.summary.idle_sessions,
            self.summary.stale_sessions,
            self.summary.overlap_count,
        );
        line!(
            "Repository information: {} recorded warning(s) and other observations, {} pending lease release request(s), {} unresolved operation(s), {} waiter(s).",
            self.advice.observation_count,
            self.lease_release_requests.count,
            self.coordinated_operations.count,
            self.waiters.count,
        );
        if self.collaboration_fence.is_some() {
            line!("Repository information: recorded collaboration fence present.");
        }
        let lease_freshness = match self.data_freshness.leases_refreshed_at_ms {
            Some(at) => format!("last recorded refresh at {at}"),
            None => "no lease refresh has been recorded".to_string(),
        };
        line!("Lease data: recorded, not refreshed ({lease_freshness}).");
        line!("Dirty sessions and promoted conflicts: not checked.");
        let integration_relation = match self.summary.integration_relation {
            crate::StatusIntegrationRelation::NotChecked => "not checked",
            crate::StatusIntegrationRelation::CurrentWithMain => "current with baseline",
            crate::StatusIntegrationRelation::AheadOfMain => "ahead of baseline",
            crate::StatusIntegrationRelation::DivergedFromMain => "diverged from baseline",
        };
        line!("Integration relation: {integration_relation}.");
        if self.data_freshness.deferred_checks.is_empty() {
            line!("Deferred checks: none.");
        } else {
            line!(
                "Deferred checks (unknown, not clean): {}.",
                self.data_freshness.deferred_checks.join(", ")
            );
        }
        if !self.summary.commands.is_empty() {
            line!("Next:");
            for command in &self.summary.commands {
                line!("  run: {command}");
            }
        }
        line!("Full details: {}.", self.drilldown.full_status_json);
        line!("Fresh audit: {}.", self.drilldown.fresh_audit_json);
        output
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn repository_advice_retains_category_without_repeating_details() {
        let temp = tempfile::tempdir().unwrap();
        let config = crate::transcripts::TranscriptsConfig::default();
        let advice =
            crate::transcripts::default_on_advice_at(temp.path(), &config, 1_800_000_000_000)
                .unwrap();
        let command_count = advice.commands.len();
        let compact = super::compact_advice(vec![advice]);
        let category = &compact.categories[0];
        assert_eq!(category.id, "transcripts.archiving-default-on");
        assert_eq!(category.severity, "notice");
        assert_eq!(category.count, 1);
        assert_eq!(category.commands_omitted_count, command_count);
        assert!(category.commands.is_empty());
        assert_eq!(category.summary, "1 recorded observation(s)");
    }
}
