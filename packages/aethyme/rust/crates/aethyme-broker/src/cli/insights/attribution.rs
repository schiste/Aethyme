use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use serde::{Deserialize, Serialize};

const MAX_HISTORY_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Default, Serialize)]
pub(super) struct TaskCosts {
    completed_sessions: usize,
    completed_sessions_with_metrics: usize,
    history_limited: bool,
    unattributed_command_rows: u64,
    completed_session_totals: Costs,
    sessions: BTreeMap<i64, Costs>,
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
    completed: &BTreeSet<i64>,
    visible: &BTreeSet<i64>,
    events: &[crate::types::Event],
) -> std::io::Result<TaskCosts> {
    let mut report = TaskCosts {
        completed_sessions: completed.len(),
        ..TaskCosts::default()
    };
    let mut costs = BTreeMap::<i64, Costs>::new();
    match File::open(root.join(".aethyme/logs/command-metrics.jsonl")) {
        Ok(mut file) => {
            let size = file.metadata()?.len();
            let from = size.saturating_sub(MAX_HISTORY_BYTES);
            report.history_limited = from != 0;
            file.seek(SeekFrom::Start(from))?;
            let reader = BufReader::new(file.take(size - from));
            // Skip the potentially partial record at the beginning of the tail.
            for line in reader.lines().skip(usize::from(from != 0)) {
                let Ok(row) = serde_json::from_str::<Record>(&line?) else {
                    continue;
                };
                if row.ts < since_ms {
                    continue;
                }
                match row.session_id {
                    Some(id) if completed.contains(&id) || visible.contains(&id) => {
                        costs.entry(id).or_default().add_record(row);
                    }
                    None => report.unattributed_command_rows += 1,
                    _ => {}
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut pending = BTreeSet::new();
    let mut duplicates = BTreeSet::new();
    for event in events {
        let Some(id) = event.session_id else { continue };
        if !completed.contains(&id) && !visible.contains(&id) {
            continue;
        }
        match event.kind.as_str() {
            "merge.conflict" if pending.insert(id) => {
                costs.entry(id).or_default().conflicts_detected += 1;
            }
            "merge.verified" | "merge.promoted" | "merge.externally_landed"
                if pending.remove(&id) =>
            {
                costs.entry(id).or_default().conflicts_resolved += 1;
            }
            crate::events::DUPLICATE_WORK_AVOIDED if duplicates.insert(id) => {
                costs.entry(id).or_default().reported_duplicate_work_avoided += 1;
            }
            _ => {}
        }
    }
    for (id, cost) in costs {
        if completed.contains(&id) {
            report.completed_sessions_with_metrics += usize::from(cost.commands != 0);
            report.completed_session_totals.add(&cost);
        }
        if visible.contains(&id) {
            report.sessions.insert(id, cost);
        }
    }
    Ok(report)
}

impl TaskCosts {
    pub(super) fn render(&self) -> String {
        let cost = &self.completed_session_totals;
        format!(
            "Task attribution: {}/{} completed sessions had measured commands; {} commands, {} ms child work, {} ms queue, {} ms broker and unattributed, {} stdout bytes, {} retries. Conflict episodes: {} detected, {} verified after conflict. Reported duplicate work avoided: {}.{}",
            self.completed_sessions_with_metrics,
            self.completed_sessions,
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
        let report = collect(
            root.path(),
            50,
            &BTreeSet::from([1, 2]),
            &BTreeSet::from([1, 2, 3]),
            &[],
        )
        .unwrap();
        assert_eq!(report.completed_sessions, 2);
        assert_eq!(report.completed_sessions_with_metrics, 1);
        assert_eq!(report.unattributed_command_rows, 1);
        assert_eq!(report.completed_session_totals.wall_ms, 9);
        assert_eq!(report.completed_session_totals.output_bytes, 13);
        assert_eq!(
            report.completed_session_totals.outcomes["outcome_unknown"],
            1
        );
        assert!(!report.sessions.contains_key(&2));
        assert_eq!(report.sessions[&3].commands, 1);
    }
}
