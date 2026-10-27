//! Guarded exec reports the completed command separately from its ownership audit.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::{Broker, BrokerOpenOptions, GitRepo};

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
        let empty_hooks = root.join("empty-hooks");
        std::fs::create_dir(&empty_hooks).unwrap();
        git(&repository, &["init", "-q", "--template=", "-b", "main"]);
        let hooks_path = empty_hooks.to_str().unwrap();
        git(
            &repository,
            &["config", "--local", "core.hooksPath", hooks_path],
        );
        git(
            &repository,
            &["config", "--local", "commit.gpgsign", "false"],
        );
        git(
            &repository,
            &["config", "--local", "core.fsmonitor", "false"],
        );
        git(
            &repository,
            &["config", "--local", "core.untrackedCache", "false"],
        );
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

    fn broker(&self) -> Broker {
        let root = self._owner.path().canonicalize().unwrap();
        let options = BrokerOpenOptions::default()
            .with_host_state_dir(&self.state)
            .with_host_cache_dir(root.join("host-cache"))
            .with_host_operation_database(self.state.join("host-operations.db"))
            .with_worktree_root(&self.worktree_root)
            .with_collaboration_state(root.join("collaboration"))
            .with_preparation_cache_root(root.join("preparation-cache"));
        Broker::open_with_options(&root.join("repository"), options).unwrap()
    }

    fn run(&self, script: &str, json: bool) -> Output {
        self.run_with_options(script, json, false)
    }

    fn run_with_options(&self, script: &str, json: bool, non_interactive: bool) -> Output {
        let mut command = Command::new(CLI);
        command.args(["advanced", "exec", "--session", &self.session.to_string()]);
        if non_interactive {
            command.arg("--non-interactive");
        }
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
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn non_interactive_json_success_keeps_complete_report_and_routes_both_child_streams_to_stderr() {
    let fixture = Fixture::new(None);
    let output = fixture.run_with_options(
        "printf OPT_IN_CHILD_STDOUT; printf OPT_IN_CHILD_STDERR >&2",
        true,
        true,
    );
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("one JSON report without child output");
    assert_eq!(value["outcome"], "passed");
    assert_eq!(value["exit_code"], 0);
    assert_eq!(value["ownership_ok"], true);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("OPT_IN_CHILD_STDOUT"), "{stderr}");
    assert!(stderr.contains("OPT_IN_CHILD_STDERR"), "{stderr}");
    assert!(value["command_ran"].as_bool().unwrap_or(false));
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

#[test]
fn guard_status_snapshots_match_existing_queries_for_renames_and_named_untracked_files() {
    let fixture = Fixture::new(None);
    git(
        &fixture.worktree,
        &["mv", "tracked.txt", "renamed -> tracked.txt"],
    );
    let preexisting_untracked = "preexisting untracked name.txt";
    std::fs::write(fixture.worktree.join(preexisting_untracked), "before\\n").unwrap();

    let repo = GitRepo::discover(&fixture.worktree).unwrap();
    let mut before_dirty = repo.dirty_paths().unwrap();
    before_dirty.sort();
    before_dirty.dedup();
    let mut before_untracked = repo.untracked_paths().unwrap();
    before_untracked.sort();
    before_untracked.dedup();
    assert_eq!(
        before_dirty,
        vec!["preexisting untracked name.txt", "renamed -> tracked.txt"]
    );
    assert_eq!(before_untracked, vec![preexisting_untracked]);

    let output = fixture.run(
        "printf changed >> 'renamed -> tracked.txt'; printf generated > 'new generated name.txt'",
        true,
    );
    let value = report(&output);

    let mut after_dirty = repo.dirty_paths().unwrap();
    after_dirty.sort();
    after_dirty.dedup();
    let mut after_untracked = repo.untracked_paths().unwrap();
    after_untracked.sort();
    after_untracked.dedup();
    let new_untracked: Vec<_> = after_untracked
        .iter()
        .filter(|path| !before_untracked.contains(path))
        .cloned()
        .collect();

    assert_eq!(value["before_dirty_paths"], serde_json::json!(before_dirty));
    assert_eq!(value["after_dirty_paths"], serde_json::json!(after_dirty));
    assert_eq!(
        value["new_untracked_paths"],
        serde_json::json!(new_untracked)
    );
    assert_eq!(
        value["modified_preexisting_dirty_paths"],
        serde_json::json!(["renamed -> tracked.txt"])
    );
}

#[test]
fn guarded_exec_does_not_refresh_diagnostics_or_implicit_leases() {
    let fixture = Fixture::new(Some("tracked.txt"));
    let mut broker = fixture.broker();
    let other = broker.start_worktree("overlapping owner", None).unwrap();
    // Fixture setup records both owners without pre-classifying their pair.
    broker
        .store()
        .claim_lease(other.id, "tracked.txt", None)
        .unwrap();
    std::fs::write(fixture.worktree.join("tracked.txt"), "acting edit\n").unwrap();
    std::fs::write(
        Path::new(&other.worktree_path).join("tracked.txt"),
        "other edit\n",
    )
    .unwrap();
    let overlaps = broker.refresh_leases().unwrap();
    let pairs = broker.overlap_pairs_snapshot(&overlaps);
    assert_eq!(pairs.len(), 1);
    assert!(
        pairs[0].classified,
        "fixture must start with a cached diagnosis"
    );
    let pairs_before = pairs;
    let mut implicit_before = broker
        .store()
        .active_leases()
        .unwrap()
        .into_iter()
        .filter(|lease| {
            lease.session_id == fixture.session && lease.kind == aethyme_broker::LeaseKind::Implicit
        })
        .map(|lease| (lease.id, lease.path, lease.created_at, lease.expires_at))
        .collect::<Vec<_>>();
    implicit_before.sort();
    let refreshed_at_before = broker.store().meta_get("leases.refreshed_at_ms").unwrap();
    drop(broker);

    let output = fixture.run("true", true);
    assert!(output.status.success(), "{output:?}");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["ownership_ok"], true);
    let mut broker = fixture.broker();
    assert_eq!(broker.overlap_pairs_snapshot(&overlaps), pairs_before);
    let mut implicit_after = broker
        .store()
        .active_leases()
        .unwrap()
        .into_iter()
        .filter(|lease| {
            lease.session_id == fixture.session && lease.kind == aethyme_broker::LeaseKind::Implicit
        })
        .map(|lease| (lease.id, lease.path, lease.created_at, lease.expires_at))
        .collect::<Vec<_>>();
    implicit_after.sort();
    assert_eq!(implicit_after, implicit_before);
    assert_eq!(
        broker.store().meta_get("leases.refreshed_at_ms").unwrap(),
        refreshed_at_before
    );

    // An explicit refresh still updates the diagnostic snapshot. This is a
    // positive control proving the fixture has an analyzable overlap.
    let overlaps = broker.refresh_leases().unwrap();
    assert!(broker.overlap_pairs_snapshot(&overlaps)[0].classified);
}
