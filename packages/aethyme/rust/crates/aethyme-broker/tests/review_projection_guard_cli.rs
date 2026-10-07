//! `review run` projects the review record onto a pull request through the
//! coordinated GitHub lane, and every write it emits has to pass the #566 ref
//! guard without an acknowledgement: creating the Aethyme comment, editing it
//! in place, creating a missing label and adding labels.
//!
//! The comment edit used `api --method PATCH repos/{owner}/{repo}/...`, which
//! the guard cannot vouch for, so after #566 a projection created its comment
//! once and then failed every later tick. This drives the real CLI, the real
//! session, the real guard and a fake `gh` that logs each write.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
mod common;

const HEAD: &str = "cccccccccccccccccccccccccccccccccccccccc";

/// `pr view` describes pull request 7 with `$AETHYME_FAKE_COMMENTS`; `label list`
/// answers the labels this fake has created; other reads answer empty; every
/// write is logged with its exact argv and succeeds.
const FAKE_GH: &str = r#"#!/bin/sh
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then
  case "$*" in
    *"--json headRefOid")
      printf '{"headRefOid":"%s"}\n' "$AETHYME_FAKE_PR_HEAD"
      exit 0
      ;;
  esac
  printf '{"number":7,"state":"OPEN","isDraft":false,"isCrossRepository":false,"authorAssociation":"MEMBER","baseRefName":"main","baseRefOid":"%s","headRefOid":"%s","changedFiles":1,"labels":[],"comments":%s,"commits":[],"files":[{"path":"docs/a.md","additions":3,"deletions":0}]}\n' "$AETHYME_FAKE_PR_BASE" "$AETHYME_FAKE_PR_HEAD" "${AETHYME_FAKE_COMMENTS:-[]}"
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "diff" ]; then
  exit 0
fi
if [ "$1" = "label" ] && [ "$2" = "list" ]; then
  printf '['
  sep=''
  for name in $(cat "$AETHYME_FAKE_GH_LABELS" 2>/dev/null); do
    printf '%s{"name":"%s"}' "$sep" "$name"
    sep=','
  done
  printf ']\n'
  exit 0
fi
if [ "$1" = "label" ] && [ "$2" = "create" ]; then
  printf '%s\n' "$3" >> "$AETHYME_FAKE_GH_LABELS"
fi
if [ "$1" = "api" ]; then
  case "$*" in
    *"-X "*) ;;
    *) printf '[]\n'; exit 0 ;;
  esac
fi
printf '%s\n' "$*" >> "$AETHYME_FAKE_GH_LOG"
if [ "$1" = "pr" ] && [ "$2" = "comment" ]; then
  printf 'https://github.com/acme/product/pull/7#issuecomment-55\n'
fi
exit 0
"#;

struct Fixture {
    repo: tempfile::TempDir,
    state: tempfile::TempDir,
    bin: tempfile::TempDir,
    base: String,
    session: String,
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
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
    String::from_utf8(output.stdout).unwrap().trim().into()
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

        let path = repo.path();
        git(path, &["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(path.join(".aethyme")).unwrap();
        std::fs::write(
            path.join(".aethyme/config.toml"),
            "[review.projection]\nenabled = true\n",
        )
        .unwrap();
        std::fs::write(path.join(".gitignore"), "/.aethyme/broker.db*\n").unwrap();
        std::fs::write(path.join("README.md"), "initial\n").unwrap();
        git(path, &["add", "-A"]);
        git(path, &["commit", "-qm", "initial"]);
        // The coordinated lane requires --repo to be this checkout's origin.
        git(
            path,
            &["remote", "add", "origin", "git@github.com:acme/product.git"],
        );
        let base = git(path, &["rev-parse", "main"]);

        let mut fixture = Self {
            repo,
            state,
            bin,
            base,
            session: String::new(),
        };
        let started = fixture.cli(
            &[
                "start",
                "--task",
                "project reviews",
                "--short-name",
                "project",
                "--json",
            ],
            &[],
        );
        assert!(
            started.status.success(),
            "start failed: {}",
            stderr(&started)
        );
        let value: serde_json::Value = serde_json::from_slice(&started.stdout).unwrap();
        let session = value.get("session").unwrap_or(&value);
        fixture.session = session["id"].as_i64().unwrap().to_string();
        fixture
    }

    fn log(&self) -> std::path::PathBuf {
        self.bin.path().join("gh.log")
    }

    fn cli(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut command = common::broker_cli(CLI, args);
        command
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
            .env("AETHYME_AGENT_PID", std::process::id().to_string())
            .env("AETHYME_FAKE_GH_LOG", self.log())
            .env("AETHYME_FAKE_GH_LABELS", self.bin.path().join("labels"))
            .env("AETHYME_FAKE_PR_HEAD", HEAD)
            .env("AETHYME_FAKE_PR_BASE", &self.base);
        for (key, value) in env {
            command.env(key, value);
        }
        command.output().unwrap()
    }

    fn review_run(&self, comments: &str) -> Output {
        self.cli(
            &[
                "advanced",
                "review",
                "run",
                "--session",
                &self.session,
                "--repo",
                "acme/product",
                "--pr",
                "7",
                "--from-provider",
                "--json",
            ],
            &[("AETHYME_FAKE_COMMENTS", comments)],
        )
    }

    fn writes(&self) -> Vec<String> {
        std::fs::read_to_string(self.log())
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn every_projection_write_runs_through_the_ref_guard_without_acknowledgement() {
    let fixture = Fixture::new();

    // First tick: no Aethyme comment and no labels exist yet.
    let first = fixture.review_run("[]");
    assert!(
        first.status.success(),
        "first tick failed: {}\n{}",
        stderr(&first),
        String::from_utf8_lossy(&first.stdout)
    );
    let writes = fixture.writes();
    assert!(
        writes.iter().any(|w| w.starts_with("pr comment 7 --body")),
        "comment not created: {writes:?}"
    );
    assert!(
        writes
            .iter()
            .any(|w| w.starts_with("label create aethyme/size:trivial")),
        "label not created: {writes:?}"
    );
    assert!(
        writes
            .iter()
            .any(|w| w.starts_with("pr edit 7 --add-label") && w.contains("aethyme/size:trivial")),
        "labels not added: {writes:?}"
    );

    // Second tick: the Aethyme comment exists with stale content, so it is
    // edited in place through the allowlisted API form.
    std::fs::remove_file(fixture.log()).unwrap();
    let stale = serde_json::json!([{
        "body": "<!-- aethyme:review -->\nstale\n",
        "url": "https://github.com/acme/product/pull/7#issuecomment-55",
        "author": {"login": "aethyme-bot"}
    }])
    .to_string();
    let second = fixture.review_run(&stale);
    assert!(
        second.status.success(),
        "second tick failed: {}\n{}",
        stderr(&second),
        String::from_utf8_lossy(&second.stdout)
    );
    let writes = fixture.writes();
    assert!(
        writes
            .iter()
            .any(|w| w.starts_with("api -X PATCH repos/acme/product/issues/comments/55 -f body=")),
        "comment not edited in place: {writes:?}"
    );
    assert!(
        !writes.iter().any(|w| w.starts_with("pr comment")),
        "a second comment was created: {writes:?}"
    );
}
