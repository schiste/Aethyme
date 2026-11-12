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

/// A failure message with the parts that differ between occurrences taken
/// out, so the same failure in two sessions counts as one reason: paths
/// become `<path>`, long hexadecimal identifiers `<id>` and numbers `N`.
/// Only the first line is kept, capped at 160 characters.
pub fn failure_shape(message: &str) -> String {
    let line = message.lines().next().unwrap_or("").trim();
    let mut shape = String::new();
    for token in line.split_whitespace() {
        if !shape.is_empty() {
            shape.push(' ');
        }
        let core = token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '/');
        let (lead, rest) = token.split_at(token.find(core).unwrap_or(0));
        let tail = &rest[core.len()..];
        shape.push_str(lead);
        if core.len() > 1 && core.contains('/') {
            shape.push_str("<path>");
        } else if core.len() >= 12 && core.chars().all(|c| c.is_ascii_hexdigit()) {
            shape.push_str("<id>");
        } else {
            let mut in_number = false;
            for c in core.chars() {
                if c.is_ascii_digit() {
                    if !in_number {
                        shape.push('N');
                    }
                    in_number = true;
                } else {
                    in_number = false;
                    shape.push(c);
                }
            }
        }
        shape.push_str(tail);
    }
    shape.chars().take(160).collect()
}

/// The `failure_class` recorded when a guarded `exec` ran and the wrapped
/// command itself exited non-zero while the guard found nothing wrong. That
/// is the wrapped command's own result (a failing test, a linter finding), not
/// a broker failure.
pub const WRAPPED_COMMAND_FAILED: &str = "wrapped_command_failed";

/// Why commands failed: for each command, its broker-side failures grouped by
/// [`failure_shape`], most frequent first, five reasons at most and the rest
/// counted as `other_failures`. `failures` yields a command surface, its exit
/// code, the message it printed and the recorded `failure_class`. Failures of
/// class [`WRAPPED_COMMAND_FAILED`] are not broker failures: they are counted
/// per command as `wrapped_command_failures` and kept out of `failures` and
/// `reasons`.
pub fn failure_reasons<'a>(
    failures: impl Iterator<Item = (&'a str, u64, &'a str, Option<&'a str>)>,
) -> serde_json::Value {
    let mut commands: BTreeMap<&str, (BTreeMap<(String, &'static str), u64>, u64)> =
        BTreeMap::new();
    for (command, exit, message, class) in failures {
        let (reasons, wrapped) = commands.entry(command).or_default();
        if class == Some(WRAPPED_COMMAND_FAILED) {
            *wrapped += 1;
        } else {
            *reasons
                .entry((failure_shape(message), exit_class(exit)))
                .or_default() += 1;
        }
    }
    let mut commands: Vec<_> = commands
        .into_iter()
        .map(|(command, (reasons, wrapped))| {
            let failures: u64 = reasons.values().sum();
            let mut reasons: Vec<_> = reasons.into_iter().collect();
            reasons.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let shown: Vec<_> = reasons.iter().take(5).collect();
            let other = failures - shown.iter().map(|(_, count)| count).sum::<u64>();
            (
                (failures, wrapped),
                serde_json::json!({
                    "command": command,
                    "failures": failures,
                    "wrapped_command_failures": wrapped,
                    "reasons": shown.iter().map(|((reason, class), count)| serde_json::json!({
                        "reason": reason, "exit_class": class, "count": count,
                    })).collect::<Vec<_>>(),
                    "other_failures": other,
                }),
            )
        })
        .collect();
    commands.sort_by_key(|command| std::cmp::Reverse(command.0));
    serde_json::Value::Array(commands.into_iter().map(|(_, command)| command).collect())
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

    #[test]
    fn the_same_failure_in_two_sessions_is_one_reason() {
        assert_eq!(
            failure_shape(
                "host resource bundle unavailable: preparation_cache: exclusive key is held by lease e4fb2dea09e7e1b5c (holder 4412)\nsecond line"
            ),
            "host resource bundle unavailable: preparation_cache: exclusive key is held by lease <id> (holder N)"
        );
        assert_eq!(
            failure_shape("session 6242 is already closed; see /Users/someone/repo/.aethyme/x.md."),
            "session N is already closed; see <path>."
        );
        assert_eq!(
            failure_shape("step \"js-deps\" failed with exit Some(2)"),
            "step \"js-deps\" failed with exit Some(N)"
        );
        assert_eq!(failure_shape(&"word ".repeat(100)).chars().count(), 160);
    }

    #[test]
    fn failure_reasons_rank_commands_and_reasons_and_count_the_remainder() {
        let mut failures = vec![
            (
                "broker.prepare",
                1,
                "cache is held by lease aaaaaaaaaaaaaaaa",
            ),
            (
                "broker.prepare",
                1,
                "cache is held by lease bbbbbbbbbbbbbbbb",
            ),
            ("broker.prepare", 3, "session 7 is already closed"),
            ("broker.push", 5, "outcome unknown"),
        ];
        let distinct: Vec<String> = (0..6)
            .map(|n| format!("reason {}", "x".repeat(n + 1)))
            .collect();
        failures.extend(
            distinct
                .iter()
                .map(|reason| ("broker.sync", 1, reason.as_str())),
        );
        let report = failure_reasons(
            failures
                .into_iter()
                .map(|(command, exit, message)| (command, exit, message, None)),
        );
        let commands: Vec<_> = report
            .as_array()
            .unwrap()
            .iter()
            .map(|command| {
                (
                    command["command"].as_str().unwrap(),
                    command["failures"].as_u64().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            commands,
            [
                ("broker.sync", 6),
                ("broker.prepare", 3),
                ("broker.push", 1)
            ]
        );
        let prepare = &report[1]["reasons"];
        assert_eq!(prepare[0]["reason"], "cache is held by lease <id>");
        assert_eq!(prepare[0]["count"], 2);
        assert_eq!(prepare[0]["exit_class"], "unclassified");
        assert_eq!(prepare[1]["exit_class"], "refused");
        assert_eq!(report[0]["reasons"].as_array().unwrap().len(), 5);
        assert_eq!(report[0]["other_failures"], 1);
        assert_eq!(report[2]["reasons"][0]["exit_class"], "outcome_unknown");
    }

    #[test]
    fn wrapped_command_failures_are_counted_apart_from_broker_failures() {
        let failures = vec![
            (
                "broker.exec",
                1,
                "the command exited 101",
                Some(WRAPPED_COMMAND_FAILED),
            ),
            (
                "broker.exec",
                1,
                "the command exited 1",
                Some(WRAPPED_COMMAND_FAILED),
            ),
            (
                "broker.exec",
                1,
                "the command exited 2",
                Some(WRAPPED_COMMAND_FAILED),
            ),
            (
                "broker.exec",
                3,
                "session 7 is already closed",
                Some("command_failed"),
            ),
            ("broker.push", 5, "outcome unknown", None),
        ];
        let report = failure_reasons(failures.into_iter());
        let exec = &report[0];
        assert_eq!(exec["command"], "broker.exec");
        assert_eq!(exec["failures"], 1);
        assert_eq!(exec["wrapped_command_failures"], 3);
        assert_eq!(exec["other_failures"], 0);
        let reasons = exec["reasons"].as_array().unwrap();
        assert_eq!(reasons.len(), 1);
        assert_eq!(reasons[0]["reason"], "session N is already closed");
        assert_eq!(reasons[0]["exit_class"], "refused");
        assert_eq!(report[1]["command"], "broker.push");
        assert_eq!(report[1]["wrapped_command_failures"], 0);
    }

    #[test]
    fn a_command_with_only_wrapped_failures_has_no_broker_failures_or_reasons() {
        let failures = vec![
            (
                "broker.exec",
                1,
                "the command exited 1",
                Some(WRAPPED_COMMAND_FAILED),
            ),
            (
                "broker.exec",
                1,
                "the command exited 1",
                Some(WRAPPED_COMMAND_FAILED),
            ),
        ];
        let report = failure_reasons(failures.into_iter());
        assert_eq!(report.as_array().unwrap().len(), 1);
        assert_eq!(report[0]["failures"], 0);
        assert_eq!(report[0]["wrapped_command_failures"], 2);
        assert!(report[0]["reasons"].as_array().unwrap().is_empty());
        assert_eq!(report[0]["other_failures"], 0);
    }
}
