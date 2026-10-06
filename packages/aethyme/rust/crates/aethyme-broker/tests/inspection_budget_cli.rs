//! `status --refresh` and `status doctor` stop their per-session Git work at
//! an inspection budget and name what they did not reach (#460). What the
//! budget cut is unknown, never reported as clean.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::Broker;

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Run the CLI with the inspection budget set to `budget_ms`, or left at its
/// default when `None`.
fn run(repo: &Path, args: &[&str], budget_ms: Option<u64>) -> serde_json::Value {
    let mut command = Command::new(CLI);
    command.args(args).current_dir(repo);
    match budget_ms {
        Some(ms) => command.env("AETHYME_STATUS_INSPECTION_BUDGET_MS", ms.to_string()),
        None => command.env_remove("AETHYME_STATUS_INSPECTION_BUDGET_MS"),
    };
    let output: Output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    session: i64,
}

/// A repository tracking a bare `origin`, with one session that holds an
/// unpushed commit and an uncommitted file -- both things a cut-short status
/// must not report as absent.
fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(
        tmp.path(),
        &["init", "-q", "--bare", "-b", "main", "origin.git"],
    );
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    let origin = tmp.path().join("origin.git");
    git(
        &repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&repo, &["push", "-q", "-u", "origin", "main"]);
    git(&repo, &["fetch", "-q", "origin"]);
    git(&repo, &["remote", "set-head", "origin", "main"]);

    let mut broker = Broker::open(&repo).unwrap();
    let session = broker.start_worktree("budget fixture", None).unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    std::fs::write(worktree.join("work.txt"), "work\n").unwrap();
    git(&worktree, &["add", "-A"]);
    git(&worktree, &["commit", "-qm", "work"]);
    std::fs::write(worktree.join("draft.txt"), "draft\n").unwrap();
    Fixture {
        _tmp: tmp,
        repo,
        session: session.id,
    }
}

fn advice<'a>(status: &'a serde_json::Value, id: &str) -> Vec<&'a serde_json::Value> {
    status["advice"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["id"] == id)
        .collect()
}

fn strings(value: &serde_json::Value) -> Vec<&str> {
    value
        .as_array()
        .map(|items| items.iter().filter_map(serde_json::Value::as_str).collect())
        .unwrap_or_default()
}

#[test]
fn an_exhausted_budget_names_every_check_it_cut_and_reports_nothing_as_clean() {
    let fixture = fixture();

    let cut = run(&fixture.repo, &["status", "--refresh", "--json"], Some(0));
    let deferred = strings(&cut["deferred_checks"]);
    for check in ["dirty_worktrees", "unpushed_commits"] {
        assert!(deferred.contains(&check), "{check} not named: {cut:#}");
    }
    // Unknown, not zero: the session is listed, never silently clean.
    assert_eq!(
        cut["unpushed_work"]["not_inspected_sessions"],
        serde_json::json!([fixture.session]),
        "{cut:#}"
    );
    assert!(
        cut["unpushed_work"]["sessions"]
            .as_array()
            .is_none_or(Vec::is_empty),
        "{cut:#}"
    );
    let rows = advice(&cut, "status.inspection-budget");
    assert_eq!(rows.len(), 1, "{cut:#}");
    assert!(
        strings(&rows[0]["evidence"])
            .iter()
            .any(|line| *line == format!("sessions not inspected: {}", fixture.session)),
        "{cut:#}"
    );
    assert!(advice(&cut, "session.dirty-worktree").is_empty(), "{cut:#}");

    // The same repository under the default budget: both facts reported, and
    // nothing claims to have been cut.
    let full = run(&fixture.repo, &["status", "--refresh", "--json"], None);
    assert!(
        advice(&full, "status.inspection-budget").is_empty(),
        "{full:#}"
    );
    assert_eq!(advice(&full, "session.dirty-worktree").len(), 1, "{full:#}");
    assert_eq!(full["summary"]["dirty_sessions"], 1, "{full:#}");
    assert_eq!(
        full["unpushed_work"]["sessions"][0]["session_id"], fixture.session,
        "{full:#}"
    );
    assert!(
        full["unpushed_work"]
            .get("not_inspected_sessions")
            .is_none(),
        "{full:#}"
    );
    for phase in [
        "checkouts",
        "unpushed",
        "promoted_conflicts",
        "integration_drift",
    ] {
        assert!(
            full["phase_timings_ms"].get(phase).is_some(),
            "{phase} not timed: {full:#}"
        );
    }
}

#[test]
fn doctor_names_the_unpushed_check_its_budget_cut() {
    let fixture = fixture();

    let cut = run(&fixture.repo, &["status", "doctor", "--json"], Some(0));
    assert_eq!(
        cut["budget_cut"],
        serde_json::json!(["unpushed_commits"]),
        "{cut:#}"
    );
    assert_eq!(
        cut["unpushed_work"]["not_inspected_sessions"],
        serde_json::json!([fixture.session]),
        "{cut:#}"
    );
    for phase in ["retention", "unpushed", "total"] {
        assert!(
            cut["phase_timings_ms"].get(phase).is_some(),
            "{phase} not timed: {cut:#}"
        );
    }

    let full = run(&fixture.repo, &["status", "doctor", "--json"], None);
    assert!(full.get("budget_cut").is_none(), "{full:#}");
    assert_eq!(
        full["unpushed_work"]["sessions"][0]["session_id"], fixture.session,
        "{full:#}"
    );
}

#[test]
fn a_cut_drift_assessment_is_named_and_points_at_the_unbounded_dry_run() {
    let fixture = fixture();
    // Upstream moves past integration, so `--refresh` assesses the drift.
    git(&fixture.repo, &["switch", "-qc", "upstream-side"]);
    std::fs::write(fixture.repo.join("upstream.txt"), "upstream\n").unwrap();
    git(&fixture.repo, &["add", "-A"]);
    git(&fixture.repo, &["commit", "-qm", "upstream"]);
    let upstream = git(&fixture.repo, &["rev-parse", "HEAD"]);
    git(&fixture.repo, &["switch", "-q", "main"]);
    git(
        &fixture.repo,
        &["update-ref", "refs/remotes/origin/main", &upstream],
    );

    let cut = run(&fixture.repo, &["status", "--refresh", "--json"], Some(0));
    assert!(
        strings(&cut["deferred_checks"]).contains(&"integration_drift"),
        "{cut:#}"
    );
    // Not assessed is not resolved: no assessment is reported at all.
    assert!(cut.get("integration_reconciliation").is_none(), "{cut:#}");
    let rows = advice(&cut, "status.inspection-budget");
    assert_eq!(rows.len(), 1, "{cut:#}");
    let upstream_ref = cut["upstream_ref"].as_str().unwrap();
    assert_eq!(
        strings(&rows[0]["commands"]),
        [format!(
            "aethyme broker advanced integration reconcile --upstream {upstream_ref} --dry-run"
        )
        .as_str()],
        "{cut:#}"
    );

    let full = run(&fixture.repo, &["status", "--refresh", "--json"], None);
    assert!(
        !strings(&full["deferred_checks"]).contains(&"integration_drift"),
        "{full:#}"
    );
    assert!(full.get("integration_reconciliation").is_some(), "{full:#}");
}
