//! Explicit, observational memory diagnosis; never process or cleanup authority.
use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;
use std::time::Duration;

use crate::session_holder::{AgentProcess, ProcessTable};

pub(crate) const SESSION_LIMIT: usize = 200;
const GROUP_LIMIT: usize = 20;
const ID_LIMIT: usize = 8;
const PROCESS_LIMIT: usize = 16_384;

pub(crate) struct Owner {
    pub session_id: i64,
    pub inactive: bool,
    pub idle_ms: i64,
    pub holder: Option<AgentProcess>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct Report {
    schema: &'static str,
    observation: &'static str,
    session_count: usize,
    omitted_groups: usize,
    memory_observation: &'static str,
    groups: Vec<Group>,
}

#[derive(Debug, serde::Serialize)]
struct Group {
    holder_pid: Option<i64>,
    inactive_session_ids: Vec<i64>,
    omitted_inactive_sessions: usize,
    active_owner_count: usize,
    oldest_idle_ms: i64,
    // Sum of process RSS, not private memory or reclaimable bytes. Shared pages
    // can appear in more than one process's RSS even within a single tree.
    agent_tree_rss_bytes: Option<u64>,
    swap_bytes: Option<u64>,
    identity_observation: &'static str,
    recommendation: &'static str,
}

struct MemorySnapshot {
    identities: ProcessTable,
    rows: BTreeMap<i64, (i64, u64)>,
}

impl MemorySnapshot {
    fn parse(text: &str) -> Option<Self> {
        let mut rows = BTreeMap::new();
        let mut identities = String::new();
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let fields: Vec<_> = line.split_whitespace().collect();
            if fields.len() < 8 || rows.len() >= PROCESS_LIMIT {
                return None;
            }
            let pid = fields[0].parse::<i64>().ok().filter(|pid| *pid > 0)?;
            let ppid = fields[1].parse::<i64>().ok().filter(|pid| *pid >= 0)?;
            let rss = fields[2].parse::<u64>().ok()?.checked_mul(1024)?;
            if rows.insert(pid, (ppid, rss)).is_some() {
                return None;
            }
            identities.push_str(&format!(
                "{} {} {}\n",
                fields[0],
                fields[1],
                fields[3..].join(" ")
            ));
        }
        if rows.is_empty() {
            return None;
        }
        Some(Self {
            identities: ProcessTable::parse(&identities),
            rows,
        })
    }

    fn capture() -> Option<Self> {
        let output = crate::bounded_output::output_limited(
            Command::new("ps").args(["-A", "-ww", "-o", "pid=,ppid=,rss=,lstart=,args="]),
            Duration::from_secs(2),
            4 * 1024 * 1024,
        )
        .ok()?;
        let crate::bounded_output::LimitedOutput::Completed(output) = output else {
            return None;
        };
        if !output.status.success() {
            return None;
        }
        Self::parse(std::str::from_utf8(&output.stdout).ok()?)
    }

    fn totals(&self, roots: &BTreeSet<i64>) -> Option<BTreeMap<i64, u64>> {
        let mut totals: BTreeMap<i64, u64> = roots.iter().map(|pid| (*pid, 0)).collect();
        for (&pid, &(_, rss)) in &self.rows {
            let mut cursor = pid;
            let mut seen = BTreeSet::new();
            for depth in 0..=128 {
                if roots.contains(&cursor) {
                    let total = totals.get_mut(&cursor)?;
                    *total = total.checked_add(rss)?;
                    break;
                }
                if cursor == 0 {
                    break;
                }
                if depth == 128 || !seen.insert(cursor) {
                    return None;
                }
                let Some(&(parent, _)) = self.rows.get(&cursor) else {
                    // An incomplete ancestry cannot establish which owner holds
                    // this RSS, so do not publish partial memory totals.
                    return None;
                };
                cursor = parent;
            }
        }
        Some(totals)
    }
}

impl Report {
    pub(crate) fn over_budget(session_count: usize) -> Self {
        Self {
            schema: "aethyme.resource-pressure/v1",
            observation: "read_only_not_cleanup_authority",
            session_count,
            omitted_groups: 0,
            memory_observation: "session_inventory_over_budget",
            groups: Vec::new(),
        }
    }

    pub(crate) fn capture(owners: Vec<Owner>) -> Self {
        let snapshot = MemorySnapshot::capture();
        Self::from_snapshot(owners, snapshot.as_ref())
    }

    fn from_snapshot(owners: Vec<Owner>, snapshot: Option<&MemorySnapshot>) -> Self {
        let session_count = owners.len();
        let mut grouped: BTreeMap<(i64, String, String, i64), Vec<Owner>> = BTreeMap::new();
        for owner in owners {
            let key = owner.holder.as_ref().map_or_else(
                || (0, String::new(), String::new(), owner.session_id),
                |holder| {
                    (
                        holder.pid,
                        holder.started.clone(),
                        holder.command.clone(),
                        0,
                    )
                },
            );
            grouped.entry(key).or_default().push(owner);
        }
        let mut groups = Vec::new();
        // Include active-only roots when assigning descendants: nested active
        // agents must not inflate an inactive ancestor's attribution.
        let roots: BTreeSet<_> = grouped
            .values()
            .filter_map(|owners| {
                let holder = owners.first()?.holder.as_ref()?;
                (snapshot?.identities.process(holder.pid).as_ref() == Some(holder))
                    .then_some(holder.pid)
            })
            .collect();
        let totals = snapshot.and_then(|snapshot| snapshot.totals(&roots));
        for owners in grouped.into_values() {
            let inactive: Vec<_> = owners.iter().filter(|owner| owner.inactive).collect();
            if inactive.is_empty() {
                continue;
            }
            let holder = owners[0].holder.clone();
            let matched = holder.as_ref().is_some_and(|holder| {
                roots.contains(&holder.pid)
                    && snapshot.is_some_and(|snapshot| {
                        snapshot.identities.process(holder.pid).as_ref() == Some(holder)
                    })
            });
            let rss = holder
                .as_ref()
                .filter(|_| matched)
                .and_then(|holder| totals.as_ref()?.get(&holder.pid).copied());
            let active_owner_count = owners.len() - inactive.len();
            groups.push(Group {
                holder_pid: holder.as_ref().map(|holder| holder.pid),
                inactive_session_ids: inactive
                    .iter()
                    .take(ID_LIMIT)
                    .map(|owner| owner.session_id)
                    .collect(),
                omitted_inactive_sessions: inactive.len().saturating_sub(ID_LIMIT),
                active_owner_count,
                oldest_idle_ms: inactive
                    .iter()
                    .map(|owner| owner.idle_ms)
                    .max()
                    .unwrap_or(0),
                agent_tree_rss_bytes: rss,
                swap_bytes: None,
                identity_observation: if holder.is_none() {
                    "unrecorded"
                } else if snapshot.is_none() {
                    "unavailable"
                } else if matched {
                    "matched"
                } else {
                    "not_matched"
                },
                recommendation: if active_owner_count > 0 {
                    "shared_with_active_sessions_review_individual_sessions"
                } else {
                    "review_inactive_sessions_no_automatic_termination"
                },
            });
        }
        groups.sort_by(|a, b| {
            b.agent_tree_rss_bytes
                .cmp(&a.agent_tree_rss_bytes)
                .then_with(|| b.oldest_idle_ms.cmp(&a.oldest_idle_ms))
                .then_with(|| a.inactive_session_ids.cmp(&b.inactive_session_ids))
        });
        let omitted_groups = groups.len().saturating_sub(GROUP_LIMIT);
        groups.truncate(GROUP_LIMIT);
        Self {
            schema: "aethyme.resource-pressure/v1",
            observation: "read_only_not_cleanup_authority",
            session_count,
            omitted_groups,
            memory_observation: if totals.is_some() {
                "complete_process_rss_sum_not_reclaimable_memory_swap_unmeasured"
            } else {
                "unavailable_or_incomplete_swap_unmeasured"
            },
            groups,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot() -> MemorySnapshot {
        MemorySnapshot::parse("10 0 100 Mon Oct 5 12:00:00 2026 codex\n11 10 20 Mon Oct 5 12:00:01 2026 child\n12 10 30 Mon Oct 5 12:00:02 2026 codex\n13 12 40 Mon Oct 5 12:00:03 2026 child\n").unwrap()
    }
    fn owner(id: i64, pid: i64, inactive: bool, snapshot: &MemorySnapshot) -> Owner {
        Owner {
            session_id: id,
            inactive,
            idle_ms: id,
            holder: snapshot.identities.process(pid),
        }
    }
    #[test]
    fn shared_holders_count_rss_once_and_protect_active_owners() {
        let snapshot = snapshot();
        let report = Report::from_snapshot(
            vec![
                owner(1, 10, true, &snapshot),
                owner(2, 10, false, &snapshot),
                owner(3, 12, true, &snapshot),
            ],
            Some(&snapshot),
        );
        assert_eq!(report.groups.len(), 2);
        assert_eq!(report.groups[0].agent_tree_rss_bytes, Some(120 * 1024));
        assert_eq!(report.groups[0].active_owner_count, 1);
        assert_eq!(report.groups[0].inactive_session_ids, [1]);
        assert_eq!(report.groups[1].agent_tree_rss_bytes, Some(70 * 1024));
        assert!(
            report.groups[0]
                .recommendation
                .contains("shared_with_active")
        );
        assert!(report.groups.iter().all(|group| group.swap_bytes.is_none()));
    }
    #[test]
    fn reused_pid_and_missing_evidence_never_mean_zero_memory() {
        let snapshot = snapshot();
        let mut stale = owner(1, 10, true, &snapshot);
        stale.holder.as_mut().unwrap().started.push_str(" old");
        let report = Report::from_snapshot(vec![stale], Some(&snapshot));
        assert_eq!(report.groups[0].identity_observation, "not_matched");
        assert_eq!(report.groups[0].agent_tree_rss_bytes, None);
        let report = Report::from_snapshot(vec![owner(1, 10, true, &snapshot)], None);
        assert_eq!(report.groups[0].identity_observation, "unavailable");
        assert_eq!(report.groups[0].agent_tree_rss_bytes, None);
    }
    #[test]
    fn active_only_nested_owner_is_not_charged_to_the_inactive_ancestor() {
        let snapshot = snapshot();
        let report = Report::from_snapshot(
            vec![
                owner(1, 10, true, &snapshot),
                owner(2, 12, false, &snapshot),
            ],
            Some(&snapshot),
        );
        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].agent_tree_rss_bytes, Some(120 * 1024));
        assert_eq!(report.groups[0].active_owner_count, 0);
        let report = Report::from_snapshot(
            (1..=12).map(|id| owner(id, 10, true, &snapshot)).collect(),
            Some(&snapshot),
        );
        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].inactive_session_ids.len(), 8);
        assert_eq!(report.groups[0].omitted_inactive_sessions, 4);
        assert_eq!(report.groups[0].agent_tree_rss_bytes, Some(190 * 1024));
    }

    #[test]
    fn malformed_duplicate_overflow_and_cycles_refuse_memory_totals() {
        for text in [
            "garbage",
            "10 0 -1 Mon Oct 5 12:00:00 2026 codex",
            "10 0 18446744073709551615 Mon Oct 5 12:00:00 2026 codex",
            "10 0 1 Mon Oct 5 12:00:00 2026 codex\n10 0 1 Mon Oct 5 12:00:00 2026 codex",
        ] {
            assert!(MemorySnapshot::parse(text).is_none());
        }
        let cycle = MemorySnapshot::parse(
            "10 11 1 Mon Oct 5 12:00:00 2026 x\n11 10 1 Mon Oct 5 12:00:00 2026 y",
        )
        .unwrap();
        assert!(cycle.totals(&BTreeSet::new()).is_none());
    }
    #[test]
    fn omitted_groups_and_session_ids_are_explicit_and_no_commands_leak() {
        let report = Report::from_snapshot(
            (1..=25)
                .map(|id| Owner {
                    session_id: id,
                    inactive: true,
                    idle_ms: id,
                    holder: None,
                })
                .collect(),
            None,
        );
        assert_eq!(report.groups.len(), GROUP_LIMIT);
        assert_eq!(report.omitted_groups, 5);
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(
            json["groups"][0]["agent_tree_rss_bytes"],
            serde_json::Value::Null
        );
        assert!(json["groups"][0].get("holder").is_none());
        assert_eq!(
            Report::over_budget(201).memory_observation,
            "session_inventory_over_budget"
        );
    }
}
