use std::path::Path;
use std::process::{Command, Output};

use aethyme_broker::{
    Broker, ExternalEventEnvelope, ExternalEventProvider, ExternalVerificationMethod,
    NewPrWatchState, VerifiedExternalSource, external_event_digest,
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
    assert!(output.status.success(), "git {args:?} failed");
    String::from_utf8(output.stdout).unwrap().trim().into()
}

fn run(repo: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap()
}

fn envelope(id: &str, commit: &str) -> ExternalEventEnvelope {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let mut event = ExternalEventEnvelope {
        schema_version: 1,
        provider: ExternalEventProvider::Github,
        provider_event_id: id.into(),
        event_type: "validation_failed".into(),
        repository: "github.com/acme/product".into(),
        target_branch: "main".into(),
        pr_number: 42,
        commit_sha: commit.into(),
        occurred_at: now,
        verified_source: VerifiedExternalSource {
            method: ExternalVerificationMethod::AuthenticatedPoll,
            verified_at: now,
        },
        normalized_digest: "0".repeat(64),
    };
    event.normalized_digest = external_event_digest(&event);
    event
}

#[test]
fn external_event_cli_ingests_inspects_and_reconciles_without_background_state() {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "initial"]);
    git(
        tmp.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/product.git",
        ],
    );
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.adopt(tmp.path(), Some("SECRET task")).unwrap();
    broker
        .store()
        .upsert_pr_watch_state(&NewPrWatchState {
            target_branch: "main".into(),
            pr_number: 42,
            activity_fingerprint: "known".into(),
            marker: "none".into(),
            last_dispatch_at: None,
            last_agent_session_id: Some(session.id),
        })
        .unwrap();
    let head = git(tmp.path(), &["rev-parse", "HEAD"]);
    let event_path = tmp.path().join("event.json");
    std::fs::write(
        &event_path,
        serde_json::to_vec_pretty(&envelope("cli-delivery", &head)).unwrap(),
    )
    .unwrap();

    let ingest = run(
        tmp.path(),
        &[
            "external-events",
            "ingest",
            event_path.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(
        ingest.status.success(),
        "{}",
        String::from_utf8_lossy(&ingest.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&ingest.stdout).unwrap();
    assert_eq!(report["event"]["status"], "advisory_created");
    let event_id = report["event"]["id"].as_i64().unwrap();

    let metric_path = tmp.path().join(".aethyme/logs/command-metrics.jsonl");
    let metrics_before = std::fs::read(&metric_path).unwrap();
    let list = run(tmp.path(), &["external-events", "list", "--all", "--json"]);
    assert!(list.status.success());
    let inventory: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(inventory["schema_version"], 1);
    assert_eq!(inventory["events"].as_array().unwrap().len(), 1);
    let show = run(
        tmp.path(),
        &["external-events", "show", &event_id.to_string(), "--json"],
    );
    assert!(show.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&show.stdout).unwrap()["id"],
        event_id
    );
    assert_eq!(std::fs::read(&metric_path).unwrap(), metrics_before);
    assert!(!tmp.path().join(".aethyme/external-events.sock").exists());
    assert!(!tmp.path().join(".aethyme/external-events.pid").exists());

    let unknown_path = tmp.path().join("unknown.json");
    std::fs::write(
        &unknown_path,
        serde_json::to_vec_pretty(&envelope("unknown-owner", &"a".repeat(40))).unwrap(),
    )
    .unwrap();
    let unknown = run(
        tmp.path(),
        &[
            "external-events",
            "ingest",
            unknown_path.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(unknown.status.success());
    let unknown: serde_json::Value = serde_json::from_slice(&unknown.stdout).unwrap();
    assert_eq!(unknown["event"]["status"], "owner_not_found");
    let unknown_id = unknown["event"]["id"].as_i64().unwrap().to_string();
    let session_id = session.id.to_string();
    let reconciled = run(
        tmp.path(),
        &[
            "external-events",
            "reconcile",
            &unknown_id,
            "--outcome",
            "assign",
            "--session",
            &session_id,
            "--reason",
            "SECRET reviewed ownership reason",
            "--json",
        ],
    );
    assert!(
        reconciled.status.success(),
        "{}",
        String::from_utf8_lossy(&reconciled.stderr)
    );
    let reconciled = String::from_utf8(reconciled.stdout).unwrap();
    assert!(reconciled.contains("advisory_created"));
    assert!(!reconciled.contains("SECRET"));
}

#[test]
fn external_event_cli_requires_the_complete_reconciliation_contract() {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    git(tmp.path(), &["add", "README.md"]);
    git(tmp.path(), &["commit", "-qm", "initial"]);
    Broker::open(tmp.path()).unwrap();

    let missing = run(tmp.path(), &["external-events", "reconcile", "1"]);
    assert!(!missing.status.success());
    let error = String::from_utf8_lossy(&missing.stderr);
    assert!(error.contains("--outcome <assign|ignore>"), "{error}");
    assert!(error.contains("--reason <text>"), "{error}");
    assert!(error.contains("--session <id>"), "{error}");
}
