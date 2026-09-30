//! Overlap between a session's change and the repository's open pull requests.
//!
//! Each case publishes a real "other" branch to a local bare `origin` behind a
//! GitHub-shaped remote, so the open PR's change is read from its fetched
//! remote-tracking ref, and a fake `gh` answers `pr list` and `pr diff` from
//! files the test writes. Nothing reaches GitHub.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::Broker;

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

const FAKE_SSH: &str = r#"#!/bin/sh
case "$*" in
  *git-upload-pack*) exec git-upload-pack "$AETHYME_TEST_GIT_REMOTE" ;;
  *git-receive-pack*) exec git-receive-pack "$AETHYME_TEST_GIT_REMOTE" ;;
esac
exit 64
"#;

/// Records every call. `pr list --head <branch>` (the session's own PR lookup)
/// answers `[]`; the open-PR listing answers from `$AETHYME_FAKE_OPEN_PRS`;
/// `pr diff <n>` answers from `$AETHYME_FAKE_PR_DIFFS/<n>` or fails.
const FAKE_GH: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$AETHYME_FAKE_GH_LOG"
if [ "$1" = "pr" ] && [ "$2" = "list" ]; then
  case "$*" in
    *--head*) printf '[]\n' ;;
    *) if [ -f "$AETHYME_FAKE_OPEN_PRS" ]; then cat "$AETHYME_FAKE_OPEN_PRS"; else printf '[]\n'; fi ;;
  esac
  exit 0
fi
if [ "$1" = "pr" ] && [ "$2" = "diff" ]; then
  if [ -f "$AETHYME_FAKE_PR_DIFFS/$3" ]; then cat "$AETHYME_FAKE_PR_DIFFS/$3"; exit 0; fi
  exit 1
fi
exit 64
"#;

fn git_output(cwd: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string()
}

fn git(cwd: &Path, args: &[&str]) {
    git_output(cwd, args);
}

/// Thirty numbered lines, with `edits` replacing some of them.
fn numbered(edits: &[(usize, &str)]) -> String {
    (1..=30)
        .map(|line| {
            edits
                .iter()
                .find(|(at, _)| *at == line)
                .map_or_else(|| format!("line {line}"), |(_, text)| (*text).to_string())
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

struct Fixture {
    tmp: tempfile::TempDir,
    repo: PathBuf,
    remote: PathBuf,
    bin: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let remote = tmp.path().join("remote.git");
        let bin = tmp.path().join("bin");
        for dir in [&repo, &remote, &bin, &tmp.path().join("diffs")] {
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
        std::fs::write(repo.join("shared.txt"), numbered(&[])).unwrap();
        std::fs::create_dir_all(repo.join(".aethyme")).unwrap();
        std::fs::write(
            repo.join(".aethyme/config.toml"),
            "[delivery]\npush_session_branches = true\n",
        )
        .unwrap();
        git(
            &repo,
            &["add", ".gitignore", "shared.txt", ".aethyme/config.toml"],
        );
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
        let fixture = Self {
            tmp,
            repo,
            remote,
            bin,
        };
        fixture.git_env(&["push", "-qu", "origin", "main"]);
        fixture
    }

    fn git_env(&self, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.repo)
            .env("AETHYME_TEST_GIT_REMOTE", &self.remote)
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
    }

    /// Publish `branch` (changing `shared.txt` as given) to origin, fetch it,
    /// and return its head. Plays the part of another agent's open PR.
    fn publish_other(&self, branch: &str, edits: &[(usize, &str)], fetch: bool) -> String {
        git(&self.repo, &["switch", "-q", "-c", branch]);
        std::fs::write(self.repo.join("shared.txt"), numbered(edits)).unwrap();
        git(&self.repo, &["commit", "-qam", "other agent's change"]);
        let head = git_output(&self.repo, &["rev-parse", "HEAD"]);
        self.git_env(&["push", "-q", "origin", branch]);
        git(&self.repo, &["switch", "-q", "main"]);
        if fetch {
            self.git_env(&["fetch", "-q", "origin"]);
        } else {
            git(
                &self.repo,
                &["update-ref", "-d", &format!("refs/remotes/origin/{branch}")],
            );
        }
        head
    }

    fn open_prs(&self, prs: &[(i64, &str, &str, &str)]) {
        let listing = prs
            .iter()
            .map(|(number, branch, oid, state)| {
                format!(
                    r#"{{"number":{number},"url":"https://github.com/acme/project/pull/{number}","state":"{state}","headRefName":"{branch}","headRefOid":"{oid}"}}"#
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        std::fs::write(
            self.tmp.path().join("open-prs.json"),
            format!("[{listing}]"),
        )
        .unwrap();
    }

    fn pr_diff(&self, number: i64, diff: &str) {
        std::fs::write(self.tmp.path().join("diffs").join(number.to_string()), diff).unwrap();
    }

    fn broker(&self) -> Broker {
        Broker::open(&self.repo)
            .unwrap()
            .with_host_operation_database(self.tmp.path().join("host-state/operations.db"))
    }

    /// A live session whose one commit edits `shared.txt` as given.
    fn session_editing(&self, edits: &[(usize, &str)]) -> (i64, String) {
        let session = self.broker().start_worktree("overlap", None).unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        std::fs::write(worktree.join("shared.txt"), numbered(edits)).unwrap();
        git(&worktree, &["commit", "-qam", "feat: session change"]);
        (session.id, session.branch)
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(CLI)
            .args(args)
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
            .env("AETHYME_TEST_GIT_REMOTE", &self.remote)
            .env("AETHYME_FAKE_GH_LOG", self.tmp.path().join("gh-log"))
            .env(
                "AETHYME_FAKE_OPEN_PRS",
                self.tmp.path().join("open-prs.json"),
            )
            .env("AETHYME_FAKE_PR_DIFFS", self.tmp.path().join("diffs"))
            .output()
            .unwrap()
    }

    fn push(&self, session: i64) -> serde_json::Value {
        json(&self.run(&["push", "--session", &session.to_string(), "--json"]))
    }

    fn status(&self) -> serde_json::Value {
        json(&self.run(&["status", "--json"]))
    }

    fn gh_log(&self) -> String {
        std::fs::read_to_string(self.tmp.path().join("gh-log")).unwrap_or_default()
    }

    fn clear_gh_log(&self) {
        let _ = std::fs::remove_file(self.tmp.path().join("gh-log"));
    }
}

fn json(output: &Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn overlap_advice(status: &serde_json::Value) -> Vec<serde_json::Value> {
    status["advice"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["id"] == "session.pr-overlap")
        .cloned()
        .collect()
}

/// Changed lines two apart from an open PR's: Git would conflict on them.
#[test]
fn nearby_changes_to_an_open_prs_lines_are_a_warning() {
    let fixture = Fixture::new();
    let other = fixture.publish_other("agent/other", &[(10, "theirs")], true);
    fixture.open_prs(&[(12, "agent/other", &other, "OPEN")]);
    let (session, _) = fixture.session_editing(&[(12, "ours")]);

    let report = fixture.push(session);
    let overlaps = report["pr_overlaps"].as_array().unwrap();
    assert_eq!(overlaps.len(), 1, "{report}");
    assert_eq!(overlaps[0]["pr"], 12);
    assert_eq!(overlaps[0]["files"], serde_json::json!(["shared.txt"]));
    assert_eq!(overlaps[0]["conflicting_hunks"], true);

    let advice = overlap_advice(&fixture.status());
    assert_eq!(advice.len(), 1, "{advice:?}");
    assert_eq!(advice[0]["severity"], "warning");
    assert!(
        advice[0]["commands"][0]
            .as_str()
            .unwrap()
            .contains("rebase after #12 merges"),
        "{advice:?}"
    );
}

/// The same file, far apart: reported at the lowest severity, not as a
/// conflict.
#[test]
fn distant_changes_to_the_same_file_are_informational() {
    let fixture = Fixture::new();
    let other = fixture.publish_other("agent/other", &[(2, "theirs")], true);
    fixture.open_prs(&[(12, "agent/other", &other, "OPEN")]);
    let (session, _) = fixture.session_editing(&[(25, "ours")]);

    let report = fixture.push(session);
    assert_eq!(
        report["pr_overlaps"][0]["conflicting_hunks"], false,
        "{report}"
    );
    let advice = overlap_advice(&fixture.status());
    assert_eq!(advice.len(), 1, "{advice:?}");
    assert_eq!(advice[0]["severity"], "info");
}

/// A session is never warned about its own pull request, and a PR the
/// listing reports as merged or closed is ignored.
#[test]
fn own_and_closed_pull_requests_are_ignored() {
    let fixture = Fixture::new();
    let other = fixture.publish_other("agent/other", &[(10, "theirs")], true);
    let (session, branch) = fixture.session_editing(&[(10, "ours")]);
    fixture.open_prs(&[
        (7, branch.as_str(), "", "OPEN"),
        (12, "agent/other", &other, "MERGED"),
    ]);

    let report = fixture.push(session);
    assert_eq!(report["pr_overlaps"], serde_json::json!([]), "{report}");
    assert_eq!(report["pr_overlaps_unknown"], serde_json::json!([]));
    assert!(overlap_advice(&fixture.status()).is_empty());
}

/// No local ref and no readable `gh` diff: the overlap is unknown, and the
/// push still succeeds.
#[test]
fn an_unreadable_pull_request_is_unknown_not_an_error() {
    let fixture = Fixture::new();
    let other = fixture.publish_other("agent/other", &[(10, "theirs")], false);
    fixture.open_prs(&[(12, "agent/other", &other, "OPEN")]);
    let (session, _) = fixture.session_editing(&[(10, "ours")]);

    let report = fixture.push(session);
    assert_eq!(report["pr_overlaps"], serde_json::json!([]), "{report}");
    assert_eq!(report["pr_overlaps_unknown"], serde_json::json!([12]));
    assert!(
        fixture.gh_log().contains("pr diff 12"),
        "{}",
        fixture.gh_log()
    );
}

/// Without a current local ref, a push reads the PR's diff through `gh`, and
/// `status` afterwards answers from the cache without calling `gh` at all.
#[test]
fn gh_diff_is_the_fallback_and_status_stays_offline() {
    let fixture = Fixture::new();
    let other = fixture.publish_other("agent/other", &[(10, "theirs")], false);
    fixture.open_prs(&[(12, "agent/other", &other, "OPEN")]);
    fixture.pr_diff(
        12,
        "diff --git a/shared.txt b/shared.txt\n--- a/shared.txt\n+++ b/shared.txt\n@@ -10 +10 @@\n-line 10\n+theirs\n",
    );
    let (session, _) = fixture.session_editing(&[(11, "ours")]);

    let report = fixture.push(session);
    assert_eq!(report["pr_overlaps"][0]["pr"], 12, "{report}");
    assert_eq!(report["pr_overlaps"][0]["conflicting_hunks"], true);

    fixture.clear_gh_log();
    assert_eq!(overlap_advice(&fixture.status()).len(), 1);
    assert_eq!(fixture.gh_log(), "", "status must not call gh");
}

/// A long listing is cut to the open-PR limit, and a push fetches at most a
/// bounded number of diffs through `gh`; the rest are unknown.
#[test]
fn the_work_per_push_is_bounded() {
    let fixture = Fixture::new();
    let prs = (100..140)
        .map(|number| (number, format!("agent/pr-{number}")))
        .collect::<Vec<_>>();
    let listing = prs
        .iter()
        .map(|(number, branch)| (*number, branch.as_str(), "0123abcd", "OPEN"))
        .collect::<Vec<_>>();
    fixture.open_prs(&listing);
    let (session, _) = fixture.session_editing(&[(10, "ours")]);

    let report = fixture.push(session);
    let unknown = report["pr_overlaps_unknown"].as_array().unwrap();
    assert_eq!(unknown.len(), 30, "listing is capped at 30 open PRs");
    assert_eq!(
        fixture.gh_log().matches("pr diff").count(),
        8,
        "{}",
        fixture.gh_log()
    );
}
