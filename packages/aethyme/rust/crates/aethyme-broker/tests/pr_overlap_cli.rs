//! Overlap between a session's change and the repository's open pull requests.
//!
//! Each case publishes a real "other" branch to a local bare `origin` behind a
//! GitHub-shaped remote, so the open PR's change is read from its fetched
//! remote-tracking ref, and a fake `gh` answers `pr list` and `pr diff` from
//! files the test writes. Nothing reaches GitHub.
#![cfg(unix)]

mod common;

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

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
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
        Self::with_tempdir(tempfile::tempdir().unwrap())
    }

    fn new_with_shell_special_path() -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("aethyme pr overlap ' fixture ")
            .tempdir()
            .unwrap();
        Self::with_tempdir(tmp)
    }

    fn with_tempdir(tmp: tempfile::TempDir) -> Self {
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
        let ssh_command = shell_quote(bin.join("ssh").to_str().unwrap());
        git(&repo, &["config", "core.sshCommand", &ssh_command]);
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
        self.broker_with_root().0
    }

    fn broker_with_root(&self) -> (Broker, PathBuf) {
        let broker = Broker::open(&self.repo)
            .unwrap()
            .with_host_operation_database(self.tmp.path().join("host-state/operations.db"));
        let repository_key = broker.worktree_root_plan().unwrap().repository_key;
        let root = self
            .tmp
            .path()
            .join("host-state/worktrees")
            .join(repository_key);
        (broker.with_worktree_root(root.clone()), root)
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
        self.run_from(&self.repo, args)
    }

    fn run_from(&self, cwd: &Path, args: &[&str]) -> Output {
        common::broker_cli(CLI, args)
            .current_dir(cwd)
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

#[test]
fn fixture_ssh_command_quotes_temp_paths_with_spaces_and_apostrophes() {
    let fixture = Fixture::new_with_shell_special_path();
    let path = fixture.tmp.path().to_string_lossy();
    assert!(
        path.contains(' '),
        "fixture path must contain a space: {path}"
    );
    assert!(
        path.contains('\''),
        "fixture path must contain an apostrophe: {path}"
    );

    let local_head = git_output(&fixture.repo, &["rev-parse", "HEAD"]);
    let remote_head = git_output(&fixture.remote, &["rev-parse", "refs/heads/main"]);
    assert_eq!(
        remote_head, local_head,
        "the initial push must reach the fixture remote"
    );
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

#[test]
fn inherited_worktree_root_cannot_redirect_fixture_direct_and_cli_worktrees() {
    const REPORT_ENV: &str = "AETHYME_TEST_POISONED_WORKTREE_REPORT";

    if let Some(report_path) = std::env::var_os(REPORT_ENV) {
        let poison_root = PathBuf::from(
            std::env::var_os("AETHYME_WORKTREE_ROOT")
                .expect("the child test process must inherit a poisoned root"),
        );
        let fixture = Fixture::new();
        let (mut broker, owned_root) = fixture.broker_with_root();
        let direct = broker
            .start_worktree("direct fixture worktree", None)
            .unwrap();
        let direct_path = PathBuf::from(direct.worktree_path);

        let cli = json(&fixture.run(&["start", "--task", "CLI fixture worktree", "--json"]));
        assert!(cli.get("planned_path_pr_overlaps").is_none());
        assert_eq!(fixture.gh_log(), "", "a start without --path stays offline");
        assert_eq!(
            broker.store().meta_get("pr_overlap.open_prs").unwrap(),
            None,
            "a start without --path must not create or refresh the PR cache"
        );
        let cli_path = PathBuf::from(cli["worktree_path"].as_str().unwrap());
        assert!(
            !poison_root.exists(),
            "the poisoned root must remain unused"
        );
        let canonical_owned_root = owned_root.canonicalize().unwrap();
        let canonical_direct_path = direct_path.canonicalize().unwrap();
        let canonical_cli_path = cli_path.canonicalize().unwrap();
        assert!(canonical_direct_path.starts_with(&canonical_owned_root));
        assert!(canonical_cli_path.starts_with(&canonical_owned_root));

        drop(broker);
        drop(fixture);
        let report = serde_json::json!({
            "owned_root": canonical_owned_root,
            "direct_path": canonical_direct_path,
            "cli_path": canonical_cli_path,
            "direct_removed": !direct_path.exists(),
            "cli_removed": !cli_path.exists(),
            "poison_untouched": !poison_root.exists(),
        });
        std::fs::write(report_path, serde_json::to_vec(&report).unwrap()).unwrap();
        return;
    }

    let parent = tempfile::tempdir().unwrap();
    let poison_root = parent.path().join("poisoned-worktrees");
    let report_path = parent.path().join("result.json");
    let host_state = parent.path().join("private-host-state");
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "inherited_worktree_root_cannot_redirect_fixture_direct_and_cli_worktrees",
            "--nocapture",
        ])
        .env("AETHYME_WORKTREE_ROOT", &poison_root)
        .env("AETHYME_HOST_STATE_DIR", host_state)
        .env(REPORT_ENV, &report_path)
        .env_remove("XDG_STATE_HOME")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child test failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(report_path).unwrap()).unwrap();
    let owned_root = PathBuf::from(report["owned_root"].as_str().unwrap());
    let direct_path = PathBuf::from(report["direct_path"].as_str().unwrap());
    let cli_path = PathBuf::from(report["cli_path"].as_str().unwrap());
    assert!(direct_path.starts_with(&owned_root));
    assert!(cli_path.starts_with(&owned_root));
    assert!(report["direct_removed"].as_bool().unwrap());
    assert!(report["cli_removed"].as_bool().unwrap());
    assert!(report["poison_untouched"].as_bool().unwrap());
    assert!(!poison_root.exists());
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

fn cache_one_open_pr(fixture: &Fixture) {
    let other = fixture.publish_other("agent/other", &[(10, "theirs")], true);
    fixture.open_prs(&[(12, "agent/other", &other, "OPEN")]);
    let (session, _) = fixture.session_editing(&[(20, "ours")]);
    fixture.push(session);
    fixture.clear_gh_log();
}

#[test]
fn start_path_reports_cached_pr_path_overlap_without_network_or_graph() {
    let fixture = Fixture::new();
    cache_one_open_pr(&fixture);

    let output = fixture.run(&[
        "start",
        "--task",
        "work on a planned path",
        "--path",
        "shared.txt",
        "--json",
    ]);
    let report = json(&output);
    let check = &report["planned_path_pr_overlaps"];
    assert_eq!(check["cache_available"], true, "{report}");
    assert_eq!(check["checked_open_pr_limit"], 30, "{report}");
    assert_eq!(check["overlaps"].as_array().unwrap().len(), 1, "{report}");
    assert_eq!(check["overlaps"][0]["pr"], 12, "{report}");
    assert_eq!(
        check["overlaps"][0]["files"],
        serde_json::json!(["shared.txt"])
    );
    assert_eq!(check["unknown_prs"], serde_json::json!([]));
    assert!(check["listed_at_ms"].is_number(), "{report}");
    assert!(
        report["scope_overlaps_warning"].is_string(),
        "fixture has no graph"
    );
    assert_eq!(
        fixture.gh_log(),
        "",
        "start must only read the local PR cache"
    );

    let human_fixture = Fixture::new();
    cache_one_open_pr(&human_fixture);
    let output = human_fixture.run(&[
        "start",
        "--task",
        "another planned path task",
        "--path",
        "shared.txt",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Heads-up: open PR #12"), "{stdout}");
    assert!(stdout.contains("path-only"), "{stdout}");
    assert_eq!(
        human_fixture.gh_log(),
        "",
        "human start must stay offline too"
    );

    // A recursive lease for `src/` includes a nested changed file, but not a
    // neighboring path whose name merely starts with the same letters.
    let prefix_fixture = Fixture::new();
    cache_one_open_pr(&prefix_fixture);
    let mut broker = prefix_fixture.broker();
    let listing: serde_json::Value = serde_json::from_str(
        &broker
            .store()
            .meta_get("pr_overlap.open_prs")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    let head_oid = listing["prs"][0]["head_oid"].as_str().unwrap();
    broker
        .store()
        .meta_set(
            "pr_overlap.ranges.12",
            &serde_json::json!({
                "head_oid": head_oid,
                "base_oid": git_output(&prefix_fixture.repo, &["rev-parse", "origin/main"]),
                "ranges": {
                    "src/child.rs": [[1, 1]],
                    "src-neighbor/child.rs": [[1, 1]],
                }
            })
            .to_string(),
        )
        .unwrap();
    drop(broker);
    prefix_fixture.clear_gh_log();
    let report = json(&prefix_fixture.run(&[
        "start",
        "--task",
        "work on the source subtree",
        "--path",
        "src/",
        "--json",
    ]));
    let check = &report["planned_path_pr_overlaps"];
    assert_eq!(check["overlaps"].as_array().unwrap().len(), 1, "{report}");
    assert_eq!(
        check["overlaps"][0]["files"],
        serde_json::json!(["src/child.rs"]),
        "a directory prefix must not match src-neighbor"
    );
    assert_eq!(
        prefix_fixture.gh_log(),
        "",
        "path matching must stay offline"
    );

    // Reuse an existing session and list its own branch as an open PR with a
    // deliberately overlapping cached range. The start-time advisor must
    // exclude that branch just as the push-time line advisor does.
    let own_fixture = Fixture::new();
    cache_one_open_pr(&own_fixture);
    let started = json(&own_fixture.run(&["start", "--task", "own branch", "--json"]));
    let branch = started["branch"].as_str().unwrap().to_string();
    let worktree = PathBuf::from(started["worktree_path"].as_str().unwrap());
    let head_oid = git_output(&worktree, &["rev-parse", "HEAD"]);
    let mut broker = own_fixture.broker();
    let mut listing: serde_json::Value = serde_json::from_str(
        &broker
            .store()
            .meta_get("pr_overlap.open_prs")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    listing["prs"][0]["head"] = serde_json::json!(branch);
    listing["prs"][0]["head_oid"] = serde_json::json!(head_oid);
    broker
        .store()
        .meta_set("pr_overlap.open_prs", &listing.to_string())
        .unwrap();
    broker
        .store()
        .meta_set(
            "pr_overlap.ranges.12",
            &serde_json::json!({
                "head_oid": head_oid,
                "base_oid": git_output(&own_fixture.repo, &["rev-parse", "origin/main"]),
                "ranges": {"shared.txt": [[1, 1]]}
            })
            .to_string(),
        )
        .unwrap();
    drop(broker);
    own_fixture.clear_gh_log();
    let report = json(&own_fixture.run_from(
        &worktree,
        &[
            "start",
            "--adopt",
            "--reuse",
            "--take-over",
            "--task",
            "reuse own branch",
            "--path",
            "shared.txt",
            "--json",
        ],
    ));
    assert_eq!(
        report["branch"], branch,
        "the reused session keeps its branch"
    );
    assert_eq!(report["planned_path_pr_overlaps"]["cache_available"], true);
    assert_eq!(
        report["planned_path_pr_overlaps"]["overlaps"],
        serde_json::json!([])
    );
    assert_eq!(
        report["planned_path_pr_overlaps"]["unknown_prs"],
        serde_json::json!([])
    );
    assert_eq!(
        own_fixture.gh_log(),
        "",
        "own-PR filtering must stay offline"
    );
}

#[test]
fn start_path_discloses_missing_cached_pr_listing_without_network() {
    let fixture = Fixture::new();
    let output = fixture.run(&[
        "start",
        "--task",
        "work on a planned path",
        "--path",
        "shared.txt",
        "--json",
    ]);
    let report = json(&output);
    let check = &report["planned_path_pr_overlaps"];
    assert_eq!(check["cache_available"], false, "{report}");
    assert!(
        check["warning"]
            .as_str()
            .unwrap()
            .contains("no readable cached")
    );
    assert_eq!(check["overlaps"], serde_json::json!([]));
    assert_eq!(fixture.gh_log(), "", "start must not refresh the listing");
}

#[test]
fn start_path_discloses_stale_or_wrong_base_listing_and_unknown_pr_paths() {
    for changed in [
        "stale-listing",
        "future-listing",
        "wrong-base",
        "unknown-pr-paths",
        "old-range-format",
        "changed-base-oid",
    ] {
        let fixture = Fixture::new();
        cache_one_open_pr(&fixture);
        let mut broker = fixture.broker();
        let mut listing: serde_json::Value = serde_json::from_str(
            &broker
                .store()
                .meta_get("pr_overlap.open_prs")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        if changed == "stale-listing" {
            listing["fetched_at_ms"] = serde_json::json!(0);
        } else if changed == "future-listing" {
            listing["fetched_at_ms"] = serde_json::json!(i64::MAX);
        } else if changed == "old-range-format" || changed == "changed-base-oid" {
            let mut ranges: serde_json::Value = serde_json::from_str(
                &broker
                    .store()
                    .meta_get("pr_overlap.ranges.12")
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            if changed == "old-range-format" {
                ranges.as_object_mut().unwrap().remove("base_oid");
            } else {
                ranges["base_oid"] = serde_json::json!("0".repeat(40));
            }
            broker
                .store()
                .meta_set("pr_overlap.ranges.12", &ranges.to_string())
                .unwrap();
        } else if changed == "wrong-base" {
            listing["base"] = serde_json::json!("develop");
        } else {
            listing["prs"][0]["head_oid"] = serde_json::json!("f".repeat(40));
        }
        broker
            .store()
            .meta_set("pr_overlap.open_prs", &listing.to_string())
            .unwrap();
        drop(broker);
        fixture.clear_gh_log();

        let output = fixture.run(&[
            "start",
            "--task",
            "work on a planned path",
            "--path",
            "shared.txt",
            "--json",
        ]);
        let report = json(&output);
        let check = &report["planned_path_pr_overlaps"];
        if changed == "stale-listing" {
            assert_eq!(check["cache_available"], false, "{report}");
            assert!(
                check["warning"]
                    .as_str()
                    .unwrap()
                    .contains("older than 24 hours")
            );
        } else if changed == "future-listing" {
            assert_eq!(check["cache_available"], false, "{report}");
            assert!(
                check["warning"]
                    .as_str()
                    .unwrap()
                    .contains("dated in the future")
            );
        } else if changed == "wrong-base" {
            assert_eq!(check["cache_available"], false, "{report}");
            assert!(
                check["warning"]
                    .as_str()
                    .unwrap()
                    .contains("cached PR listing is for base develop"),
                "{report}"
            );
        } else {
            assert_eq!(check["cache_available"], true, "{report}");
            assert_eq!(check["unknown_prs"], serde_json::json!([12]), "{report}");
            assert_eq!(check["overlaps"], serde_json::json!([]), "{report}");
        }
        assert_eq!(fixture.gh_log(), "", "start must not fetch missing PR data");
    }

    // The PR ref exists locally and its listing head is current, but no
    // changed-path ranges have been cached. Start must report unknown instead
    // of deriving those paths with a potentially expensive Git diff.
    let fixture = Fixture::new();
    let head = fixture.publish_other("agent/other", &[(10, "theirs")], true);
    let fetched_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let listing = serde_json::json!({
        "fetched_at_ms": fetched_at_ms,
        "base": "main",
        "prs": [{
            "number": 12,
            "url": "https://github.com/acme/project/pull/12",
            "head": "agent/other",
            "head_oid": head,
        }],
    });
    let mut broker = fixture.broker();
    broker
        .store()
        .meta_set("pr_overlap.open_prs", &listing.to_string())
        .unwrap();
    assert_eq!(
        broker.store().meta_get("pr_overlap.ranges.12").unwrap(),
        None,
        "fixture keeps a valid local PR ref but no cached changed paths"
    );
    drop(broker);
    fixture.clear_gh_log();
    let report = json(&fixture.run(&[
        "start",
        "--task",
        "work on a planned path",
        "--path",
        "shared.txt",
        "--json",
    ]));
    let check = &report["planned_path_pr_overlaps"];
    assert_eq!(check["cache_available"], true, "{report}");
    assert_eq!(check["unknown_prs"], serde_json::json!([12]), "{report}");
    assert_eq!(check["overlaps"], serde_json::json!([]), "{report}");
    assert_eq!(
        fixture.gh_log(),
        "",
        "uncached paths must remain unknown without GitHub fallback"
    );

    // A malformed/older cache may contain more PRs than the listing command's
    // limit. The advisory inspects only the first MAX_OPEN_PRS entries and
    // explicitly reports that additional open PRs may have been omitted.
    let fixture = Fixture::new();
    let fetched_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let prs = (100..=130)
        .map(|number| {
            serde_json::json!({
                "number": number,
                "url": format!("https://github.com/acme/project/pull/{number}"),
                "head": format!("agent/pr-{number}"),
                "head_oid": "f".repeat(40),
            })
        })
        .collect::<Vec<_>>();
    let listing = serde_json::json!({
        "fetched_at_ms": fetched_at_ms,
        "base": "main",
        "prs": prs,
    });
    let mut broker = fixture.broker();
    broker
        .store()
        .meta_set("pr_overlap.open_prs", &listing.to_string())
        .unwrap();
    drop(broker);
    fixture.clear_gh_log();

    let report = json(&fixture.run(&[
        "start",
        "--task",
        "work on a planned path",
        "--path",
        "shared.txt",
        "--json",
    ]));
    let check = &report["planned_path_pr_overlaps"];
    assert_eq!(check["cache_available"], true, "{report}");
    assert_eq!(check["checked_open_pr_limit"], 30, "{report}");
    assert_eq!(check["listing_may_be_truncated"], true, "{report}");
    let unknown = check["unknown_prs"].as_array().unwrap();
    assert_eq!(unknown.len(), 30, "{report}");
    assert_eq!(unknown.first(), Some(&serde_json::json!(100)), "{report}");
    assert_eq!(unknown.last(), Some(&serde_json::json!(129)), "{report}");
    assert!(!unknown.contains(&serde_json::json!(130)), "{report}");
    assert_eq!(check["overlaps"], serde_json::json!([]), "{report}");
    assert_eq!(
        fixture.gh_log(),
        "",
        "the cached-listing check stays offline"
    );
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

/// The overlap check must not treat a valid JSON prefix as the whole listing
/// once the broker's per-stream capture bound was exceeded.
#[test]
fn a_truncated_open_pr_listing_is_reported_as_unavailable() {
    const CAPTURE_LIMIT: usize = 16 * 1024 * 1024;
    let fixture = Fixture::new();
    let valid_listing = r#"[{"number":12,"url":"https://github.com/acme/project/pull/12","state":"OPEN","headRefName":"agent/other","headRefOid":"0123abcd"}]"#;
    let mut listing = Vec::with_capacity(CAPTURE_LIMIT + 1);
    listing.extend_from_slice(valid_listing.as_bytes());
    listing.resize(CAPTURE_LIMIT + 1, b' ');
    std::fs::write(fixture.tmp.path().join("open-prs.json"), listing).unwrap();
    let (session, _) = fixture.session_editing(&[(10, "ours")]);

    let report = fixture.push(session);
    assert_eq!(report["pr_overlaps"], serde_json::json!([]), "{report}");
    assert_eq!(
        report["pr_overlaps_unknown"],
        serde_json::json!([]),
        "{report}"
    );
    assert_eq!(
        report["pr_overlap_listing_unavailable"], true,
        "the valid JSON prefix is not a complete listing: {report}"
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
    let start = json(&fixture.run(&[
        "start",
        "--task",
        "plan shared path",
        "--path",
        "shared.txt",
        "--json",
    ]));
    assert_eq!(
        start["planned_path_pr_overlaps"]["unknown_prs"],
        serde_json::json!([12])
    );
    assert_eq!(
        fixture.gh_log(),
        "",
        "start must not fetch provider-only ranges"
    );
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

#[test]
fn recorded_status_warning_is_invalidated_when_push_inputs_move() {
    for changed in [
        "session-head",
        "baseline",
        "baseline-ref",
        "listing",
        "expired",
        "future",
    ] {
        let fixture = Fixture::new();
        let other = fixture.publish_other("agent/other", &[(2, "theirs")], true);
        fixture.open_prs(&[(12, "agent/other", &other, "OPEN")]);
        let (id, _) = fixture.session_editing(&[(3, "ours")]);
        fixture.push(id);
        assert_eq!(overlap_advice(&fixture.status()).len(), 1);
        let mut broker = fixture.broker();
        match changed {
            "session-head" => {
                let session = broker.store().session(id).unwrap();
                git(
                    Path::new(&session.worktree_path),
                    &["commit", "--allow-empty", "-qm", "new head"],
                );
            }
            "baseline" => git(
                &fixture.repo,
                &["update-ref", "refs/remotes/origin/main", &other],
            ),
            "baseline-ref" => {
                git(
                    &fixture.repo,
                    &["update-ref", "refs/remotes/origin/other", "HEAD"],
                );
                git(
                    &fixture.repo,
                    &[
                        "symbolic-ref",
                        "refs/remotes/origin/HEAD",
                        "refs/remotes/origin/other",
                    ],
                );
            }
            "listing" | "expired" | "future" => {
                let mut listing: serde_json::Value = serde_json::from_str(
                    &broker
                        .store()
                        .meta_get("pr_overlap.open_prs")
                        .unwrap()
                        .unwrap(),
                )
                .unwrap();
                if changed == "expired" {
                    listing["fetched_at_ms"] = serde_json::json!(0);
                } else if changed == "future" {
                    listing["fetched_at_ms"] = serde_json::json!(i64::MAX);
                } else {
                    listing["prs"][0]["head_oid"] = serde_json::json!("f".repeat(40));
                }
                let raw = listing.to_string();
                broker
                    .store()
                    .meta_set("pr_overlap.open_prs", &raw)
                    .unwrap();
                if changed == "expired" || changed == "future" {
                    // Keep the listing binding matching: this case must fail
                    // freshness, not merely the listing-identity check.
                    let key = format!("pr_overlap.checked.{id}");
                    let mut recorded: serde_json::Value =
                        serde_json::from_str(&broker.store().meta_get(&key).unwrap().unwrap())
                            .unwrap();
                    recorded["listing"] = serde_json::json!(raw);
                    broker
                        .store()
                        .meta_set(&key, &recorded.to_string())
                        .unwrap();
                }
            }
            _ => unreachable!(),
        }
        fixture.clear_gh_log();
        let status = fixture.status();
        assert!(
            overlap_advice(&status).is_empty(),
            "obsolete warning survived {changed}: {status}"
        );
        assert_eq!(fixture.gh_log(), "", "routine status must stay offline");
    }
}

#[test]
fn push_refreshes_a_future_dated_pr_listing() {
    let fixture = Fixture::new();
    let other = fixture.publish_other("agent/other", &[(10, "theirs")], true);
    fixture.open_prs(&[(12, "agent/other", &other, "OPEN")]);
    let (session, _) = fixture.session_editing(&[(20, "ours")]);
    fixture.push(session);
    let mut broker = fixture.broker();
    let mut listing: serde_json::Value = serde_json::from_str(
        &broker
            .store()
            .meta_get("pr_overlap.open_prs")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    listing["fetched_at_ms"] = serde_json::json!(i64::MAX);
    broker
        .store()
        .meta_set("pr_overlap.open_prs", &listing.to_string())
        .unwrap();
    drop(broker);
    fixture.clear_gh_log();
    fixture.push(session);
    assert!(
        fixture.gh_log().contains("pr list"),
        "future listing must be refreshed: {}",
        fixture.gh_log()
    );
    let mut broker = fixture.broker();
    let refreshed: serde_json::Value = serde_json::from_str(
        &broker
            .store()
            .meta_get("pr_overlap.open_prs")
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert!(refreshed["fetched_at_ms"].as_i64().unwrap() < i64::MAX);
}
