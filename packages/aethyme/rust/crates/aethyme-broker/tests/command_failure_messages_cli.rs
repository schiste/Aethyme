//! A failed broker command keeps the error it printed, redacted and capped,
//! so the failure can be explained after its terminal is gone, and `doctor`
//! lists the last day's failures with that message.

use std::path::Path;
use std::process::{Command, Output};

use aethyme_broker::{NewSession, SessionOrigin};

mod common;

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

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
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_repo(repo: &Path) {
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    git(repo, &["add", "README.md"]);
    git(repo, &["commit", "-qm", "init"]);
}

fn test_broker(repo: &Path) -> common::FixtureBroker {
    common::open_fixture_repository_path_broker(repo).unwrap()
}

fn run(repo: &Path, args: &[&str]) -> Output {
    run_with_env(repo, args, &[])
}

fn fresh_unbound_child(repo: &Path, args: &[&str]) -> Command {
    let fixture_root = repo
        .parent()
        .expect("fixture repository has an owner")
        .canonicalize()
        .expect("canonicalize fixture owner");
    let mut command = common::broker_cli_for_fixture(CLI, args, &fixture_root);
    let host_state = common::fixture_host_state_dir(&fixture_root);
    let worktree_root = fixture_root.join("worktrees");
    command
        .current_dir(repo)
        .env("AETHYME_BROKER_DB", repo.join(".aethyme/broker.db"))
        .env(
            "AETHYME_BROKER_OPEN_HOST_OPERATION_DB",
            host_state.join("host-operations.db"),
        )
        .env("AETHYME_BROKER_OPEN_HOST_STATE_EXPLICIT", "true")
        .env("AETHYME_BROKER_OPEN_WORKTREE_ROOT", worktree_root)
        .env(
            "AETHYME_BROKER_OPEN_COLLABORATION_ROOT",
            host_state.join("collaboration"),
        )
        .env_remove("AETHYME_WORKTREE_ROOT");
    command
}

fn run_with_env(repo: &Path, args: &[&str], environment: &[(&str, &str)]) -> Output {
    let broker = test_broker(repo);
    let mut command = fresh_unbound_child(repo, args);
    for (key, value) in environment {
        command.env(key, value);
    }
    drop(broker);
    command.output().unwrap()
}

/// Payloads of every `broker.command.failed` event, oldest first.
fn failed_payloads(repo: &Path) -> Vec<serde_json::Value> {
    let mut broker = test_broker(repo);
    broker
        .store()
        .events_after_filtered(0, 1000, Some("broker.command.failed"))
        .unwrap()
        .into_iter()
        .map(|event| serde_json::from_str(event.payload_json.as_deref().unwrap()).unwrap())
        .collect()
}

fn fixture() -> common::FixtureRepository {
    let fixture = common::FixtureRepository::new();
    init_repo(fixture.path());
    drop(test_broker(fixture.path()));
    fixture
}

fn last_failure_message(repo: &Path) -> String {
    let payloads = failed_payloads(repo);
    let last = payloads.last().expect("a broker.command.failed event");
    last["message"]
        .as_str()
        .unwrap_or_else(|| panic!("no message recorded: {last}"))
        .to_string()
}

#[test]
fn a_failed_command_records_the_error_it_printed() {
    let tmp = fixture();
    let output = run(
        tmp.path(),
        &["start", "--task", "t", "--base", "no-such-ref"],
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no-such-ref"), "{stderr}");

    let message = last_failure_message(tmp.path());
    assert!(
        message.contains("cannot select a safe base") && message.contains("no-such-ref"),
        "{message}"
    );
}

#[test]
fn credentials_in_the_error_are_never_stored() {
    let tmp = fixture();
    run(
        tmp.path(),
        &["start", "--task", "t", "--base", "ghp_SECRET0123456789"],
    );
    let message = last_failure_message(tmp.path());
    assert!(!message.contains("SECRET0123456789"), "{message}");
    assert!(message.ends_with("[redacted]"), "{message}");

    run(
        tmp.path(),
        &[
            "start",
            "--task",
            "t",
            "--base",
            "https://u:PW-SECRET@example.invalid/x",
        ],
    );
    let message = last_failure_message(tmp.path());
    assert!(!message.contains("PW-SECRET"), "{message}");
    assert!(
        message.contains("https://[redacted]@example.invalid/x"),
        "{message}"
    );
}

#[test]
fn a_long_error_is_capped() {
    let tmp = fixture();
    let long = "r".repeat(2000);
    run(tmp.path(), &["start", "--task", "t", "--base", &long]);
    let message = last_failure_message(tmp.path());
    assert_eq!(message.chars().count(), 501, "{message}");
    assert!(message.ends_with('…'));
}

#[test]
fn a_coordinated_failure_records_the_providers_reason() {
    let tmp = fixture();
    let mut broker = test_broker(tmp.path());
    let session = broker
        .store()
        .register_session(&NewSession {
            worktree_path: tmp.path().to_string_lossy().into_owned(),
            branch: "main".into(),
            origin: SessionOrigin::Adopted,
            task: None,
            diff_base: None,
            adoption_base: None,
            adopted_head: None,
            repository_contract: None,
            pid: None,
            command: None,
            log_path: None,
            agent_identity: None,
        })
        .unwrap();
    drop(broker);

    let session_id = session.id.to_string();
    let output = run(
        tmp.path(),
        &[
            "advanced",
            "git",
            "--session",
            &session_id,
            "--reason",
            "test",
            "--",
            "checkout",
            "does-not-exist",
        ],
    );
    assert!(!output.status.success());
    let message = last_failure_message(tmp.path());
    assert!(
        message.contains("coordinated git operation") && message.contains("does-not-exist"),
        "the provider's own reason must be kept, not only the operation id: {message}"
    );
}

#[test]
fn doctor_lists_recent_failures_with_their_message() {
    let tmp = fixture();
    run(
        tmp.path(),
        &["start", "--task", "t", "--base", "no-such-ref"],
    );

    let json = run(tmp.path(), &["status", "doctor", "--json"]);
    let report: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap_or_else(|error| {
        panic!(
            "doctor --json: {error}: {}",
            String::from_utf8_lossy(&json.stderr)
        )
    });
    let failures = report["recent_command_failures"]
        .as_array()
        .unwrap_or_else(|| panic!("no recent_command_failures: {report}"));
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(failures[0]["command_surface"], "broker.start");
    assert!(
        failures[0]["message"]
            .as_str()
            .is_some_and(|message| message.contains("no-such-ref")),
        "{failures:?}"
    );

    let text = String::from_utf8_lossy(&run(tmp.path(), &["status", "doctor"]).stdout).into_owned();
    assert!(
        text.contains("recent command failures: 1") && text.contains("no-such-ref"),
        "{text}"
    );
}

#[test]
fn doctor_omits_the_section_when_nothing_failed() {
    let tmp = fixture();
    let json = run(tmp.path(), &["status", "doctor", "--json"]);
    let report: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert!(
        report.get("recent_command_failures").is_none(),
        "an empty list is omitted: {report}"
    );
}

/// #555: a read nobody bounded still ends inside its default budget, with the
/// phase that spent it and the environment exit code, instead of outliving the
/// caller's own timeout.
#[cfg(unix)]
#[test]
fn an_unbounded_read_ends_at_its_default_budget_with_a_diagnostic() {
    let tmp = fixture();
    let hook = tmp.path().join(".git/slow-fsmonitor");
    std::fs::write(&hook, "#!/bin/sh\nsleep 6\n").unwrap();
    std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    git(
        tmp.path(),
        &["config", "core.fsmonitor", hook.to_str().unwrap()],
    );
    let mut broker = test_broker(tmp.path());
    let session = broker
        .store()
        .register_session(&NewSession {
            worktree_path: tmp.path().to_string_lossy().into_owned(),
            branch: "main".into(),
            origin: SessionOrigin::Adopted,
            task: None,
            diff_base: None,
            adoption_base: None,
            adopted_head: None,
            repository_contract: None,
            pid: None,
            command: None,
            log_path: None,
            agent_identity: None,
        })
        .unwrap();
    drop(broker);

    let session_id = session.id.to_string();
    let started = std::time::Instant::now();
    // No --queue-timeout and no --no-wait: the caller asked for no bound.
    let output = run_with_env(
        tmp.path(),
        &["advanced", "git", "--session", &session_id, "--", "status"],
        &[("AETHYME_BROKER_READ_BUDGET_SECS", "1")],
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "the read outlived its budget: {:?}",
        started.elapsed()
    );
    assert_eq!(
        output.status.code(),
        Some(i32::from(aethyme_broker::exit_status::ENVIRONMENT))
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("waiting for `git` to respond"), "{stderr}");
    assert!(stderr.contains("1s budget"), "{stderr}");
}
