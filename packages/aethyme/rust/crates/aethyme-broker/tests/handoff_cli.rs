use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::{Broker, GateStatus, NewGateResult};

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

fn init_repo(repo: &Path) {
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-qm", "init"]);
}

fn finished_fixture() -> (tempfile::TempDir, PathBuf, i64) {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let worktree = tmp.path().join(".aethyme/worktrees/handoff-cli");
    std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    git(
        tmp.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent/handoff-cli",
            worktree.to_str().unwrap(),
            "main",
        ],
    );
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker
        .adopt(&worktree, Some("handoff CLI fixture"))
        .unwrap();
    std::fs::write(worktree.join("done.txt"), "done\n").unwrap();
    git(&worktree, &["add", "-A"]);
    git(&worktree, &["commit", "-qm", "done"]);
    broker
        .store()
        .claim_lease(session.id, "src/", None)
        .unwrap();
    assert!(broker.submit(session.id).unwrap().promoted);
    let tree = git(&worktree, &["rev-parse", "HEAD^{tree}"]);
    broker
        .store()
        .record_gate_result(&NewGateResult {
            gate_name: "handoff-cli-gate".into(),
            tree_hash: tree,
            definition_hash: "test-definition".into(),
            status: GateStatus::Pass,
            failure_class: None,
            exit_code: Some(0),
            duration_ms: Some(4),
            log_path: Some("/redacted/gate.log".into()),
            session_id: Some(session.id),
        })
        .unwrap();
    assert!(broker.finish(session.id).unwrap().closed);
    (tmp, worktree, session.id)
}

fn event_fingerprints(repo: &Path) -> Vec<(i64, String, Option<String>)> {
    let mut broker = Broker::open(repo).unwrap();
    broker
        .store()
        .events_after(0, i64::MAX)
        .unwrap()
        .into_iter()
        .map(|event| (event.id, event.kind, event.payload_json))
        .collect()
}

fn sorted_keys(value: &serde_json::Value) -> Vec<String> {
    let mut keys = value
        .as_object()
        .unwrap()
        .keys()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    keys.sort();
    keys
}

#[test]
fn handoff_json_by_session_has_a_stable_read_only_contract() {
    let (tmp, _worktree, session_id) = finished_fixture();
    let before = event_fingerprints(tmp.path());
    let session = session_id.to_string();
    let output = run(tmp.path(), &["handoff", "--session", &session, "--json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        sorted_keys(&report),
        [
            "cleanup_safe",
            "delivery",
            "event_id",
            "last_gate",
            "latest_queue_entry_id",
            "latest_queue_status",
            "leases_held",
            "pending_work",
            "recommended_next_action",
            "recorded_at",
            "session_id",
            "status",
        ]
    );
    assert_eq!(report["session_id"], session_id);
    assert_eq!(report["status"], "closed");
    assert_eq!(report["delivery"]["promoted"], true);
    assert_eq!(report["delivery"]["published"], false);
    assert_eq!(report["last_gate"]["gate"], "handoff-cli-gate");
    assert!(
        report["leases_held"]
            .as_array()
            .unwrap()
            .iter()
            .any(|lease| lease["path"] == "src/")
    );
    assert!(report["event_id"].is_i64());
    assert!(report["recorded_at"].is_i64());
    assert!(report.get("worktree_path").is_none());
    assert_eq!(event_fingerprints(tmp.path()), before);
    assert!(
        !tmp.path()
            .join(".aethyme/logs/command-metrics.jsonl")
            .exists()
    );
}

#[test]
fn handoff_by_worktree_returns_the_latest_completed_session() {
    let (tmp, worktree, first_session_id) = finished_fixture();
    let mut broker = Broker::open(tmp.path()).unwrap();
    let second = broker
        .adopt(&worktree, Some("follow-up on the same worktree"))
        .unwrap();
    assert!(second.id > first_session_id);
    assert!(broker.finish(second.id).unwrap().closed);

    let output = run(
        tmp.path(),
        &["handoff", "--worktree", worktree.to_str().unwrap()],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains(&format!("Session {} handoff: closed", second.id)),
        "{text}"
    );
    assert!(
        text.contains("delivery: submitted=no, promoted=no, published=no"),
        "{text}"
    );
    assert!(text.contains("leases: 0 recorded"), "{text}");
    assert!(text.contains("last gate: none recorded"), "{text}");
    assert!(text.contains("cleanup safe: no"), "{text}");
}

#[test]
fn handoff_by_absolute_worktree_survives_worktree_removal() {
    let (tmp, worktree, session_id) = finished_fixture();
    git(
        tmp.path(),
        &["worktree", "remove", "--force", worktree.to_str().unwrap()],
    );
    assert!(!worktree.exists());

    let output = run(
        tmp.path(),
        &[
            "handoff",
            "--worktree",
            worktree.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["session_id"], session_id);
}

#[test]
fn handoff_requires_one_selector_and_a_completed_handoff() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.adopt(tmp.path(), Some("still active")).unwrap();
    let session_id = session.id.to_string();

    let missing = run(tmp.path(), &["handoff", "--session", &session_id]);
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("has no completed handoff"));

    let neither = run(tmp.path(), &["handoff"]);
    assert!(!neither.status.success());
    assert!(
        String::from_utf8_lossy(&neither.stderr)
            .contains("requires --session <id> or --worktree <path>")
    );

    let both = run(
        tmp.path(),
        &[
            "handoff",
            "--session",
            &session_id,
            "--worktree",
            tmp.path().to_str().unwrap(),
        ],
    );
    assert!(!both.status.success());
    assert!(String::from_utf8_lossy(&both.stderr).contains("not both"));
}
