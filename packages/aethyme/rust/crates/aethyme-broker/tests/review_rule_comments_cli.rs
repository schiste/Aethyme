//! #596: a review rule posts a comment whose content is a configured template,
//! keeps it current in place, and never posts where it must not.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
mod common;

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

const COMMENT_LARGE: &str = r#"
[review.trigger]
enabled = true

[[review.trigger.rule]]
name = "large-pr-heads-up"
min_tier = "large"
comment = { template = "large-pr", key = "size-heads-up" }

[review.comments.large-pr]
body = """
This pull request is **{{tier}}** ({{files_changed}} files, {{churn}} changed lines).
Please consider splitting it, or request a review: @codex review
"""

[review.projection]
reserved = ["skip-review"]
"#;

/// A fake `gh` that keeps the pull request's comments as files under
/// `$AETHYME_FAKE_GH_STATE/comments/<id>`, so a tick sees what the previous one wrote,
/// and logs every write to `$AETHYME_FAKE_GH_STATE/writes`.
const FAKE_GH: &str = r#"#!/bin/sh
STATE="$AETHYME_FAKE_GH_STATE"
mkdir -p "$STATE/comments"
json_string() {
  # Escape a file's content as a JSON string.
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
    author=aethyme-bot
    [ -f "$STATE/authors/$file" ] && author=$(cat "$STATE/authors/$file")
    printf '{"url":"https://github.com/acme/product/pull/7#issuecomment-%s","author":{"login":"%s"},"body":' "$file" "$author"
    json_string "$STATE/comments/$file"
    printf '}'
  done
  printf ']'
}
if [ "$1" = "pr" ] && [ "$2" = "view" ]; then
  case "$*" in
    *"--json headRefOid")
      printf '{"headRefOid":"%s"}\n' "$FAKE_PR_HEAD"
      exit 0
      ;;
  esac
  printf '{"number":7,"state":"OPEN","isDraft":false,"isCrossRepository":%s,"authorAssociation":"MEMBER","baseRefName":"main","baseRefOid":"%s","headRefOid":"%s","changedFiles":1,"labels":%s,"comments":%s,"commits":[],"files":%s,"reviews":[]}\n' "${FAKE_FORK:-false}" "$FAKE_PR_BASE" "$FAKE_PR_HEAD" "${FAKE_LABELS:-[]}" "$(comments_json)" "$FAKE_PR_FILES"
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "comment" ]; then
  echo "create" >> "$STATE/writes"
  id=$(( $(ls "$STATE/comments" | wc -l) + 101 ))
  printf '%s' "$5" > "$STATE/comments/$id"
  echo "https://github.com/acme/product/pull/7#issuecomment-$id"
  exit 0
fi
if [ "$1" = "api" ] && [ "$2" = "user" ]; then
  [ -n "$FAKE_NO_VIEWER" ] && exit 1
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

struct Fixture {
    root: tempfile::TempDir,
    fake_bin: PathBuf,
    state: PathBuf,
    base: String,
    session: String,
    env: Vec<(String, String)>,
}

impl Fixture {
    fn new(policy: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path();
        git(path, &["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(path.join(".aethyme")).unwrap();
        std::fs::write(path.join(".aethyme/config.toml"), policy).unwrap();
        std::fs::write(
            path.join(".gitignore"),
            "/.aethyme/broker.db*\n/host-state/\n/fake-bin/\n/fake-state/\n",
        )
        .unwrap();
        std::fs::write(path.join("README.md"), "initial\n").unwrap();
        git(path, &["add", "-A"]);
        git(path, &["commit", "-qm", "initial"]);
        git(
            path,
            &["remote", "add", "origin", "git@github.com:acme/product.git"],
        );
        let fake_bin = path.join("fake-bin");
        std::fs::create_dir(&fake_bin).unwrap();
        let gh = fake_bin.join("gh");
        std::fs::write(&gh, FAKE_GH).unwrap();
        let mut permissions = std::fs::metadata(&gh).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&gh, permissions).unwrap();
        let state = path.join("fake-state");
        std::fs::create_dir_all(state.join("authors")).unwrap();
        let base = git(path, &["rev-parse", "main"]);
        let mut fixture = Self {
            root,
            fake_bin,
            state,
            base,
            session: String::new(),
            env: Vec::new(),
        };
        let started = fixture.broker(
            &[
                "start",
                "--task",
                "route reviews",
                "--short-name",
                "router",
                "--json",
            ],
            "",
            "[]",
        );
        fixture.session = started["id"]
            .as_i64()
            .or_else(|| started["session_id"].as_i64())
            .expect("a session id")
            .to_string();
        fixture
    }

    fn command(&self, args: &[&str], head: &str, files: &str) -> std::process::Output {
        let path = format!(
            "{}:{}",
            self.fake_bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        common::broker_cli(CLI, args)
            .current_dir(self.root.path())
            .env("PATH", path)
            .env(
                "AETHYME_HOST_STATE_DIR",
                self.root.path().join("host-state"),
            )
            .env("AETHYME_AGENT_PID", std::process::id().to_string())
            .env("AETHYME_FAKE_GH_STATE", &self.state)
            .env("FAKE_PR_HEAD", head)
            .env("FAKE_PR_FILES", files)
            .env("FAKE_PR_BASE", &self.base)
            .envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .output()
            .unwrap()
    }

    fn broker(&self, args: &[&str], head: &str, files: &str) -> serde_json::Value {
        let output = self.command(args, head, files);
        assert!(
            output.status.success(),
            "{args:?} failed: {}\n{}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        serde_json::from_slice(&output.stdout).unwrap_or(serde_json::Value::Null)
    }

    fn run(&self, head: &str, files: &str) -> serde_json::Value {
        self.broker(
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
            head,
            files,
        )
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

const HEAD_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HEAD_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const TRIVIAL: &str = r#"[{"path":"docs/a.md","additions":3,"deletions":0}]"#;
const LARGE: &str = r#"[{"path":"src/lib.rs","additions":2000,"deletions":0}]"#;
const LARGER: &str = r#"[{"path":"src/lib.rs","additions":2500,"deletions":10}]"#;

#[test]
fn a_large_change_gets_exactly_one_rendered_comment_and_a_trivial_one_none() {
    let fixture = Fixture::new(COMMENT_LARGE);
    fixture.run(HEAD_A, TRIVIAL);
    assert!(fixture.writes().is_empty(), "{:?}", fixture.writes());

    let report = fixture.run(HEAD_A, LARGE);
    assert_eq!(fixture.writes(), ["create"], "{report:#}");
    let comments = fixture.comments();
    assert_eq!(comments.len(), 1);
    assert!(
        comments[0].starts_with("<!-- aethyme:comment:size-heads-up -->\n"),
        "{}",
        comments[0]
    );
    assert!(
        comments[0].contains("This pull request is **large** (1 files, 2000 changed lines)."),
        "{}",
        comments[0]
    );
    assert!(comments[0].contains("@codex review"));
    assert_eq!(
        report["rule_comments"][0]["decision"], "create",
        "{report:#}"
    );
    assert_eq!(report["rule_comments"][0]["rule"], "large-pr-heads-up");
}

#[test]
fn an_unchanged_tick_writes_nothing_and_a_new_rendering_edits_the_same_comment() {
    let fixture = Fixture::new(COMMENT_LARGE);
    fixture.run(HEAD_A, LARGE);
    let report = fixture.run(HEAD_A, LARGE);
    assert_eq!(
        fixture.writes(),
        ["create"],
        "a repeat tick must be a no-op"
    );
    assert_eq!(report["rule_comments"][0]["decision"], "none");

    fixture.run(HEAD_B, LARGER);
    assert_eq!(fixture.writes(), ["create", "update 101"]);
    let comments = fixture.comments();
    assert_eq!(comments.len(), 1, "edited in place, never a second comment");
    assert!(
        comments[0].contains("2510 changed lines"),
        "{}",
        comments[0]
    );
}

#[test]
fn on_unmatch_delete_removes_the_comment_and_keep_leaves_it() {
    let fixture = Fixture::new(COMMENT_LARGE);
    fixture.run(HEAD_A, LARGE);
    fixture.run(HEAD_B, TRIVIAL);
    assert_eq!(fixture.comments().len(), 1, "on_unmatch defaults to keep");

    let deleting = COMMENT_LARGE.replace(
        r#"key = "size-heads-up" }"#,
        r#"key = "size-heads-up", on_unmatch = "delete" }"#,
    );
    std::fs::write(fixture.root.path().join(".aethyme/config.toml"), deleting).unwrap();
    fixture.run(HEAD_B, TRIVIAL);
    assert_eq!(fixture.writes(), ["create", "delete 101"]);
    assert!(fixture.comments().is_empty());
}

#[test]
fn a_fork_or_a_reserved_label_suppresses_the_comment() {
    let mut fork = Fixture::new(COMMENT_LARGE);
    fork.env.push(("FAKE_FORK".into(), "true".into()));
    let report = fork.run(HEAD_A, LARGE);
    assert!(fork.writes().is_empty());
    assert_eq!(report["rule_comments"][0]["decision"], "suppressed");
    assert!(
        report["rule_comments"][0]["why"]
            .as_str()
            .unwrap()
            .contains("fork")
    );

    let mut reserved = Fixture::new(COMMENT_LARGE);
    reserved.env.push((
        "FAKE_LABELS".into(),
        r#"[{"name":"aethyme/skip-review"}]"#.into(),
    ));
    reserved.run(HEAD_A, LARGE);
    assert!(reserved.writes().is_empty());
}

#[test]
fn an_unknown_identity_or_somebody_elses_marker_is_never_written_over() {
    let mut anonymous = Fixture::new(COMMENT_LARGE);
    anonymous.env.push(("FAKE_NO_VIEWER".into(), "1".into()));
    anonymous.run(HEAD_A, LARGE);
    assert!(
        anonymous.writes().is_empty(),
        "without knowing who it is, the broker cannot tell its comment from a spoof"
    );

    // Somebody else pastes the marker: the broker posts its own comment and
    // leaves theirs alone.
    let fixture = Fixture::new(COMMENT_LARGE);
    std::fs::create_dir_all(fixture.state.join("comments")).unwrap();
    std::fs::write(
        fixture.state.join("comments/100"),
        "<!-- aethyme:comment:size-heads-up -->\nspoofed",
    )
    .unwrap();
    std::fs::write(fixture.state.join("authors/100"), "intruder").unwrap();
    fixture.run(HEAD_A, LARGE);
    assert_eq!(fixture.writes(), ["create"]);
    assert_eq!(
        std::fs::read_to_string(fixture.state.join("comments/100")).unwrap(),
        "<!-- aethyme:comment:size-heads-up -->\nspoofed"
    );
}

#[test]
fn pull_request_controlled_text_never_reaches_the_comment() {
    // The template may name only measured values; the path below is the only
    // repository-controlled text that reaches a variable (through `reasons`),
    // and it is rendered inert.
    let policy = COMMENT_LARGE
        .replace(
            "Please consider splitting it",
            "Why: {{reasons}}. Please consider splitting it",
        )
        .replace(
            "[review.projection]",
            "[review.classification]\nsensitive_paths = [\"src/**\"]\n\n[review.projection]",
        );
    let fixture = Fixture::new(&policy);
    let files =
        r#"[{"path":"src/@everyone [click](https://evil).rs","additions":2000,"deletions":0}]"#;
    fixture.run(HEAD_A, files);
    let comments = fixture.comments();
    assert_eq!(comments.len(), 1);
    assert!(!comments[0].contains("@everyone"), "{}", comments[0]);
    assert!(
        !comments[0].contains("[click](https://evil)"),
        "{}",
        comments[0]
    );
}

#[test]
fn an_unknown_variable_or_a_missing_template_is_rejected_at_load() {
    let bad_variable = COMMENT_LARGE.replace("{{tier}}", "{{title}}");
    let fixture = Fixture::new("");
    std::fs::write(
        fixture.root.path().join(".aethyme/config.toml"),
        bad_variable,
    )
    .unwrap();
    let output = fixture.command(
        &[
            "advanced",
            "review",
            "plan",
            "--base",
            "main",
            "--pr",
            "7",
            "--repo",
            "acme/product",
        ],
        "",
        "[]",
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("review.comments.large-pr"), "{stderr}");
    assert!(stderr.contains("unknown variable `{{title}}`"), "{stderr}");

    let missing = COMMENT_LARGE.replace(r#"template = "large-pr""#, r#"template = "nope""#);
    std::fs::write(fixture.root.path().join(".aethyme/config.toml"), missing).unwrap();
    let output = fixture.command(
        &[
            "advanced",
            "review",
            "plan",
            "--base",
            "main",
            "--pr",
            "7",
            "--repo",
            "acme/product",
        ],
        "",
        "[]",
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("comment template `nope` is not defined"),
        "{stderr}"
    );
}

#[test]
fn plan_shows_the_rendered_comment_and_its_decision_without_writing() {
    let fixture = Fixture::new(COMMENT_LARGE);
    let root = fixture.root.path();
    git(root, &["checkout", "-q", "-b", "change"]);
    let lines: String = (0..900).map(|line| format!("line {line}\n")).collect();
    std::fs::write(root.join("big.rs"), lines).unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-qm", "large"]);
    let plan = fixture.broker(
        &[
            "advanced",
            "review",
            "plan",
            "--base",
            "main",
            "--pr",
            "7",
            "--repo",
            "acme/product",
        ],
        "",
        "[]",
    );
    let decision = &plan["rule_comments"][0];
    assert_eq!(decision["decision"], "create", "{plan:#}");
    assert!(
        decision["body"]
            .as_str()
            .unwrap()
            .contains("This pull request is **large**"),
        "{decision}"
    );
    assert!(fixture.writes().is_empty());
}
