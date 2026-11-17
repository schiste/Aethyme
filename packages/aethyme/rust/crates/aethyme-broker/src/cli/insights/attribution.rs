use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use serde::{Deserialize, Serialize};

const MAX_HISTORY_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Default, Serialize)]
pub(super) struct TaskCosts {
    completed_tasks: usize,
    completed_tasks_with_metrics: usize,
    history_limited: bool,
    history_read_error: Option<String>,
    legacy_or_invalid_command_rows: u64,
    unattributed_command_rows: u64,
    completed_task_totals: Costs,
    tasks_truncated: bool,
    tasks: Vec<TaskRow>,
}

#[derive(Serialize)]
struct TaskRow {
    session_id: i64,
    started_at_ms: i64,
    completed: bool,
    costs: Costs,
}

type TaskKey = (i64, i64);

struct TaskWindows(BTreeMap<i64, Vec<i64>>);

impl TaskWindows {
    fn new(input: &crate::insights::InsightsInput, events: &[crate::types::Event]) -> Self {
        let mut starts: BTreeMap<_, _> = input
            .sessions
            .iter()
            .map(|session| (session.session_id, vec![session.created_at]))
            .collect();
        for event in events
            .iter()
            .filter(|event| event.kind == crate::events::SESSION_REUSED)
        {
            if let Some(list) = event.session_id.and_then(|id| starts.get_mut(&id)) {
                list.push(event.ts);
            }
        }
        for list in starts.values_mut() {
            list.sort_unstable();
            list.dedup();
        }
        Self(starts)
    }

    fn at(&self, id: i64, timestamp: i64) -> Option<TaskKey> {
        let starts = self.0.get(&id)?;
        let index = starts
            .partition_point(|start| *start <= timestamp)
            .checked_sub(1)?;
        Some((id, starts[index]))
    }
}

#[derive(Default, Serialize)]
struct Costs {
    commands: u64,
    wall_ms: u64,
    child_command_ms: u64,
    queue_wait_ms: u64,
    broker_and_unattributed_ms: u64,
    output_bytes: u64,
    retries: u64,
    outcomes: BTreeMap<String, u64>,
    conflicts_detected: u64,
    conflicts_resolved: u64,
    reported_duplicate_work_avoided: u64,
}

#[derive(Deserialize)]
struct Record {
    ts: i64,
    session_id: Option<i64>,
    duration_ms: u64,
    output_bytes: u64,
    outcome: String,
    attribution: Attribution,
}

#[derive(Deserialize)]
struct Attribution {
    child_command_ms: u64,
    queue_wait_ms: u64,
    broker_and_unattributed_ms: u64,
    retries: u64,
}

impl Costs {
    fn add_record(&mut self, row: Record) {
        self.commands += 1;
        self.wall_ms += row.duration_ms;
        self.child_command_ms += row.attribution.child_command_ms;
        self.queue_wait_ms += row.attribution.queue_wait_ms;
        self.broker_and_unattributed_ms += row.attribution.broker_and_unattributed_ms;
        self.output_bytes += row.output_bytes;
        self.retries += row.attribution.retries;
        if matches!(
            row.outcome.as_str(),
            "succeeded" | "outcome_unknown" | "refused" | "environment_failure" | "failed"
        ) {
            *self.outcomes.entry(row.outcome).or_default() += 1;
        }
    }

    fn add(&mut self, item: &Self) {
        self.commands += item.commands;
        self.wall_ms += item.wall_ms;
        self.child_command_ms += item.child_command_ms;
        self.queue_wait_ms += item.queue_wait_ms;
        self.broker_and_unattributed_ms += item.broker_and_unattributed_ms;
        self.output_bytes += item.output_bytes;
        self.retries += item.retries;
        self.conflicts_detected += item.conflicts_detected;
        self.conflicts_resolved += item.conflicts_resolved;
        self.reported_duplicate_work_avoided += item.reported_duplicate_work_avoided;
        for (outcome, count) in &item.outcomes {
            *self.outcomes.entry(outcome.clone()).or_default() += count;
        }
    }
}

pub(super) fn collect(
    root: &Path,
    since_ms: i64,
    limit: usize,
    input: &crate::insights::InsightsInput,
    events: &[crate::types::Event],
) -> TaskCosts {
    let windows = TaskWindows::new(input, events);
    let mut report = TaskCosts::default();
    let mut costs = BTreeMap::<TaskKey, Costs>::new();
    let mut completed = BTreeSet::new();
    let mut mark_completion = |id: i64, at: i64| {
        if at >= since_ms
            && let Some(task) = windows.at(id, at)
        {
            completed.insert(task);
        }
    };
    for row in &input.sessions {
        if let Some(at) = row.closed_at {
            mark_completion(row.session_id, at);
        }
    }
    for pr in &input.pull_requests {
        if let Some(at) = pr.merged_at {
            for id in &pr.session_ids {
                mark_completion(*id, at);
            }
        }
    }
    let mut pending = BTreeSet::new();
    let mut duplicates = BTreeSet::new();
    for event in events.iter().filter(|e| e.ts >= since_ms) {
        let Some(key) = event.session_id.and_then(|id| windows.at(id, event.ts)) else {
            continue;
        };
        match event.kind.as_str() {
            "session.closed" | "session.cleaned" | "merge.promoted" | "merge.externally_landed" => {
                mark_completion(key.0, event.ts);
                if event.kind.starts_with("merge.") && pending.remove(&key) {
                    costs.entry(key).or_default().conflicts_resolved += 1;
                }
            }
            "merge.conflict" if pending.insert(key) => {
                costs.entry(key).or_default().conflicts_detected += 1;
            }
            "merge.verified" if pending.remove(&key) => {
                costs.entry(key).or_default().conflicts_resolved += 1;
            }
            crate::events::DUPLICATE_WORK_AVOIDED if duplicates.insert(key) => {
                costs
                    .entry(key)
                    .or_default()
                    .reported_duplicate_work_avoided += 1;
            }
            _ => {}
        }
    }
    if let Err(error) = read_command_costs(root, since_ms, &windows, &mut report, &mut costs) {
        report.history_read_error = Some(error.kind().to_string());
    }

    report.completed_tasks = completed.len();
    let mut observed = costs.into_iter().collect::<Vec<_>>();
    observed.sort_unstable_by_key(|((id, started), _)| std::cmp::Reverse((*started, *id)));
    report.tasks_truncated = observed.len() > limit;
    for ((id, started), cost) in observed {
        let is_complete = completed.contains(&(id, started));
        if is_complete {
            report.completed_tasks_with_metrics += usize::from(cost.commands != 0);
            report.completed_task_totals.add(&cost);
        }
        if report.tasks.len() < limit {
            report.tasks.push(TaskRow {
                session_id: id,
                started_at_ms: started,
                completed: is_complete,
                costs: cost,
            });
        }
    }
    report
}

fn read_command_costs(
    root: &Path,
    since_ms: i64,
    windows: &TaskWindows,
    report: &mut TaskCosts,
    costs: &mut BTreeMap<TaskKey, Costs>,
) -> std::io::Result<()> {
    let mut file = match File::open(root.join(".aethyme/logs/command-metrics.jsonl")) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let size = file.metadata()?.len();
    let from = size.saturating_sub(MAX_HISTORY_BYTES);
    report.history_limited = from != 0;
    file.seek(SeekFrom::Start(from))?;
    let reader = BufReader::new(file.take(size - from));
    // Skip the potentially partial record at the beginning of the tail.
    for line in reader.lines().skip(usize::from(from != 0)) {
        let Ok(row) = serde_json::from_str::<Record>(&line?) else {
            report.legacy_or_invalid_command_rows += 1;
            continue;
        };
        if row.ts < since_ms {
            continue;
        }
        if let Some(key) = row.session_id.and_then(|id| windows.at(id, row.ts)) {
            costs.entry(key).or_default().add_record(row);
        } else {
            report.unattributed_command_rows += 1;
        }
    }
    Ok(())
}

impl TaskCosts {
    pub(super) fn render(&self) -> String {
        let cost = &self.completed_task_totals;
        format!(
            "Task attribution: {}/{} completed tasks had measured commands; {} commands, {} ms child work, {} ms queue, {} ms broker and unattributed, {} stdout bytes, {} retries. Conflict episodes: {} detected, {} verified after conflict. Reported duplicate work avoided: {}.{}",
            self.completed_tasks_with_metrics,
            self.completed_tasks,
            cost.commands,
            cost.child_command_ms,
            cost.queue_wait_ms,
            cost.broker_and_unattributed_ms,
            cost.output_bytes,
            cost.retries,
            cost.conflicts_detected,
            cost.conflicts_resolved,
            cost.reported_duplicate_work_avoided,
            if self.history_limited {
                " Command history limited to its newest 16 MiB."
            } else {
                ""
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn costs_account_for_completed_tasks_and_expose_missing_and_unattributed_records() {
        let root = tempfile::tempdir().unwrap();
        let logs = root.path().join(".aethyme/logs");
        std::fs::create_dir_all(&logs).unwrap();
        let path = logs.join("command-metrics.jsonl");
        let row = |id, timestamp| {
            serde_json::json!({
                "ts": timestamp,
                "session_id": id,
                "duration_ms": 9,
                "output_bytes": 13,
                "outcome": "outcome_unknown",
                "attribution": {
                    "child_command_ms": 5, "queue_wait_ms": 3, "broker_and_unattributed_ms": 1,
                    "retries": 1,
                }
            })
        };
        std::fs::write(
            &path,
            format!(
                "standalone diagnostic\n{}\n{}\n{}\n{}\n",
                row(Some(1), 100),
                row(Some(1), 0),
                row(None, 100),
                row(Some(3), 100)
            ),
        )
        .unwrap();
        let session = |id, closed_at| crate::insights::SessionInsightRow {
            session_id: id,
            origin: crate::SessionOrigin::Spawned,
            created_at: 1,
            closed_at,
            in_flight: closed_at.is_none(),
            active_ms: None,
            signals: 0,
        };
        let input = crate::insights::InsightsInput {
            sessions: vec![
                session(1, Some(150)),
                session(2, Some(80)),
                session(3, None),
            ],
            ..Default::default()
        };
        let event = |id, ts, kind: &str| crate::types::Event {
            id,
            schema_version: 1,
            ts,
            kind: kind.into(),
            session_id: Some(1),
            payload_json: None,
        };
        let events = [
            event(1, 120, "session.closed"),
            event(2, 130, crate::events::SESSION_REUSED),
        ];
        let report = collect(root.path(), 50, 5, &input, &events);
        assert_eq!(report.completed_tasks, 3);
        assert_eq!(report.completed_tasks_with_metrics, 1);
        assert_eq!(report.unattributed_command_rows, 1);
        assert_eq!(report.completed_task_totals.wall_ms, 9);
        assert_eq!(report.completed_task_totals.output_bytes, 13);
        assert_eq!(report.completed_task_totals.outcomes["outcome_unknown"], 1);
        assert_eq!(report.tasks.len(), 2);
        assert!(
            report
                .tasks
                .iter()
                .any(|row| row.session_id == 1 && row.started_at_ms == 1)
        );
    }
}
