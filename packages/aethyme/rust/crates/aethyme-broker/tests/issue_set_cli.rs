//! Exercise issue-set planning and authorization without contacting GitHub.
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
fn plans_without_writes_and_denies_unapproved_target_policy() {
    let repo = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let gh = bin.path().join("gh");
    std::fs::write(&gh, r##"#!/bin/sh
printf '%s\n' "$*" >> "$AETHYME_FAKE_GH_LOG"
case "$*" in
  'api --hostname github.com repos/owner/repo/labels?per_page=100&page=1') printf '[{"name":"feature"}]\n' ;;
  'api --hostname github.com repos/owner/repo/issues?state=all&per_page=100&page=1') printf '[]\n' ;;
  'api --hostname github.com repos/owner/repo/commits?per_page=1') printf '[{"sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]\n' ;;
  'api --hostname github.com -H Accept: application/vnd.github.raw+json repos/owner/repo/contents/.aethyme/config.toml?ref=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa') printf '[issue_workflows]\nissue_sets = "deny"\n' ;;
  *) printf 'unexpected fake gh command: %s\n' "$*" >&2; exit 1 ;;
esac
"##).unwrap();
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
            .env("AETHYME_FAKE_GH_LOG", &log)
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
    let started = run(&["start", "--task", "test issue set", "--json"]);
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&started.stdout).unwrap();
    git(
        repo.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/owner/repo.git",
        ],
    );
    let session = value["session"]["id"]
        .as_i64()
        .or_else(|| value["id"].as_i64())
        .unwrap()
        .to_string();
    std::fs::write(repo.path().join("epic.md"), "See {{child}}").unwrap();
    std::fs::write(repo.path().join("child.md"), "See {{epic}}").unwrap();
    std::fs::write(
        repo.path().join("set.toml"),
        r#"set_id = "sample"
[parent]
key = "epic"
title = "Epic"
body_file = "epic.md"
labels = ["feature"]
[[issues]]
key = "child"
title = "Child"
body_file = "child.md"
depends_on = ["epic"]
"#,
    )
    .unwrap();
    let args = [
        "advanced",
        "issues",
        "apply",
        "set.toml",
        "--session",
        &session,
        "--repo",
        "owner/repo",
        "--reason",
        "test authorized set",
        "--json",
    ];
    let mut dry_args = args.to_vec();
    dry_args.push("--dry-run");
    let dry = run(&dry_args);
    assert!(
        dry.status.success(),
        "stdout: {} stderr: {}",
        String::from_utf8_lossy(&dry.stdout),
        String::from_utf8_lossy(&dry.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&dry.stdout).unwrap();
    let actions: Vec<_> = plan
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["action"].as_str().unwrap())
        .collect();
    assert_eq!(
        actions.iter().filter(|action| **action == "create").count(),
        2
    );
    assert!(actions.contains(&"ensure_sub_issue"));
    assert!(actions.contains(&"ensure_dependency"));
    let denied = run(&args);
    assert!(!denied.status.success());
    let denial: serde_json::Value = serde_json::from_slice(&denied.stdout).unwrap();
    assert!(
        denial["error"]["message"]
            .as_str()
            .unwrap()
            .contains("issue_workflows.issue_sets")
    );
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(!calls.contains("issue create"), "{calls}");
    assert!(!calls.contains("issue edit"), "{calls}");
    assert!(!calls.contains("-X POST"), "{calls}");
    assert!(!calls.contains("-X DELETE"), "{calls}");
}
