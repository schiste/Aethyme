//! Opt-in measurement of read-only command output, and the cheap poll surface.
//!
//! Inspection is contractually side-effect free, which also makes the commands
//! that dominate agent token cost invisible to telemetry. Measuring them is
//! therefore opt-in: the invariant holds unless an operator asks otherwise.

use std::path::Path;
use std::process::{Command, Output};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
mod common;

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new(args.first().map(|_| "git").unwrap_or("git"))
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
}

fn run(repo: &Path, measure: Option<&str>, args: &[&str]) -> Output {
    let mut cmd = common::broker_cli_for_fixture(CLI, args, repo);
    cmd.current_dir(repo);
    match measure {
        Some(value) => cmd.env("AETHYME_MEASURE_OUTPUT", value),
        None => cmd.env_remove("AETHYME_MEASURE_OUTPUT"),
    };
    cmd.output().unwrap()
}

fn fixture() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(repo.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    std::fs::write(repo.path().join("a.txt"), "a\n").unwrap();
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-qm", "init"]);
    assert!(
        run(repo.path(), None, &["advanced", "scaffold"])
            .status
            .success()
    );
    repo
}

fn metrics(repo: &Path) -> Vec<u8> {
    std::fs::read(repo.join(".aethyme/logs/command-metrics.jsonl")).unwrap_or_default()
}

/// The documented invariant, for the commands it actually covers.
///
/// It is narrower than "all reads": `command_records_metric` exempts a specific
/// set — certify, queue, metrics, handoff, worktree-root — while `status` and
/// `agents` have always recorded. Only the exempt set is asserted here.
#[test]
fn exempt_read_only_commands_write_no_telemetry_by_default() {
    let repo = fixture();
    run(repo.path(), None, &["status"]);
    let before = metrics(repo.path());
    for args in [
        vec!["advanced", "queue"],
        vec!["advanced", "queue", "--active"],
        vec!["certify"],
    ] {
        run(repo.path(), None, &args);
    }
    assert_eq!(
        metrics(repo.path()),
        before,
        "inspection must leave the metrics file byte-identical"
    );
}

/// With the opt-in set, the same commands are measured.
#[test]
fn opting_in_records_read_only_command_output() {
    let repo = fixture();
    run(repo.path(), None, &["status"]);
    let before = metrics(repo.path()).len();
    assert!(
        run(repo.path(), Some("1"), &["advanced", "queue"])
            .status
            .success()
    );
    let after = metrics(repo.path());
    assert!(
        after.len() > before,
        "an opted-in read must record one telemetry line"
    );
    let last: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&after).lines().last().unwrap()).unwrap();
    assert_eq!(last["command"], "queue");
    assert!(
        last["output_bytes"].as_i64().unwrap() > 0,
        "the recorded size must be the bytes actually printed: {last}"
    );
}

/// Falsey values must not silently enable it.
#[test]
fn falsey_opt_in_values_leave_the_invariant_intact() {
    let repo = fixture();
    run(repo.path(), None, &["status"]);
    let before = metrics(repo.path());
    for value in ["0", "false", "no", "off", ""] {
        assert!(
            run(repo.path(), Some(value), &["advanced", "queue"])
                .status
                .success()
        );
    }
    assert_eq!(
        metrics(repo.path()),
        before,
        "only an affirmative value opts in"
    );
}

/// `--active` excludes terminal entries, which is what makes it a cheap poll:
/// the inventory grows with history forever while the active view does not.
#[test]
fn active_queue_excludes_terminal_entries() {
    let repo = fixture();
    // An empty queue distinguishes the two views by wording, not size.
    let empty = run(repo.path(), None, &["advanced", "queue", "--active"]);
    assert!(
        String::from_utf8_lossy(&empty.stdout).contains("No queue entry is in flight"),
        "an empty active view must read differently from an empty inventory: {}",
        String::from_utf8_lossy(&empty.stdout)
    );

    // Produce one terminal entry, then prove the views diverge.
    let started = run(
        repo.path(),
        None,
        &["start", "--task", "queue filter", "--json"],
    );
    assert!(started.status.success());
    let session: serde_json::Value = serde_json::from_slice(&started.stdout).unwrap();
    let worktree = session["worktree_path"].as_str().unwrap().to_string();
    let id = session["id"].as_i64().unwrap().to_string();
    std::fs::write(Path::new(&worktree).join("work.txt"), "work\n").unwrap();
    git(Path::new(&worktree), &["add", "work.txt"]);
    git(Path::new(&worktree), &["commit", "-qm", "work"]);
    assert!(
        run(repo.path(), None, &["submit", "--session", &id])
            .status
            .success()
    );
    // A new head supersedes the verified entry: that is the terminal one.
    std::fs::write(Path::new(&worktree).join("work.txt"), "more work\n").unwrap();
    git(Path::new(&worktree), &["add", "work.txt"]);
    git(Path::new(&worktree), &["commit", "-qm", "more work"]);
    assert!(
        run(repo.path(), None, &["submit", "--session", &id])
            .status
            .success()
    );

    let all =
        String::from_utf8_lossy(&run(repo.path(), None, &["advanced", "queue"]).stdout).to_string();
    let active =
        String::from_utf8_lossy(&run(repo.path(), None, &["advanced", "queue", "--active"]).stdout)
            .to_string();
    assert!(
        all.contains("superseded"),
        "the inventory keeps terminal entries: {all}"
    );
    assert!(
        !active.contains("superseded"),
        "the active view must drop terminal entries: {active}"
    );
    assert!(
        active.len() < all.len(),
        "with history present the active view is the smaller poll: {} vs {}",
        active.len(),
        all.len()
    );
}

/// Every status call writes its metric row, the compact default included, so
/// a caller that polls status can be identified by form, focus and caller.
#[test]
fn default_compact_status_records_its_form_caller_and_section_timings() {
    let repo = fixture();
    let last_row = |repo: &Path| -> serde_json::Value {
        let bytes = metrics(repo);
        let text = String::from_utf8_lossy(&bytes);
        serde_json::from_str(text.lines().last().expect("a metric row")).unwrap()
    };

    let before = metrics(repo.path()).len();
    let output = run(repo.path(), None, &["status", "--json"]);
    assert!(output.status.success(), "{output:?}");
    let printed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(printed["form"], "compact");
    assert!(
        metrics(repo.path()).len() > before,
        "compact status wrote no row"
    );
    let row = last_row(repo.path());
    assert_eq!(row["command"], "status", "{row}");
    assert_eq!(row["exit"], 0);
    assert_eq!(row["status_form"], "compact", "{row}");
    assert_eq!(row["status_json"], true);
    assert_eq!(row["status_session_flag"], false);
    let recorded = row["output_bytes"].as_u64().unwrap();
    let printed_bytes = output.stdout.len() as u64;
    assert!(
        recorded > 0 && recorded.abs_diff(printed_bytes) <= 1,
        "{row}"
    );
    assert!(row["duration_ms"].as_i64().is_some(), "{row}");
    if let Some(caller) = row["caller_hint"].as_str() {
        assert!(!caller.contains('/') && !caller.contains(' '), "{row}");
    }
    let phases = row["phase_timings_ms"]
        .as_object()
        .expect("section timings");
    // Zero timings are omitted from a row, so only the nonzero ones can be
    // required: absent means 0 ms.
    assert!(!phases.is_empty(), "no section timings: {row}");
    for (section, ms) in phases {
        assert!(ms.is_u64(), "{section} is not a timing: {row}");
    }

    let full = run(repo.path(), None, &["status", "--full", "--json"]);
    assert!(full.status.success(), "{full:?}");
    let row = last_row(repo.path());
    assert_eq!(row["status_form"], "full", "{row}");
}
