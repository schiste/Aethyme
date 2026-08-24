use std::path::Path;
use std::process::{Command, Output};

use aethyme_broker::{Broker, GateStatus, NewGateResult, events};

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

fn promoted_fixture() -> (tempfile::TempDir, i64) {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "init"]);

    let worktree = tmp.path().join(".aethyme/worktrees/finish-cli");
    std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    git(
        tmp.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent/finish-cli",
            worktree.to_str().unwrap(),
            "main",
        ],
    );
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.adopt(&worktree, Some("finish CLI fixture")).unwrap();
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
            gate_name: "finish-cli-gate".into(),
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
    (tmp, session.id)
}

#[test]
fn finish_cli_json_is_structured_and_persists_a_redacted_handoff() {
    let (tmp, session_id) = promoted_fixture();
    let session = session_id.to_string();
    let output = run(tmp.path(), &["finish", "--session", &session, "--json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "closed");
    assert_eq!(report["delivery"]["submitted"], true);
    assert_eq!(report["delivery"]["promoted"], true);
    assert_eq!(report["delivery"]["published"], false);
    assert_eq!(report["pending_work"]["present"], false);
    let src_lease = report["leases_held"]
        .as_array()
        .unwrap()
        .iter()
        .find(|lease| lease["path"] == "src/")
        .unwrap();
    assert_eq!(src_lease["state"], "active");
    assert_eq!(report["last_gate"]["gate"], "finish-cli-gate");
    assert_eq!(report["last_gate"]["cache_source"], "executed");
    assert!(report["last_gate"]["recorded_at"].is_i64());
    assert!(report["last_gate"]["tree_hash"].is_string());
    assert_eq!(report["cleanup_safe"], false);
    assert!(
        report["recommended_next_action"]
            .as_str()
            .unwrap()
            .starts_with("aethyme broker ship plan --entry ")
    );

    let mut broker = Broker::open(tmp.path()).unwrap();
    let event = broker
        .store()
        .events_after(0, i64::MAX)
        .unwrap()
        .into_iter()
        .find(|event| event.kind == events::SESSION_FINISHED)
        .unwrap();
    let payload = event.payload_json.unwrap();
    assert!(!payload.contains(tmp.path().to_str().unwrap()));
    assert!(!payload.contains("redacted/gate.log"));
}

#[test]
fn finish_cli_text_summarizes_the_structured_handoff() {
    let (tmp, session_id) = promoted_fixture();
    let session = session_id.to_string();
    let output = run(tmp.path(), &["finish", "--session", &session]);
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("delivery: submitted=yes, promoted=yes, published=no"),
        "{text}"
    );
    assert!(
        text.contains("pending work: no (0 dirty paths, 0 unsubmitted commits)"),
        "{text}"
    );
    assert!(text.contains("leases held:"), "{text}");
    assert!(text.contains("explicit active src/"), "{text}");
    assert!(
        text.contains("last gate: finish-cli-gate pass on tree"),
        "{text}"
    );
    assert!(text.contains("(executed)"), "{text}");
    assert!(text.contains("cleanup safe: no"), "{text}");
    assert!(
        text.contains("recommended next: aethyme broker ship plan --entry"),
        "{text}"
    );
}
