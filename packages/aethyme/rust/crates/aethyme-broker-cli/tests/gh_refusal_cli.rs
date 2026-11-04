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

use aethyme_broker::{Broker, BrokerOpenOptions};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
mod common;

/// `pr update-branch` answers with the stderr and exit status the test sets,
/// or kills itself to stand in for a crash part-way through the request.
const FAKE_GH: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$AETHYME_FAKE_GH_LOG"
case "$1 $2" in
  'pr view')
    printf '{"headRefName":"agent/pr-506","headRefOid":"%s","baseRefName":"main"}\n' \
      1111111111111111111111111111111111111111
    exit 0
    ;;
  'pr update-branch')
    if [ -n "$AETHYME_FAKE_GH_DELAY" ]; then sleep "$AETHYME_FAKE_GH_DELAY"; fi
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
        self.run_with_delay(args, stderr, rc, "")
    }

    fn run_with_delay(&self, args: &[&str], stderr: &str, rc: &str, delay: &str) -> Output {
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
            .env("AETHYME_FAKE_GH_LOG", self.bin.path().join("gh.log"))
            .env("AETHYME_FAKE_GH_STDERR", stderr)
            .env("AETHYME_FAKE_GH_RC", rc)
            .env("AETHYME_FAKE_GH_DELAY", delay)
            .output()
            .unwrap()
    }

    fn broker(&self) -> Broker {
        Broker::open_with_options(
            self.repo.path(),
            BrokerOpenOptions::default()
                .with_host_state_dir(self.state.path())
                .with_host_operation_database(self.state.path().join("host-operations.db")),
        )
        .unwrap()
    }

    fn seed_open_pr_listing(&self) -> String {
        let fetched_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let listing = serde_json::json!({
            "fetched_at_ms": fetched_at_ms,
            "base": "main",
            "prs": [{
                "number": 506,
                "url": "https://github.com/schiste/Aethyme/pull/506",
                "head": "agent/pr-506",
                "head_oid": "1111111111111111111111111111111111111111"
            }]
        })
        .to_string();
        let mut broker = self.broker();
        broker
            .store()
            .meta_set("pr_overlap.open_prs", &listing)
            .unwrap();
        listing
    }

    fn cached_open_pr_listing(&self) -> Option<String> {
        let mut broker = self.broker();
        broker.store().meta_get("pr_overlap.open_prs").unwrap()
    }

    fn gh_operation(
        &self,
        flags: &[&str],
        gh_args: &[&str],
        stderr: &str,
        rc: &str,
        delay: &str,
    ) -> (Output, String) {
        let mut args = vec![
            "advanced",
            "gh",
            "--session",
            &self.session,
            "--repo",
            "schiste/Aethyme",
            "--reason",
            "update the PR operation fixture",
        ];
        args.extend(flags);
        args.push("--");
        args.extend(gh_args);
        let output = self.run_with_delay(&args, stderr, rc, delay);
        let listed = self.run(&["advanced", "operations", "list", "--json"], "", "0");
        let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
        let operations = listed["operations"].as_array().unwrap();
        assert_eq!(operations.len(), 1, "{listed}");
        let status = operations[0]["status"].as_str().unwrap().to_string();
        (output, status)
    }

    /// Run `gh pr update-branch` through the broker and return the status the
    /// journal recorded for it.
    fn update_branch(&self, stderr: &str, rc: &str) -> (Output, String) {
        self.gh_operation(&[], &["pr", "update-branch", "506"], stderr, rc, "")
    }
}

fn assert_invalidated_listing(fixture: &Fixture, original: &str) {
    let current = fixture
        .cached_open_pr_listing()
        .expect("the stale listing sentinel remains in the cache key");
    assert_ne!(
        current, original,
        "the successful or uncertain write invalidates the listing"
    );
    let value: serde_json::Value = serde_json::from_str(&current).unwrap();
    assert_eq!(value["fetched_at_ms"], 0);
    assert_eq!(value["base"], "");
    assert_eq!(value["prs"], serde_json::json!([]));
}

#[test]
fn a_successful_pr_topology_write_invalidates_cached_open_pr_membership() {
    let fixture = Fixture::new();
    let original = fixture.seed_open_pr_listing();
    let (output, status) = fixture.update_branch("", "0");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(status, "succeeded");
    assert_invalidated_listing(&fixture, &original);
}

#[test]
fn changing_a_pr_base_invalidates_cached_open_pr_membership() {
    let fixture = Fixture::new();
    let original = fixture.seed_open_pr_listing();
    let (output, status) = fixture.gh_operation(
        &[],
        &["pr", "edit", "506", "--base", "develop"],
        "",
        "0",
        "",
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(status, "succeeded");
    assert_invalidated_listing(&fixture, &original);
}

#[test]
fn a_conflict_refusal_is_failed_and_does_not_write_block() {
    let fixture = Fixture::new();
    let original = fixture.seed_open_pr_listing();
    let (output, status) = fixture.update_branch("X Cannot update PR branch due to conflicts", "1");

    assert_eq!(status, "failed");
    assert_eq!(
        fixture.cached_open_pr_listing().as_deref(),
        Some(original.as_str())
    );
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
    let original = fixture.seed_open_pr_listing();
    let (output, status) = fixture.update_branch(
        "Post \"https://api.github.com/graphql\": read: connection reset by peer",
        "1",
    );

    assert_eq!(status, "outcome_unknown");
    assert_invalidated_listing(&fixture, &original);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("write-blocked"), "{stderr}");
}

#[test]
fn a_timed_out_pr_topology_write_invalidates_cached_open_pr_membership() {
    let fixture = Fixture::new();
    let original = fixture.seed_open_pr_listing();
    let (output, status) = fixture.gh_operation(
        &["--queue-timeout", "5"],
        &["pr", "update-branch", "506"],
        "",
        "0",
        "30",
    );

    assert_eq!(status, "outcome_unknown");
    assert!(!output.status.success());
    assert_invalidated_listing(&fixture, &original);
    let gh_log = std::fs::read_to_string(fixture.bin.path().join("gh.log")).unwrap();
    assert!(gh_log.contains("pr update-branch 506"), "{gh_log}");
}

#[test]
fn a_body_only_pr_edit_keeps_cached_open_pr_membership() {
    let fixture = Fixture::new();
    let original = fixture.seed_open_pr_listing();
    let (output, status) = fixture.gh_operation(
        &[],
        &[
            "pr",
            "edit",
            "506",
            "--body=do not close; --base=release; state=closed",
        ],
        "",
        "0",
        "",
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(status, "succeeded");
    assert_eq!(
        fixture.cached_open_pr_listing().as_deref(),
        Some(original.as_str())
    );
}

#[test]
fn a_killed_command_stays_outcome_unknown_even_after_a_refusal_message() {
    let fixture = Fixture::new();
    let (_, status) = fixture.update_branch("X Cannot update PR branch due to conflicts", "kill");

    assert_eq!(status, "outcome_unknown");
}
