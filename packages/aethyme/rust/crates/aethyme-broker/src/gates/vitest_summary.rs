//! Strict reading of the final vitest summary in a gate log.
//!
//! Used only to tell a completed test failure from an environment error when
//! environment text (such as "permission denied") appears in the log. Only
//! vitest is read. Its summary is two adjacent count lines with a total each,
//! so it can be checked against itself. pytest and cargo end with one line
//! whose counts have no total to check, Jest prints lines that any script can
//! resemble, and TAP already has its own reader in `gates.rs`.
//!
//! Where the environment text stands decides the rest. Between the run banner
//! and the failed block it is the output of tests, most of them passing.
//! Before the banner it is the setup; inside the failed block it is why a test
//! failed (`pg_ctl: command not found`); after the summary it is a later
//! command. In those three places it may be the host's fault and stays one.

use std::path::Path;

/// The long dash vitest draws headings and dividers with.
const RULE: char = '\u{23af}';

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
    let mut first_banner = None;
    let mut last = None;
    let mut previous = None;
    for (index, line) in lines.iter().enumerate() {
        if is_run_banner(line) {
            open_runs += 1;
            first_banner = first_banner.or(Some(index));
        } else if line.trim_start().starts_with("Test Files  ") {
            let Some(remaining) = open_runs.checked_sub(1) else {
                return false;
            };
            open_runs = remaining;
            previous = last.replace(index);
        }
    }
    let (Some(first_banner), Some(last)) = (first_banner, last.filter(|_| open_runs == 0)) else {
        return false;
    };

    // The block of failed suites and tests this run printed before its
    // summary. Its first error must be numbered 1: a truncated log may have
    // lost an earlier heading, and then nothing shows what failed there. A log
    // that lost the whole block has no heading, and keeps today's result too.
    let this_run = previous.map_or(0, |previous| previous + 1);
    let Some(block) = (this_run..last).find(|index| is_failed_heading(lines[*index])) else {
        return false;
    };
    if lines[block..last]
        .iter()
        .find_map(|line| error_number(line))
        != Some(1)
    {
        return false;
    }
    // From the first failed heading of any run, so a nested or an earlier
    // run's failed block counts as well.
    let first_block = lines[..=block]
        .iter()
        .position(|line| is_failed_heading(line))
        .unwrap_or(block);
    if lines[..first_banner]
        .iter()
        .chain(&lines[first_block..last])
        .any(|line| has_environment_text(line))
    {
        return false;
    }

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
                || has_environment_text(line)
        })
}

fn is_run_banner(line: &str) -> bool {
    line.trim_start()
        .strip_prefix("RUN  v")
        .is_some_and(starts_with_digit)
}

/// `⎯⎯⎯ Failed Tests 2 ⎯⎯⎯` or `⎯⎯⎯ Failed Suites 1 ⎯⎯⎯`.
fn is_failed_heading(line: &str) -> bool {
    let text = line.trim_matches(RULE).trim();
    ["Failed Suites ", "Failed Tests "]
        .iter()
        .any(|title| text.strip_prefix(title).and_then(parse_count).is_some())
}

/// The position in a divider such as `⎯⎯⎯[1/2]⎯`, printed after each error.
fn error_number(line: &str) -> Option<u64> {
    let (number, total) = line
        .strip_prefix(RULE)?
        .trim_matches(RULE)
        .strip_prefix('[')?
        .strip_suffix(']')?
        .split_once('/')?;
    parse_count(total)?;
    parse_count(number)
}

fn has_environment_text(line: &str) -> bool {
    super::contains_environment_error(&line.to_ascii_lowercase())
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

    const BLOCK: &str = "\n⎯⎯⎯ Failed Tests 2 ⎯⎯⎯\n\n FAIL  a.test.ts > adds\nAssertionError: expected 1 to be 2\n⎯⎯⎯[1/2]⎯\n\n";

    fn report() -> String {
        format!("{BANNER}stderr | a.test.ts\nEACCES: permission denied\n\n{BLOCK}{SUMMARY}")
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
        assert!(failure_report(&report().replace(
            "16133 passed | 3 skipped",
            "16128 passed | 4 expected fail | 3 skipped | 1 todo"
        )));
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
            (
                "type errors",
                "   Start at",
                "Type Errors  1 failed\n   Start at",
            ),
            ("leaks", "   Start at", "      Leaks  1 leak\n   Start at"),
            ("start value", "Start at  10:11:12", "Start at  unknown"),
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
            "{BANNER}{BANNER}{passing}{BLOCK}{SUMMARY}"
        )));
        // An earlier or nested failure followed by a pass stays ambiguous.
        assert!(!failure_report(&format!("{}{BANNER}{passing}", report())));
        assert!(!failure_report(&format!(
            "{BANNER}{BANNER}{BLOCK}{SUMMARY}{passing}"
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
            "{BANNER}\naethyme: gate output capture truncated; observed 9 bytes\naethyme: classifier evidence observed in complete gate stream: permission denied\n{BLOCK}{SUMMARY}"
        )));
    }

    #[test]
    fn host_faults_around_the_failure_stay_environment_errors() {
        for text in [
            "sh: pg_ctl: command not found",
            "Error: connect EACCES: permission denied /run/db.sock",
        ] {
            // In a failed test's error.
            let failed = report().replace("AssertionError: expected 1 to be 2", text);
            assert!(failed.contains(text));
            assert!(!failure_report(&failed), "{text}");
            // In a failed suite's error, ahead of the failed tests.
            assert!(!failure_report(&report().replace(
                BLOCK,
                &format!("\n⎯⎯ Failed Suites 1 ⎯⎯\n\n{text}\n⎯⎯⎯[1/3]⎯\n{BLOCK}")
            )));
            // In the setup before the first banner.
            assert!(!failure_report(&format!("{text}\n{}", report())));
            // In an earlier run's failed block.
            let earlier = format!("{BANNER}{BLOCK}{SUMMARY}").replace("expected 1 to be 2", text);
            assert!(!failure_report(&format!(
                "{earlier}{BANNER}{BLOCK}{SUMMARY}"
            )));
            // The control: the same text as test output is tolerated.
            assert!(failure_report(
                &report().replace("EACCES: permission denied", text)
            ));
        }
    }

    #[test]
    fn the_failed_block_must_be_present_from_its_first_error() {
        let without = report().replace(BLOCK, "\n");
        assert!(!failure_report(&without));
        // Another run's block does not stand in for this run's.
        assert!(failure_report(&format!(
            "{BANNER}{BLOCK}{SUMMARY}{BANNER}{BLOCK}{SUMMARY}"
        )));
        assert!(!failure_report(&format!(
            "{BANNER}{BLOCK}{SUMMARY}{BANNER}{SUMMARY}"
        )));
        for (rule, from, to) in [
            ("no heading count", "Failed Tests 2", "Failed Tests"),
            ("no divider", "⎯⎯⎯[1/2]⎯\n", ""),
            ("earlier errors lost", "[1/2]", "[2/2]"),
            ("divider total", "[1/2]", "[1/x]"),
        ] {
            let report = report();
            assert!(report.contains(from), "{rule}");
            assert!(!failure_report(&report.replace(from, to)), "{rule}");
        }
        assert!(failure_report(
            &report().replace("Failed Tests 2", "Failed Suites 2")
        ));
        // Truncation inside the block: its heading in the retained prefix puts
        // the footer's classifier evidence inside the block.
        let footer = "\naethyme: gate output capture truncated; observed 9 bytes\naethyme: classifier evidence observed in complete gate stream: permission denied\n";
        assert!(!failure_report(
            &report().replace("AssertionError", &format!("{footer}AssertionError"))
        ));
        // The suites heading was lost; the tests heading is in the tail.
        assert!(!failure_report(&format!(
            "{BANNER}{footer}{}{SUMMARY}",
            BLOCK.replace("[1/2]", "[2/3]")
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
