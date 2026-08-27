use std::path::Path;
use std::process::{Command, Output};

use aethyme_broker::{
    AdvisoryEvidence, AdvisoryResolutionState, AdvisorySeverity, BROKER_ADVISORY_RELPATH, Broker,
    NewAdvisory,
};

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

fn run(repo: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap()
}

fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "init"]);
    tmp
}

fn sample(identity: &str, session_id: Option<i64>, queue_entry_id: Option<i64>) -> NewAdvisory {
    NewAdvisory {
        identity: identity.into(),
        session_id,
        severity: AdvisorySeverity::Warning,
        queue_entry_id,
        integration_sha: Some("a".repeat(40)),
        paths: vec!["src/z.rs".into(), "src/a.rs".into(), "src/z.rs".into()],
        evidence: vec![AdvisoryEvidence {
            kind: "integration_drift".into(),
            summary: "the reviewed integration tip moved".into(),
        }],
    }
}

#[test]
fn list_show_and_ack_keep_database_authoritative_and_projection_current() {
    let tmp = fixture();
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.adopt(tmp.path(), Some("advisory fixture")).unwrap();
    let head = git(tmp.path(), &["rev-parse", "HEAD"]);
    let entry = broker.store().submit(session.id, &head, &head).unwrap();
    let created = broker
        .persist_advisory(sample(
            "integration-drift:fixture",
            Some(session.id),
            Some(entry.id),
        ))
        .unwrap();
    assert_eq!(created.paths, ["src/a.rs", "src/z.rs"]);

    let projection_path = tmp.path().join(BROKER_ADVISORY_RELPATH);
    let projection = std::fs::read_to_string(&projection_path).unwrap();
    assert!(projection.contains("integration-drift:fixture"));
    assert!(projection.contains(&format!("advisories ack {}", created.id)));
    assert!(
        std::fs::read_dir(projection_path.parent().unwrap())
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp."))
    );
    let event_count_before_reads = broker.store().events_after(0, i64::MAX).unwrap().len();

    let listed = run(tmp.path(), &["advisories", "list", "--json"]);
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed["outstanding_count"], 1);
    assert_eq!(listed["includes_acknowledged"], false);
    assert_eq!(listed["advisories"][0]["id"], created.id);
    assert_eq!(listed["advisories"][0]["session_id"], session.id);
    assert_eq!(listed["advisories"][0]["queue_entry_id"], entry.id);
    assert_eq!(listed["advisories"][0]["integration_sha"], "a".repeat(40));

    let id = created.id.to_string();
    let shown = run(tmp.path(), &["advisories", "show", &id]);
    let shown_text = String::from_utf8_lossy(&shown.stdout);
    assert!(shown.status.success());
    assert!(shown_text.contains("integration-drift:fixture"));
    assert!(shown_text.contains("integration_drift"));
    assert_eq!(
        broker.store().events_after(0, i64::MAX).unwrap().len(),
        event_count_before_reads,
        "list and show must remain read-only"
    );

    let acknowledged = run(tmp.path(), &["advisories", "ack", &id, "--json"]);
    assert!(
        acknowledged.status.success(),
        "{}",
        String::from_utf8_lossy(&acknowledged.stderr)
    );
    let acknowledged: serde_json::Value = serde_json::from_slice(&acknowledged.stdout).unwrap();
    assert_eq!(acknowledged["resolution_state"], "acknowledged");
    assert!(acknowledged["acknowledged_at"].as_i64().is_some());

    let projection = std::fs::read_to_string(&projection_path).unwrap();
    assert!(projection.contains("No outstanding advisories."));
    assert!(!projection.contains("integration-drift:fixture"));
    assert!(broker.advisories(false).unwrap().is_empty());
    let history = broker.advisory_list(true).unwrap();
    assert_eq!(history.advisories.len(), 1);
    assert_eq!(
        history.advisories[0].resolution_state,
        AdvisoryResolutionState::Acknowledged
    );

    let all = run(tmp.path(), &["advisories", "list", "--all", "--json"]);
    let all: serde_json::Value = serde_json::from_slice(&all.stdout).unwrap();
    assert_eq!(all["outstanding_count"], 0);
    assert_eq!(all["includes_acknowledged"], true);
    assert_eq!(all["advisories"].as_array().unwrap().len(), 1);
    assert!(git(tmp.path(), &["status", "--short"]).is_empty());
}

#[test]
fn projection_failure_does_not_erase_authoritative_advisory_state() {
    let tmp = fixture();
    let projection = tmp.path().join(BROKER_ADVISORY_RELPATH);
    std::fs::create_dir_all(&projection).unwrap();
    let mut broker = Broker::open(tmp.path()).unwrap();

    let result = broker.persist_advisory(sample("projection-failure", None, None));
    assert!(result.is_err());
    let stored = broker.advisories(false).unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].identity, "projection-failure");

    std::fs::remove_dir(&projection).unwrap();
    broker.refresh_advisory_projection().unwrap();
    assert!(
        std::fs::read_to_string(projection)
            .unwrap()
            .contains("projection-failure")
    );
}

#[test]
fn concurrent_producers_leave_one_complete_authoritative_projection() {
    let tmp = fixture();
    let root = tmp.path().to_path_buf();
    Broker::open(&root).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let mut workers = Vec::new();
    for identity in ["concurrent:first", "concurrent:second"] {
        let root = root.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            Broker::open(&root)
                .unwrap()
                .persist_advisory(sample(identity, None, None))
                .unwrap();
        }));
    }
    barrier.wait();
    for worker in workers {
        worker.join().unwrap();
    }

    let broker = Broker::open(&root).unwrap();
    assert_eq!(broker.advisories(false).unwrap().len(), 2);
    let projection = std::fs::read_to_string(root.join(BROKER_ADVISORY_RELPATH)).unwrap();
    assert!(projection.contains("concurrent:first"));
    assert!(projection.contains("concurrent:second"));
    assert_eq!(projection.matches("## WARNING").count(), 2);
}

#[test]
fn advisory_cli_rejects_missing_invalid_and_unknown_ids() {
    let tmp = fixture();
    Broker::open(tmp.path()).unwrap();
    for args in [
        &["advisories", "show"][..],
        &["advisories", "show", "zero"][..],
        &["advisories", "ack", "0"][..],
        &["advisories", "ack", "999"][..],
    ] {
        let output = run(tmp.path(), args);
        assert!(!output.status.success(), "unexpected success for {args:?}");
    }
}
