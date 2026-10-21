mod common;

use std::path::Path;
use std::process::{Command, Output};

use aethyme_broker::{
    Broker, ExternalEventEnvelope, ExternalEventProvider, ExternalVerificationMethod,
    NewPrWatchState, VerifiedExternalSource, external_event_digest,
};
use common::{FixtureRepository, open_fixture_repository_broker};

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

fn fresh_unbound_child(repo: &Path, args: &[&str]) -> Command {
    let fixture_root = repo
        .parent()
        .expect("fixture repository has an owner")
        .canonicalize()
        .expect("canonicalize fixture owner");
    let mut command = common::broker_cli_for_fixture(CLI, args, &fixture_root);
    let host_state = common::fixture_host_state_dir(&fixture_root);
    let worktree_root = fixture_root.join("worktrees");
    command
        .current_dir(repo)
        .env("AETHYME_BROKER_DB", repo.join(".aethyme/broker.db"))
        .env(
            "AETHYME_BROKER_OPEN_HOST_OPERATION_DB",
            host_state.join("host-operations.db"),
        )
        .env("AETHYME_BROKER_OPEN_HOST_STATE_EXPLICIT", "true")
        .env("AETHYME_BROKER_OPEN_WORKTREE_ROOT", worktree_root)
        .env(
            "AETHYME_BROKER_OPEN_COLLABORATION_ROOT",
            host_state.join("collaboration"),
        )
        .env_remove("AETHYME_WORKTREE_ROOT");
    command
}

fn run(_broker: &Broker, repo: &Path, args: &[&str]) -> Output {
    fresh_unbound_child(repo, args).output().unwrap()
}

fn fixture() -> FixtureRepository {
    let fixture = FixtureRepository::new();
    let repo = fixture.path();
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-qm", "initial"]);
    fixture
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
    let fixture = fixture();
    let repo = fixture.path();
    git(
        repo,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/product.git",
        ],
    );
    let mut broker = open_fixture_repository_broker(&fixture).unwrap();
    let session = broker.adopt(repo, Some("SECRET task")).unwrap();
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
    let head = git(repo, &["rev-parse", "HEAD"]);
    let event_path = repo.join("event.json");
    std::fs::write(
        &event_path,
        serde_json::to_vec_pretty(&envelope("cli-delivery", &head)).unwrap(),
    )
    .unwrap();

    let ingest = run(
        &broker,
        repo,
        &[
            "advanced",
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

    let metric_path = repo.join(".aethyme/logs/command-metrics.jsonl");
    let metrics_before = std::fs::read(&metric_path).unwrap();
    let list = run(
        &broker,
        repo,
        &["advanced", "external-events", "list", "--all", "--json"],
    );
    assert!(list.status.success());
    let inventory: serde_json::Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(inventory["schema_version"], 1);
    assert_eq!(inventory["events"].as_array().unwrap().len(), 1);
    let show = run(
        &broker,
        repo,
        &[
            "advanced",
            "external-events",
            "show",
            &event_id.to_string(),
            "--json",
        ],
    );
    assert!(show.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&show.stdout).unwrap()["id"],
        event_id
    );
    assert_eq!(std::fs::read(&metric_path).unwrap(), metrics_before);
    assert!(!repo.join(".aethyme/external-events.sock").exists());
    assert!(!repo.join(".aethyme/external-events.pid").exists());

    let unknown_path = repo.join("unknown.json");
    std::fs::write(
        &unknown_path,
        serde_json::to_vec_pretty(&envelope("unknown-owner", &"a".repeat(40))).unwrap(),
    )
    .unwrap();
    let unknown = run(
        &broker,
        repo,
        &[
            "advanced",
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
        &broker,
        repo,
        &[
            "advanced",
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
    let fixture = fixture();
    let repo = fixture.path();
    let broker = open_fixture_repository_broker(&fixture).unwrap();

    let missing = run(
        &broker,
        repo,
        &["advanced", "external-events", "reconcile", "1"],
    );
    assert!(!missing.status.success());
    let error = String::from_utf8_lossy(&missing.stderr);
    assert!(error.contains("--outcome <assign|ignore>"), "{error}");
    assert!(error.contains("--reason <text>"), "{error}");
    assert!(error.contains("--session <id>"), "{error}");
}
