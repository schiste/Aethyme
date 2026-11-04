//! `broker advanced owners` is a documented read-only query (#1238): it must
//! observe broker state without initializing, maintaining, or delivering.
mod common;

use std::path::Path;
use std::process::{Command, Output};

use aethyme_broker::{
    AdvisoryAudience, AdvisoryEvidence, AdvisoryProducer, AdvisorySeverity, NewAdvisory,
};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
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

/// One commit on `main` landed by `Merge pull request #7`; returns its base.
fn fixture() -> (common::FixtureRepository, String) {
    let f = common::FixtureRepository::new();
    let repo = f.path();
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    git(repo, &["add", ".gitignore"]);
    git(repo, &["commit", "-qm", "init"]);
    let base = git(repo, &["rev-parse", "HEAD"]);
    git(repo, &["checkout", "-q", "-b", "agent/a"]);
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    git(repo, &["add", "a.txt"]);
    git(repo, &["commit", "-qm", "work on a"]);
    git(repo, &["checkout", "-q", "main"]);
    git(
        repo,
        &[
            "merge",
            "-q",
            "--no-ff",
            "agent/a",
            "-m",
            "Merge pull request #7 from o/agent/a",
        ],
    );
    (f, base)
}

fn owners(f: &common::FixtureRepository, cwd: &Path, base: &str) -> Output {
    let range = format!("{base}..main");
    let mut command = common::broker_cli_for_fixture(
        CLI,
        &["advanced", "owners", "--commits", &range, "--json"],
        f.path(),
    );
    command.current_dir(cwd).output().unwrap()
}

fn report(output: &Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn owners_does_not_create_broker_state() {
    let (f, base) = fixture();
    let report = report(&owners(&f, f.path(), &base));
    assert_eq!(report["commits"].as_array().unwrap().len(), 2);
    assert_eq!(report["commits"][0]["pull_request"], 7);
    assert!(
        !f.path().join(".aethyme/broker.db").exists(),
        "owners created broker state"
    );
    assert!(
        !common::fixture_host_state_dir(f.path()).exists(),
        "owners created host state"
    );
}

/// From a registered session worktree, owners still names the owner but
/// leaves the session's advisories undelivered and event history unchanged.
#[test]
fn owners_from_a_session_worktree_delivers_nothing() {
    let (f, base) = fixture();
    let mut broker = common::open_fixture_repository_broker(&f)
        .unwrap()
        .into_broker();
    let worktree = f.worktree_root().join("agent-b");
    std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    git(
        f.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent/b",
            worktree.to_str().unwrap(),
        ],
    );
    let session = broker.adopt(&worktree, Some("owners fixture")).unwrap();
    broker
        .persist_advisory(NewAdvisory {
            identity: "owners-observation:fixture".into(),
            audience: AdvisoryAudience::Session,
            producer: AdvisoryProducer::Coordination,
            session_id: Some(session.id),
            severity: AdvisorySeverity::Warning,
            queue_entry_id: None,
            integration_sha: None,
            paths: vec!["a.txt".into()],
            evidence: vec![AdvisoryEvidence {
                kind: "fixture".into(),
                summary: "an outstanding advisory".into(),
            }],
        })
        .unwrap();
    let events_before = broker.store().events_after(0, i64::MAX).unwrap().len();
    let shown_before = broker.advisory_delivery_summary().unwrap().shown_advisories;
    assert_eq!(shown_before, 0);

    let report = report(&owners(&f, &worktree, &base));
    assert_eq!(report["commits"][0]["branch"], "agent/a");

    assert_eq!(
        broker.advisory_delivery_summary().unwrap().shown_advisories,
        0,
        "owners recorded advisory delivery for the cwd session"
    );
    assert_eq!(
        broker.store().events_after(0, i64::MAX).unwrap().len(),
        events_before,
        "owners appended broker events"
    );
}
