//! A `gh` command that may write a branch ref is held to the cross-session
//! guard by allowlist (#393).
//!
//! Only two exact forms are resolved to a branch: `pr merge <N>
//! --merge|--squash|--rebase [-d] [--match-head-commit <sha>]` and `api -X
//! DELETE repos/<owner>/<repo>/git/refs/heads/<branch>`. Everything else that
//! may write a ref is refused unless the operator passes `--destructive
//! --ref-write-acknowledged`. A head-deleting merge is bound to the head
//! commit the check read, so the branch checked is the branch deleted.
//!
//! Every case drives a fake `gh` on `PATH`, so nothing reaches GitHub, and an
//! SSH `origin` with `GIT_SSH_COMMAND=false` keeps every fetch offline.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
mod common;

const HEAD_OID: &str = "1111111111111111111111111111111111111111";

/// `pr view` answers with the head the test sets (or fails, or returns
/// garbage); every other call is logged with its exact argv.
const FAKE_GH: &str = r#"#!/bin/sh
if [ "$1 $2" = 'pr view' ]; then
  case "$AETHYME_FAKE_VIEW" in
    fail) echo 'GraphQL: Could not resolve to a PullRequest' >&2; exit 1 ;;
    garbage) echo 'not json'; exit 0 ;;
  esac
  printf '{"headRefName":"%s","headRefOid":"%s","baseRefName":"%s"}\n' \
    "$AETHYME_FAKE_HEAD" "$AETHYME_FAKE_OID" "${AETHYME_FAKE_BASE:-main}"
  exit 0
fi
printf '%s\n' "$*" >> "$AETHYME_FAKE_GH_LOG"
printf 'GH_BROWSER=%s EDITOR=%s GIT_SSH_COMMAND=%s GH_CONFIG_DIR=%s GH_HOST=%s GH_PROMPT_DISABLED=%s\n' \
  "$GH_BROWSER" "$EDITOR" "$GIT_SSH_COMMAND" "$GH_CONFIG_DIR" "$GH_HOST" "$GH_PROMPT_DISABLED" \
  >> "$AETHYME_FAKE_GH_LOG.env"
printf 'ARGV0=%s\nPATH=%s\nHOME=%s\nGIT_CONFIG_GLOBAL=%s\nGIT_CONFIG_NOSYSTEM=%s\n' \
  "$0" "$PATH" "$HOME" "$GIT_CONFIG_GLOBAL" "$GIT_CONFIG_NOSYSTEM" >> "$AETHYME_FAKE_GH_LOG.child"
i=0
while [ "$i" -lt "${GIT_CONFIG_COUNT:-0}" ]; do
  eval "key=\$GIT_CONFIG_KEY_$i; value=\$GIT_CONFIG_VALUE_$i"
  printf 'config.%s=%s\n' "$key" "$value" >> "$AETHYME_FAKE_GH_LOG.child"
  i=$((i + 1))
done
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
        // The guard requires --repo to be this checkout's origin.
        git(
            repo.path(),
            &[
                "remote",
                "add",
                "origin",
                "git@github.com:schiste/Aethyme.git",
            ],
        );

        let empty = || Session {
            id: String::new(),
            branch: String::new(),
        };
        let mut fixture = Self {
            repo,
            state,
            bin,
            own: empty(),
            other: empty(),
        };
        fixture.own = fixture.start("own work", "own");
        fixture.other = fixture.start("other agent's task", "other");
        fixture
    }

    fn start(&self, task: &str, short_name: &str) -> Session {
        let started = self.cli(
            &[
                "start",
                "--task",
                task,
                "--short-name",
                short_name,
                "--json",
            ],
            &[],
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

    fn cli(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        self.cli_in(self.repo.path(), args, env)
    }

    fn cli_in(&self, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut command = common::broker_cli(CLI, args);
        command
            .current_dir(dir)
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
            .env("AETHYME_FAKE_OID", HEAD_OID)
            .env("AETHYME_FAKE_GH_LOG", self.log());
        for (key, value) in env {
            command.env(key, value);
        }
        command.output().unwrap()
    }

    /// `broker advanced gh` from the fixture's own session; the PR's head is
    /// answered as `head`.
    fn gh_env(&self, flags: &[&str], gh: &[&str], head: &str, env: &[(&str, &str)]) -> Output {
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
        args.extend(gh);
        let mut env = env.to_vec();
        env.push(("AETHYME_FAKE_HEAD", head));
        self.cli(&args, &env)
    }

    fn gh(&self, flags: &[&str], gh: &[&str], head: &str) -> Output {
        self.gh_env(flags, gh, head, &[])
    }

    fn ran(&self) -> String {
        std::fs::read_to_string(self.log()).unwrap_or_default()
    }

    fn journaled(&self) -> usize {
        let listed = self.cli(&["advanced", "operations", "list", "--json"], &[]);
        let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
        listed["operations"].as_array().unwrap().len()
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_refused(fixture: &Fixture, output: &Output, needle: &str) {
    assert!(
        !output.status.success(),
        "unexpectedly ran: {}",
        fixture.ran()
    );
    assert!(
        stderr(output).contains(needle),
        "missing {needle:?}: {}",
        stderr(output)
    );
    assert_eq!(fixture.ran(), "", "nothing may run");
    assert_eq!(fixture.journaled(), 0, "a refusal leaves no operation");
}

fn assert_ran_exactly(fixture: &Fixture, output: &Output, argv: &str) {
    assert!(output.status.success(), "{}", stderr(output));
    assert_eq!(fixture.ran(), format!("{argv}\n"));
}

const MAY_WRITE: &str = "may write a branch ref";
const DESTRUCTIVE: &[&str] = &["--destructive"];

// --- The allowlisted merge form ---------------------------------------

#[test]
fn a_head_deleting_merge_of_another_sessions_branch_is_refused() {
    let fixture = Fixture::new();
    let output = fixture.gh(
        DESTRUCTIVE,
        &["pr", "merge", "7", "--squash", "--delete-branch"],
        &fixture.other.branch,
    );
    assert_refused(
        &fixture,
        &output,
        &format!(
            "branch {} belongs to live session {}",
            fixture.other.branch, fixture.other.id
        ),
    );
}

#[test]
fn deleting_the_sessions_own_head_is_bound_to_the_checked_head_commit() {
    let fixture = Fixture::new();
    let output = fixture.gh(
        DESTRUCTIVE,
        &["pr", "merge", "7", "--squash", "-d"],
        &fixture.own.branch,
    );
    // TOCTOU: the head read for the check is the head GitHub must still see.
    assert_ran_exactly(
        &fixture,
        &output,
        &format!("pr merge 7 --squash -d --match-head-commit {HEAD_OID}"),
    );
}

#[test]
fn a_callers_match_head_commit_must_be_the_checked_head() {
    let fixture = Fixture::new();
    let stale = "2222222222222222222222222222222222222222";
    let output = fixture.gh(
        DESTRUCTIVE,
        &[
            "pr",
            "merge",
            "7",
            "--merge",
            "-d",
            "--match-head-commit",
            stale,
        ],
        &fixture.own.branch,
    );
    assert_refused(&fixture, &output, "is not pull request #7's head");

    let output = fixture.gh(
        DESTRUCTIVE,
        &[
            "pr",
            "merge",
            "7",
            "--merge",
            "-d",
            "--match-head-commit",
            HEAD_OID,
        ],
        &fixture.own.branch,
    );
    assert_ran_exactly(
        &fixture,
        &output,
        &format!("pr merge 7 --merge -d --match-head-commit {HEAD_OID}"),
    );
}

#[test]
fn every_merge_checks_its_head_even_without_delete_branch() {
    // GitHub deletes the head on merge when the repository says so, so a
    // merge without `-d` is checked and bound like one with it.
    let fixture = Fixture::new();
    let output = fixture.gh(
        &[],
        &["pr", "merge", "7", "--squash"],
        &fixture.other.branch,
    );
    assert_refused(
        &fixture,
        &output,
        &format!("belongs to live session {}", fixture.other.id),
    );
    let output = fixture.gh(&[], &["pr", "merge", "7", "--squash"], &fixture.own.branch);
    assert_ran_exactly(
        &fixture,
        &output,
        &format!("pr merge 7 --squash --match-head-commit {HEAD_OID}"),
    );
}

#[test]
fn the_named_owner_may_be_crossed_into_with_cross_session() {
    let fixture = Fixture::new();
    let output = fixture.gh(
        &["--destructive", "--cross-session", &fixture.other.id],
        &["pr", "merge", "7", "--rebase", "-d"],
        &fixture.other.branch,
    );
    assert_ran_exactly(
        &fixture,
        &output,
        &format!("pr merge 7 --rebase -d --match-head-commit {HEAD_OID}"),
    );
}

// --- Fail-closed: every doubt refuses -----------------------------------

#[test]
fn a_head_lookup_that_fails_or_answers_nonsense_refuses() {
    let fixture = Fixture::new();
    for view in ["fail", "garbage"] {
        let output = fixture.gh_env(
            DESTRUCTIVE,
            &["pr", "merge", "7", "--squash", "-d"],
            &fixture.own.branch,
            &[("AETHYME_FAKE_VIEW", view)],
        );
        assert_refused(&fixture, &output, MAY_WRITE);
    }
    // An empty head is no answer either.
    let output = fixture.gh(DESTRUCTIVE, &["pr", "merge", "7", "--squash", "-d"], "");
    assert_refused(&fixture, &output, MAY_WRITE);
}

#[test]
fn another_host_or_a_repository_other_than_origin_refuses() {
    let fixture = Fixture::new();
    let output = fixture.gh_env(
        DESTRUCTIVE,
        &["pr", "merge", "7", "--squash", "-d"],
        &fixture.own.branch,
        &[("GH_HOST", "ghe.example.com")],
    );
    assert_refused(&fixture, &output, "GH_HOST");
    git(
        fixture.repo.path(),
        &[
            "remote",
            "set-url",
            "origin",
            "git@github.com:someone/else.git",
        ],
    );
    let output = fixture.gh(
        DESTRUCTIVE,
        &["pr", "merge", "7", "--squash", "-d"],
        &fixture.own.branch,
    );
    assert!(
        !output.status.success(),
        "ran against a non-origin repository"
    );
    assert_eq!(fixture.ran(), "");
}

// --- Parser differentials: anything outside the exact forms refuses ------

#[test]
fn every_spelling_outside_the_exact_forms_is_refused_unless_acknowledged() {
    let fixture = Fixture::new();
    let other = format!(
        "repos/schiste/Aethyme/git/refs/heads/{}",
        fixture.other.branch
    );
    let encoded = format!(
        "repos/schiste/Aethyme/git/refs/heads/{}",
        fixture.other.branch.replace('/', "%2F")
    );
    let slashed = format!("/{other}");
    let trailing = format!("{other}/");
    let cases: Vec<Vec<&str>> = vec![
        // Clustered short flags.
        vec!["pr", "merge", "7", "-sd"],
        // `=`-joined values.
        vec!["pr", "merge", "7", "--squash", "--delete-branch=true"],
        // Repeated flags.
        vec!["pr", "merge", "7", "--squash", "-d", "--delete-branch"],
        // Positional tricks.
        vec!["pr", "merge", "7", "--squash", "--", "-d"],
        // Other commands that delete or rewrite a head.
        vec!["pr", "close", "7", "-d"],
        vec!["pr", "update-branch", "7", "--rebase"],
        vec!["repo", "sync", "--force", "--branch", "x"],
        // Aliases and extensions.
        vec!["co", "7"],
        vec!["extension", "exec", "x"],
        // Method spellings.
        vec!["api", "-XDELETE", &other],
        vec!["api", "--method", "DELETE", &other],
        vec!["api", "-X", "delete", &other],
        // A method implied by fields.
        vec!["api", &other, "-F", "sha=0"],
        // Endpoints that are not exactly the checked path.
        vec!["api", "-X", "DELETE", &encoded],
        vec!["api", "-X", "DELETE", &slashed],
        vec!["api", "-X", "DELETE", &trailing],
        // GraphQL and header tricks.
        vec![
            "api",
            "graphql",
            "-f",
            "query=mutation { deleteRef(input:{}) { x } }",
        ],
        vec!["api", &other, "-H", "X-HTTP-Method-Override: DELETE"],
    ];
    for case in cases {
        let output = fixture.gh(
            &[
                "--effect",
                "destructive",
                "--scope",
                "github:test",
                "--destructive",
            ],
            &case,
            &fixture.other.branch,
        );
        // Refused by the guard, or earlier by the broker's own validation;
        // either way nothing may run and nothing may be journaled.
        assert!(!output.status.success(), "{case:?} ran");
        assert_eq!(fixture.ran(), "", "{case:?} ran");
    }
    assert_eq!(fixture.journaled(), 0, "a refusal leaves no operation");
}

#[test]
fn the_exact_branch_delete_runs_byte_identical_or_is_refused_for_an_owner() {
    let fixture = Fixture::new();
    let other = format!(
        "repos/schiste/Aethyme/git/refs/heads/{}",
        fixture.other.branch
    );
    let output = fixture.gh(DESTRUCTIVE, &["api", "-X", "DELETE", &other], "");
    assert_refused(
        &fixture,
        &output,
        &format!("belongs to live session {}", fixture.other.id),
    );
    let unowned = "repos/schiste/Aethyme/git/refs/heads/agent/nobody";
    let output = fixture.gh(DESTRUCTIVE, &["api", "-X", "DELETE", unowned], "");
    assert_ran_exactly(&fixture, &output, &format!("api -X DELETE {unowned}"));
}

#[test]
fn unclassified_branch_ref_mutations_are_refused_even_with_acknowledgement() {
    let cases: Vec<Vec<&str>> = vec![
        vec![
            "api",
            "-X",
            "PATCH",
            "repos/schiste/Aethyme/git/refs/heads/main",
            "-f",
            "sha=0",
            "-F",
            "force=true",
        ],
        vec![
            "api",
            "-X",
            "DELETE",
            "repos/schiste/Aethyme/git/refs/heads/%6Dain",
        ],
        vec![
            "api",
            "graphql",
            "-f",
            "query=mutation { updateRef(input:{}) { x } }",
        ],
    ];
    for mutation in cases {
        let fixture = Fixture::new();
        let output = fixture.gh(
            &[
                "--effect",
                "destructive",
                "--scope",
                "github:test",
                "--destructive",
                "--ref-write-acknowledged",
            ],
            &mutation,
            "",
        );
        assert_refused(&fixture, &output, MAY_WRITE);
    }
}

#[test]
fn an_unverifiable_write_is_refused_when_shared_branches_exist() {
    let fixture = Fixture::new();
    let mutation = ["api", "graphql", "-f", "query=mutation { x }"];
    let output = fixture.gh(&[], &mutation, "");
    assert_refused(&fixture, &output, "--ref-write-acknowledged");
    let output = fixture.gh(&["--ref-write-acknowledged"], &mutation, "");
    assert_refused(&fixture, &output, MAY_WRITE);
    let output = fixture.gh(
        &["--destructive", "--ref-write-acknowledged"],
        &mutation,
        "",
    );
    assert_refused(&fixture, &output, "shared branch");
}

// --- Exact writes that cannot touch a ref run without acknowledgement ----

#[test]
fn allowlisted_comment_review_label_and_issue_writes_run_unacknowledged() {
    let fixture = Fixture::new();
    let r = "repos/schiste/Aethyme";
    let cases: Vec<(Vec<&str>, Vec<String>)> = vec![
        (
            vec![],
            vec![
                "-X".into(),
                "POST".into(),
                format!("{r}/issues/5/comments"),
                "-f".into(),
                "body=hi".into(),
            ],
        ),
        (
            vec![],
            vec![
                "-X".into(),
                "PATCH".into(),
                format!("{r}/issues/comments/9"),
                "-f".into(),
                "body=x".into(),
            ],
        ),
        (
            vec!["--destructive"],
            vec![
                "-X".into(),
                "DELETE".into(),
                format!("{r}/issues/comments/9"),
            ],
        ),
        (
            vec![],
            vec![
                "-X".into(),
                "POST".into(),
                format!("{r}/pulls/5/comments/9/replies"),
                "-f".into(),
                "body=x".into(),
            ],
        ),
        (
            vec![],
            vec![
                "-X".into(),
                "POST".into(),
                format!("{r}/pulls/5/reviews"),
                "-f".into(),
                "event=COMMENT".into(),
            ],
        ),
        (
            vec![],
            vec![
                "-X".into(),
                "POST".into(),
                format!("{r}/issues/5/labels"),
                "-f".into(),
                "labels[]=bug".into(),
            ],
        ),
        (
            vec![],
            vec![
                "-X".into(),
                "PATCH".into(),
                format!("{r}/issues/5"),
                "-f".into(),
                "state=closed".into(),
            ],
        ),
    ];
    for (flags, api) in cases {
        let mut gh: Vec<&str> = vec!["api"];
        gh.extend(api.iter().map(String::as_str));
        let output = fixture.gh(&flags, &gh, "");
        assert!(output.status.success(), "{gh:?}: {}", stderr(&output));
        assert!(
            fixture.ran().contains(&gh[1..].join(" ")),
            "{gh:?} did not run"
        );
    }
}

#[test]
fn near_misses_of_the_allowlisted_writes_are_refused() {
    let fixture = Fixture::new();
    let r = "repos/schiste/Aethyme";
    let cases: Vec<Vec<String>> = vec![
        // Another repository.
        vec![
            "-X".into(),
            "POST".into(),
            "repos/someone/else/issues/5/comments".into(),
            "-f".into(),
            "body=x".into(),
        ],
        // A non-numeric id.
        vec![
            "-X".into(),
            "POST".into(),
            format!("{r}/issues/five/comments"),
            "-f".into(),
            "body=x".into(),
        ],
        // An extra path segment.
        vec![
            "-X".into(),
            "POST".into(),
            format!("{r}/issues/5/comments/x"),
            "-f".into(),
            "body=x".into(),
        ],
        // A body the broker cannot see.
        vec![
            "-X".into(),
            "POST".into(),
            format!("{r}/issues/5/comments"),
            "--input".into(),
            "body.json".into(),
        ],
        // GraphQL.
        vec![
            "-X".into(),
            "POST".into(),
            "graphql".into(),
            "-f".into(),
            "query=mutation { x }".into(),
        ],
        // An encoded slash.
        vec![
            "-X".into(),
            "POST".into(),
            format!("{r}/issues%2F5/comments"),
            "-f".into(),
            "body=x".into(),
        ],
        // A repeated method.
        vec![
            "-X".into(),
            "GET".into(),
            "-X".into(),
            "POST".into(),
            format!("{r}/issues/5/comments"),
            "-f".into(),
            "body=x".into(),
        ],
    ];
    for api in cases {
        let mut gh: Vec<&str> = vec!["api"];
        gh.extend(api.iter().map(String::as_str));
        let output = fixture.gh(&[], &gh, "");
        assert!(!output.status.success(), "{gh:?} ran");
        assert_eq!(fixture.ran(), "", "{gh:?} ran");
    }
    assert_eq!(fixture.journaled(), 0, "a refusal leaves no operation");
}

// --- Second adversarial review --------------------------------------------

#[test]
fn browsers_are_refused_and_the_gh_environment_is_scrubbed() {
    let fixture = Fixture::new();
    for gh in [
        &["browse"][..],
        &["pr", "view", "7", "--web"],
        &[
            "config",
            "set",
            "browser",
            "sh -c 'git push origin --delete x'",
        ],
        &["run", "download", "1", "-D", ".git/hooks"],
    ] {
        // Refused by the guard, or earlier because `config` has no inferred
        // effect; either way nothing runs and nothing is journaled.
        let output = fixture.gh(&[], gh, "");
        assert!(!output.status.success(), "{gh:?} ran");
        assert_eq!(fixture.ran(), "", "{gh:?} ran");
        let output = fixture.gh(&["--effect", "write", "--scope", "github:test"], gh, "");
        assert!(
            !output.status.success(),
            "{gh:?} ran with a declared effect"
        );
        assert_eq!(fixture.ran(), "", "{gh:?} ran");
    }
    assert_eq!(fixture.journaled(), 0, "a refusal leaves no operation");
    // A command that runs gets none of the variables gh or git turn into
    // commands or targets, and prompts are off.
    let output = fixture.gh_env(
        &[],
        &["issue", "comment", "1", "--body", "x"],
        "",
        &[
            ("GH_BROWSER", "evil"),
            ("EDITOR", "evil"),
            ("GH_CONFIG_DIR", "/tmp/evil"),
        ],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let env = std::fs::read_to_string(format!("{}.env", fixture.log().display())).unwrap();
    assert_eq!(
        env,
        "GH_BROWSER=false EDITOR= GIT_SSH_COMMAND= GH_CONFIG_DIR= GH_HOST= GH_PROMPT_DISABLED=1\n"
    );
}

#[test]
fn updating_another_sessions_head_is_refused() {
    let fixture = Fixture::new();
    let output = fixture.gh(&[], &["pr", "update-branch", "7"], &fixture.other.branch);
    assert_refused(
        &fixture,
        &output,
        &format!("belongs to live session {}", fixture.other.id),
    );
    let output = fixture.gh(&[], &["pr", "update-branch", "7"], &fixture.own.branch);
    assert_ran_exactly(&fixture, &output, "pr update-branch 7");
}

#[test]
fn a_base_owned_by_another_session_is_refused() {
    let fixture = Fixture::new();
    let owner = format!("belongs to live session {}", fixture.other.id);
    let output = fixture.gh_env(
        &[],
        &["pr", "merge", "7", "--squash"],
        &fixture.own.branch,
        &[("AETHYME_FAKE_BASE", &fixture.other.branch)],
    );
    assert_refused(&fixture, &output, &owner);
    for gh in [
        vec!["pr", "edit", "7", "--base", &fixture.other.branch],
        vec![
            "pr",
            "create",
            "--base",
            &fixture.other.branch,
            "--head",
            &fixture.own.branch,
            "--title",
            "t",
            "--body",
            "b",
        ],
    ] {
        let output = fixture.gh(&[], &gh, "");
        assert_refused(&fixture, &output, &owner);
    }
}

/// The default branch is shared, never a session's own: a session whose
/// recorded branch is `main` must not turn every merge into main, or a
/// delete of main, into a write to "its" branch.
#[test]
fn a_session_recorded_on_the_default_branch_does_not_own_it() {
    let fixture = Fixture::new();
    let repo = fixture.repo.path();
    // No `origin/HEAD`: the default branch is unresolved, and the fail-closed
    // fallback still treats `main` as shared.
    // A linked worktree checked out on main, adopted as a session.
    git(repo, &["checkout", "-q", "-b", "scratch"]);
    let linked = tempfile::tempdir().unwrap();
    let worktree = linked.path().join("on-main");
    git(
        repo,
        &["worktree", "add", "-q", worktree.to_str().unwrap(), "main"],
    );
    let adopted = fixture.cli_in(
        &worktree,
        &[
            "start",
            "--adopt",
            "--task",
            "work recorded on main",
            "--short-name",
            "on-main",
            "--json",
        ],
        &[],
    );
    assert!(
        adopted.status.success(),
        "adopt failed: {}",
        stderr(&adopted)
    );
    let value: serde_json::Value = serde_json::from_slice(&adopted.stdout).unwrap();
    let session = value.get("session").unwrap_or(&value);
    assert_eq!(session["branch"].as_str(), Some("main"));

    // The guard lets the merge into main run: gh received it. (What the
    // offline fixture does after the merge is not this test's concern.)
    let output = fixture.gh(&[], &["pr", "merge", "7", "--squash"], &fixture.own.branch);
    assert!(
        !stderr(&output).contains("belongs to live session"),
        "{}",
        stderr(&output)
    );
    assert!(
        fixture
            .ran()
            .starts_with("pr merge 7 --squash --match-head-commit "),
        "gh did not run the merge: {} / {}",
        fixture.ran(),
        stderr(&output)
    );
}

const SHARED: &str = "shared default or integration branch";

/// Sets `origin/HEAD -> origin/main`, as a clone does, so the default branch
/// is resolved from the repository and not guessed.
fn name_default_branch(fixture: &Fixture) {
    let repo = fixture.repo.path();
    git(repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(
        repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
}

/// "Owned by no session" is not "allowed": the default and integration
/// branches can never be deleted, force-updated or rebased through the
/// broker, even with --destructive and no session recording them.
#[test]
fn the_default_and_integration_branches_are_never_rewritten() {
    let fixture = Fixture::new();
    name_default_branch(&fixture);
    for branch in ["main", "aethyme/integration"] {
        let path = format!("repos/schiste/Aethyme/git/refs/heads/{branch}");
        let output = fixture.gh(DESTRUCTIVE, &["api", "-X", "DELETE", &path], "");
        assert_refused(&fixture, &output, SHARED);
    }
    // update-branch writes the head; a PR whose head is main is refused.
    let output = fixture.gh(&[], &["pr", "update-branch", "7"], "main");
    assert_refused(&fixture, &output, SHARED);
    // A merge may delete its head; a PR from main is refused too.
    let output = fixture.gh(&[], &["pr", "merge", "7", "--merge"], "main");
    assert_refused(&fixture, &output, SHARED);
}

/// The exemption is the exact default branch name, resolved from the
/// repository: look-alike names neither get it nor get protected by it.
#[test]
fn only_the_exact_default_branch_name_is_shared() {
    let fixture = Fixture::new();
    name_default_branch(&fixture);
    for branch in ["main2", "origin/main", "xmain"] {
        let path = format!("repos/schiste/Aethyme/git/refs/heads/{branch}");
        let output = fixture.gh(DESTRUCTIVE, &["api", "-X", "DELETE", &path], "");
        assert_ran_exactly(&fixture, &output, &format!("api -X DELETE {path}"));
        std::fs::remove_file(fixture.log()).unwrap();
    }
}

// --- Third review: binary choice, git configuration, symlinked downloads --

fn child_env(fixture: &Fixture) -> std::collections::HashMap<String, String> {
    std::fs::read_to_string(format!("{}.child", fixture.log().display()))
        .unwrap()
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

#[test]
fn gh_runs_by_absolute_path_with_a_fixed_path() {
    let fixture = Fixture::new();
    // A directory the caller puts on PATH after gh: gh's own git lookups
    // must never reach it.
    let evil = tempfile::tempdir().unwrap();
    let caller_path = format!(
        "{}:{}:{}",
        fixture.bin.path().display(),
        evil.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = fixture.gh_env(
        &[],
        &["issue", "comment", "1", "--body", "x"],
        "",
        &[("PATH", &caller_path)],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let env = child_env(&fixture);
    let gh = fixture.bin.path().join("gh").canonicalize().unwrap();
    assert_eq!(
        env["ARGV0"],
        gh.display().to_string(),
        "gh ran by absolute path"
    );
    assert!(
        !env["PATH"].contains(&evil.path().display().to_string()),
        "the caller's PATH leaked: {}",
        env["PATH"]
    );
    assert_eq!(
        env["PATH"], "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin",
        "the child PATH is exactly the trusted directories"
    );
}

#[test]
fn gh_and_its_git_read_no_relocated_home_or_global_config() {
    let fixture = Fixture::new();
    let output = fixture.gh_env(
        &[],
        &["issue", "comment", "1", "--body", "x"],
        "",
        &[("HOME", "/tmp/relocated-home")],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let env = child_env(&fixture);
    assert_ne!(env["HOME"], "/tmp/relocated-home");
    assert!(!env["HOME"].is_empty());
    assert_eq!(env["GIT_CONFIG_GLOBAL"], "/dev/null");
    assert_eq!(env["GIT_CONFIG_NOSYSTEM"], "1");
    // The repository's own config cannot run a helper, a hook or a file
    // transport in the git gh starts.
    assert_eq!(env["config.credential.helper"], "");
    assert_eq!(env["config.core.hooksPath"], "/dev/null");
    assert_eq!(env["config.protocol.file.allow"], "never");
}

#[test]
fn commands_that_run_git_on_user_configuration_are_refused() {
    let fixture = Fixture::new();
    for gh in [
        &["extension", "install", "someone/gh-x"][..],
        &["extension", "upgrade", "--all"],
        &["gist", "clone", "abc"],
        &["issue", "develop", "1", "--checkout"],
        &["pr", "create", "--title", "t", "--body", "b"],
    ] {
        let output = fixture.gh(&["--effect", "write", "--scope", "github:test"], gh, "");
        assert!(!output.status.success(), "{gh:?} ran");
        assert_eq!(fixture.ran(), "", "{gh:?} ran");
    }
    assert_eq!(fixture.journaled(), 0);
    let output = fixture.gh(
        &[],
        &[
            "pr",
            "create",
            "--head",
            &fixture.own.branch,
            "--title",
            "t",
            "--body",
            "b",
        ],
        "",
    );
    assert!(output.status.success(), "{}", stderr(&output));
}

#[test]
fn downloads_with_a_destination_are_refused_and_plain_ones_run() {
    let fixture = Fixture::new();
    for gh in [
        &["run", "download", "1", "-D", "x"][..],
        &["run", "download", "1", "--dir=x"],
        &["run", "download", "1", "-Dx"],
        &["run", "download", "1", "--dir", "x"],
        &["release", "download", "v1", "--output", "x.zip"],
        &["release", "download", "v1", "-O", "x.zip"],
        &["release", "download", "v1", "--clobber"],
        &["release", "download", "v1", "--archive=zip"],
        // A second positional operand.
        &["run", "download", "1", "x"],
    ] {
        let output = fixture.gh(&[], gh, "");
        assert_refused(&fixture, &output, MAY_WRITE);
    }
    let output = fixture.gh(&[], &["run", "download", "1", "-n", "logs"], "");
    assert_ran_exactly(&fixture, &output, "run download 1 -n logs");
}

// --- Graph integrity on the merge lane (#280, #292) ------------------

/// A brokered merge in a repository whose committed graph is authoritative
/// checks the head it merges and prints the verdict as advice. Whatever the
/// verdict, the merge runs exactly as checked: graph integrity never refuses.
#[test]
fn a_brokered_merge_reports_graph_integrity_as_advice_and_still_runs() {
    let fixture = Fixture::new();
    // Brokered gh runs in the session's worktree, so its policy is read there.
    let worktrees = String::from_utf8(
        Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(fixture.repo.path())
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let own_worktree = worktrees
        .split("\n\n")
        .find(|block| block.contains(&format!("branch refs/heads/{}", fixture.own.branch)))
        .and_then(|block| block.lines().next())
        .and_then(|line| line.strip_prefix("worktree "))
        .expect("the session's worktree")
        .to_string();
    std::fs::create_dir_all(Path::new(&own_worktree).join(".aethyme")).unwrap();
    std::fs::write(
        Path::new(&own_worktree).join(".aethyme/config.toml"),
        "[graph]\nauthority = 'committed_fragments'\nrepository = 'fixture'\n",
    )
    .unwrap();
    let head = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(fixture.repo.path())
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();

    // A head available locally is checked: it has no committed fragments, so
    // the verdict is not fresh, and the merge still runs.
    let output = fixture.gh_env(
        &[],
        &["pr", "merge", "7", "--squash"],
        &fixture.own.branch,
        &[("AETHYME_FAKE_OID", head.as_str())],
    );
    assert_ran_exactly(
        &fixture,
        &output,
        &format!("pr merge 7 --squash --match-head-commit {head}"),
    );
    assert!(
        stderr(&output).contains("[graph-integrity] pull request #7:")
            && stderr(&output).contains("blocks nothing"),
        "{}",
        stderr(&output)
    );

    // A head that is not available locally is an unknown verdict, never fresh,
    // and never a refusal.
    std::fs::remove_file(fixture.log()).unwrap();
    let output = fixture.gh(&[], &["pr", "merge", "7", "--squash"], &fixture.own.branch);
    assert_ran_exactly(
        &fixture,
        &output,
        &format!("pr merge 7 --squash --match-head-commit {HEAD_OID}"),
    );
    assert!(
        stderr(&output).contains("not available locally"),
        "{}",
        stderr(&output)
    );
}

/// Without an authoritative graph policy the merge lane says nothing about
/// graph integrity.
#[test]
fn a_brokered_merge_without_a_graph_policy_says_nothing_about_the_graph() {
    let fixture = Fixture::new();
    let output = fixture.gh(&[], &["pr", "merge", "7", "--squash"], &fixture.own.branch);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        !stderr(&output).contains("[graph-integrity]"),
        "{}",
        stderr(&output)
    );
}
