//! `[session] guidance`: the working agreement `broker start`, `--reuse` and
//! `--adopt` hand an agent, with a built-in default, a per-repository
//! replacement, an off switch, and a fallback that never fails a session.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod common;

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

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
    String::from_utf8_lossy(&output.stdout).into_owned()
}

struct Fixture {
    tmp: tempfile::TempDir,
    repo: PathBuf,
    host: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    std::fs::write(
        repo.join(".gitignore"),
        "/.aethyme/*\n!/.aethyme/config.toml\n",
    )
    .unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    std::fs::create_dir_all(repo.join(".aethyme")).unwrap();
    std::fs::write(
        repo.join(".aethyme/broker.toml"),
        "[retention]\nartifact_sweep_budget_ms = 0\nstartup_budget_ms = 5\n",
    )
    .unwrap();
    let host = tmp.path().join("host");
    std::fs::create_dir_all(&host).unwrap();
    Fixture {
        repo: repo.canonicalize().unwrap(),
        host,
        tmp,
    }
}

impl Fixture {
    fn configure(&self, text: &str) {
        std::fs::write(self.repo.join(".aethyme/config.toml"), text).unwrap();
    }

    /// Commit `text` as the config, publish it to a bare origin as the
    /// default branch, and point `origin/HEAD` at it.
    fn publish_config(&self, text: &str) {
        self.configure(text);
        git(&self.repo, &["add", ".aethyme/config.toml"]);
        git(&self.repo, &["commit", "-qm", "config"]);
        let origin = self.tmp.path().join("origin.git");
        if !origin.exists() {
            git(self.tmp.path(), &["init", "-q", "--bare", "origin.git"]);
            git(
                &self.repo,
                &["remote", "add", "origin", origin.to_str().unwrap()],
            );
        }
        git(&self.repo, &["push", "-q", "origin", "main"]);
        git(&self.repo, &["fetch", "-q", "origin"]);
        git(&self.repo, &["remote", "set-head", "origin", "main"]);
    }

    fn run(&self, args: &[&str]) -> Output {
        common::broker_cli(CLI, args)
            .current_dir(&self.repo)
            .env("AETHYME_HOST_STATE_DIR", &self.host)
            .env_remove("AETHYME_WORKTREE_ROOT")
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{args:?}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn start_json(&self, task: &str) -> serde_json::Value {
        serde_json::from_str(&self.ok(&["start", "--task", task, "--json"])).unwrap()
    }
}

fn lines(started: &serde_json::Value) -> Vec<String> {
    started["guidance"]
        .as_array()
        .unwrap_or_else(|| panic!("no guidance array: {started}"))
        .iter()
        .map(|line| line.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn start_shows_the_default_agreement_when_none_is_configured() {
    let fx = fixture();
    let started = fx.start_json("default agreement");
    assert_eq!(started["guidance_source"], "default", "{started}");
    let lines = lines(&started);
    assert_eq!(lines.len(), 6, "{lines:?}");
    assert!(lines.iter().any(|line| line.contains("shared component")));
    assert!(started.get("guidance_warning").is_none(), "{started}");

    let text = fx.ok(&["start", "--task", "default agreement text"]);
    assert!(
        text.contains("Working agreement:\n  1. Commit one small, coherent step at a time"),
        "{text}"
    );
    assert!(
        text.contains("\n  6. Before finishing: nothing unintended committed or pushed"),
        "{text}"
    );
}

#[test]
fn repository_guidance_replaces_the_default() {
    let fx = fixture();
    fx.configure("schema = 1\n[session]\nguidance = [\"Run the linter first.\", \"Ask before migrations.\"]\n");
    let started = fx.start_json("repository agreement");
    assert_eq!(started["guidance_source"], "repository", "{started}");
    assert_eq!(
        lines(&started),
        ["Run the linter first.", "Ask before migrations."]
    );
    let text = fx.ok(&["start", "--task", "repository agreement text"]);
    assert!(
        text.contains(
            "Working agreement:\n  1. Run the linter first.\n  2. Ask before migrations.\n"
        ),
        "{text}"
    );
}

#[test]
fn an_empty_list_turns_the_agreement_off() {
    let fx = fixture();
    fx.configure("schema = 1\n[session]\nguidance = []\n");
    let started = fx.start_json("no agreement");
    assert_eq!(started["guidance_source"], "disabled", "{started}");
    assert!(lines(&started).is_empty());
    let text = fx.ok(&["start", "--task", "no agreement text"]);
    assert!(!text.contains("Working agreement"), "{text}");
}

#[test]
fn invalid_guidance_warns_and_still_starts_with_the_default() {
    let fx = fixture();
    let seven = (1..=7).map(|n| format!("\"line {n}\"")).collect::<Vec<_>>();
    fx.configure(&format!(
        "schema = 1\n[session]\nguidance = [{}]\n",
        seven.join(", ")
    ));
    let started = fx.start_json("invalid agreement");
    assert_eq!(started["guidance_source"], "default", "{started}");
    assert_eq!(lines(&started).len(), 6);
    let warning = started["guidance_warning"]
        .as_str()
        .unwrap_or_else(|| panic!("{started}"));
    assert!(warning.contains("at most 6"), "{warning}");
    let text = fx.ok(&["start", "--task", "invalid agreement text"]);
    assert!(
        text.contains("warning: [session] guidance ignored"),
        "{text}"
    );
    assert!(text.contains("Working agreement:"), "{text}");
}

#[test]
fn the_default_names_broker_push_only_when_the_push_lane_is_on() {
    let fx = fixture();
    let without = fx.start_json("no push lane");
    assert!(!lines(&without)[0].contains("broker push"), "{without}");

    fx.publish_config("schema = 1\n[delivery]\npush_session_branches = true\n");
    let with = fx.start_json("push lane");
    assert!(lines(&with)[0].contains("aethyme broker push"), "{with}");
    assert!(lines(&with)[0].contains("draft PR"), "{with}");
}

#[test]
fn the_committed_default_branch_config_wins_over_the_working_file() {
    let fx = fixture();
    fx.publish_config("schema = 1\n[session]\nguidance = [\"Committed agreement.\"]\n");
    // An uncommitted edit in the main checkout does not change the agreement:
    // the broker reads the copy on the fetched default branch.
    fx.configure("schema = 1\n[session]\nguidance = [\"Local edit.\"]\n");
    let started = fx.start_json("committed wins");
    assert_eq!(started["guidance_source"], "repository", "{started}");
    assert_eq!(lines(&started), ["Committed agreement."]);
}

#[test]
fn reuse_and_adopt_show_the_agreement_too() {
    let fx = fixture();
    fx.configure("schema = 1\n[session]\nguidance = [\"Reuse agreement.\"]\n");
    let started = fx.start_json("first task");
    let worktree = started["worktree_path"].as_str().unwrap().to_owned();
    let session = started["id"].as_i64().unwrap().to_string();
    fx.ok(&["finish", "close", "--session", &session]);

    let adopted: serde_json::Value = serde_json::from_str(&fx.ok(&[
        "start",
        "--adopt",
        &worktree,
        "--task",
        "adopted task",
        "--json",
    ]))
    .unwrap();
    assert_eq!(lines(&adopted), ["Reuse agreement."], "{adopted}");

    let reused = fx.ok(&["start", "--reuse", &worktree, "--task", "reused task"]);
    assert!(
        reused.contains("Working agreement:\n  1. Reuse agreement."),
        "{reused}"
    );
}
