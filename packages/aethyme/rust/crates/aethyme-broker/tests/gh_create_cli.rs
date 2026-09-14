//! `gh issue create` must never leave "was it created?" unanswered (#184).
//!
//! Two halves, and they answer the question at different times. A label `gh`
//! would refuse is refused first, before anything is journaled or sent, so the
//! answer is "no" by construction. Everything a pre-flight check cannot catch
//! -- a label deleted a second later, a permissions failure part-way through --
//! is answered afterwards by asking the repository what it now contains.
//!
//! Every case here drives a fake `gh` on `PATH`, so nothing reaches GitHub and
//! no private data is published.
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

/// A `gh` whose every answer comes from the environment, and which records
/// what it was asked so a test can assert what was *not* sent.
const FAKE_GH: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$AETHYME_FAKE_GH_LOG"
limit=''
previous=''
for argument in "$@"; do
  if [ "$previous" = '--limit' ]; then limit="$argument"; fi
  previous="$argument"
done
case "$1 $2" in
  'label list')
    if [ -n "$AETHYME_FAKE_GH_LABELS" ]; then
      printf '%s\n' "$AETHYME_FAKE_GH_LABELS"
      exit 0
    fi
    printf 'label vocabulary unavailable\n' >&2
    exit 1
    ;;
  'issue list')
    if [ "$limit" = '1' ]; then payload="$AETHYME_FAKE_GH_WATERMARK"; else payload="$AETHYME_FAKE_GH_LISTING"; fi
    if [ -n "$payload" ]; then
      printf '%s\n' "$payload"
      exit 0
    fi
    printf 'listing unavailable\n' >&2
    exit 1
    ;;
  'issue create')
    if [ -n "$AETHYME_FAKE_GH_CREATE_STDOUT" ]; then
      printf '%s\n' "$AETHYME_FAKE_GH_CREATE_STDOUT"
    fi
    if [ "${AETHYME_FAKE_GH_CREATE_RC:-0}" != '0' ]; then
      printf 'could not add label: dette not found\n' >&2
    fi
    exit "${AETHYME_FAKE_GH_CREATE_RC:-0}"
    ;;
esac
exit 0
"#;

struct Fixture {
    repo: tempfile::TempDir,
    state: tempfile::TempDir,
    bin: tempfile::TempDir,
    session: String,
    labels: String,
    watermark: String,
    listing: String,
    create_stdout: String,
    create_rc: String,
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
            session: String::new(),
            labels: r#"[{"name":"bug"},{"name":"area:broker"}]"#.into(),
            watermark: r#"[{"number":10}]"#.into(),
            listing: "[]".into(),
            create_stdout: String::new(),
            create_rc: "0".into(),
        };
        let started = fixture.run(&["start", "--task", "gh create", "--json"]);
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

    fn log(&self) -> String {
        std::fs::read_to_string(self.repo.path().join("gh-log")).unwrap_or_default()
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(CLI)
            .args(args)
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
            .env("AETHYME_FAKE_GH_LOG", self.repo.path().join("gh-log"))
            .env("AETHYME_FAKE_GH_LABELS", &self.labels)
            .env("AETHYME_FAKE_GH_WATERMARK", &self.watermark)
            .env("AETHYME_FAKE_GH_LISTING", &self.listing)
            .env("AETHYME_FAKE_GH_CREATE_STDOUT", &self.create_stdout)
            .env("AETHYME_FAKE_GH_CREATE_RC", &self.create_rc)
            .output()
            .unwrap()
    }

    fn create(&self, labels: &[&str]) -> Output {
        let mut args = vec![
            "gh",
            "--session",
            &self.session,
            "--repo",
            "schiste/Aethyme",
            "--reason",
            "file the bug this test is about",
            "--json",
            "--",
            "issue",
            "create",
            "--title",
            "a bug",
            "--body",
            "b",
        ];
        for label in labels {
            args.push("--label");
            args.push(label);
        }
        self.run(&args)
    }
}

/// The reported failure: a complete payload, refused at the last moment for a
/// label. Refusing before anything is sent is what makes the answer to "was it
/// created?" unambiguous -- there is no operation to reconcile.
#[test]
fn an_undefined_label_is_refused_before_the_create_is_sent() {
    let fixture = Fixture::new();
    let refused = fixture.create(&["dette"]);

    assert!(!refused.status.success());
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        stderr.contains("does not define the label \"dette\""),
        "{stderr}"
    );
    assert!(
        stderr.contains("nothing was sent and nothing was created"),
        "{stderr}"
    );
    assert!(stderr.contains("area:broker, bug"), "{stderr}");

    let log = fixture.log();
    assert!(log.contains("label list"), "{log}");
    assert!(!log.contains("issue create"), "{log}");

    let operations = fixture.run(&["operations", "list", "--json"]);
    let listed: serde_json::Value = serde_json::from_slice(&operations.stdout).unwrap();
    assert_eq!(listed["operations"].as_array().map(Vec::len), Some(0));
}

/// GitHub matches label names without regard to case, so the check must too --
/// refusing on case alone would reject a name the command was going to apply.
#[test]
fn a_defined_label_passes_the_check_whatever_its_case() {
    let mut fixture = Fixture::new();
    fixture.labels = r#"[{"name":"Dette"},{"name":"bug"}]"#.into();
    fixture.create_stdout = "https://github.com/schiste/Aethyme/issues/11".into();

    let created = fixture.create(&["dette"]);
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    assert!(fixture.log().contains("issue create"), "{}", fixture.log());
}

/// Unreadable is not empty, and neither is a refusal. A machine that is
/// offline or rate-limited may still be one where the write would work, and
/// turning that into "unknown label" would refuse valid commands for a reason
/// that has nothing to do with labels.
#[test]
fn an_unreadable_label_vocabulary_does_not_refuse_the_write() {
    let mut fixture = Fixture::new();
    fixture.labels = String::new();
    fixture.create_stdout = "https://github.com/schiste/Aethyme/issues/11".into();

    let created = fixture.create(&["dette"]);
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    assert!(fixture.log().contains("issue create"), "{}", fixture.log());
}

/// `gh` prints the new issue's URL once the API call has returned, so a URL on
/// stdout is proof of creation even when the process then exits non-zero.
#[test]
fn a_failed_create_that_printed_its_url_is_recorded_as_created() {
    let mut fixture = Fixture::new();
    fixture.create_rc = "1".into();
    fixture.create_stdout = "https://github.com/schiste/Aethyme/issues/11".into();

    let report = fixture.create(&[]);
    let value: serde_json::Value = serde_json::from_slice(&report.stdout).unwrap();
    assert_eq!(value["operation"]["status"], "succeeded");
    assert_eq!(value["command_success"], false);

    let details: serde_json::Value =
        serde_json::from_str(value["operation"]["details_json"].as_str().unwrap()).unwrap();
    let evidence = &details["create_reconciliation"]["evidence"];
    assert_eq!(evidence["classification"], "succeeded");
    assert_eq!(evidence["source"], "command_output");
    assert_eq!(
        evidence["created"],
        "https://github.com/schiste/Aethyme/issues/11"
    );
}

/// Stdout says nothing, so the repository is asked. An issue numbered above
/// the watermark carrying the planned title is one this run created.
#[test]
fn a_failed_silent_create_found_above_the_watermark_is_recorded_as_created() {
    let mut fixture = Fixture::new();
    fixture.create_rc = "1".into();
    fixture.listing =
        r#"[{"number":11,"title":"a bug","url":"https://github.com/schiste/Aethyme/issues/11"}]"#
            .into();

    let report = fixture.create(&[]);
    let value: serde_json::Value = serde_json::from_slice(&report.stdout).unwrap();
    assert_eq!(value["operation"]["status"], "succeeded");

    let details: serde_json::Value =
        serde_json::from_str(value["operation"]["details_json"].as_str().unwrap()).unwrap();
    let evidence = &details["create_reconciliation"]["evidence"];
    assert_eq!(evidence["classification"], "succeeded");
    assert_eq!(evidence["source"], "post_create_observation");
    assert_eq!(evidence["number"], 11);
}

/// The same title, but below the watermark: it predates the command, so this
/// run created nothing. Without the watermark this would read as success.
#[test]
fn a_failed_silent_create_absent_from_the_repository_is_recorded_as_failed() {
    let mut fixture = Fixture::new();
    fixture.create_rc = "1".into();
    fixture.listing =
        r#"[{"number":9,"title":"a bug","url":"https://github.com/schiste/Aethyme/issues/9"}]"#
            .into();

    let report = fixture.create(&[]);
    let value: serde_json::Value = serde_json::from_slice(&report.stdout).unwrap();
    assert_eq!(value["operation"]["status"], "failed");

    let operation = value["operation"]["id"].as_i64().unwrap().to_string();
    let shown = fixture.run(&["operations", "show", &operation, "--json"]);
    let shown: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(shown["reconciliation"]["required"], false);
    assert_eq!(
        shown["reconciliation"]["evidence"]["evidence"]["classification"],
        "failed"
    );
}

/// The operator reads the answer in the output, not only in the journal: that
/// is the acceptance criterion #184 states first.
#[test]
fn the_create_outcome_is_stated_in_the_human_output() {
    let mut fixture = Fixture::new();
    fixture.create_rc = "1".into();
    fixture.create_stdout = "https://github.com/schiste/Aethyme/issues/11".into();

    let report = fixture.create_plain();
    let stdout = String::from_utf8_lossy(&report.stdout);
    assert!(
        stdout.contains(
            "the command failed, but it had already created \
             https://github.com/schiste/Aethyme/issues/11"
        ),
        "{stdout}"
    );
}

impl Fixture {
    fn create_plain(&self) -> Output {
        self.run(&[
            "gh",
            "--session",
            &self.session,
            "--repo",
            "schiste/Aethyme",
            "--reason",
            "file the bug this test is about",
            "--",
            "issue",
            "create",
            "--title",
            "a bug",
            "--body",
            "b",
        ])
    }
}
