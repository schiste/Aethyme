//! The guided-init command is a top-level CLI route and reports its final readiness snapshot.

use std::path::Path;
use std::process::{Command, Output};

use aethyme_testkit::{aethyme_bin, tmp_dir};

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "Readiness Test")
        .env("GIT_AUTHOR_EMAIL", "readiness@example.com")
        .env("GIT_COMMITTER_NAME", "Readiness Test")
        .env("GIT_COMMITTER_EMAIL", "readiness@example.com")
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_repo(root: &Path) {
    git(root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("README.md"), "readiness fixture\n").unwrap();
    git(root, &["add", "README.md"]);
    git(root, &["commit", "-qm", "test: initialize fixture"]);
}

fn run_cli(root: &Path, host_state: &Path, args: &[&str]) -> Output {
    Command::new(aethyme_bin())
        .args(args)
        .current_dir(root)
        .env("AETHYME_HOST_STATE_DIR", host_state)
        .env_remove("AETHYME_ROOT")
        .env_remove("AETHYME_BROKER_DB")
        .env("XDG_CONFIG_HOME", root.join("empty-config"))
        .output()
        .expect("run aethyme CLI")
}

#[test]
fn guided_init_reports_one_post_run_readiness_snapshot() {
    let text_repo = tmp_dir();
    init_repo(text_repo.path());
    let text_state = text_repo.path().join("host-state");
    let text = run_cli(text_repo.path(), &text_state, &["init"]);
    assert!(
        text.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&text.stdout),
        String::from_utf8_lossy(&text.stderr)
    );
    let stdout = String::from_utf8_lossy(&text.stdout);
    assert!(stdout.contains("Repository initialized."));
    assert_eq!(stdout.matches("Operating mode:").count(), 1);
    assert!(stdout.contains("Operating mode: conflict_only"));
    assert!(stdout.contains("Coordination: ready"));
    assert!(stdout.contains("Agent context: not ready"));
    assert!(stdout.contains("Validation: limited"));
    assert!(stdout.contains("Parallel execution: not ready"));
    assert!(stdout.contains("Next actions:"));

    let json_repo = tmp_dir();
    init_repo(json_repo.path());
    let json_state = json_repo.path().join("host-state");
    let json = run_cli(json_repo.path(), &json_state, &["init", "--json"]);
    assert!(
        json.status.success(),
        "{}",
        String::from_utf8_lossy(&json.stderr)
    );
    let document: serde_json::Value = serde_json::from_slice(&json.stdout)
        .expect("--json emits exactly one JSON document with no prose");
    assert_eq!(document["readiness"]["operating_mode"], "conflict_only");
    let json_text = String::from_utf8(json.stdout).unwrap();
    let positions = ["certify", "scaffold", "gates", "changed", "readiness"]
        .map(|field| json_text.find(&format!("\n  \"{field}\":")).unwrap());
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));

    let certify_repo = tmp_dir();
    init_repo(certify_repo.path());
    let certify_state = certify_repo.path().join("host-state");
    let certify = run_cli(certify_repo.path(), &certify_state, &["certify", "--json"]);
    assert!(
        certify.status.success(),
        "{}",
        String::from_utf8_lossy(&certify.stderr)
    );
    let certify_document: serde_json::Value = serde_json::from_slice(&certify.stdout).unwrap();
    assert!(certify_document.get("readiness").is_none());
}
