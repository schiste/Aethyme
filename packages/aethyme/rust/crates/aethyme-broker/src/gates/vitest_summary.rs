//! Strict reading of the final vitest summary in a gate log.
//!
//! Used only to tell a completed test failure from an environment error when
//! environment text (such as "permission denied") appears in the log. Only
//! vitest is read. Its summary is two adjacent count lines with a total each,
//! so it can be checked against itself. pytest and cargo end with one line
//! whose counts have no total to check, Jest prints lines that any script can
//! resemble, and TAP already has its own reader in `gates.rs`.

use std::path::Path;

/// Whether the log ends with a complete, consistent vitest summary that
/// reports failing tests, with no environment text after it. An unreadable or
/// oversized log, and anything partial, contradictory or ambiguous, is `false`.
pub(super) fn complete_failure(log_path: &Path) -> bool {
    use std::io::Read;

    let Ok(file) = std::fs::File::open(log_path) else {
        return false;
    };
    let mut bytes = Vec::new();
    if file
        .take(super::bounded_log::MAX_LOG_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() as u64 > super::bounded_log::MAX_LOG_BYTES
    {
        return false;
    }
    // A truncated log can be cut inside a character, so decode lossily. The
    // summary lines themselves are ASCII and must match exactly.
    failure_report(&String::from_utf8_lossy(&bytes))
}

fn failure_report(log: &str) -> bool {
    let plain = strip_csi(log);
    let lines: Vec<&str> = plain.lines().map(str::trim_end).collect();

    // Each run prints a banner and then its summary. A nested run (a test that
    // runs vitest) closes inside its parent, so banners and summaries balance
    // like brackets. An open banner means some run, possibly the outer one,
    // never reported; a summary without a banner cannot be attributed.
    let mut open_runs = 0_usize;
    let mut last = None;
    for (index, line) in lines.iter().enumerate() {
        if is_run_banner(line) {
            open_runs += 1;
        } else if line.trim_start().starts_with("Test Files  ") {
            let Some(remaining) = open_runs.checked_sub(1) else {
                return false;
            };
            open_runs = remaining;
            last = Some(index);
        }
    }
    let Some(last) = last.filter(|_| open_runs == 0) else {
        return false;
    };

    // The lines vitest prints together. Anything between them, including an
    // `Errors` line for errors outside tests, leaves today's result.
    let [files, tests, start, duration, after @ ..] = &lines[last..] else {
        return false;
    };
    files.strip_prefix(" Test Files  ").is_some_and(failed_counts)
        && tests.strip_prefix("      Tests  ").is_some_and(failed_counts)
        && start
            .strip_prefix("   Start at  ")
            .is_some_and(starts_with_digit)
        && duration
            .strip_prefix("   Duration  ")
            .is_some_and(starts_with_digit)
        // A later runner's summary under a prefix, a later run's start, or a
        // later command's environment error: this summary is not the last word.
        // The truncation footer also lands here when it follows the summary.
        && !after.iter().any(|line| {
            line.contains("Test Files")
                || line.contains("RUN  v")
                || super::contains_environment_error(&line.to_ascii_lowercase())
        })
}

fn is_run_banner(line: &str) -> bool {
    line.trim_start()
        .strip_prefix("RUN  v")
        .is_some_and(starts_with_digit)
}

fn starts_with_digit(text: &str) -> bool {
    text.starts_with(|first: char| first.is_ascii_digit())
}

/// Parse `2 failed | 1720 passed | 3 skipped (1725)`: known states in vitest's
/// own order, each once and above zero, adding up to the total, some failed.
fn failed_counts(text: &str) -> bool {
    const STATES: [&str; 5] = ["failed", "passed", "expected fail", "skipped", "todo"];

    let Some((states, total)) = text
        .strip_suffix(')')
        .and_then(|rest| rest.split_once(" ("))
        .and_then(|(states, total)| Some((states, parse_count(total)?)))
    else {
        return false;
    };
    let mut next_state = 0;
    let mut sum = 0_u64;
    let mut failed = false;
    for part in states.split(" | ") {
        let Some((count, name)) = part
            .split_once(' ')
            .and_then(|(count, name)| Some((parse_count(count)?, name)))
        else {
            return false;
        };
        let Some(offset) = STATES[next_state..].iter().position(|state| *state == name) else {
            return false;
        };
        failed |= next_state + offset == 0;
        next_state += offset + 1;
        let Some(added) = sum.checked_add(count) else {
            return false;
        };
        sum = added;
    }
    failed && sum == total
}

fn parse_count(text: &str) -> Option<u64> {
    if !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok().filter(|count| *count > 0)
}

/// Remove CSI sequences (colors). Any other escape stays and fails the match.
fn strip_csi(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("\u{1b}[") {
        plain.push_str(&rest[..start]);
        let body = &rest[start + 2..];
        match body.find(|c: char| !(' '..='?').contains(&c)) {
            Some(end) if body[end..].starts_with(|c: char| ('@'..='~').contains(&c)) => {
                rest = &body[end + 1..];
            }
            _ => {
                plain.push_str("\u{1b}[");
                rest = body;
            }
        }
    }
    plain.push_str(rest);
    plain
}

#[cfg(test)]
mod tests {
    use super::*;

    const BANNER: &str = " RUN  v4.1.11 /work/app\n\n";
    const SUMMARY: &str = " Test Files  2 failed | 1720 passed (1722)\n      Tests  2 failed | 16133 passed | 3 skipped (16138)\n   Start at  10:11:12\n   Duration  475.50s (transform 1.00s)\n";

    fn report() -> String {
        format!("{BANNER}stderr | a.test.ts\nEACCES: permission denied\n\n{SUMMARY}")
    }

    #[test]
    fn a_complete_failure_summary_is_read_plain_or_colored() {
        assert!(failure_report(&report()));
        assert!(failure_report(&report().replace('\n', "\r\n")));
        let colored = report()
            .replace(
                " RUN  v4.1.11 ",
                "\u{1b}[1m\u{1b}[46m RUN \u{1b}[49m\u{1b}[22m \u{1b}[36mv4.1.11 \u{1b}[39m",
            )
            .replace(" Test Files ", "\u{1b}[2m Test Files \u{1b}[22m")
            .replace(
                "2 failed",
                "\u{1b}[1m\u{1b}[31m2 failed\u{1b}[39m\u{1b}[22m",
            )
            .replace(" | ", "\u{1b}[2m | \u{1b}[22m")
            .replace("(1722)", "\u{1b}[90m(1722)\u{1b}[39m");
        assert!(failure_report(&colored));
        // Wrapper output after the summary is ordinary.
        assert!(failure_report(&format!(
            "{}\nELIFECYCLE Command failed with exit code 1.\n",
            report()
        )));
    }

    #[test]
    fn every_summary_rule_is_required() {
        for (rule, from, to) in [
            ("banner", BANNER, ""),
            ("file total", "(1722)", "(1723)"),
            ("test total", "(16138)", "(16139)"),
            ("no file total", " (1722)", ""),
            ("no test total", " (16138)", ""),
            (
                "files all passed",
                "2 failed | 1720 passed (1722)",
                "1722 passed (1722)",
            ),
            (
                "tests all passed",
                "2 failed | 16133 passed | 3 skipped (16138)",
                "16135 passed | 3 skipped (16138)",
            ),
            (
                "no tests",
                "2 failed | 16133 passed | 3 skipped (16138)",
                "no tests",
            ),
            (
                "zero failed",
                "2 failed | 1720 passed (1722)",
                "0 failed | 1722 passed (1722)",
            ),
            ("unknown state", "3 skipped", "3 pending"),
            (
                "repeated state",
                "16133 passed | 3 skipped",
                "16133 passed | 3 passed",
            ),
            (
                "state order",
                "16133 passed | 3 skipped",
                "3 skipped | 16133 passed",
            ),
            ("signed count", "2 failed | 1720", "+2 failed | 1720"),
            ("overflow", "1720 passed", "18446744073709551615 passed"),
            ("trailing text", "(1722)", "(1722) ok"),
            ("separated pair", "      Tests  2", "noise\n      Tests  2"),
            (
                "no tests line",
                "      Tests  2 failed | 16133 passed | 3 skipped (16138)\n",
                "",
            ),
            (
                "errors outside tests",
                "   Start at",
                "     Errors  1 error\n   Start at",
            ),
            ("no start", "   Start at  10:11:12\n", ""),
            (
                "no duration",
                "   Duration  475.50s (transform 1.00s)\n",
                "",
            ),
            ("duration value", "Duration  475.50s", "Duration  unknown"),
            ("indented files", " Test Files  2", "   Test Files  2"),
            ("indented tests", "      Tests  2", "        Tests  2"),
        ] {
            let report = report();
            assert!(report.contains(from), "{rule}");
            assert!(!failure_report(&report.replace(from, to)), "{rule}");
        }
    }

    #[test]
    fn only_the_final_summary_decides() {
        let passing = " Test Files  1 passed (1)\n      Tests  4 passed (4)\n   Start at  10:11:12\n   Duration  12ms\n";
        // An earlier runner or a nested run that passed does not matter.
        assert!(failure_report(&format!("{BANNER}{passing}{}", report())));
        assert!(failure_report(&format!(
            "{BANNER}{BANNER}{passing}{SUMMARY}"
        )));
        // An earlier or nested failure followed by a pass stays ambiguous.
        assert!(!failure_report(&format!("{}{BANNER}{passing}", report())));
        assert!(!failure_report(&format!(
            "{BANNER}{BANNER}{SUMMARY}{passing}"
        )));
        // A nested failure whose outer run never reported, and a later run
        // that never reported.
        assert!(!failure_report(&format!("{BANNER}{}", report())));
        assert!(!failure_report(&format!("{}{BANNER}", report())));
        // A malformed final summary does not fall back to an earlier one.
        assert!(!failure_report(&format!(
            "{}{BANNER}{}",
            report(),
            SUMMARY.replace("(1722)", "(9)")
        )));
        // Later summaries under a task runner's line prefix.
        assert!(!failure_report(&format!(
            "{}web:test:  Test Files  1 passed (1)\n",
            report()
        )));
        assert!(!failure_report(&format!(
            "{}web:test:  RUN  v4.1.11 /work/web\n",
            report()
        )));
    }

    #[test]
    fn environment_text_after_the_summary_stays_ambiguous() {
        assert!(!failure_report(&format!(
            "{}sh: ./next-step: Permission denied\n",
            report()
        )));
        // Summary in the retained prefix, output omitted after it.
        assert!(!failure_report(&format!(
            "{}\naethyme: gate output capture truncated; observed 9 bytes\naethyme: classifier evidence observed in complete gate stream: permission denied\ntail\n",
            report()
        )));
        // Summary in the retained tail.
        assert!(failure_report(&format!(
            "{BANNER}\naethyme: gate output capture truncated; observed 9 bytes\naethyme: classifier evidence observed in complete gate stream: permission denied\n{SUMMARY}"
        )));
    }

    #[test]
    fn unreadable_and_oversized_logs_are_not_reports() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("gate.log");
        assert!(!complete_failure(&log));
        // A cut inside a character elsewhere in the log is tolerated.
        let mut bytes = vec![0xe2, 0x9c, b'\n'];
        bytes.extend_from_slice(report().as_bytes());
        std::fs::write(&log, &bytes).unwrap();
        assert!(complete_failure(&log));
        let padding = usize::try_from(super::super::bounded_log::MAX_LOG_BYTES).unwrap();
        bytes.resize(padding + 1, b'\n');
        std::fs::write(&log, &bytes).unwrap();
        assert!(!complete_failure(&log));
    }

    #[test]
    fn strip_csi_removes_only_complete_sequences() {
        assert_eq!(strip_csi("a\u{1b}[1;31mb\u{1b}[0mc"), "abc");
        assert_eq!(strip_csi("a\u{1b}[31"), "a\u{1b}[31");
        assert_eq!(strip_csi("a\u{1b}[\u{e9}"), "a\u{1b}[\u{e9}");
        assert_eq!(strip_csi("a\u{1b}]0;t"), "a\u{1b}]0;t");
    }
}
