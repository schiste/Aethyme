//! `broker exec` must distinguish its two failure causes (#133).
//!
//! `ok` is `command_success && audit.ok`, so a wrapped command that exits
//! non-zero and a guard that refuses an out-of-ownership write are separate
//! events. Reporting both as an ownership failure sends the reader to debug
//! leases when the command simply failed on its own.

use std::path::Path;
use std::process::{Command, Output};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::process::Stdio;

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
mod common;

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

fn run(repo: &Path, state: &Path, args: &[&str]) -> Output {
    common::broker_cli(CLI, args)
        .current_dir(repo)
        .env("AETHYME_HOST_STATE_DIR", state)
        .output()
        .unwrap()
}

fn fixture() -> (tempfile::TempDir, tempfile::TempDir, String) {
    let repo = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(repo.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    std::fs::write(repo.path().join("owned.txt"), "owned\n").unwrap();
    std::fs::write(repo.path().join("other.txt"), "other\n").unwrap();
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-qm", "init"]);
    let started = run(
        repo.path(),
        state.path(),
        &[
            "start",
            "--task",
            "exec guard",
            "--path",
            "owned.txt",
            "--json",
        ],
    );
    assert!(
        started.status.success(),
        "start: {}",
        String::from_utf8_lossy(&started.stderr)
    );
    let session: serde_json::Value = serde_json::from_slice(&started.stdout).unwrap();
    let id = session["id"].as_i64().unwrap().to_string();
    let worktree = session["worktree_path"].as_str().unwrap().to_string();
    (repo, state, format!("{id}\u{1}{worktree}"))
}

fn split(v: &str) -> (&str, &str) {
    let mut parts = v.split('\u{1}');
    (parts.next().unwrap(), parts.next().unwrap())
}

/// The command fails on its own and touches nothing: the message must not
/// blame ownership, because the guard rejected nothing.
#[test]
fn a_command_that_fails_on_its_own_is_not_reported_as_an_ownership_failure() {
    let (repo, state, packed) = fixture();
    let (id, worktree) = split(&packed);
    let out = Command::new(CLI)
        .args([
            "advanced",
            "exec",
            "--session",
            id,
            "--",
            "sh",
            "-c",
            "exit 3",
        ])
        .current_dir(worktree)
        .env("AETHYME_HOST_STATE_DIR", state.path())
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "the command failed, so exec must fail: {text}"
    );
    assert!(
        text.contains("exited 3"),
        "the wrapped command's exit code is the useful fact: {text}"
    );
    assert!(
        text.contains("no ownership violation"),
        "the message must clear the guard rather than implicate it: {text}"
    );
    assert!(
        !text.contains("failed ownership or command checks"),
        "the conflated message must be gone: {text}"
    );
    drop(repo);
}

/// The command succeeds but writes outside the session's leases: that is a
/// genuine guard refusal and must say so.
#[test]
fn a_write_outside_ownership_is_reported_as_a_refusal() {
    let (repo, state, packed) = fixture();
    let (id, worktree) = split(&packed);
    let out = Command::new(CLI)
        .args([
            "advanced",
            "exec",
            "--session",
            id,
            "--",
            "sh",
            "-c",
            "printf changed >> other.txt; touch runtime.sock",
        ])
        .current_dir(worktree)
        .env("AETHYME_HOST_STATE_DIR", state.path())
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "an out-of-ownership write must fail: {text}"
    );
    assert!(
        text.contains("outside") && text.contains("ownership"),
        "a real refusal must name ownership: {text}"
    );
    assert!(
        !text.contains("no ownership violation"),
        "this one really is an ownership violation: {text}"
    );
    let mut broker = aethyme_broker::Broker::open_with_options(
        repo.path(),
        aethyme_broker::BrokerOpenOptions::default()
            .with_host_state_dir(state.path())
            .with_worktree_root(Path::new(worktree).parent().unwrap()),
    )
    .unwrap();
    let events = broker.store().events_after(0, i64::MAX).unwrap();
    let out_of_lease = events
        .iter()
        .find(|event| event.kind == "guard.out_of_lease_write")
        .expect("out-of-lease evidence is durable");
    assert!(
        out_of_lease
            .payload_json
            .as_deref()
            .is_some_and(|payload| payload.contains("other.txt") && !payload.contains("printf"))
    );
    let untracked = events
        .iter()
        .find(|event| event.kind == "guard.untracked_artifact")
        .expect("new untracked artifact evidence is durable");
    assert!(
        untracked
            .payload_json
            .as_deref()
            .is_some_and(|payload| payload.contains("runtime.sock") && !payload.contains(worktree))
    );
    drop(repo);
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn completed_cli_exec_exposes_a_queryable_receipt_without_command_details() {
    let (repo, state, packed) = fixture();
    let (id, worktree) = split(&packed);
    let out = Command::new(CLI)
        .args([
            "advanced",
            "exec",
            "--session",
            id,
            "--non-interactive",
            "--json",
            "--",
            "sh",
            "-c",
            "exit 0",
        ])
        .current_dir(worktree)
        .env("AETHYME_HOST_STATE_DIR", state.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "exec: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let receipt_id = report["execution_receipt_id"].as_str().unwrap();
    assert_eq!(receipt_id.len(), 32);

    let status = run(
        repo.path(),
        state.path(),
        &[
            "advanced",
            "exec-status",
            "--session",
            id,
            "--receipt",
            receipt_id,
            "--json",
        ],
    );
    assert!(
        status.status.success(),
        "exec-status: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(value["schema"], "aethyme.broker-exec-status/v1");
    assert_eq!(value["session_id"].as_i64().unwrap().to_string(), id);
    assert_eq!(value["receipt"]["receipt_id"], receipt_id);
    assert_eq!(value["receipt"]["phase"], "completed");
    assert_eq!(value["receipt"]["command_outcome"], "succeeded");
    assert_eq!(value["receipt"]["ownership_audit"], "passed");
    assert_eq!(value["receipt"]["process_group_cleanup"], "verified");
    for secret_or_authority_field in ["pid", "command", "argv", "environment", "output"] {
        assert!(
            value["receipt"].get(secret_or_authority_field).is_none(),
            "receipt must not expose {secret_or_authority_field}"
        );
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn disconnected_exec_keeps_a_running_receipt_queryable_without_revalidation() {
    use std::time::{Duration, Instant};

    let (repo, state, packed) = fixture();
    let (id, worktree) = split(&packed);
    let mut exec = Command::new(CLI)
        .args([
            "advanced",
            "exec",
            "--session",
            id,
            "--non-interactive",
            "--timeout",
            "10",
            "--",
            "sh",
            "-c",
            "sleep 2",
        ])
        .current_dir(worktree)
        .env("AETHYME_HOST_STATE_DIR", state.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    let receipt_id = loop {
        let status = run(
            repo.path(),
            state.path(),
            &["advanced", "exec-status", "--session", id, "--json"],
        );
        assert!(
            status.status.success(),
            "exec-status: {}",
            String::from_utf8_lossy(&status.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
        if let Some(receipt) = value["receipts"]
            .as_array()
            .and_then(|receipts| receipts.first())
            .filter(|receipt| receipt["phase"] == "running")
        {
            break receipt["receipt_id"].as_str().unwrap().to_owned();
        }
        assert!(Instant::now() < deadline, "exec never published its running receipt");
        std::thread::sleep(Duration::from_millis(50));
    };

    exec.kill().unwrap();
    let _ = exec.wait().unwrap();

    std::thread::sleep(Duration::from_secs(3));

    let status = run(
        repo.path(),
        state.path(),
        &[
            "advanced",
            "exec-status",
            "--session",
            id,
            "--receipt",
            &receipt_id,
            "--json",
        ],
    );
    assert!(
        status.status.success(),
        "exec-status: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    let receipt = &value["receipt"];
    assert_eq!(receipt["receipt_id"], receipt_id);
    assert_eq!(receipt["phase"], "running");
    assert_eq!(receipt["command_outcome"], "unknown");
    assert_eq!(receipt["process_revalidation"], "not_revalidated");
    assert!(receipt.get("pid").is_none());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn rejected_exec_preflight_does_not_create_a_receipt_or_dispatch_the_command() {
    let (repo, state, _) = fixture();
    let marker = state.path().join("exec-preflight-must-not-run");
    let marker_arg = marker.to_string_lossy().into_owned();
    let out = run(
        repo.path(),
        state.path(),
        &[
            "advanced",
            "exec",
            "--session",
            "999999",
            "--non-interactive",
            "--json",
            "--",
            "sh",
            "-c",
            "touch \"$1\"",
            "sh",
            &marker_arg,
        ],
    );
    assert!(!out.status.success(), "unknown session must be refused");
    assert!(!marker.exists(), "preflight refusal must not dispatch a child");

    let status = run(
        repo.path(),
        state.path(),
        &[
            "advanced",
            "exec-status",
            "--session",
            "999999",
            "--json",
        ],
    );
    assert!(status.status.success());
    let value: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(value["receipts"].as_array().unwrap().len(), 0);
}
