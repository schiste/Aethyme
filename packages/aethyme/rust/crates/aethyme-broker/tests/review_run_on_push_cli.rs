//! `[review] run_on_push`: a real `broker push` of a session branch with an
//! open pull request runs one review for it, end to end, through a local bare
//! remote behind a GitHub-shaped `origin` and a fake `gh`.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::Broker;

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
mod common;

const FAKE_SSH: &str = r#"#!/bin/sh
case "$*" in
  *git-upload-pack*) exec git-upload-pack "$AETHYME_TEST_GIT_REMOTE" ;;
  *git-receive-pack*) exec git-receive-pack "$AETHYME_TEST_GIT_REMOTE" ;;
esac
exit 64
"#;

/// Answers the push's pull request lookup and the review run's reads, keeps
/// comments as files under `$STATE/comments/<id>` and logs writes to
/// `$STATE/writes`.
const FAKE_GH: &str = r#"#!/bin/sh
STATE="$AETHYME_FAKE_GH_STATE"
mkdir -p "$STATE/comments"
json_string() {
  printf '"'
  sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' "$1" | awk '{ if (NR > 1) printf "\\n"; printf "%s", $0 }'
  printf '"'
}
comments_json() {
  printf '['
  first=1
  for file in $(ls "$STATE/comments" | sort -n); do
    [ $first -eq 1 ] || printf ','
    first=0
    printf '{"url":"https://github.com/acme/project/pull/7#issuecomment-%s","author":{"login":"aethyme-bot"},"body":' "$file"
    json_string "$STATE/comments/$file"
    printf '}'
  done
  printf ']'
}
draft=${AETHYME_FAKE_DRAFT:-false}
if [ "$1" = "pr" ] && [ "$2" = "list" ]; then
  printf '[{"number":7,"url":"https://github.com/acme/project/pull/7","state":"OPEN","headRefName":"%s","isDraft":%s}]\n' "$AETHYME_FAKE_BRANCH" "$draft"
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then
  [ -n "$AETHYME_FAKE_VIEW_FAIL" ] && { echo "gh: provider unavailable" >&2; exit 1; }
  case "$*" in
    *"--json headRefOid")
      printf '{"headRefOid":"%s"}\n' "$AETHYME_FAKE_PR_HEAD"
      exit 0
      ;;
  esac
  printf '{"number":7,"state":"OPEN","isDraft":%s,"isCrossRepository":false,"authorAssociation":"MEMBER","baseRefName":"main","baseRefOid":"%s","headRefOid":"%s","changedFiles":1,"labels":[],"comments":%s,"commits":[],"files":%s,"reviews":[]}\n' "$draft" "$AETHYME_FAKE_PR_BASE" "$AETHYME_FAKE_PR_HEAD" "$(comments_json)" "$AETHYME_FAKE_PR_FILES"
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "comment" ]; then
  echo "create" >> "$STATE/writes"
  id=$(( $(ls "$STATE/comments" | wc -l) + 101 ))
  printf '%s' "$5" > "$STATE/comments/$id"
  echo "https://github.com/acme/project/pull/7#issuecomment-$id"
  exit 0
fi
if [ "$1" = "api" ] && [ "$2" = "user" ]; then
  echo "aethyme-bot"
  exit 0
fi
if [ "$1" = "api" ] && [ "$2" = "-X" ]; then
  id=${4##*/}
  case "$3" in
    PATCH)
      echo "update $id" >> "$STATE/writes"
      printf '%s' "${6#body=}" > "$STATE/comments/$id"
      ;;
    DELETE)
      echo "delete $id" >> "$STATE/writes"
      rm -f "$STATE/comments/$id"
      ;;
  esac
  printf '{}\n'
  exit 0
fi
if [ "$1" = "api" ] || [ "$1" = "label" ]; then
  printf '[]\n'
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "diff" ]; then
  exit 0
fi
echo "unexpected gh $*" >&2
exit 1
"#;

const LARGE: &str = r#"[{"path":"src/lib.rs","additions":2000,"deletions":0}]"#;

fn policy(run_on_push: bool) -> String {
    format!(
        r#"[delivery]
push_session_branches = true

[review]
run_on_push = {run_on_push}

[review.trigger]
enabled = true

[[review.trigger.rule]]
name = "large-pr-heads-up"
min_tier = "large"
comment = {{ template = "large-pr", key = "size-heads-up" }}

[review.comments.large-pr]
body = """
@codex review
This pull request is **{{{{tier}}}}** ({{{{churn}}}} changed lines) at {{{{head}}}}.
"""
"#
    )
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
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

struct Fixture {
    tmp: tempfile::TempDir,
    repo: PathBuf,
    remote: PathBuf,
    bin: PathBuf,
    state: PathBuf,
    base: String,
}

impl Fixture {
    fn new(run_on_push: bool) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let remote = tmp.path().join("remote.git");
        let bin = tmp.path().join("bin");
        let state = tmp.path().join("gh-state");
        for dir in [&repo, &remote, &bin, &state] {
            std::fs::create_dir_all(dir).unwrap();
        }
        for (name, body) in [("ssh", FAKE_SSH), ("gh", FAKE_GH)] {
            let path = bin.join(name);
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        git(&remote, &["init", "--bare", "-q", "-b", "main"]);
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(
            repo.join(".gitignore"),
            ".aethyme/*\n!.aethyme/config.toml\n",
        )
        .unwrap();
        std::fs::create_dir_all(repo.join(".aethyme")).unwrap();
        std::fs::write(repo.join(".aethyme/config.toml"), policy(run_on_push)).unwrap();
        std::fs::write(repo.join("tracked.txt"), "base\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "init"]);
        git(
            &repo,
            &["remote", "add", "origin", "git@github.com:acme/project.git"],
        );
        git(
            &repo,
            &[
                "config",
                "core.sshCommand",
                bin.join("ssh").to_str().unwrap(),
            ],
        );
        let base = git(&repo, &["rev-parse", "main"]);
        let fixture = Self {
            tmp,
            repo,
            remote,
            bin,
            state,
            base,
        };
        let output = Command::new("git")
            .args(["push", "-qu", "origin", "main"])
            .current_dir(&fixture.repo)
            .env("AETHYME_TEST_GIT_REMOTE", &fixture.remote)
            .output()
            .unwrap();
        assert!(output.status.success());
        fixture
    }

    /// A live session with one committed change: (id, branch, head).
    fn session(&self) -> (i64, String, String) {
        let session = Broker::open(&self.repo)
            .unwrap()
            .with_host_operation_database(self.tmp.path().join("host-state/operations.db"))
            .start_worktree("push early", None)
            .unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        std::fs::write(worktree.join("feature.txt"), "one\n").unwrap();
        git(&worktree, &["add", "feature.txt"]);
        git(&worktree, &["commit", "-qm", "feat: first step"]);
        let head = git(&worktree, &["rev-parse", "HEAD"]);
        (session.id, session.branch, head)
    }

    fn push(&self, session: i64, branch: &str, head: &str, env: &[(&str, &str)]) -> Output {
        let id = session.to_string();
        common::broker_cli(CLI, &["push", "--session", &id, "--json"])
            .current_dir(&self.repo)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("AETHYME_HOST_STATE_DIR", self.tmp.path().join("host-state"))
            .env("AETHYME_AGENT_PID", std::process::id().to_string())
            .env("AETHYME_TEST_GIT_REMOTE", &self.remote)
            .env("AETHYME_FAKE_GH_STATE", &self.state)
            .env("AETHYME_FAKE_BRANCH", branch)
            .env("AETHYME_FAKE_PR_HEAD", head)
            .env("AETHYME_FAKE_PR_BASE", &self.base)
            .env("AETHYME_FAKE_PR_FILES", LARGE)
            .envs(env.iter().copied())
            .output()
            .unwrap()
    }

    fn writes(&self) -> Vec<String> {
        std::fs::read_to_string(self.state.join("writes"))
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    fn comments(&self) -> Vec<String> {
        let mut ids: Vec<u64> = std::fs::read_dir(self.state.join("comments"))
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
                    .collect()
            })
            .unwrap_or_default();
        ids.sort_unstable();
        ids.iter()
            .map(|id| std::fs::read_to_string(self.state.join(format!("comments/{id}"))).unwrap())
            .collect()
    }
}

fn report(output: &Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "push failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn a_push_runs_one_review_and_a_repeated_push_posts_nothing_new() {
    let fixture = Fixture::new(true);
    let (session, branch, head) = fixture.session();

    let first = report(&fixture.push(session, &branch, &head, &[]));
    assert_eq!(first["review_run"]["performed"], true, "{first:#}");
    assert_eq!(first["review_run"]["pull_request"], 7);
    assert_eq!(fixture.writes(), ["create"]);
    let comments = fixture.comments();
    assert_eq!(comments.len(), 1);
    assert!(comments[0].contains("@codex review"), "{}", comments[0]);
    assert!(comments[0].contains("**large**"), "{}", comments[0]);
    assert!(
        comments[0].contains(&head),
        "{{head}} renders: {}",
        comments[0]
    );

    // Same head, same rendering: the second push must not post or edit.
    let second = report(&fixture.push(session, &branch, &head, &[]));
    assert_eq!(second["review_run"]["performed"], true, "{second:#}");
    assert_eq!(fixture.writes(), ["create"], "no new comment, no edit");
    assert_eq!(fixture.comments().len(), 1);
}

#[test]
fn nothing_runs_when_run_on_push_is_off() {
    let fixture = Fixture::new(false);
    let (session, branch, head) = fixture.session();

    let pushed = report(&fixture.push(session, &branch, &head, &[]));
    assert!(pushed.get("review_run").is_none(), "{pushed:#}");
    assert!(fixture.writes().is_empty());
    assert!(fixture.comments().is_empty());
}

#[test]
fn a_draft_pull_request_is_skipped() {
    let fixture = Fixture::new(true);
    let (session, branch, head) = fixture.session();

    let pushed = report(&fixture.push(session, &branch, &head, &[("AETHYME_FAKE_DRAFT", "true")]));
    assert_eq!(pushed["review_run"]["performed"], false, "{pushed:#}");
    assert!(
        pushed["review_run"]["skipped"]
            .as_str()
            .is_some_and(|why| why.contains("draft")),
        "{pushed:#}"
    );
    assert!(fixture.writes().is_empty());
}

#[test]
fn a_failed_review_run_still_lets_the_push_succeed_with_a_warning() {
    let fixture = Fixture::new(true);
    let (session, branch, head) = fixture.session();

    let output = fixture.push(session, &branch, &head, &[("AETHYME_FAKE_VIEW_FAIL", "1")]);
    let pushed = report(&output);
    assert_eq!(
        pushed["pushed_oid"],
        head.as_str(),
        "the push itself landed"
    );
    assert_eq!(pushed["review_run"]["performed"], false, "{pushed:#}");
    assert!(pushed["review_run"]["error"].is_string(), "{pushed:#}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("warning: review run on push did not complete"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Rerun: aethyme broker advanced review run"),
        "{stderr}"
    );
    assert!(fixture.writes().is_empty());
}
