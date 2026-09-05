use std::path::Path;
use std::process::{Command, Output};

use aethyme_broker::{
    AdvisoryAudience, AdvisoryEvidence, AdvisoryProducer, AdvisoryResolutionState,
    AdvisorySeverity, BROKER_ADVISORY_RELPATH, Broker, MergeStatus, NewAdvisory,
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
        audience: AdvisoryAudience::Session,
        producer: AdvisoryProducer::Coordination,
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

fn record_conflict(broker: &mut Broker, session_id: i64, sequence: i64, paths: &[&str]) {
    let head = format!("{sequence:040x}");
    let entry = broker.store().submit(session_id, &head, &head).unwrap();
    let details = serde_json::json!({"conflicts": paths}).to_string();
    broker
        .store()
        .set_merge_status(entry.id, MergeStatus::Conflict, None, Some(&details))
        .unwrap();
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
        "content-free delivery correlation must not expand event history"
    );

    let metrics = run(tmp.path(), &["advisories", "metrics", "--json"]);
    assert!(metrics.status.success());
    let metrics_text = String::from_utf8(metrics.stdout).unwrap();
    let metrics_json: serde_json::Value = serde_json::from_str(&metrics_text).unwrap();
    assert_eq!(metrics_json["schema_version"], 1);
    assert_eq!(metrics_json["summary"]["shown_advisories"], 1);
    assert!(metrics_json["summary"]["total_shows"].as_u64().unwrap() >= 2);
    for forbidden in [
        "advisory fixture",
        "src/z.rs",
        "the reviewed integration tip moved",
        "task",
        "paths",
        "evidence",
    ] {
        assert!(!metrics_text.contains(forbidden), "leaked {forbidden:?}");
    }

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
fn maintainer_recommendations_reopen_suppress_and_resolve_deterministically() {
    let tmp = fixture();
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.adopt(tmp.path(), Some("private task text")).unwrap();
    for sequence in 1..=3 {
        record_conflict(
            &mut broker,
            session.id,
            sequence,
            &["src/reopen.rs", "src/resolve.rs"],
        );
    }
    drop(broker);

    let initial = run(tmp.path(), &["advisories", "list", "--json"]);
    assert!(initial.status.success());
    let initial: serde_json::Value = serde_json::from_slice(&initial.stdout).unwrap();
    let advisories = initial["advisories"].as_array().unwrap();
    assert_eq!(advisories.len(), 2);
    assert!(
        advisories
            .iter()
            .all(|item| item["audience"] == "maintainer")
    );
    let reopen_id = advisories
        .iter()
        .find(|item| item["paths"][0] == "src/reopen.rs")
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    let resolve_id = advisories
        .iter()
        .find(|item| item["paths"][0] == "src/resolve.rs")
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    assert!(
        !tmp.path().join(BROKER_ADVISORY_RELPATH).exists(),
        "maintainer refresh must not create the session delivery projection"
    );

    assert!(
        run(tmp.path(), &["advisories", "ack", &reopen_id.to_string()])
            .status
            .success()
    );
    let mut broker = Broker::open(tmp.path()).unwrap();
    let unrelated = broker
        .store()
        .submit(session.id, &format!("{:040x}", 4), &format!("{:040x}", 4))
        .unwrap();
    broker
        .store()
        .set_merge_status(unrelated.id, MergeStatus::Rejected, None, None)
        .unwrap();
    drop(broker);
    let unchanged = run(tmp.path(), &["advisories", "list", "--json"]);
    let unchanged: serde_json::Value = serde_json::from_slice(&unchanged.stdout).unwrap();
    assert!(
        unchanged["advisories"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["id"] != reopen_id),
        "unrelated history must not reopen acknowledged evidence"
    );

    let mut broker = Broker::open(tmp.path()).unwrap();
    record_conflict(&mut broker, session.id, 5, &["src/reopen.rs"]);
    drop(broker);
    let reopened = run(tmp.path(), &["advisories", "list", "--json"]);
    let reopened: serde_json::Value = serde_json::from_slice(&reopened.stdout).unwrap();
    assert_eq!(
        reopened["advisories"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["id"] == reopen_id)
            .unwrap()["resolution_state"],
        "outstanding"
    );

    assert!(
        run(
            tmp.path(),
            &["advisories", "suppress", &reopen_id.to_string()]
        )
        .status
        .success()
    );
    let mut broker = Broker::open(tmp.path()).unwrap();
    record_conflict(&mut broker, session.id, 6, &["src/reopen.rs"]);
    for sequence in 7..=206 {
        let head = format!("{sequence:040x}");
        let entry = broker.store().submit(session.id, &head, &head).unwrap();
        broker
            .store()
            .set_merge_status(entry.id, MergeStatus::Rejected, None, None)
            .unwrap();
    }
    drop(broker);

    let refreshed = run(tmp.path(), &["advisories", "list", "--all", "--json"]);
    assert!(
        refreshed.status.success(),
        "{}",
        String::from_utf8_lossy(&refreshed.stderr)
    );
    let refreshed: serde_json::Value = serde_json::from_slice(&refreshed.stdout).unwrap();
    let rows = refreshed["advisories"].as_array().unwrap();
    assert_eq!(
        rows.iter().find(|item| item["id"] == reopen_id).unwrap()["resolution_state"],
        "suppressed"
    );
    let resolved = rows.iter().find(|item| item["id"] == resolve_id).unwrap();
    assert_eq!(resolved["resolution_state"], "resolved");
    assert_eq!(resolved["resolution_evidence"], "bounded_clean_window");
    let serialized = serde_json::to_string(&refreshed).unwrap();
    for forbidden in ["private task text", "command", "environment", "/tmp/"] {
        assert!(!serialized.contains(forbidden), "leaked {forbidden:?}");
    }
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
fn maintainer_advisories_stay_out_of_session_projection() {
    let tmp = fixture();
    let mut broker = Broker::open(tmp.path()).unwrap();
    let mut recommendation = sample("history:conflict:src-lib", None, None);
    recommendation.audience = AdvisoryAudience::Maintainer;
    recommendation.producer = AdvisoryProducer::ConflictHistory;
    let created = broker.persist_advisory(recommendation).unwrap();

    assert_eq!(created.audience, AdvisoryAudience::Maintainer);
    assert_eq!(created.producer, AdvisoryProducer::ConflictHistory);
    assert!(
        std::fs::read_to_string(tmp.path().join(BROKER_ADVISORY_RELPATH))
            .unwrap()
            .contains("No outstanding advisories.")
    );
    let listed = broker.advisory_list(false).unwrap();
    assert_eq!(listed.advisories, [created]);
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
    let session_advisory = Broker::open(tmp.path())
        .unwrap()
        .persist_advisory(sample("session-cannot-be-suppressed", None, None))
        .unwrap();
    for args in [
        &["advisories", "show"][..],
        &["advisories", "show", "zero"][..],
        &["advisories", "ack", "0"][..],
        &["advisories", "ack", "999"][..],
        &["advisories", "suppress", "999"][..],
    ] {
        let output = run(tmp.path(), args);
        assert!(!output.status.success(), "unexpected success for {args:?}");
    }
    let suppression = run(
        tmp.path(),
        &["advisories", "suppress", &session_advisory.id.to_string()],
    );
    assert!(!suppression.status.success());
    assert!(String::from_utf8_lossy(&suppression.stderr).contains("session-facing"));
}

#[test]
fn session_commands_surface_notices_without_corrupting_json() {
    let tmp = fixture();
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.adopt(tmp.path(), Some("notice target")).unwrap();
    let mut advisory = sample("promotion-session-notice", Some(session.id), None);
    advisory.evidence.push(AdvisoryEvidence {
        kind: "safe_next_action".into(),
        summary: "aethyme broker status --json".into(),
    });
    let advisory = broker.persist_advisory(advisory).unwrap();

    let session_id = session.id.to_string();
    let command = run(
        tmp.path(),
        &[
            "leases",
            "plan",
            "src/a.rs",
            "--session",
            &session_id,
            "--json",
        ],
    );
    assert!(command.status.success());
    serde_json::from_slice::<serde_json::Value>(&command.stdout)
        .expect("session command stdout remains stable JSON");
    let stderr = String::from_utf8_lossy(&command.stderr);
    assert!(stderr.contains(&format!("Aethyme advisory {}", advisory.id)));
    assert!(stderr.contains(&"a".repeat(40)));
    assert!(stderr.contains("safe next action: aethyme broker status --json"));

    let status = run(tmp.path(), &["status", "--json"]);
    assert!(status.status.success());
    let status_json: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status_json["outstanding_advisories"][0]["id"], advisory.id);
    assert!(
        String::from_utf8_lossy(&status.stderr)
            .contains(&format!("Aethyme advisory {}", advisory.id)),
        "cwd-associated commands surface the live session advisory"
    );

    broker.acknowledge_advisory(advisory.id).unwrap();
    let quiet = run(
        tmp.path(),
        &["leases", "plan", "src/a.rs", "--session", &session_id],
    );
    assert!(quiet.status.success());
    assert!(!String::from_utf8_lossy(&quiet.stderr).contains("Aethyme advisory"));
}
