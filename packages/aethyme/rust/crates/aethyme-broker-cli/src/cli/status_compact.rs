//! Compact routine status output. This path starts from `StatusBrief` and does
//! not construct the full `StatusView` or inspect per-session worktrees.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::Serialize;

const DETAIL_LIMIT: usize = 10;

const SCHEMA: &str = "aethyme.broker-status-compact/v1";

#[derive(Debug, Serialize)]
pub(crate) struct CompactStatus {
    schema: &'static str,
    summary: CompactSummary,
    data_freshness: CompactFreshness,
    advice: CompactAdvice,
    waiters: CompactWaiters,
    session_contexts: BoundedRecords<CompactSessionContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    collaboration_fence: Option<crate::CollaborationFence>,
    lease_release_requests: BoundedRecords<CompactLeaseRequest>,
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
            message: summary.message,
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
            commands: summary.commands,
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
    details_omitted: bool,
}

#[derive(Debug, Serialize)]
struct CompactWaiters {
    count: usize,
    details_omitted: bool,
    omitted_count: usize,
    records: Vec<crate::CurrentWaiter>,
}

#[derive(Debug, Serialize)]
struct BoundedRecords<T> {
    count: usize,
    omitted_count: usize,
    records: Vec<T>,
}

impl<T> BoundedRecords<T> {
    fn new(rows: Vec<T>) -> Self {
        let count = rows.len();
        Self {
            count,
            omitted_count: count.saturating_sub(DETAIL_LIMIT),
            records: rows.into_iter().take(DETAIL_LIMIT).collect(),
        }
    }
}

#[derive(Debug, Serialize)]
struct CompactSessionContext {
    session_id: i64,
    context: String,
}

#[derive(Debug, Serialize)]
struct CompactLeaseRequest {
    request_id: i64,
    requester_session_id: i64,
    holder_session_id: i64,
    path: String,
    #[serde(skip_serializing)]
    requested_at: i64,
}

#[derive(Debug, Serialize)]
struct CompactOperation {
    id: i64,
    session_id: i64,
    repository: String,
    scope: String,
    status: String,
    holding_lock: bool,
    blocked_by: Option<i64>,
}

impl CompactOperation {
    fn role(&self) -> String {
        if self.status == "outcome_unknown" {
            "needs reconciliation".to_string()
        } else if self.holding_lock {
            "holding".to_string()
        } else if let Some(blocker) = self.blocked_by {
            format!("blocked by {blocker}")
        } else {
            "not holding".to_string()
        }
    }
}

#[derive(Debug, Serialize)]
struct CompactOperations {
    count: usize,
    holding_lock_count: usize,
    waiting_count: usize,
    omitted_count: usize,
    records: Vec<CompactOperation>,
}

#[derive(Debug, Serialize)]
struct CompactDrilldown {
    full_status_json: &'static str,
    fresh_audit_json: &'static str,
}

#[derive(Default)]
struct AdviceGroup {
    count: usize,
    summaries: BTreeSet<String>,
    commands: BTreeSet<String>,
}

impl CompactStatus {
    pub(crate) fn from_brief(
        brief: crate::StatusBrief,
        context: crate::broker::RoutineStatusContext,
    ) -> Self {
        let session_contexts = BoundedRecords::new(
            context
                .session_contexts
                .into_iter()
                .map(|(session_id, context)| CompactSessionContext {
                    session_id,
                    context,
                })
                .collect(),
        );
        let lease_release_requests = BoundedRecords::new(
            context
                .lease_release_requests
                .into_iter()
                .map(|request| CompactLeaseRequest {
                    request_id: request.request_id,
                    requester_session_id: request.requester_session_id,
                    holder_session_id: request.holder_session_id,
                    path: request.path,
                    requested_at: request.requested_at,
                })
                .collect(),
        );
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
            omitted_count: operations_count.saturating_sub(DETAIL_LIMIT),
            records: context
                .coordinated_operations
                .into_iter()
                .take(DETAIL_LIMIT)
                .map(|op| CompactOperation {
                    id: op.id,
                    session_id: op.session_id,
                    repository: op.repository,
                    scope: op.scope,
                    status: op.status,
                    holding_lock: op.holding_lock,
                    blocked_by: op.blocked_by,
                })
                .collect(),
        };
        let waiter_count = brief.waiters.len();
        let observation_count = brief.advice.len();
        let mut groups = BTreeMap::<(String, String), AdviceGroup>::new();
        for item in brief.advice {
            let key = (item.id.to_string(), item.severity.as_str().to_string());
            let group = groups.entry(key).or_default();
            group.count += 1;
            group.summaries.insert(item.summary);
            group.commands.extend(item.commands);
        }

        let categories = groups
            .into_iter()
            .map(|((id, severity), group)| {
                let summary_variants_omitted = group.summaries.len() > 1;
                let summary = if let Some(summary) = group.summaries.first() {
                    if summary_variants_omitted {
                        format!(
                            "{} observations in this category; per-observation summaries are omitted",
                            group.count
                        )
                    } else {
                        summary.clone()
                    }
                } else {
                    format!("{} observations in this category", group.count)
                };
                CompactAdviceCategory {
                    id,
                    severity,
                    count: group.count,
                    duplicate_count: group.count.saturating_sub(1),
                    summary,
                    summary_variants_omitted,
                    commands: group.commands.into_iter().collect(),
                    details_omitted: true,
                }
            })
            .collect::<Vec<_>>();
        let category_count = categories.len();

        Self {
            schema: SCHEMA,
            summary: CompactSummary::from_brief(brief.summary),
            data_freshness: CompactFreshness {
                leases_refreshed: brief.leases_refreshed,
                leases_refreshed_at_ms: brief.leases_refreshed_at_ms,
                deferred_checks: brief.deferred_checks,
                deferred_checks_are_unknown: true,
            },
            advice: CompactAdvice {
                category_count,
                observation_count,
                categories,
            },
            waiters: CompactWaiters {
                count: waiter_count,
                details_omitted: waiter_count > DETAIL_LIMIT,
                omitted_count: waiter_count.saturating_sub(DETAIL_LIMIT),
                records: brief.waiters.into_iter().take(DETAIL_LIMIT).collect(),
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
                "per-observation summary variants when a category is grouped",
                "session contexts, pending lease requests, operations and waiters after the first 10 rows of each list",
                "lease request reasons, operation liveness details and other per-session metadata",
            ],
            drilldown: CompactDrilldown {
                full_status_json: "aethyme broker status --json",
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
        for session in &self.session_contexts.records {
            line!("  session {}: {}", session.session_id, session.context);
        }
        if self.session_contexts.omitted_count > 0 {
            line!(
                "  {} additional session contexts omitted; inspect full status.",
                self.session_contexts.omitted_count
            );
        }
        if let Some(fence) = &self.collaboration_fence {
            line!(
                "Collaboration fence: broker.db requires schema {} or newer ({}; {}).",
                fence.min_compatible_schema,
                fence.reason,
                fence.state
            );
        }
        if self.lease_release_requests.count > 0 {
            line!(
                "Lease release requests: {} pending (they do not block edits in separate worktrees)",
                self.lease_release_requests.count
            );
            for request in &self.lease_release_requests.records {
                let waited = crate::clock::epoch_ms()
                    .saturating_sub(request.requested_at)
                    .max(0) as u64
                    / 1_000;
                line!(
                    "  request {}: session {} asked session {} to release {} ({} ago)",
                    request.request_id,
                    request.requester_session_id,
                    request.holder_session_id,
                    request.path,
                    crate::operations::humanize_duration(waited)
                );
            }
            line!("  Inspect the holder's task and relevant diff before following up or waiting.");
            if self.lease_release_requests.omitted_count > 0 {
                line!(
                    "  {} additional pending lease requests omitted; inspect full status.",
                    self.lease_release_requests.omitted_count
                );
            }
        }
        if self.coordinated_operations.count > 0 {
            line!(
                "Coordinated operations: {} unresolved, {} holding a write lock, {} waiting (recorded prepared rows; process waits not verified)",
                self.coordinated_operations.count,
                self.coordinated_operations.holding_lock_count,
                self.coordinated_operations.waiting_count
            );
            for op in &self.coordinated_operations.records {
                let role = op.role();
                line!(
                    "  op {} sess {} lock {} scope {} {} {}",
                    op.id,
                    op.session_id,
                    op.repository,
                    op.scope,
                    op.status,
                    role
                );
            }
            if self.coordinated_operations.omitted_count > 0 {
                line!(
                    "  {} additional operations omitted.",
                    self.coordinated_operations.omitted_count
                );
            }
            line!("  inspect: aethyme broker advanced operations list");
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
        line!(
            "Advice: {} categories, {} observations.",
            self.advice.category_count,
            self.advice.observation_count
        );
        for category in &self.advice.categories {
            let duplicates = if category.duplicate_count == 0 {
                String::new()
            } else {
                format!(", {} duplicates", category.duplicate_count)
            };
            line!(
                "- {} [{}; count {}{}]: {}",
                category.id,
                category.severity,
                category.count,
                duplicates,
                category.summary
            );
            for command in &category.commands {
                line!("  run: {command}");
            }
        }
        line!(
            "Waiters: {} ({} additional rows omitted).",
            self.waiters.count,
            self.waiters.omitted_count
        );
        for waiter in &self.waiters.records {
            line!("  {}", crate::waiters::describe(waiter));
        }
        line!(
            "Details omitted: advice reasons/evidence, request reasons, operation liveness and other session metadata."
        );
        line!("Full details: {}.", self.drilldown.full_status_json);
        line!("Fresh audit: {}.", self.drilldown.fresh_audit_json);
        output
    }
}

#[cfg(test)]
mod tests {
    use super::CompactOperation;

    #[test]
    fn compact_operation_roles_do_not_invent_waits() {
        let mut operation = CompactOperation {
            id: 1,
            session_id: 2,
            repository: "fixture/repo".into(),
            scope: "repository".into(),
            status: "outcome_unknown".into(),
            holding_lock: false,
            blocked_by: None,
        };
        assert_eq!(operation.role(), "needs reconciliation");
        operation.status = "prepared".into();
        assert_eq!(operation.role(), "not holding");
        operation.blocked_by = Some(3);
        assert_eq!(operation.role(), "blocked by 3");
        operation.status = "outcome_unknown".into();
        assert_eq!(operation.role(), "needs reconciliation");
        operation.status = "running".into();
        operation.holding_lock = true;
        operation.blocked_by = None;
        assert_eq!(operation.role(), "holding");
    }
}
