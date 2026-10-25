//! Offline controls for the scheduled PR monitor and delivery adapter.

#![cfg(unix)]

use std::process::Command;

#[test]
fn the_monitor_starts_the_delivery_adapter_only_when_the_outbox_has_work() {
    let script = aethyme_testkit::paths::repo_root()
        .join("packages/aethyme/scripts/adapters/test_pr_monitor.py");
    let output = Command::new("python3")
        .arg(script)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("Ran 10 tests"), "{stderr}");
    for name in [
        "test_verified_empty_outbox_starts_no_transport",
        "test_expired_pass_cannot_spawn_or_open_transport",
        "test_real_shell_entrypoint_reaches_only_synthetic_empty_broker",
        "test_combined_pass_log_is_capped_with_visible_truncation",
    ] {
        assert!(stderr.contains(name), "missing control {name}: {stderr}");
    }
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
