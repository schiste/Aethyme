//! Compact routine status output. This path starts from `StatusBrief` and does
//! not construct the full `StatusView` or inspect per-session worktrees.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::Serialize;

const SCHEMA: &str = "aethyme.broker-status-compact/v1";

#[derive(Debug, Serialize)]
pub(crate) struct CompactStatus {
    schema: &'static str,
    summary: crate::StatusSummary,
    data_freshness: CompactFreshness,
    advice: CompactAdvice,
    waiters: CompactWaiters,
    phase_timings_ms: BTreeMap<String, u64>,
    omitted_details: &'static [&'static str],
    drilldown: CompactDrilldown,
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
            summary: brief.summary,
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
        let _ = writeln!(output, "Broker status (compact)");
        let _ = writeln!(output, "{}", self.summary.message);
        let _ = writeln!(
            output,
            "Sessions: {} active, {} idle, {} stale; {} dirty; {} overlap pairs.",
            self.summary.active_sessions,
            self.summary.idle_sessions,
            self.summary.stale_sessions,
            self.summary.dirty_sessions,
            self.summary.overlap_count,
        );
        let lease_freshness = match self.data_freshness.leases_refreshed_at_ms {
            Some(at) => format!("last recorded refresh at {at}"),
            None => "no lease refresh has been recorded".to_string(),
        };
        let _ = writeln!(
            output,
            "Lease data: recorded, not refreshed ({lease_freshness})."
        );
        if self.data_freshness.deferred_checks.is_empty() {
            let _ = writeln!(output, "Deferred checks: none.");
        } else {
            let _ = writeln!(
                output,
                "Deferred checks (unknown, not clean): {}.",
                self.data_freshness.deferred_checks.join(", ")
            );
        }
        if !self.summary.commands.is_empty() {
            let _ = writeln!(output, "Next:");
            for command in &self.summary.commands {
                let _ = writeln!(output, "  run: {command}");
            }
        }
        let _ = writeln!(
            output,
            "Advice: {} categories, {} observations.",
            self.advice.category_count, self.advice.observation_count
        );
        for category in &self.advice.categories {
            let duplicates = if category.duplicate_count == 0 {
                String::new()
            } else {
                format!(", {} duplicates", category.duplicate_count)
            };
            let _ = writeln!(
                output,
                "- {} [{}; count {}{}]: {}",
                category.id, category.severity, category.count, duplicates, category.summary
            );
            for command in &category.commands {
                let _ = writeln!(output, "  run: {command}");
            }
        }
        let _ = writeln!(
            output,
            "Waiters: {} (per-waiter details omitted).",
            self.waiters.count
        );
        let _ = writeln!(
            output,
            "Details omitted: reasons, evidence, item identities and waiter resources."
        );
        let _ = writeln!(output, "Full details: {}.", self.drilldown.full_status_json);
        let _ = writeln!(output, "Fresh audit: {}.", self.drilldown.fresh_audit_json);
        output
    }
}
