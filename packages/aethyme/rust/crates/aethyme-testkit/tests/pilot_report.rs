use serde_json::{Value, json};
use std::io::Write;
use std::process::{Command, Output, Stdio};

fn filter(name: &str, input: &str, slurp: bool) -> Output {
    let mut command = Command::new("jq");
    command.arg("-e");
    if slurp {
        command.arg("-s");
    }
    let mut child = command
        .arg("-f")
        .arg(
            aethyme_testkit::paths::repo_root()
                .join("scripts")
                .join(name),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("jq is a documented development/installer prerequisite");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn metrics() -> Value {
    json!({
        "gates_executed": [{"gate": "PRIVATE_GATE", "runs": 2, "total_ms": 300}],
        "gate_cache_hits": 1, "gate_time_saved_ms": 100,
        "conflicts_caught_pre_gate": 1, "overlaps_warned": 2,
        "commands": [{"command": "PRIVATE_COMMAND", "count": 3, "total_ms": 400,
            "total_output_bytes": 800, "output_sampled_calls": 2}],
        "task": "PRIVATE_TASK", "path": "/PRIVATE_PATH", "secret": "PRIVATE_SECRET"
    })
}

#[test]
fn export_is_numeric_allowlist_and_rejects_invalid_counters() {
    let output = filter("pilot-report.jq", &metrics().to_string(), false);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("PRIVATE"));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["counters"]["gate_runs"], 2);
    assert_eq!(report["counters"]["command_output_bytes"], 800);
    assert_eq!(report.as_object().unwrap().len(), 2);
    assert!(
        report["counters"]
            .as_object()
            .unwrap()
            .values()
            .all(Value::is_u64)
    );
    for bad in [
        Value::Null,
        json!(-1),
        json!(1.5),
        json!("3"),
        json!(9007199254740992_u64),
    ] {
        let mut input = metrics();
        input["gate_cache_hits"] = bad;
        assert!(
            !filter("pilot-report.jq", &input.to_string(), false)
                .status
                .success()
        );
    }
    let mut missing = metrics();
    missing.as_object_mut().unwrap().remove("commands");
    assert!(
        !filter("pilot-report.jq", &missing.to_string(), false)
            .status
            .success()
    );
}

#[test]
fn comparison_preserves_deltas_and_refuses_counter_reset() {
    let baseline = filter("pilot-report.jq", &metrics().to_string(), false);
    assert!(baseline.status.success());
    let mut followup_metrics = metrics();
    followup_metrics["gate_cache_hits"] = json!(4);
    let followup = filter("pilot-report.jq", &followup_metrics.to_string(), false);
    assert!(followup.status.success());
    let pair = format!(
        "{}\n{}",
        String::from_utf8_lossy(&baseline.stdout),
        String::from_utf8_lossy(&followup.stdout)
    );
    let delta = filter("pilot-compare.jq", &pair, true);
    assert!(
        delta.status.success(),
        "{}",
        String::from_utf8_lossy(&delta.stderr)
    );
    let value: Value = serde_json::from_slice(&delta.stdout).unwrap();
    assert_eq!(value["counter_deltas"]["gate_cache_hits"], 3);
    let reversed = format!(
        "{}\n{}",
        String::from_utf8_lossy(&followup.stdout),
        String::from_utf8_lossy(&baseline.stdout)
    );
    assert!(!filter("pilot-compare.jq", &reversed, true).status.success());
    assert!(
        !filter(
            "pilot-compare.jq",
            &String::from_utf8_lossy(&baseline.stdout),
            true
        )
        .status
        .success()
    );
}
