//! Integration drift is reported and repaired by its actual shape (#290 phase 3).
//!
//! Two states were collapsed into one refusal: integration *diverged* from the
//! tracked upstream, which needs a reviewed reconciliation, and integration
//! merely *behind* it, which is a fast-forward that discards nothing. Treating
//! the second as the first meant every pull-request merge left the gap in
//! place, and every session started afterwards inherited it.

use std::path::Path;
use std::process::Command;

use aethyme_broker::{AutomaticIntegrationCleanupState, Broker, StatusAdviceSeverity};

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

/// A repository whose `main` tracks `origin/main`, with integration pinned at
/// the commit `main` started from, then `origin/main` advanced by `ahead`
/// commits. Integration is therefore a strict ancestor of upstream.
fn fixture(ahead: usize) -> (tempfile::TempDir, Broker) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("README.md"), "fixture\n").unwrap();
    std::fs::write(root.join(".gitignore"), "/.aethyme/\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "init"]);
    let base = git(root, &["rev-parse", "HEAD"]);

    git(
        root,
        &["update-ref", "refs/heads/aethyme/integration", &base],
    );
    // The upstream commits must NOT be on local `main`. `integration_head`
    // already fast-forwards integration onto the main checkout's HEAD when it
    // is an ancestor of it (#40), so advancing local main here would mask the
    // state under test -- integration behind the *tracked upstream* while the
    // local branch is behind it too, which is what a pull-request merge leaves
    // behind and what #290 phase 3.1 is about.
    git(root, &["switch", "-qc", "upstream-side"]);
    for n in 0..ahead {
        std::fs::write(root.join(format!("upstream{n}.txt")), "upstream\n").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-qm", &format!("upstream {n}")]);
    }
    let head = git(root, &["rev-parse", "HEAD"]);
    git(root, &["switch", "-q", "main"]);
    git(root, &["update-ref", "refs/remotes/origin/main", &head]);
    git(root, &["config", "remote.origin.url", "."]);
    git(
        root,
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    );
    git(root, &["config", "branch.main.remote", "origin"]);
    git(root, &["config", "branch.main.merge", "refs/heads/main"]);

    let broker = Broker::open(root).unwrap();
    (tmp, broker)
}

/// Phase 3.1. Nothing was promoted, so the reconciliation plan is empty — the
/// case the previous guard required to be non-empty and therefore refused.
#[test]
fn automatic_cleanup_fast_forwards_when_integration_carries_nothing_of_its_own() {
    let (tmp, mut broker) = fixture(3);
    let upstream = git(tmp.path(), &["rev-parse", "refs/remotes/origin/main"]);

    let report = broker
        .auto_cleanup_landed_integration("refs/remotes/origin/main")
        .unwrap();

    assert_eq!(
        report.state,
        AutomaticIntegrationCleanupState::Cleaned,
        "{}",
        report.explanation
    );
    assert_eq!(report.new_integration, upstream);
    assert!(
        report.explanation.contains("fast-forwarded"),
        "{}",
        report.explanation
    );
    assert_eq!(
        git(tmp.path(), &["rev-parse", "refs/heads/aethyme/integration"]),
        upstream
    );
}

/// Phase 3.2. Behind-but-clean is a notice naming the repair, not a block:
/// blocking taught operators to wait for the block instead of keeping the ref
/// current, and to reach for a reviewed reconciliation when a fast-forward was
/// the whole answer.
#[test]
fn status_reports_a_clean_lag_as_a_notice_rather_than_a_block() {
    let (_tmp, broker) = fixture(2);
    let status = broker.status_snapshot(0).unwrap();

    let advice = status
        .advice
        .iter()
        .find(|entry| entry.id == "integration.fast-forward-available")
        .expect("a clean lag must be reported");

    assert_eq!(advice.severity, StatusAdviceSeverity::Notice);
    assert!(
        advice.summary.contains("fast-forward"),
        "{}",
        advice.summary
    );
    assert!(
        !status
            .advice
            .iter()
            .any(|entry| entry.severity == StatusAdviceSeverity::Blocked),
        "a clean lag must not block: {:?}",
        status.advice
    );
}

/// The guard that matters: integration holding work upstream lacks is
/// divergence, and must still refuse rather than fast-forward over it.
#[test]
fn divergence_is_still_refused() {
    let (tmp, mut broker) = fixture(2);
    let root = tmp.path();

    // Put a commit on integration that upstream does not have.
    let integration = git(root, &["rev-parse", "refs/heads/aethyme/integration"]);
    git(root, &["switch", "-qc", "tmp-integration", &integration]);
    std::fs::write(root.join("only-on-integration.txt"), "mine\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "unique to integration"]);
    let diverged = git(root, &["rev-parse", "HEAD"]);
    git(
        root,
        &["update-ref", "refs/heads/aethyme/integration", &diverged],
    );
    git(root, &["switch", "-q", "main"]);

    let report = broker
        .auto_cleanup_landed_integration("refs/remotes/origin/main")
        .unwrap();
    assert_eq!(
        report.state,
        AutomaticIntegrationCleanupState::Deferred,
        "{}",
        report.explanation
    );
    assert_eq!(
        git(root, &["rev-parse", "refs/heads/aethyme/integration"]),
        diverged,
        "a diverged integration ref must not move"
    );
}

/// Put `count` commits on integration that `origin/main` lacks, the shape a
/// repository leaves behind when it promoted once and later switched to
/// verify-only (the SP42 case: six-week-old drafts superseded by merged PRs).
fn add_leftover_integration_work(root: &Path, count: usize) -> String {
    let integration = git(root, &["rev-parse", "refs/heads/aethyme/integration"]);
    git(root, &["switch", "-qc", "leftover", &integration]);
    for n in 0..count {
        std::fs::write(root.join(format!("leftover{n}.txt")), "draft\n").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-qm", &format!("leftover {n}")]);
    }
    let head = git(root, &["rev-parse", "HEAD"]);
    git(
        root,
        &["update-ref", "refs/heads/aethyme/integration", &head],
    );
    git(root, &["switch", "-q", "main"]);
    head
}

fn set_promote_mode(root: &Path, mode: &str) {
    std::fs::create_dir_all(root.join(".aethyme")).unwrap();
    std::fs::write(
        root.join(".aethyme/config.toml"),
        format!("[promote]\nmode = \"{mode}\"\n"),
    )
    .unwrap();
}

/// A verify-only repository's leftover integration work must surface on the
/// routine (unrefreshed) status every session runs first, not only after a
/// pull-request merge defers the post-merge cleanup.
#[test]
fn verify_only_status_reports_leftover_integration_work_without_refresh() {
    let (tmp, _broker) = fixture(1);
    let root = tmp.path();
    add_leftover_integration_work(root, 2);
    set_promote_mode(root, "verify-only");
    let mut broker = Broker::open(root).unwrap();

    let status = broker.status_current(0).unwrap();
    let advice = status
        .advice
        .iter()
        .find(|entry| entry.id == "integration.leftover-work")
        .unwrap_or_else(|| panic!("leftover work must be reported: {:?}", status.advice));

    assert_eq!(advice.severity, StatusAdviceSeverity::Warning);
    assert!(
        advice.summary.contains("2 commits origin/main lacks"),
        "{}",
        advice.summary
    );
    assert_eq!(
        advice.commands,
        vec!["aethyme broker advanced integration reconcile --upstream origin/main --dry-run"]
    );
}

/// Integration behind the published branch carries nothing of its own; it is
/// the normal verify-only state and must stay silent.
#[test]
fn verify_only_status_is_silent_when_integration_holds_nothing_of_its_own() {
    let (tmp, _broker) = fixture(2);
    set_promote_mode(tmp.path(), "verify-only");
    let mut broker = Broker::open(tmp.path()).unwrap();

    let status = broker.status_current(0).unwrap();
    assert!(
        !status
            .advice
            .iter()
            .any(|entry| entry.id == "integration.leftover-work"),
        "{:?}",
        status.advice
    );
}

/// Promoting repositories keep their own integration rows: work on
/// integration ahead of the published branch is the normal pending layer.
#[test]
fn promoting_status_does_not_report_pending_layer_as_leftover() {
    let (tmp, _broker) = fixture(1);
    add_leftover_integration_work(tmp.path(), 1);
    let mut broker = Broker::open(tmp.path()).unwrap();

    let status = broker.status_current(0).unwrap();
    assert!(
        !status
            .advice
            .iter()
            .any(|entry| entry.id == "integration.leftover-work"),
        "{:?}",
        status.advice
    );
}

fn doctor_cli(root: &Path, args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_broker-cli-shim"))
        .args(["status", "doctor"])
        .args(args)
        .current_dir(root)
        .env(
            "AETHYME_HOST_STATE_DIR",
            root.join(".aethyme/test-host-state"),
        )
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A repository health check must find the leftover work `status` reports,
/// with the same dry-run next action, and leave every ref where it was.
#[test]
fn verify_only_doctor_reports_leftover_integration_work() {
    let (tmp, _broker) = fixture(1);
    let root = tmp.path();
    let leftover = add_leftover_integration_work(root, 2);
    set_promote_mode(root, "verify-only");
    let upstream = git(root, &["rev-parse", "refs/remotes/origin/main"]);
    let mut broker = Broker::open(root).unwrap();

    let report = broker.doctor().unwrap();
    let advice = report
        .leftover_integration_work
        .as_ref()
        .expect("doctor must report leftover integration work");
    assert_eq!(advice.id, "integration.leftover-work");
    assert_eq!(advice.severity, StatusAdviceSeverity::Warning);
    assert!(
        advice.summary.contains("2 commits origin/main lacks"),
        "{}",
        advice.summary
    );
    let command = "aethyme broker advanced integration reconcile --upstream origin/main --dry-run";
    assert_eq!(advice.commands, vec![command]);
    assert!(report.healthy(), "leftover work is reported, not unhealthy");
    drop(broker);

    let json: serde_json::Value =
        serde_json::from_str(&doctor_cli(root, &["--json"])).expect("doctor --json");
    assert_eq!(
        json["leftover_integration_work"]["commands"][0], command,
        "{json}"
    );
    let text = doctor_cli(root, &[]);
    assert!(
        text.contains("integration leftover work: integration carries 2 commits"),
        "{text}"
    );
    assert!(text.contains(&format!("  run: {command}")), "{text}");

    assert_eq!(
        git(root, &["rev-parse", "refs/heads/aethyme/integration"]),
        leftover,
        "doctor must not move integration"
    );
    assert_eq!(
        git(root, &["rev-parse", "refs/remotes/origin/main"]),
        upstream,
        "doctor must not move the published branch"
    );
}

/// Integration behind the published branch holds nothing of its own, and the
/// JSON key is omitted rather than reported empty.
#[test]
fn verify_only_doctor_is_silent_when_integration_holds_nothing_of_its_own() {
    let (tmp, _broker) = fixture(2);
    set_promote_mode(tmp.path(), "verify-only");
    let mut broker = Broker::open(tmp.path()).unwrap();

    assert!(broker.doctor().unwrap().leftover_integration_work.is_none());
    drop(broker);
    let json: serde_json::Value =
        serde_json::from_str(&doctor_cli(tmp.path(), &["--json"])).expect("doctor --json");
    assert!(json.get("leftover_integration_work").is_none(), "{json}");
}

/// Promoting repositories keep their pending layer on integration; `doctor`
/// follows `status` and does not call it leftover.
#[test]
fn promoting_doctor_does_not_report_pending_layer_as_leftover() {
    let (tmp, _broker) = fixture(1);
    add_leftover_integration_work(tmp.path(), 1);
    let mut broker = Broker::open(tmp.path()).unwrap();

    assert!(broker.doctor().unwrap().leftover_integration_work.is_none());
}
