//! Compact routine status output. This path starts from `StatusBrief` and does
//! not construct the full `StatusView` or inspect per-session worktrees.

use std::collections::{BTreeMap, BTreeSet};
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
    pub(crate) fn from_brief(brief: crate::StatusBrief) -> Self {
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
                count: brief.waiters.len(),
                details_omitted: true,
            },
            phase_timings_ms: brief.phase_timings_ms,
            omitted_details: &[
                "advice.reason",
                "advice.evidence",
                "advice.session_id",
                "advice.queue_entry_id",
                "per-observation summary variants when a category is grouped",
                "waiter identities and resources",
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
            "Sessions: {} live ({} active, {} idle, {} stale); {} recorded overlap pairs.",
            self.summary.live_sessions,
            self.summary.active_sessions,
            self.summary.idle_sessions,
            self.summary.stale_sessions,
            self.summary.overlap_count,
        );
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
            "Waiters: {} (per-waiter details omitted).",
            self.waiters.count
        );
        line!("Details omitted: reasons, evidence, item identities and waiter resources.");
        line!("Full details: {}.", self.drilldown.full_status_json);
        line!("Fresh audit: {}.", self.drilldown.fresh_audit_json);
        output
    }
}
