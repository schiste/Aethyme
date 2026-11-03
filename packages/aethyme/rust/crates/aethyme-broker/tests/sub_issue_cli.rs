//! Exercise native sub-issue changes without contacting GitHub.
#![cfg(unix)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn git(repo: &Path, args: &[&str]) {
    let result = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn resolves_all_children_before_coordinated_mutations() {
    let repo = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let gh = bin.path().join("gh");
    std::fs::write(
        &gh,
        r##"#!/bin/sh
printf '%s\n' "$*" >> "$FAKE_GH_LOG"
case "$*" in
  'api repos/owner/repo/issues/2') printf '{"number":2,"id":1002}\n' ;;
  'api repos/owner/repo/issues/3') printf '{"number":3,"id":1003}\n' ;;
  'api repos/owner/repo/issues/4') printf '{"number":4,"id":1004,"pull_request":{}}\n' ;;
  'api -X POST repos/owner/repo/issues/1/sub_issues -F sub_issue_id='*) printf '{}\n' ;;
  *) printf 'unexpected fake gh command: %s\n' "$*" >&2; exit 1 ;;
esac
"##,
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    git(repo.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(repo.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    std::fs::write(repo.path().join("a.txt"), "a\n").unwrap();
    git(repo.path(), &["add", ".gitignore", "a.txt"]);
    git(repo.path(), &["commit", "-qm", "init"]);
    let log = repo.path().join("gh-log");
    let run = |args: &[&str]| -> Output {
        common::broker_cli(CLI, args)
            .current_dir(repo.path())
            .env("AETHYME_HOST_STATE_DIR", state.path())
            .env("FAKE_GH_LOG", &log)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin.path().display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .output()
            .unwrap()
    };
    let started = run(&["start", "--task", "test sub-issue", "--json"]);
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&started.stdout).unwrap();
    let session = value["session"]["id"]
        .as_i64()
        .or_else(|| value["id"].as_i64())
        .unwrap()
        .to_string();
    let arguments = |last: &'static str| {
        vec![
            "advanced",
            "issues",
            "sub-issue",
            "add",
            "1",
            "2",
            last,
            "--session",
            &session,
            "--repo",
            "owner/repo",
            "--reason",
            "test authorized issue relationship",
            "--json",
        ]
    };
    let invalid = run(&arguments("4"));
    assert!(!invalid.status.success());
    let reads = std::fs::read_to_string(&log).unwrap();
    assert!(!reads.contains("-X POST"), "{reads}");
    std::fs::write(&log, "").unwrap();
    let result = run(&arguments("3"));
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(receipt.as_array().unwrap().len(), 2);
    assert_eq!(receipt[0]["child"], 2);
    assert!(receipt[0]["operation"].as_i64().unwrap() > 0);
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains("sub_issue_id=1002"), "{calls}");
    assert!(calls.contains("sub_issue_id=1003"), "{calls}");
}
