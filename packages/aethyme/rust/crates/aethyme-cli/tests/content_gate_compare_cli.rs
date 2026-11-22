use serde_json::{Value, json};
use std::{fs, process::Command};

#[test]
fn offline_comparison_reports_match_and_refuses_broker_execution() {
    let v: Value = serde_json::from_str(include_str!(
        "../../aethyme-broker/src/content_gate/portable-verdict-vector.json"
    ))
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let file = |name: &str, data: &[u8]| {
        let path = dir.path().join(name);
        fs::write(&path, data).unwrap();
        path
    };
    let old = file("old", v["portable"].to_string().as_bytes());
    let base = file("base", v["base"].to_string().as_bytes());
    let after = file("after", v["after"].to_string().as_bytes());
    let artifact = |name: &str| file(name, v["artifacts"][name].as_str().unwrap().as_bytes());
    let manifest = json!({"previous":old,"fresh":old,"base":base,"after":after,
        "change":artifact("change"),"resolved_policy":artifact("resolved_policy"),
        "host_options":artifact("host_options"),"collection":null,
        "evaluator_identity":artifact("evaluator"),"validator_name":"aexeo",
        "validator_version":"0.0.24","required_checks":["gate:7:article:words"],
        "fresh_exit_code":0,"fresh_timed_out":false});
    let path = file("manifest", manifest.to_string().as_bytes());
    let output = Command::new(env!("CARGO_BIN_EXE_aethyme"))
        .args(["content-gate", "compare-v2"])
        .arg(&path)
        .env_remove("AETHYME_GATE_WORKER_ID")
        .output()
        .unwrap();
    assert!(output.status.success());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"], "match");
    assert_eq!(result["publication_authorized"], false);
    assert!(result["timings_ms"]["compare_ms"].is_number());
    assert!(result.get("coverage").is_none());
    let measured = Command::new(env!("CARGO_BIN_EXE_aethyme"))
        .args(["content-gate", "compare-v2"])
        .arg(&path)
        .arg("--telemetry")
        .env_remove("AETHYME_GATE_WORKER_ID")
        .output()
        .unwrap();
    assert!(measured.status.success());
    let telemetry: Value = serde_json::from_slice(&measured.stdout).unwrap();
    assert_eq!(
        telemetry["coverage"],
        json!({"policy_gate_count":1,"policy_check_count":1})
    );
    let output_text = String::from_utf8_lossy(&measured.stdout);
    assert!(!output_text.contains("article"));
    assert!(!output_text.contains(dir.path().to_str().unwrap()));
    fs::write(&path, b"{}").unwrap();
    let rejected = Command::new(env!("CARGO_BIN_EXE_aethyme"))
        .args(["content-gate", "compare-v2"])
        .arg(&path)
        .arg("--telemetry")
        .env_remove("AETHYME_GATE_WORKER_ID")
        .output()
        .unwrap();
    assert_eq!(rejected.status.code(), Some(2));
    let rejected_report: Value = serde_json::from_slice(&rejected.stdout).unwrap();
    assert_eq!(rejected_report["coverage"], Value::Null);
    assert_eq!(rejected_report["publication_authorized"], false);
    fs::write(&path, manifest.to_string().as_bytes()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_aethyme"))
        .args(["content-gate", "compare-v2"])
        .arg(&path)
        .env("AETHYME_GATE_WORKER_ID", "test")
        .env("AETHYME_CONTENT_GATE_MODE", "v1-uncached")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn supervised_comparison_runs_fresh_validator_and_refuses_broker_execution() {
    use sha2::{Digest, Sha256};

    let v: Value = serde_json::from_str(include_str!(
        "../../aethyme-broker/src/content_gate/portable-verdict-vector.json"
    ))
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let file = |name: &str, data: &[u8]| {
        let path = dir.path().join(name);
        fs::write(&path, data).unwrap();
        path
    };
    let old = file("old", v["portable"].to_string().as_bytes());
    let base = file("base", v["base"].to_string().as_bytes());
    let after = file("after", v["after"].to_string().as_bytes());
    let artifact = |name: &str| file(name, v["artifacts"][name].as_str().unwrap().as_bytes());
    let shell = fs::canonicalize("/bin/sh").unwrap();
    let digest = format!("{:x}", Sha256::digest(fs::read(&shell).unwrap()));
    let manifest = json!({"previous":old,"base":base,"after":after,
        "change":artifact("change"),"resolved_policy":artifact("resolved_policy"),
        "host_options":artifact("host_options"),"collection":null,
        "evaluator_identity":artifact("evaluator"),"validator_name":"aexeo",
        "validator_version":"0.0.24","required_checks":["gate:7:article:words"],
        "validator":[shell,"-c","/bin/cp \"$SOURCE\" \"$CONTENT_GATE_VERDICT_FILE\""],
        "validator_sha256":digest,"env":{"SOURCE":old},"timeout_seconds":5,
        "additional":{"source":old}});
    let path = file("manifest", manifest.to_string().as_bytes());
    let output = Command::new(env!("CARGO_BIN_EXE_aethyme"))
        .args(["content-gate", "run-v2"])
        .arg(&path)
        .env_remove("AETHYME_GATE_WORKER_ID")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"], "match");
    assert_eq!(result["publication_authorized"], false);
    assert!(result["timings_ms"]["validator_ms"].is_number());
    let output = Command::new(env!("CARGO_BIN_EXE_aethyme"))
        .args(["content-gate", "run-v2"])
        .arg(&path)
        .env("AETHYME_GATE_WORKER_ID", "test")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}
