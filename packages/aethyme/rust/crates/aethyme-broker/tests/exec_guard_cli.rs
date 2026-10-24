//! `broker exec` must distinguish its two failure causes (#133).
//!
//! `ok` is `command_success && audit.ok`, so a wrapped command that exits
//! non-zero and a guard that refuses an out-of-ownership write are separate
//! events. Reporting both as an ownership failure sends the reader to debug
//! leases when the command simply failed on its own.

use aethyme_broker::BrokerStore;
use std::path::Path;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::process::Stdio;
use std::process::{Command, Output};

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
fn failed_to_spawn_command_records_a_not_started_receipt() {
    let (repo, state, packed) = fixture();
    let (id, worktree) = split(&packed);
    let missing = state.path().join("missing-exec-command");
    let mut exec = Command::new(CLI);
    exec.args([
        "advanced",
        "exec",
        "--session",
        id,
        "--non-interactive",
        "--json",
        "--",
    ])
    .arg(missing)
    .current_dir(worktree)
    .env("AETHYME_HOST_STATE_DIR", state.path());
    let out = exec.output().unwrap();
    assert!(
        !out.status.success(),
        "a missing executable must fail to start"
    );

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
    let receipt = value["receipts"]
        .as_array()
        .and_then(|receipts| receipts.first())
        .expect("failed spawn remains queryable");
    assert_eq!(receipt["phase"], "not_started");
    assert_eq!(receipt["command_outcome"], "not_started");
    assert_eq!(receipt["ownership_audit"], "not_run");
    assert_eq!(receipt["process_group_cleanup"], "not_applicable");
    assert!(receipt["exit_code"].is_null());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn disconnected_exec_keeps_a_running_receipt_queryable_without_revalidation() {
    use std::time::{Duration, Instant};

    let (repo, state, packed) = fixture();
    let (id, worktree) = split(&packed);
    let started_marker = state.path().join("exec-child-started");
    let release_marker = state.path().join("exec-child-release");
    // Keep the supervised child alive until the test disconnects its broker
    // client. The loop has a hard bound as a fallback if cleanup regresses.
    let wait_for_release = concat!(
        "printf started > \"$AETHYME_TEST_EXEC_STARTED\"; ",
        "i=0; while [ ! -e \"$AETHYME_TEST_EXEC_RELEASE\" ] ",
        "&& [ \"$i\" -lt 600 ]; do sleep 0.05; i=$((i + 1)); done"
    );
    let mut exec = Command::new(CLI)
        .args([
            "advanced",
            "exec",
            "--session",
            id,
            "--non-interactive",
            "--timeout",
            "40",
            "--",
            "sh",
            "-c",
            wait_for_release,
        ])
        .current_dir(worktree)
        .env("AETHYME_HOST_STATE_DIR", state.path())
        .env("AETHYME_TEST_EXEC_STARTED", &started_marker)
        .env("AETHYME_TEST_EXEC_RELEASE", &release_marker)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(15);
    let receipt_result = loop {
        let status = run(
            repo.path(),
            state.path(),
            &["advanced", "exec-status", "--session", id, "--json"],
        );
        if !status.status.success() {
            break Err(format!(
                "exec-status: {}",
                String::from_utf8_lossy(&status.stderr)
            ));
        }
        let value: serde_json::Value = match serde_json::from_slice(&status.stdout) {
            Ok(value) => value,
            Err(error) => break Err(format!("exec-status returned invalid JSON: {error}")),
        };
        if let Some(receipt) = value["receipts"]
            .as_array()
            .and_then(|receipts| receipts.first())
            .filter(|receipt| receipt["phase"] == "running" && started_marker.is_file())
        {
            if let Some(receipt_id) = receipt["receipt_id"].as_str() {
                break Ok(receipt_id.to_owned());
            }
            break Err("running receipt omitted its identifier".to_owned());
        }
        if Instant::now() >= deadline {
            break Err(
                "exec never published a running receipt after starting its child".to_owned(),
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    if matches!(exec.try_wait(), Ok(None)) {
        exec.kill().unwrap();
    }
    let _ = exec.wait().unwrap();
    // Let the synthetic child exit if the supervisor did not already stop it
    // when the client disconnected.
    std::fs::write(&release_marker, b"release\n").unwrap();
    let receipt_id = receipt_result.unwrap_or_else(|reason| panic!("{reason}"));

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

fn exec_status_telemetry_state(repo: &Path) -> (Vec<(i64, String)>, Option<Vec<u8>>) {
    let database = repo.join(".aethyme/broker.db");
    assert!(
        database.is_file(),
        "fixture should create its repository database"
    );
    let store = BrokerStore::open_snapshot_at(&database).expect("fixture broker database opens");
    let events = store
        .events_after(0, 10_000)
        .expect("read fixture broker events")
        .into_iter()
        .map(|event| (event.id, event.kind))
        .collect();
    let metrics_path = repo.join(".aethyme/logs/command-metrics.jsonl");
    let metrics = match std::fs::read(metrics_path) {
        Ok(metrics) => Some(metrics),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("read fixture command metrics: {error}"),
    };
    (events, metrics)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn exec_status_refuses_extra_input_and_conflicting_selectors() {
    let (repo, state, packed) = fixture();
    let (id, _) = split(&packed);
    let telemetry_before = exec_status_telemetry_state(repo.path());

    let valid = common::broker_cli(CLI, &["advanced", "exec-status", "--session", id, "--json"])
        .current_dir(repo.path())
        .env("AETHYME_HOST_STATE_DIR", state.path())
        .env("AETHYME_MEASURE_OUTPUT", "0")
        .output()
        .unwrap();
    assert!(
        valid.status.success(),
        "valid query: {}",
        String::from_utf8_lossy(&valid.stderr)
    );
    let valid_json: serde_json::Value = serde_json::from_slice(&valid.stdout).unwrap();
    assert_eq!(valid_json["schema"], "aethyme.broker-exec-status/v1");
    assert_eq!(valid_json["session_id"], id.parse::<i64>().unwrap());
    assert_eq!(valid_json["receipts"].as_array().unwrap().len(), 0);
    assert_eq!(exec_status_telemetry_state(repo.path()), telemetry_before);

    let malformed = [
        vec![
            "advanced",
            "exec-status",
            "--session",
            id,
            "unexpected",
            "--json",
        ],
        vec![
            "advanced",
            "exec-status",
            "--session",
            id,
            "--json",
            "--",
            "sh",
            "-c",
            "exit 0",
        ],
        vec![
            "advanced",
            "exec-status",
            "--session",
            id,
            "--receipt",
            "0123456789abcdef0123456789abcdef",
            "--limit",
            "20",
            "--json",
        ],
        vec![
            "advanced",
            "exec-status",
            "--session",
            id,
            "--receipt",
            "not-a-receipt",
            "--json",
        ],
        vec![
            "advanced",
            "exec-status",
            "--session",
            id,
            "--receipt",
            "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
            "--json",
        ],
        vec![
            "advanced",
            "exec-status",
            "--session",
            id,
            "--limit",
            "0",
            "--json",
        ],
        vec![
            "advanced",
            "exec-status",
            "--session",
            id,
            "--task",
            "ignored",
            "--json",
        ],
    ];
    let unopened_database = state.path().join("must-not-open.db");
    std::fs::write(&unopened_database, b"leave untouched").unwrap();
    let unopened_database_before = std::fs::read(&unopened_database).unwrap();

    for args in malformed {
        let out = common::broker_cli(CLI, &args)
            .current_dir(repo.path())
            .env("AETHYME_HOST_STATE_DIR", state.path())
            .env("AETHYME_BROKER_DB", &unopened_database)
            .env("AETHYME_MEASURE_OUTPUT", "0")
            .output()
            .unwrap();
        assert!(
            !out.status.success(),
            "invalid exec-status form was accepted: {args:?}"
        );
        assert!(
            out.stderr.is_empty(),
            "JSON usage error should be on stdout: {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let error: serde_json::Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|_| panic!("invalid form did not return JSON error: {args:?}"));
        assert_eq!(error["schema"], "aethyme.broker-error/v1", "{args:?}");
        assert_eq!(error["error"]["code"], 2, "{args:?}");
        let message = error["error"]["message"].as_str().unwrap();
        assert!(message.contains("exec-status"), "{args:?}: {message}");
        assert!(error.get("receipt").is_none(), "{args:?}");
        assert!(error.get("receipts").is_none(), "{args:?}");
        assert_eq!(
            std::fs::read(&unopened_database).unwrap(),
            unopened_database_before,
            "invalid form touched its configured database: {args:?}"
        );
        assert_eq!(exec_status_telemetry_state(repo.path()), telemetry_before);
    }
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
    assert!(
        !marker.exists(),
        "preflight refusal must not dispatch a child"
    );

    let status = run(
        repo.path(),
        state.path(),
        &["advanced", "exec-status", "--session", "999999", "--json"],
    );
    assert!(status.status.success());
    let value: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(value["receipts"].as_array().unwrap().len(), 0);
}

/// A terminal adapter may stop waiting while its broker process remains alive.
/// The running receipt must remain discoverable and later bind to completion.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn an_early_returning_caller_can_query_the_same_receipt_through_completion() {
    use std::time::{Duration, Instant};
    let (repo, state, packed) = fixture();
    let (id, worktree) = split(&packed);
    let started = state.path().join("early-return-started");
    let release = state.path().join("early-return-release");
    let mut exec = Command::new(CLI)
        .args([
            "advanced",
            "exec",
            "--session",
            id,
            "--non-interactive",
            "--timeout",
            "8",
            "--json",
            "--",
            "sh",
            "-c",
            concat!(
                "printf started > \"$1\"; i=0; ",
                "while [ ! -e \"$2\" ] && [ \"$i\" -lt 100 ]; ",
                "do sleep 0.05; i=$((i + 1)); done"
            ),
            "fixture",
            started.to_str().unwrap(),
            release.to_str().unwrap(),
        ])
        .current_dir(worktree)
        .env("AETHYME_HOST_STATE_DIR", state.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
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
            "{}",
            String::from_utf8_lossy(&status.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
        if started.is_file() {
            if let Some(receipt) = value["receipts"]
                .as_array()
                .unwrap()
                .iter()
                .find(|receipt| receipt["phase"] == "running")
            {
                assert!(exec.try_wait().unwrap().is_none());
                break receipt["receipt_id"].as_str().unwrap().to_owned();
            }
        }
        if Instant::now() >= deadline {
            std::fs::write(&release, b"release").unwrap();
            if exec.try_wait().unwrap().is_none() {
                exec.kill().unwrap();
            }
            let output = exec.wait_with_output().unwrap();
            panic!(
                "running receipt not observed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    std::fs::write(&release, b"release").unwrap();
    let output = exec.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["execution_receipt_id"], receipt_id);
    assert_eq!(report["command_success"], true);
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
    assert!(status.status.success());
    let value: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(value["receipt"]["receipt_id"], receipt_id);
    assert_eq!(value["receipt"]["phase"], "completed");
    assert_eq!(value["receipt"]["process_group_cleanup"], "verified");
}
