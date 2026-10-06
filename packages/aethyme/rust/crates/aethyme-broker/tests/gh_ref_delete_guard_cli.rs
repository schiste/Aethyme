//! A `gh` command that deletes or rewrites a branch is held to the same
//! cross-session guard as `git push --delete` (#393).
//!
//! `gh pr merge --delete-branch` never names the branch it deletes, so the
//! guard used to see nothing to check and the head branch of another live
//! session could be deleted from any session id. The broker now resolves the
//! head branch first and refuses before anything is journaled or run.
//!
//! Every case drives a fake `gh` on `PATH`, so nothing reaches GitHub.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
mod common;

/// `pr view` answers with the head branch the test sets; every other call is
/// logged, so a test can tell whether the mutation itself ever ran.
const FAKE_GH: &str = r#"#!/bin/sh
if [ "$1 $2" = 'pr view' ]; then
  printf '%s\n' "$AETHYME_FAKE_HEAD"
  exit 0
fi
printf '%s\n' "$*" >> "$AETHYME_FAKE_GH_LOG"
exit 0
"#;

struct Session {
    id: String,
    branch: String,
}

struct Fixture {
    repo: tempfile::TempDir,
    state: tempfile::TempDir,
    bin: tempfile::TempDir,
    own: Session,
    other: Session,
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

        let mut fixture = Self {
            repo,
            state,
            bin,
            own: Session {
                id: String::new(),
                branch: String::new(),
            },
            other: Session {
                id: String::new(),
                branch: String::new(),
            },
        };
        fixture.own = fixture.start("own work", "own");
        fixture.other = fixture.start("other agent's task", "other");
        fixture
    }

    fn start(&self, task: &str, short_name: &str) -> Session {
        let started = self.run(
            &[
                "start",
                "--task",
                task,
                "--short-name",
                short_name,
                "--json",
            ],
            "",
        );
        assert!(
            started.status.success(),
            "start failed: {}",
            String::from_utf8_lossy(&started.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&started.stdout).unwrap();
        let session = value.get("session").unwrap_or(&value);
        Session {
            id: session["id"].as_i64().unwrap().to_string(),
            branch: session["branch"].as_str().unwrap().to_string(),
        }
    }

    fn log(&self) -> std::path::PathBuf {
        self.bin.path().join("gh.log")
    }

    fn run(&self, args: &[&str], head: &str) -> Output {
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
            .env("AETHYME_FAKE_HEAD", head)
            .env("AETHYME_FAKE_GH_LOG", self.log())
            .output()
            .unwrap()
    }

    /// `broker advanced gh` from the fixture's own session, the PR's head
    /// branch answered as `head`.
    fn gh(&self, flags: &[&str], gh_args: &[&str], head: &str) -> Output {
        let mut args = vec![
            "advanced",
            "gh",
            "--session",
            &self.own.id,
            "--repo",
            "schiste/Aethyme",
            "--reason",
            "test of the cross-session branch guard",
        ];
        args.extend(flags);
        args.push("--");
        args.extend(gh_args);
        self.run(&args, head)
    }

    fn mutations(&self) -> String {
        std::fs::read_to_string(self.log()).unwrap_or_default()
    }

    fn journaled(&self) -> usize {
        let listed = self.run(&["advanced", "operations", "list", "--json"], "");
        let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
        listed["operations"].as_array().unwrap().len()
    }
}

#[test]
fn merging_with_delete_branch_refuses_another_live_sessions_head() {
    let fixture = Fixture::new();

    let output = fixture.gh(
        &["--destructive"],
        &["pr", "merge", "7", "--squash", "--delete-branch"],
        &fixture.other.branch,
    );

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!(
            "branch {} belongs to live session {}",
            fixture.other.branch, fixture.other.id
        )),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!("--cross-session {}", fixture.other.id)),
        "{stderr}"
    );
    assert_eq!(fixture.mutations(), "", "the merge must never run");
    assert_eq!(fixture.journaled(), 0, "a refusal leaves no operation");
}

#[test]
fn merging_with_delete_branch_needs_destructive_and_says_so() {
    let fixture = Fixture::new();

    let output = fixture.gh(&[], &["pr", "merge", "7", "-d"], &fixture.own.branch);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--destructive"), "{stderr}");
    assert!(stderr.contains("run instead:"), "{stderr}");
    assert_eq!(fixture.mutations(), "");
}

#[test]
fn deleting_the_sessions_own_head_branch_is_allowed() {
    let fixture = Fixture::new();

    let output = fixture.gh(
        &["--destructive"],
        &["pr", "merge", "7", "--squash", "--delete-branch"],
        &fixture.own.branch,
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        fixture
            .mutations()
            .contains("pr merge 7 --squash --delete-branch"),
        "{}",
        fixture.mutations()
    );
}

#[test]
fn a_merge_that_keeps_the_branch_is_unchanged() {
    let fixture = Fixture::new();

    // The head belongs to the other session, but nothing deletes it: no
    // --destructive, no lookup, no refusal.
    let output = fixture.gh(
        &[],
        &["pr", "merge", "7", "--squash"],
        &fixture.other.branch,
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(fixture.mutations().contains("pr merge 7 --squash"));
}

#[test]
fn deleting_another_sessions_branch_through_the_api_is_refused() {
    let fixture = Fixture::new();
    let endpoint = format!(
        "repos/schiste/Aethyme/git/refs/heads/{}",
        fixture.other.branch
    );

    let output = fixture.gh(&["--destructive"], &["api", "-X", "DELETE", &endpoint], "");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!("belongs to live session {}", fixture.other.id)),
        "{stderr}"
    );
    assert_eq!(fixture.mutations(), "");
}

#[test]
fn the_named_owner_may_be_crossed_into_with_cross_session() {
    let fixture = Fixture::new();

    let output = fixture.gh(
        &["--destructive", "--cross-session", &fixture.other.id],
        &["pr", "merge", "7", "--delete-branch"],
        &fixture.other.branch,
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(fixture.mutations().contains("pr merge 7 --delete-branch"));
}
