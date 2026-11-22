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
    let output = Command::new(env!("CARGO_BIN_EXE_aethyme"))
        .args(["content-gate", "compare-v2"])
        .arg(&path)
        .env("AETHYME_GATE_WORKER_ID", "test")
        .env("AETHYME_CONTENT_GATE_MODE", "v1-uncached")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}
