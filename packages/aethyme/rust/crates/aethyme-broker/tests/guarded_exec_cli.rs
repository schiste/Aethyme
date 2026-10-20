//! Guarded exec reports the completed command separately from its ownership audit.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::{Broker, BrokerOpenOptions};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

struct Fixture {
    _owner: tempfile::TempDir,
    state: PathBuf,
    worktree_root: PathBuf,
    worktree: PathBuf,
    session: i64,
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
}

impl Fixture {
    fn new(claimed_path: Option<&str>) -> Self {
        let owner = tempfile::tempdir().unwrap();
        let root = owner.path().canonicalize().unwrap();
        let repository = root.join("repository");
        let state = root.join("host-state");
        let worktree_root = root.join("worktrees");
        let preparation = root.join("preparation-cache");
        std::fs::create_dir(&repository).unwrap();
        std::fs::create_dir(&preparation).unwrap();
        git(&repository, &["init", "-q", "-b", "main"]);
        std::fs::write(repository.join("tracked.txt"), "original\n").unwrap();
        git(&repository, &["add", "tracked.txt"]);
        git(&repository, &["commit", "-qm", "fixture"]);
        let options = BrokerOpenOptions::default()
            .with_host_state_dir(&state)
            .with_host_cache_dir(root.join("host-cache"))
            .with_host_operation_database(state.join("host-operations.db"))
            .with_worktree_root(&worktree_root)
            .with_collaboration_state(root.join("collaboration"))
            .with_preparation_cache_root(preparation);
        let mut broker = Broker::open_with_options(&repository, options).unwrap();
        let session = broker
            .start_worktree("guarded exec CLI fixture", None)
            .unwrap();
        if let Some(path) = claimed_path {
            broker.claim_lease(session.id, path, None).unwrap();
        }
        let worktree = PathBuf::from(session.worktree_path);
        assert!(worktree.starts_with(&worktree_root));
        Self {
            _owner: owner,
            state,
            worktree_root,
            worktree,
            session: session.id,
        }
    }

    fn run(&self, script: &str, json: bool) -> Output {
        let mut command = Command::new(CLI);
        command.args(["advanced", "exec", "--session", &self.session.to_string()]);
        if json {
            command.arg("--json");
        }
        command.args(["--", "/bin/sh", "-c", script]);
        command
            .current_dir(&self.worktree)
            .env_remove("AETHYME_REPO")
            .env_remove("AETHYME_BROKER_DB")
            .env_remove("AETHYME_AGENT")
            .env("AETHYME_AGENT_PID", std::process::id().to_string())
            .env("AETHYME_HOST_STATE_DIR", &self.state)
            .env("AETHYME_WORKTREE_ROOT", &self.worktree_root)
            .output()
            .unwrap()
    }
}

fn report(output: &Output) -> serde_json::Value {
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    serde_json::from_slice(&output.stdout)
        .expect("one completed-exec report, not a second error object")
}

#[test]
fn successful_unleased_write_reports_post_execution_ownership_failure() {
    let fixture = Fixture::new(None);
    let output = fixture.run("printf generated > 'generated file.txt'", true);
    let value = report(&output);
    assert_eq!(value["exit_code"], 0);
    assert_eq!(value["command_success"], true);
    assert_eq!(value["ownership_ok"], false);
    assert_eq!(value["outcome"], "ownership_failed");
    assert_eq!(value["command_ran"], true);
    assert_eq!(value["edits_remain"], true);
    assert_eq!(
        value["outside_lease_paths"],
        serde_json::json!(["generated file.txt"])
    );
    assert_eq!(
        value["next_actions"],
        serde_json::json!([format!(
            "aethyme broker advanced leases claim 'generated file.txt' --session {}",
            fixture.session
        )])
    );
    assert_eq!(
        std::fs::read(fixture.worktree.join("generated file.txt")).unwrap(),
        b"generated"
    );
}

#[test]
fn failed_unleased_write_preserves_both_failure_outcomes() {
    let fixture = Fixture::new(None);
    let value = report(&fixture.run("printf generated > generated.txt; exit 7", true));
    assert_eq!(value["exit_code"], 7);
    assert_eq!(value["command_success"], false);
    assert_eq!(value["ownership_ok"], false);
    assert_eq!(value["outcome"], "command_and_ownership_failed");
    assert_eq!(
        value["changed_paths_remaining"],
        serde_json::json!(["generated.txt"])
    );
    assert!(!value["next_actions"].as_array().unwrap().is_empty());
    assert!(fixture.worktree.join("generated.txt").exists());
}

#[test]
fn failing_command_editing_claimed_preexisting_dirty_path_is_not_an_ownership_failure() {
    let fixture = Fixture::new(Some("tracked.txt"));
    std::fs::write(fixture.worktree.join("tracked.txt"), "already dirty\n").unwrap();
    let output = fixture.run("printf changed > tracked.txt; exit 7", true);
    let value = report(&output);
    assert_eq!(value["exit_code"], 7);
    assert_eq!(value["command_success"], false);
    assert_eq!(value["ownership_ok"], true);
    assert_eq!(value["outcome"], "command_failed");
    assert_eq!(
        value["modified_preexisting_dirty_paths"],
        serde_json::json!(["tracked.txt"])
    );
    assert_eq!(value["outside_lease_paths"], serde_json::json!([]));
    assert_eq!(value["next_actions"], serde_json::json!([]));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("outside this session's ownership"));
}

#[test]
fn human_output_says_command_already_ran_and_prints_quoted_claim_action() {
    let fixture = Fixture::new(None);
    let output = fixture.run("printf generated > 'generated file.txt'", false);
    assert_eq!(output.status.code(), Some(1));
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("command passed (0)"), "{text}");
    assert!(
        text.contains("command already ran and its changes remain"),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "aethyme broker advanced leases claim 'generated file.txt' --session {}",
            fixture.session
        )),
        "{text}"
    );
    assert!(fixture.worktree.join("generated file.txt").exists());
}

#[test]
fn json_success_routes_child_output_to_stderr_and_keeps_one_report() {
    let fixture = Fixture::new(None);
    let output = fixture.run("printf CHILD_STDOUT; printf CHILD_STDERR >&2", true);
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("one JSON report without child output");
    assert_eq!(value["outcome"], "passed");
    assert_eq!(value["exit_code"], 0);
    assert_eq!(value["ownership_ok"], true);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("CHILD_STDOUT"), "{stderr}");
    assert!(stderr.contains("CHILD_STDERR"), "{stderr}");
    assert!(
        value["command"]
            .as_array()
            .unwrap()
            .iter()
            .any(|arg| arg.as_str().is_some_and(|arg| arg.contains("CHILD_STDOUT"))),
        "the report intentionally retains the original command arguments"
    );
}

#[test]
fn json_failure_preserves_both_child_streams_without_contaminating_report() {
    let fixture = Fixture::new(None);
    let output = fixture.run(
        "printf FAILED_STDOUT; printf FAILED_STDERR >&2; exit 7",
        true,
    );
    let value = report(&output);
    assert_eq!(value["outcome"], "command_failed");
    assert_eq!(value["exit_code"], 7);
    assert_eq!(value["ownership_ok"], true);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("FAILED_STDOUT"), "{stderr}");
    assert!(stderr.contains("FAILED_STDERR"), "{stderr}");
    assert!(
        value["command"].as_array().unwrap().iter().any(|arg| arg
            .as_str()
            .is_some_and(|arg| arg.contains("FAILED_STDOUT"))),
        "the report intentionally retains the original command arguments"
    );
}

#[test]
fn text_mode_preserves_inherited_child_stdout() {
    let fixture = Fixture::new(None);
    let output = fixture.run(
        "printf TEXT_CHILD_STDOUT; printf TEXT_CHILD_STDERR >&2",
        false,
    );
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains("TEXT_CHILD_STDOUT"), "{stdout}");
    assert!(stderr.contains("TEXT_CHILD_STDERR"), "{stderr}");
    assert!(!stderr.contains("TEXT_CHILD_STDOUT"), "{stderr}");
}

#[test]
fn ownership_report_preserves_literal_quotes_arrows_tabs_and_newlines() {
    let fixture = Fixture::new(None);
    let path = "quote\" -> tab\tline\n.txt";
    let script = format!("printf generated > '{path}'");
    let output = fixture.run(&script, true);
    let value = report(&output);
    assert_eq!(value["outside_lease_paths"], serde_json::json!([path]));
    assert_eq!(value["new_untracked_paths"], serde_json::json!([path]));
    assert_eq!(value["outcome"], "ownership_failed");
    assert_eq!(
        std::fs::read(fixture.worktree.join(path)).unwrap(),
        b"generated"
    );
}
