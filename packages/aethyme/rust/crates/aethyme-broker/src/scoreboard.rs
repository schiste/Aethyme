//! Per-release scoreboard over `command-metrics.jsonl`.
//!
//! Impact is measured by deploying a release and reading what it recorded, so
//! the rows are grouped by the `aethyme_version` that wrote them. Rows from
//! before that field existed fall under [`UNVERSIONED`]. Everything here is a
//! pure fold over lines: no clock, no repository, no broker state.

use std::collections::BTreeMap;

/// The group for rows written before rows named their release.
pub const UNVERSIONED: &str = "unversioned";

/// How a command ended, by what the caller does next. Derived from the exit
/// code alone, so it needs nothing the row does not already carry.
pub fn exit_class(exit: u64) -> &'static str {
    match exit as u8 {
        crate::exit_status::SUCCESS => "ok",
        crate::exit_status::USAGE => "usage",
        crate::exit_status::REFUSED => "refused",
        crate::exit_status::VERIFICATION_FAILED => "verification_failed",
        crate::exit_status::OUTCOME_UNKNOWN => "outcome_unknown",
        crate::exit_status::ENVIRONMENT => "host",
        crate::exit_status::LEASE_WAIT_GRANTED
        | crate::exit_status::LEASE_WAIT_DECLINED
        | crate::exit_status::LEASE_WAIT_TIMED_OUT
        | crate::exit_status::LEASE_WAIT_CANCELLED => "lease_wait",
        // FAILED and anything unassigned: nothing classified it, which is the
        // bucket to drive to zero.
        _ => "unclassified",
    }
}

#[derive(Default)]
struct CommandTotals {
    durations_ms: Vec<u64>,
    exit_classes: BTreeMap<&'static str, u64>,
    phase_ms: BTreeMap<String, u64>,
}

#[derive(Default)]
struct ReleaseTotals {
    first_ts: Option<i64>,
    last_ts: Option<i64>,
    commands: BTreeMap<String, CommandTotals>,
}

/// Nearest-rank percentile of an ascending slice; 0 when empty.
fn percentile(sorted: &[u64], percent: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (sorted.len() * percent).div_ceil(100).max(1);
    sorted[rank - 1]
}

/// Fold metric rows into one entry per release, oldest first activity first.
/// Lines that are not JSON objects are skipped, as every reader of this file
/// does: a torn final line must not hide the rest.
pub fn by_release<'a>(lines: impl Iterator<Item = &'a str>) -> serde_json::Value {
    let mut releases: BTreeMap<String, ReleaseTotals> = BTreeMap::new();
    for line in lines {
        let Ok(serde_json::Value::Object(row)) = serde_json::from_str(line) else {
            continue;
        };
        let version = row
            .get("aethyme_version")
            .and_then(|version| version.as_str())
            .unwrap_or(UNVERSIONED);
        let release = releases.entry(version.to_string()).or_default();
        if let Some(ts) = row.get("ts").and_then(|ts| ts.as_i64()) {
            release.first_ts = Some(release.first_ts.map_or(ts, |first| first.min(ts)));
            release.last_ts = Some(release.last_ts.map_or(ts, |last| last.max(ts)));
        }
        let command = row
            .get("command")
            .and_then(|command| command.as_str())
            .unwrap_or("?");
        let totals = release.commands.entry(command.to_string()).or_default();
        totals.durations_ms.push(
            row.get("duration_ms")
                .and_then(|duration| duration.as_u64())
                .unwrap_or(0),
        );
        let exit = row.get("exit").and_then(|exit| exit.as_u64()).unwrap_or(0);
        *totals.exit_classes.entry(exit_class(exit)).or_default() += 1;
        if let Some(phases) = row.get("phase_timings_ms").and_then(|p| p.as_object()) {
            for (phase, elapsed) in phases {
                *totals.phase_ms.entry(phase.clone()).or_default() += elapsed.as_u64().unwrap_or(0);
            }
        }
    }

    let mut releases: Vec<(String, ReleaseTotals)> = releases.into_iter().collect();
    releases.sort_by_key(|(_, release)| release.first_ts.unwrap_or(i64::MAX));
    serde_json::Value::Array(
        releases
            .into_iter()
            .map(|(version, release)| release_json(&version, release))
            .collect(),
    )
}

fn release_json(version: &str, release: ReleaseTotals) -> serde_json::Value {
    let mut calls = 0u64;
    let mut total_ms = 0u64;
    let mut not_ok = 0u64;
    let commands: Vec<serde_json::Value> = release
        .commands
        .into_iter()
        .map(|(command, mut totals)| {
            totals.durations_ms.sort_unstable();
            let count = totals.durations_ms.len() as u64;
            let sum: u64 = totals.durations_ms.iter().sum();
            let ok = totals.exit_classes.get("ok").copied().unwrap_or(0);
            calls += count;
            total_ms += sum;
            not_ok += count - ok;
            // The five phases that cost most, so a regression names where.
            let mut phases: Vec<(String, u64)> = totals.phase_ms.into_iter().collect();
            phases.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            phases.truncate(5);
            serde_json::json!({
                "command": command,
                "calls": count,
                "total_ms": sum,
                "p50_ms": percentile(&totals.durations_ms, 50),
                "p95_ms": percentile(&totals.durations_ms, 95),
                "not_ok": count - ok,
                "exit_classes": totals.exit_classes,
                "top_phases_ms": phases
                    .into_iter()
                    .map(|(phase, ms)| serde_json::json!({"phase": phase, "total_ms": ms}))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::json!({
        "aethyme_version": version,
        "first_ts": release.first_ts,
        "last_ts": release.last_ts,
        "calls": calls,
        "total_ms": total_ms,
        "not_ok": not_ok,
        "commands": commands,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_grouped_by_the_release_that_wrote_them() {
        let log = [
            r#"{"ts":10,"command":"submit","duration_ms":900,"exit":0}"#,
            r#"{"ts":20,"aethyme_version":"0.8.70","command":"submit","duration_ms":100,"exit":0,"phase_timings_ms":{"ownership":70,"planning":20}}"#,
            r#"{"ts":30,"aethyme_version":"0.8.70","command":"submit","duration_ms":300,"exit":3,"phase_timings_ms":{"ownership":10}}"#,
            r#"{"ts":40,"aethyme_version":"0.8.70","command":"exec","duration_ms":50,"exit":6}"#,
            "torn line",
            r#"{"ts":50,"aethyme_version":"0.8.71","command":"submit","duration_ms":5,"exit":1}"#,
        ];
        let board = by_release(log.into_iter());
        let versions: Vec<&str> = board
            .as_array()
            .unwrap()
            .iter()
            .map(|release| release["aethyme_version"].as_str().unwrap())
            .collect();
        assert_eq!(versions, [UNVERSIONED, "0.8.70", "0.8.71"]);

        let release = &board[1];
        assert_eq!(release["calls"], 3);
        assert_eq!(release["total_ms"], 450);
        assert_eq!(release["not_ok"], 2);
        assert_eq!(release["first_ts"], 20);
        assert_eq!(release["last_ts"], 40);
        let submit = &release["commands"][1];
        assert_eq!(submit["command"], "submit");
        assert_eq!(submit["p50_ms"], 100);
        assert_eq!(submit["p95_ms"], 300);
        assert_eq!(submit["exit_classes"]["ok"], 1);
        assert_eq!(submit["exit_classes"]["refused"], 1);
        assert_eq!(submit["top_phases_ms"][0]["phase"], "ownership");
        assert_eq!(submit["top_phases_ms"][0]["total_ms"], 80);
        assert_eq!(release["commands"][0]["exit_classes"]["host"], 1);
        assert_eq!(board[2]["commands"][0]["exit_classes"]["unclassified"], 1);
    }

    #[test]
    fn percentiles_use_the_nearest_rank() {
        assert_eq!(percentile(&[], 50), 0);
        assert_eq!(percentile(&[7], 95), 7);
        assert_eq!(percentile(&[1, 2, 3, 4], 50), 2);
        assert_eq!(percentile(&[1, 2, 3, 4], 95), 4);
    }

    #[test]
    fn every_assigned_exit_code_has_a_class() {
        assert_eq!(exit_class(0), "ok");
        assert_eq!(exit_class(1), "unclassified");
        assert_eq!(exit_class(2), "usage");
        assert_eq!(exit_class(3), "refused");
        assert_eq!(exit_class(4), "verification_failed");
        assert_eq!(exit_class(5), "outcome_unknown");
        assert_eq!(exit_class(6), "host");
        assert_eq!(exit_class(12), "lease_wait");
        assert_eq!(exit_class(137), "unclassified");
    }
}
