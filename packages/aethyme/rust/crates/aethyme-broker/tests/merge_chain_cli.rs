//! `broker advanced merge-chain` against a scripted `gh`.
//!
//! The chain is only worth having if it refuses to merge what nobody tested:
//! a head that moved, a red check, a red default branch. Every case drives the
//! real CLI with a fake `gh` on `PATH` whose answers come from files, so the
//! provider's state can change as the chain writes -- a `pr merge` makes the
//! next `pr view` read MERGED -- and nothing reaches GitHub.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
mod common;

/// Answers from `$AETHYME_FAKE_GH_DIR`. A read of key `k` returns file `k.<n>`
/// for its n-th call, sticking at the last file present, so a test scripts a
/// check that is pending and then passes. Writes leave marks that switch
/// `pr view` to its `@updated` / `@merged` script and branch runs to their
/// `@dispatched` script, and fail when a `fail-<write>-<pr>` file exists.
const FAKE_GH: &str = r#"#!/bin/sh
D="$AETHYME_FAKE_GH_DIR"
printf '%s\n' "$*" >> "$D/log"
respond() {
  key="$1"
  n=$(cat "$D/$key.count" 2>/dev/null || echo 0)
  n=$((n + 1))
  echo "$n" > "$D/$key.count"
  while [ "$n" -gt 0 ] && [ ! -f "$D/$key.$n" ]; do n=$((n - 1)); done
  if [ "$n" -eq 0 ]; then echo "no scripted response for $key" >&2; exit 1; fi
  cat "$D/$key.$n"
  exit 0
}
fail_if() {
  if [ -f "$D/fail-$1" ]; then cat "$D/fail-$1" >&2; exit 1; fi
}
case "$1" in
  pr)
    n="$3"
    case "$2" in
      view)
        k="pr-$n"
        for m in merged updated; do
          if [ -f "$D/mark-$m-$n" ] && [ -f "$D/pr-$n@$m.1" ]; then k="pr-$n@$m"; break; fi
        done
        respond "$k"
        ;;
      ready) fail_if "ready-$n"; touch "$D/mark-ready-$n"; exit 0 ;;
      update-branch) fail_if "update-$n"; touch "$D/mark-updated-$n"; exit 0 ;;
      merge) fail_if "merge-$n"; touch "$D/mark-merged-$n"; exit 0 ;;
    esac
    ;;
  workflow)
    if [ "$2" = run ]; then touch "$D/mark-dispatched"; exit 0; fi
    ;;
  api)
    path="$2"
    case "$path" in
      */compare/*) respond "compare-${path##*...}" ;;
      */check-runs*)
        sha="${path#*/commits/}"
        respond "checks-${sha%%/*}"
        ;;
      */actions/runs*)
        sha="${path#*head_sha=}"
        sha="${sha%%&*}"
        if [ -f "$D/mark-dispatched" ] && [ -f "$D/runs-$sha@dispatched.1" ]; then
          respond "runs-$sha@dispatched"
        fi
        respond "runs-$sha"
        ;;
    esac
    ;;
esac
exit 0
"#;

struct Fixture {
    repo: tempfile::TempDir,
    state: tempfile::TempDir,
    bin: tempfile::TempDir,
    gh: tempfile::TempDir,
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

fn pr(number: u64, state: &str, draft: bool, head: &str, merge: Option<&str>) -> String {
    let merge = merge.map_or("null".to_string(), |oid| format!(r#"{{"oid":"{oid}"}}"#));
    format!(
        r#"{{"number":{number},"state":"{state}","isDraft":{draft},"headRefOid":"{head}","baseRefName":"main","mergeCommit":{merge}}}"#
    )
}

fn checks(runs: &[(&str, &str, Option<&str>, &str)]) -> String {
    runs_json("check_runs", "started_at", runs)
}

fn branch_runs(runs: &[(&str, &str, Option<&str>, &str)]) -> String {
    runs_json("workflow_runs", "created_at", runs)
}

fn runs_json(key: &str, time: &str, runs: &[(&str, &str, Option<&str>, &str)]) -> String {
    let items: Vec<String> = runs
        .iter()
        .enumerate()
        .map(|(index, (name, status, conclusion, at))| {
            let conclusion = conclusion.map_or("null".to_string(), |value| format!("\"{value}\""));
            format!(
                r#"{{"id":{},"name":"{name}","status":"{status}","conclusion":{conclusion},"{time}":"{at}"}}"#,
                index + 1
            )
        })
        .collect();
    format!(r#"{{"{key}":[{}]}}"#, items.join(","))
}

const GREEN: &[(&str, &str, Option<&str>, &str)] =
    &[("lint", "completed", Some("success"), "2026-10-03T10:00:00Z")];

impl Fixture {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt as _;

        let repo = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let bin = tempfile::tempdir().unwrap();
        let gh = tempfile::tempdir().unwrap();
        let fake = bin.path().join("gh");
        std::fs::write(&fake, FAKE_GH).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        git(repo.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(repo.path().join(".gitignore"), "/.aethyme/\n").unwrap();
        std::fs::write(repo.path().join("a.txt"), "a\n").unwrap();
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-qm", "init"]);

        let mut fixture = Self {
            repo,
            state,
            bin,
            gh,
            session: String::new(),
        };
        let started = fixture.run(&["start", "--task", "merge chain", "--json"]);
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

    /// Scripts the `n`-th answer for `key`.
    fn script(&self, key: &str, n: u32, body: &str) {
        std::fs::write(self.gh.path().join(format!("{key}.{n}")), body).unwrap();
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.gh.path().join("log")).unwrap_or_default()
    }

    fn run(&self, args: &[&str]) -> Output {
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
            .env("AETHYME_FAKE_GH_DIR", self.gh.path())
            .output()
            .unwrap()
    }

    fn chain(&self, extra: &[&str]) -> Output {
        let mut args = vec![
            "advanced",
            "merge-chain",
            "--session",
            &self.session,
            "--repo",
            "o/n",
            "--reason",
            "land the queue this test is about",
            "--poll-seconds",
            "0",
            // A regression must fail fast, not spin for the hour-long default.
            "--checks-timeout",
            "10",
            "--main-timeout",
            "10",
        ];
        args.extend_from_slice(extra);
        self.run(&args)
    }

    /// Writes that reached the provider, in order.
    fn writes(&self) -> Vec<String> {
        self.log()
            .lines()
            .filter(|line| {
                line.starts_with("pr ready")
                    || line.starts_with("pr update-branch")
                    || line.starts_with("pr merge")
                    || line.starts_with("workflow run")
            })
            .map(str::to_string)
            .collect()
    }
}

fn text(output: &Output) -> String {
    format!(
        "status {:?}\n--- stdout\n{}\n--- stderr\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// A draft behind its base is readied, updated, verified on its *new* head --
/// where a superseded, cancelled run of a check must not count -- merged with
/// that head pinned, and the base branch verified on the merge commit.
#[test]
fn a_draft_behind_its_base_is_readied_updated_verified_and_merged() {
    let fixture = Fixture::new();
    fixture.script("pr-5", 1, &pr(5, "OPEN", true, "aaa", None));
    fixture.script("compare-aaa", 1, r#"{"behind_by":2}"#);
    fixture.script("pr-5@updated", 1, &pr(5, "OPEN", false, "bbb", None));
    fixture.script(
        "checks-bbb",
        1,
        &checks(&[("lint", "in_progress", None, "2026-10-03T10:00:00Z")]),
    );
    fixture.script(
        "checks-bbb",
        2,
        // Newest first, as the provider lists them: the superseded run comes
        // after the one that replaced it.
        &checks(&[
            ("lint", "completed", Some("success"), "2026-10-03T10:05:00Z"),
            (
                "lint",
                "completed",
                Some("cancelled"),
                "2026-10-03T10:00:00Z",
            ),
        ]),
    );
    fixture.script(
        "pr-5@merged",
        1,
        &pr(5, "MERGED", false, "bbb", Some("mmm")),
    );
    fixture.script(
        "runs-mmm",
        1,
        &branch_runs(&[("CI", "in_progress", None, "2026-10-03T10:10:00Z")]),
    );
    fixture.script(
        "runs-mmm",
        2,
        &branch_runs(&[("CI", "completed", Some("success"), "2026-10-03T10:10:00Z")]),
    );

    let output = fixture.chain(&["5"]);
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(
        fixture.writes(),
        vec![
            "pr ready 5",
            "pr update-branch 5",
            "pr merge 5 --merge --match-head-commit bbb",
        ],
        "{}",
        text(&output)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("merge-chain complete: 1"), "{stdout}");

    let operations = fixture.run(&["advanced", "operations", "list", "--json"]);
    let listed: serde_json::Value = serde_json::from_slice(&operations.stdout).unwrap();
    assert_eq!(
        listed["operations"].as_array().map(Vec::len),
        Some(3),
        "every write is a journaled coordinated operation"
    );
}

/// A red check stops the chain before the merge, and the next pull request is
/// never even read.
#[test]
fn a_failing_check_stops_the_chain_before_anything_merges() {
    let fixture = Fixture::new();
    fixture.script("pr-5", 1, &pr(5, "OPEN", false, "aaa", None));
    fixture.script("compare-aaa", 1, r#"{"behind_by":0}"#);
    fixture.script(
        "checks-aaa",
        1,
        &checks(&[
            ("lint", "completed", Some("success"), "2026-10-03T10:00:00Z"),
            (
                "tests",
                "completed",
                Some("failure"),
                "2026-10-03T10:00:00Z",
            ),
        ]),
    );

    let output = fixture.chain(&["5", "6"]);
    assert_eq!(output.status.code(), Some(4), "{}", text(&output));
    assert!(fixture.writes().is_empty(), "{}", fixture.log());
    assert!(!fixture.log().contains("pr view 6"), "{}", fixture.log());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("tests (failure)"), "{stdout}");
    assert!(stdout.contains("next:"), "{stdout}");
}

/// A push while the checks run means the green result describes a commit
/// that is no longer the head; merging would land the untested one.
#[test]
fn a_head_that_moves_while_checks_run_stops_the_chain() {
    let fixture = Fixture::new();
    fixture.script("pr-5", 1, &pr(5, "OPEN", false, "aaa", None));
    fixture.script("pr-5", 2, &pr(5, "OPEN", false, "ccc", None));
    fixture.script("compare-aaa", 1, r#"{"behind_by":0}"#);
    fixture.script("checks-aaa", 1, &checks(GREEN));

    let output = fixture.chain(&["--checks-timeout", "5", "5"]);
    assert_eq!(output.status.code(), Some(3), "{}", text(&output));
    assert!(fixture.writes().is_empty(), "{}", fixture.log());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("moved from aaa to ccc"), "{stdout}");
}

/// A red default branch after a merge stops the chain: landing more on top
/// would bury the failure under unrelated merges.
#[test]
fn a_red_base_after_a_merge_stops_the_chain() {
    let fixture = Fixture::new();
    fixture.script("pr-5", 1, &pr(5, "OPEN", false, "aaa", None));
    fixture.script("compare-aaa", 1, r#"{"behind_by":0}"#);
    fixture.script("checks-aaa", 1, &checks(GREEN));
    fixture.script(
        "pr-5@merged",
        1,
        &pr(5, "MERGED", false, "aaa", Some("mmm")),
    );
    fixture.script(
        "runs-mmm",
        1,
        &branch_runs(&[("CI", "completed", Some("failure"), "2026-10-03T10:10:00Z")]),
    );

    let output = fixture.chain(&["5", "6"]);
    assert_eq!(output.status.code(), Some(4), "{}", text(&output));
    assert_eq!(
        fixture.writes(),
        vec!["pr merge 5 --merge --match-head-commit aaa"]
    );
    assert!(!fixture.log().contains("pr view 6"), "{}", fixture.log());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("main is red after merging #5"), "{stdout}");
}

/// Re-running after a stop skips what already merged, verifies the base on
/// the last merge commit first, then carries on with the rest.
#[test]
fn a_rerun_skips_merged_pull_requests_and_reverifies_the_last_one() {
    let fixture = Fixture::new();
    fixture.script("pr-5", 1, &pr(5, "MERGED", false, "aaa", Some("m5")));
    fixture.script(
        "runs-m5",
        1,
        &branch_runs(&[("CI", "completed", Some("success"), "t")]),
    );
    fixture.script("pr-6", 1, &pr(6, "OPEN", false, "bbb", None));
    fixture.script("compare-bbb", 1, r#"{"behind_by":0}"#);
    fixture.script("checks-bbb", 1, &checks(GREEN));
    fixture.script("pr-6@merged", 1, &pr(6, "MERGED", false, "bbb", Some("m6")));
    fixture.script(
        "runs-m6",
        1,
        &branch_runs(&[("CI", "completed", Some("success"), "t")]),
    );

    let output = fixture.chain(&["5", "6"]);
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(
        fixture.writes(),
        vec!["pr merge 6 --merge --match-head-commit bbb"]
    );
    let log = fixture.log();
    let verified = log.find("head_sha=m5").expect("m5 re-verified");
    let merged = log.find("pr merge 6").expect("6 merged");
    assert!(verified < merged, "{log}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("#5 already merged"), "{stdout}");
}

/// When no default-branch run appears on the merge commit, the configured
/// gates workflow is dispatched and its result decides.
#[test]
fn a_missing_base_run_dispatches_the_gates_workflow() {
    let fixture = Fixture::new();
    fixture.script("pr-5", 1, &pr(5, "OPEN", false, "aaa", None));
    fixture.script("compare-aaa", 1, r#"{"behind_by":0}"#);
    fixture.script("checks-aaa", 1, &checks(GREEN));
    fixture.script(
        "pr-5@merged",
        1,
        &pr(5, "MERGED", false, "aaa", Some("mmm")),
    );
    fixture.script("runs-mmm", 1, &branch_runs(&[]));
    fixture.script(
        "runs-mmm@dispatched",
        1,
        &branch_runs(&[("Gates", "completed", Some("success"), "t")]),
    );

    let output = fixture.chain(&[
        "--gates-workflow",
        "gates.yml",
        "--dispatch-after",
        "0",
        "5",
    ]);
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(
        fixture.writes(),
        vec![
            "pr merge 5 --merge --match-head-commit aaa",
            "workflow run gates.yml --ref main",
        ]
    );
}

/// Without a gates workflow there is nothing to dispatch: a merge commit no
/// run ever covers is a stop that says so, not a pass.
#[test]
fn a_merge_commit_no_run_covers_is_not_a_pass() {
    let fixture = Fixture::new();
    fixture.script("pr-5", 1, &pr(5, "OPEN", false, "aaa", None));
    fixture.script("compare-aaa", 1, r#"{"behind_by":0}"#);
    fixture.script("checks-aaa", 1, &checks(GREEN));
    fixture.script(
        "pr-5@merged",
        1,
        &pr(5, "MERGED", false, "aaa", Some("mmm")),
    );
    fixture.script("runs-mmm", 1, &branch_runs(&[]));

    let output = fixture.chain(&["--main-timeout", "0", "5"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--gates-workflow"), "{stdout}");
}

/// The dry run reads and plans; it writes nothing and needs no reason.
#[test]
fn a_dry_run_plans_without_writing() {
    let fixture = Fixture::new();
    fixture.script("pr-5", 1, &pr(5, "OPEN", true, "aaa", None));
    fixture.script("compare-aaa", 1, r#"{"behind_by":3}"#);

    let output = fixture.run(&[
        "advanced",
        "merge-chain",
        "--session",
        &fixture.session,
        "--repo",
        "o/n",
        "--dry-run",
        "5",
    ]);
    assert!(output.status.success(), "{}", text(&output));
    assert!(fixture.writes().is_empty(), "{}", fixture.log());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("gh pr ready"), "{stdout}");
    assert!(stdout.contains("update-branch (3 behind main)"), "{stdout}");
}

/// `--session` resolves against this checkout's broker, so a `--repo` its
/// session does not point at is refused before the provider is asked
/// anything -- not at the first write, after the checks wait.
#[test]
fn a_repo_the_session_does_not_point_at_is_refused_before_any_call() {
    let fixture = Fixture::new();
    git(
        fixture.repo.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/someone/else.git",
        ],
    );
    fixture.script("pr-5", 1, &pr(5, "OPEN", false, "aaa", None));

    let output = fixture.chain(&["5"]);
    assert_eq!(output.status.code(), Some(3), "{}", text(&output));
    assert!(fixture.log().is_empty(), "{}", fixture.log());
}
