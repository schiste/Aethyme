//! Implementation-blind CLI tests for `aethyme ai-ready`.
//!
//! Ported verbatim from `tests/local/test_ai_ready_cli.py`
//! (python-retirement Phase 7), which itself replaced
//! `tests/scorecard/test_cli.py` after the Phase 4 native flip.
//! Detector/engine/formatter behaviour is covered by the Rust unit tests
//! in `aethyme-quality`; what is pinned here is the CLI contract.

use aethyme_testkit::repos::{build_good_scorecard_repo, build_problematic_scorecard_repo, read};
use aethyme_testkit::{invoke_aethyme, tmp_dir};
use serde_json::Value;

#[test]
fn scan_good_repo_markdown() {
    let tmp = tmp_dir();
    let repo = build_good_scorecard_repo(tmp.path());
    let result = invoke_aethyme(["ai-ready", "--repo", &repo.display().to_string(), "--format", "md"]);
    assert!(matches!(result.exit_code, 0 | 1), "{}", result.output);
    assert!(
        result.output.contains("AI-Readiness Scorecard Report") || result.output.contains("Score"),
        "{}",
        result.output
    );
}

#[test]
fn scan_good_repo_json() {
    let tmp = tmp_dir();
    let repo = build_good_scorecard_repo(tmp.path());
    let output_file = tmp.path().join("scorecard.json");
    let result = invoke_aethyme([
        "ai-ready",
        "--repo",
        &repo.display().to_string(),
        "--format",
        "json",
        "--output",
        &output_file.display().to_string(),
    ]);
    assert!(matches!(result.exit_code, 0 | 1), "{}", result.output);
    assert!(output_file.exists());
    let data: Value = serde_json::from_str(&read(&output_file)).unwrap();
    assert!(data.get("scan_id").is_some());
    assert!(data.get("score").is_some());
}

#[test]
fn scan_problematic_repo() {
    let tmp = tmp_dir();
    let repo = build_problematic_scorecard_repo(tmp.path());
    let result = invoke_aethyme([
        "ai-ready",
        "--repo",
        &repo.display().to_string(),
        "--format",
        "md",
    ]);
    assert!(matches!(result.exit_code, 1 | 2), "{}", result.output);
}

#[test]
fn selective_detectors() {
    let tmp = tmp_dir();
    let repo = build_good_scorecard_repo(tmp.path());
    let result = invoke_aethyme([
        "ai-ready",
        "--repo",
        &repo.display().to_string(),
        "--detectors",
        "data-ui-coverage,folder-docs",
        "--format",
        "md",
    ]);
    assert!(matches!(result.exit_code, 0 | 1), "{}", result.output);
}

#[test]
fn both_formats_output() {
    let tmp = tmp_dir();
    let repo = build_good_scorecard_repo(tmp.path());
    let result = invoke_aethyme([
        "ai-ready",
        "--repo",
        &repo.display().to_string(),
        "--format",
        "both",
        "--output",
        &tmp.path().join("scorecard").display().to_string(),
    ]);
    assert!(matches!(result.exit_code, 0 | 1), "{}", result.output);
    let produced = std::fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.ends_with(".json") || name.ends_with(".md")
        })
        .count();
    assert!(produced > 0, "--format both wrote neither a .json nor a .md report");
}

#[test]
fn exit_codes() {
    let tmp = tmp_dir();
    let good = invoke_aethyme([
        "ai-ready",
        "--repo",
        &build_good_scorecard_repo(tmp.path()).display().to_string(),
    ]);
    assert!(matches!(good.exit_code, 0 | 1), "{}", good.output);

    let problematic = invoke_aethyme([
        "ai-ready",
        "--repo",
        &build_problematic_scorecard_repo(tmp.path()).display().to_string(),
    ]);
    assert!(matches!(problematic.exit_code, 1 | 2), "{}", problematic.output);
}

#[test]
fn invalid_repo_path() {
    let result = invoke_aethyme(["ai-ready", "--repo", "/nonexistent/path"]);
    result.expect_code(2);
    result.assert_contains("does not exist");
}

#[test]
fn help_names_the_options() {
    let result = invoke_aethyme(["ai-ready", "--help"]);
    result.ok();
    for flag in ["--repo", "--repo-id", "--format", "--output", "--detectors"] {
        result.assert_contains(flag);
    }
}

/// The parts of the report contract the Phase 4 corpus goldens froze.
#[test]
fn report_shape_on_problematic_repo() {
    let tmp = tmp_dir();
    let repo = build_problematic_scorecard_repo(tmp.path());
    let result = invoke_aethyme([
        "ai-ready",
        "--repo",
        &repo.display().to_string(),
        "--format",
        "json",
    ]);
    result.expect_code(2); // blocker present

    let start = result.output.find('{').expect("JSON payload in output");
    let end = result.output.rfind('}').expect("JSON payload in output");
    let data: Value = serde_json::from_str(&result.output[start..=end]).unwrap();

    assert_eq!(data["score"], 24);
    assert_eq!(
        data["summary"],
        serde_json::json!({
            "total_findings": 13,
            "blockers": 1,
            "warnings": 11,
            "info": 1,
        })
    );
    let detectors: Vec<&str> = data["detectors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|detector| detector["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        detectors,
        [
            "data-ui-coverage",
            "folder-docs",
            "relative-links",
            "i18n-gaps",
            "generated-files",
            "schema-drift",
            "route-coverage",
            "ability-coverage",
        ]
    );
}
