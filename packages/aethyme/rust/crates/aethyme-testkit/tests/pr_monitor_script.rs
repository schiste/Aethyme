//! The scheduled PR monitor pass (`scripts/adapters/aethyme-pr-monitor.sh`).

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

fn executable(path: &Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// #417: an empty pass used to start the Chau7 adapter, which claimed nothing
/// every five minutes, day and night. The adapter now starts only when the
/// outbox lists something, or when the listing itself failed.
#[test]
fn the_monitor_starts_the_delivery_adapter_only_when_the_outbox_has_work() {
    let fixture = tempfile::tempdir().unwrap();
    let bin = fixture.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let calls = fixture.path().join("calls");
    executable(
        &bin.join("aethyme"),
        r#"#!/bin/sh
printf 'broker %s\n' "$*" >> "$TEST_CALLS"
case "$*" in
    *"watch pr tick"*) printf 'PR scheduler tick: 0 due\n' ;;
    *"deliveries list"*) printf '%s\n' "$TEST_OUTBOX"; exit "${TEST_LIST_EXIT:-0}" ;;
esac
"#,
    );
    executable(
        &bin.join("python3"),
        "#!/bin/sh\nprintf 'adapter %s\\n' \"$*\" >> \"$TEST_CALLS\"\n",
    );
    let script = aethyme_testkit::paths::repo_root()
        .join("packages/aethyme/scripts/adapters/aethyme-pr-monitor.sh");
    let pass = |outbox: &str, list_exit: &str| {
        let _ = std::fs::remove_file(&calls);
        let output = Command::new("bash")
            .arg(&script)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("AETHYME_PR_MONITOR_REPO", fixture.path())
            .env("AETHYME_BROKER_BIN", bin.join("aethyme"))
            .env("AETHYME_PR_MONITOR_WORKER", "worker-1")
            .env("TEST_CALLS", &calls)
            .env("TEST_OUTBOX", outbox)
            .env("TEST_LIST_EXIT", list_exit)
            .output()
            .unwrap();
        assert!(output.status.success(), "the monitor never fails a pass");
        (
            std::fs::read_to_string(&calls).unwrap_or_default(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        )
    };

    let adapter_ran = |calls: &str| calls.lines().any(|line| line.starts_with("adapter "));

    let (idle, log) = pass("[]", "0");
    assert!(
        idle.contains("broker broker advanced watch pr tick"),
        "{idle}"
    );
    assert!(
        idle.contains("deliveries list --adapter chau7 --json"),
        "{idle}"
    );
    assert!(
        !adapter_ran(&idle),
        "an empty outbox starts no adapter: {idle}"
    );
    assert!(log.contains("adapter not started"), "{log}");

    let (busy, _) = pass("[\n  {\"id\": 1, \"status\": \"pending\"}\n]", "0");
    assert!(
        adapter_ran(&busy),
        "a pending delivery starts the adapter: {busy}"
    );
    assert!(busy.contains("--worker worker-1"), "{busy}");

    let (unknown, log) = pass("broker unavailable", "1");
    assert!(
        adapter_ran(&unknown),
        "a failed listing must not suppress delivery: {unknown}"
    );
    assert!(log.contains("delivery listing failed"), "{log}");
}

/// Offline Python controls exercise transport limits without a live Chau7/broker.
#[test]
fn the_delivery_adapter_protocol_and_budget_controls_pass() {
    let script = aethyme_testkit::paths::repo_root()
        .join("packages/aethyme/scripts/adapters/test_chau7_delivery_adapter.py");
    let output = Command::new("python3")
        .arg(script)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("Ran 16 tests"),
        "expected all offline controls: {stderr}"
    );
    for name in [
        "test_trickled_bytes_do_not_restart_request_deadline",
        "test_notification_flood_has_a_finite_message_budget",
        "test_broker_output_overflow_is_not_treated_as_a_valid_prefix",
        "test_broker_timeout_stops_and_reaps_only_its_owned_child",
        "test_main_shares_one_pass_budget_and_closes_the_client",
        "test_failed_send_never_submits_or_acknowledges_delivery",
        "test_failed_submit_is_not_acknowledged_as_delivered",
        "test_tool_result_shapes_fail_with_typed_protocol_errors",
    ] {
        assert!(
            stderr.contains(name),
            "missing selected control {name}: {stderr}"
        );
    }
}
