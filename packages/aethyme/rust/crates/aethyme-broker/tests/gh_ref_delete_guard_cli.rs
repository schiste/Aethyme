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
        // The guard requires --repo to be this checkout's origin. SSH with
        // GIT_SSH_COMMAND=false keeps every fetch offline.
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
            .env("GIT_SSH_COMMAND", "false")
            .env_remove("GH_HOST")
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

// Each case below is a spelling that deleted or force-moved another live
// session's branch past the first version of this guard (security review of
// PR #566). Every one must be refused with nothing run.

fn assert_refused_unrun(fixture: &Fixture, output: &Output, needle: &str) {
    assert!(!output.status.success(), "unexpectedly ran");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(needle), "missing {needle:?}: {stderr}");
    assert_eq!(fixture.mutations(), "", "the mutation must never run");
    assert_eq!(fixture.journaled(), 0, "a refusal leaves no operation");
}

#[test]
fn closing_with_delete_branch_refuses_another_live_sessions_head() {
    let fixture = Fixture::new();
    let output = fixture.gh(
        &["--destructive"],
        &["pr", "close", "7", "--delete-branch"],
        &fixture.other.branch,
    );
    assert_refused_unrun(
        &fixture,
        &output,
        &format!("belongs to live session {}", fixture.other.id),
    );
}

#[test]
fn graphql_ref_mutations_are_refused_in_every_spelling() {
    let fixture = Fixture::new();
    let document = fixture.bin.path().join("delete.graphql");
    std::fs::write(
        &document,
        "mutation { deleteRef(input: {refId: \"REF_x\"}) { clientMutationId } }",
    )
    .unwrap();
    let from_file = format!("query=@{}", document.display());
    for args in [
        vec![
            "api",
            "graphql",
            "-f",
            "query=mutation { deleteRef(input: {refId: \"REF_x\"}) { clientMutationId } }",
        ],
        vec!["api", "graphql", "-F", from_file.as_str()],
        vec!["api", "graphql", "--input", "-"],
        vec![
            "api",
            "graphql",
            "-f",
            "query=mutation { updateRef(input: {refId: \"REF_x\", oid: \"0\", force: true}) { clientMutationId } }",
        ],
    ] {
        let output = fixture.gh(
            &[
                "--effect",
                "destructive",
                "--scope",
                "github:test",
                "--destructive",
            ],
            &args,
            "",
        );
        assert_refused_unrun(&fixture, &output, "cannot pin");
    }
}

#[test]
fn a_repeated_method_flag_cannot_turn_a_delete_into_a_read() {
    let fixture = Fixture::new();
    let endpoint = format!(
        "repos/schiste/Aethyme/git/refs/heads/{}",
        fixture.other.branch
    );
    // gh keeps the last -X: this is a DELETE, and must not pass as a read.
    let output = fixture.gh(&[], &["api", "-X", "GET", "-X", "DELETE", &endpoint], "");
    assert_refused_unrun(&fixture, &output, "--destructive");
    let output = fixture.gh(
        &["--destructive"],
        &["api", "-X", "GET", "--method=delete", &endpoint],
        "",
    );
    assert_refused_unrun(
        &fixture,
        &output,
        &format!("belongs to live session {}", fixture.other.id),
    );
}

#[test]
fn encoded_or_ambiguous_ref_endpoints_are_resolved_or_refused() {
    let fixture = Fixture::new();
    let encoded = fixture.other.branch.replace('/', "%2F");
    for endpoint in [
        format!("repos/schiste/Aethyme/git/refs/heads%2F{encoded}"),
        format!(
            "https://api.github.com/repos/schiste/Aethyme/git/refs/heads/{}",
            fixture.other.branch.replace('/', "%252F")
        ),
        format!(
            "/repos/schiste/Aethyme/git/refs/heads/{}/",
            fixture.other.branch
        ),
        format!(
            "repos/schiste/Aethyme/git/refs/tags/../heads/{}",
            fixture.other.branch
        ),
        format!(
            "repos/schiste/Aethyme/Git/Refs/Heads/{}",
            fixture.other.branch
        ),
    ] {
        let output = fixture.gh(&["--destructive"], &["api", "-X", "DELETE", &endpoint], "");
        assert!(!output.status.success(), "{endpoint} ran");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("belongs to live session") || stderr.contains("cannot pin"),
            "{endpoint}: {stderr}"
        );
        assert_eq!(fixture.mutations(), "", "{endpoint} ran");
    }
}

#[test]
fn a_method_override_header_is_refused() {
    let fixture = Fixture::new();
    let endpoint = format!(
        "repos/schiste/Aethyme/git/refs/heads/{}",
        fixture.other.branch
    );
    let output = fixture.gh(
        &["--destructive"],
        &[
            "api",
            "-X",
            "POST",
            "-H",
            "X-HTTP-Method-Override: DELETE",
            &endpoint,
        ],
        "",
    );
    assert_refused_unrun(&fixture, &output, "method-override");
}

#[test]
fn a_pull_request_url_for_another_repository_is_refused() {
    let fixture = Fixture::new();
    let output = fixture.gh(
        &["--destructive"],
        &[
            "pr",
            "merge",
            "https://github.com/someone/else/pull/7",
            "--delete-branch",
        ],
        &fixture.own.branch,
    );
    assert_refused_unrun(&fixture, &output, "is in someone/else, not schiste/Aethyme");
}

#[test]
fn with_no_pr_named_the_head_is_what_gh_resolves_not_the_local_branch() {
    let fixture = Fixture::new();
    // The session's own branch is checked out, but gh resolves the PR from
    // its push/merge configuration, here another session's branch.
    let output = fixture.gh(
        &["--destructive"],
        &["pr", "merge", "--delete-branch"],
        &fixture.other.branch,
    );
    assert_refused_unrun(
        &fixture,
        &output,
        &format!("belongs to live session {}", fixture.other.id),
    );
}

#[test]
fn aliases_and_extensions_are_refused() {
    let fixture = Fixture::new();
    for args in [
        vec!["mymerge", "7"],
        vec!["co", "7"],
        vec!["extension", "exec", "something"],
    ] {
        let output = fixture.gh(
            &[
                "--effect",
                "destructive",
                "--scope",
                "github:test",
                "--destructive",
            ],
            &args,
            &fixture.other.branch,
        );
        assert_refused_unrun(&fixture, &output, "cannot pin");
    }
}

#[test]
fn a_forced_repo_sync_of_another_sessions_branch_is_refused() {
    let fixture = Fixture::new();
    let output = fixture.gh(
        &["--destructive"],
        &["repo", "sync", "--force", "--branch", &fixture.other.branch],
        "",
    );
    assert_refused_unrun(
        &fixture,
        &output,
        &format!("belongs to live session {}", fixture.other.id),
    );
}

// Parser differentials: spellings gh's own flag parser (cobra/pflag) reads
// differently from a naive scan. Each must reach the same verdict gh would.

fn assert_owner_refused(fixture: &Fixture, flags: &[&str], args: &[&str]) {
    let output = fixture.gh(flags, args, &fixture.other.branch);
    assert_refused_unrun(
        fixture,
        &output,
        &format!("belongs to live session {}", fixture.other.id),
    );
}

#[test]
fn clustered_short_flags_are_parsed_as_gh_parses_them() {
    let fixture = Fixture::new();
    // `-sdtsubj`: squash, delete-branch, then -t consumes "subj".
    assert_owner_refused(
        &fixture,
        &["--destructive"],
        &["pr", "merge", "7", "-sdtsubj"],
    );
}

#[test]
fn equals_joined_boolean_values_are_parsed_as_gh_parses_them() {
    let fixture = Fixture::new();
    // pflag accepts 1/t/T/true/True/TRUE for a boolean.
    assert_owner_refused(
        &fixture,
        &["--destructive"],
        &["pr", "merge", "7", "--delete-branch=1"],
    );
}

#[test]
fn the_last_of_repeated_flags_wins_as_in_gh() {
    let fixture = Fixture::new();
    assert_owner_refused(
        &fixture,
        &["--destructive"],
        &["pr", "merge", "7", "--delete-branch=false", "-d"],
    );
}

#[test]
fn fields_imply_a_write_even_without_a_method_flag() {
    let fixture = Fixture::new();
    let endpoint = format!(
        "repos/schiste/Aethyme/git/refs/heads/{}",
        fixture.other.branch
    );
    // gh POSTs when fields are given; this is not a read.
    assert_owner_refused(
        &fixture,
        &["--destructive"],
        &[
            "api",
            &endpoint,
            "-F",
            "sha=0000000000000000000000000000000000000000",
        ],
    );
}

#[test]
fn a_repository_override_is_refused() {
    let fixture = Fixture::new();
    // `-dR…` slips past a prefix check for `-R`; gh reads it as a second target.
    let output = fixture.gh(
        &["--destructive"],
        &["pr", "merge", "7", "-dRsomeone/else"],
        &fixture.own.branch,
    );
    assert_refused_unrun(&fixture, &output, "--repo override");
}

#[test]
fn an_unpinnable_write_runs_only_when_acknowledged() {
    let fixture = Fixture::new();
    let mutation = [
        "api",
        "graphql",
        "-f",
        "query=mutation { mergeBranch(input: {}) { clientMutationId } }",
    ];
    let output = fixture.gh(&["--destructive"], &mutation, "");
    assert_refused_unrun(&fixture, &output, "--ref-write-acknowledged");
    let output = fixture.gh(&["--ref-write-acknowledged"], &mutation, "");
    assert!(
        !output.status.success(),
        "acknowledgement needs --destructive"
    );
    assert_eq!(fixture.mutations(), "");

    let output = fixture.gh(
        &["--destructive", "--ref-write-acknowledged"],
        &mutation,
        "",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(fixture.mutations().contains("api graphql"));
}
