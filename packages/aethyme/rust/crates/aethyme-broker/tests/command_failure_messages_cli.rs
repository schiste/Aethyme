//! A failed broker command keeps the error it printed, redacted and capped,
//! so the failure can be explained after its terminal is gone, and `doctor`
//! lists the last day's failures with that message.

use std::path::Path;
use std::process::{Command, Output};

use aethyme_broker::{Broker, NewSession, SessionOrigin};

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

fn host_state_dir(repo: &Path) -> std::path::PathBuf {
    repo.join(".aethyme/test-host-state")
}

fn test_broker(repo: &Path) -> Broker {
    Broker::open(repo)
        .unwrap()
        .with_host_operation_database(host_state_dir(repo).join("host-operations.db"))
}

fn run(repo: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
        .current_dir(repo)
        .env("AETHYME_HOST_STATE_DIR", host_state_dir(repo))
        .output()
        .unwrap()
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

fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    drop(test_broker(tmp.path()));
    tmp
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
