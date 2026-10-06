//! A write GitHub definitively refused had no effect, so it must not
//! write-block the repository; a write whose fate is unclear still must.
//!
//! Observed 2026-10-03: `gh pr update-branch 506` exited with "Cannot update
//! PR branch due to conflicts", the PR head was unchanged, and the operation
//! was journaled `outcome_unknown`, write-blocking the repository until a
//! human reconciled it as failed.
//!
//! Every case drives a fake `gh` on `PATH`, so nothing reaches GitHub.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
mod common;

/// `pr update-branch` answers with the stderr and exit status the test sets,
/// or kills itself to stand in for a crash part-way through the request.
const FAKE_GH: &str = r#"#!/bin/sh
case "$1 $2" in
  'pr view')
    printf '{"headRefName":"agent/pr-506","headRefOid":"%s","baseRefName":"main"}\n' \
      1111111111111111111111111111111111111111
    exit 0
    ;;
  'pr update-branch')
    printf '%s\n' "$AETHYME_FAKE_GH_STDERR" >&2
    if [ "$AETHYME_FAKE_GH_RC" = 'kill' ]; then kill -9 $$; fi
    exit "$AETHYME_FAKE_GH_RC"
    ;;
esac
exit 0
"#;

struct Fixture {
    repo: tempfile::TempDir,
    state: tempfile::TempDir,
    bin: tempfile::TempDir,
    session: String,
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}");
}

impl Fixture {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt as _;

        let repo = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let gh = bin.path().join("gh");
        std::fs::write(&gh, FAKE_GH).unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();

        git(repo.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(repo.path().join(".gitignore"), "/.aethyme/\n").unwrap();
        std::fs::write(repo.path().join("a.txt"), "a\n").unwrap();
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-qm", "init"]);
        // The branch guard requires --repo to be this checkout's origin.
        git(
            repo.path(),
            &[
                "remote",
                "add",
                "origin",
                "git@github.com:schiste/Aethyme.git",
            ],
        );

        let mut fixture = Self {
            repo,
            state,
            bin,
            session: String::new(),
        };
        let started = fixture.run(&["start", "--task", "gh refusal", "--json"], "", "0");
        assert!(
            started.status.success(),
            "start failed: {}",
            String::from_utf8_lossy(&started.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&started.stdout).unwrap();
        fixture.session = value["session"]["id"]
            .as_i64()
            .or_else(|| value["id"].as_i64())
            .unwrap()
            .to_string();
        fixture
    }

    fn run(&self, args: &[&str], stderr: &str, rc: &str) -> Output {
        common::broker_cli(CLI, args)
            .current_dir(self.repo.path())
            .env("AETHYME_HOST_STATE_DIR", self.state.path())
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.path().display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("GIT_SSH_COMMAND", "false")
            .env("AETHYME_FAKE_GH_STDERR", stderr)
            .env("AETHYME_FAKE_GH_RC", rc)
            .output()
            .unwrap()
    }

    /// Run `gh pr update-branch` through the broker and return the status the
    /// journal recorded for it.
    fn update_branch(&self, stderr: &str, rc: &str) -> (Output, String) {
        let output = self.run(
            &[
                "advanced",
                "gh",
                "--session",
                &self.session,
                "--repo",
                "schiste/Aethyme",
                "--reason",
                "update the PR branch this test is about",
                "--",
                "pr",
                "update-branch",
                "506",
            ],
            stderr,
            rc,
        );
        let listed = self.run(&["advanced", "operations", "list", "--json"], "", "0");
        let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
        let operations = listed["operations"].as_array().unwrap();
        assert_eq!(operations.len(), 1, "{listed}");
        let status = operations[0]["status"].as_str().unwrap().to_string();
        (output, status)
    }
}

#[test]
fn a_conflict_refusal_is_failed_and_does_not_write_block() {
    let fixture = Fixture::new();
    let (output, status) = fixture.update_branch("X Cannot update PR branch due to conflicts", "1");

    assert_eq!(status, "failed");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("write-blocked"), "{stderr}");
    assert!(
        stderr.contains("Cannot update PR branch due to conflicts"),
        "{stderr}"
    );
}

#[test]
fn a_transport_failure_stays_outcome_unknown() {
    let fixture = Fixture::new();
    let (output, status) = fixture.update_branch(
        "Post \"https://api.github.com/graphql\": read: connection reset by peer",
        "1",
    );

    assert_eq!(status, "outcome_unknown");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("write-blocked"), "{stderr}");
}

#[test]
fn a_killed_command_stays_outcome_unknown_even_after_a_refusal_message() {
    let fixture = Fixture::new();
    let (_, status) = fixture.update_branch("X Cannot update PR branch due to conflicts", "kill");

    assert_eq!(status, "outcome_unknown");
}
