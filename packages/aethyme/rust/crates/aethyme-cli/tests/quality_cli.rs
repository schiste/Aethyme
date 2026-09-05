use std::process::Command;

use aethyme_testkit::{invoke_aethyme, tmp_dir};
use serde_json::Value;

fn tracked_repo() -> tempfile::TempDir {
    let tmp = tmp_dir();
    assert!(
        Command::new("git")
            .arg("init")
            .arg("-q")
            .arg(tmp.path())
            .status()
            .unwrap()
            .success()
    );
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(
        tmp.path().join("src/lib.rs"),
        "pub fn answer() -> u8 { 42 }\n",
    )
    .unwrap();
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["add", "src/lib.rs"])
            .status()
            .unwrap()
            .success()
    );
    tmp
}

#[test]
fn inspect_skips_irrelevant_detectors_with_reasons() {
    let repo = tracked_repo();
    std::fs::write(
        repo.path().join("scratch.tsx"),
        "export const X = () => <button>untracked</button>;\n",
    )
    .unwrap();
    let result = invoke_aethyme([
        "quality",
        "inspect",
        "--repo",
        &repo.path().display().to_string(),
        "--format",
        "json",
    ]);
    result.ok();
    let report: Value = serde_json::from_str(result.output.trim()).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["totals"]["findings"], 1); // folder-docs only
    assert_eq!(report["operational_readiness"]["authoritative"], false);
    let data_ui = report["detectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|detector| detector["name"] == "data-ui-coverage")
        .unwrap();
    assert_eq!(data_ui["status"], "skipped");
    assert!(
        data_ui["applicability_reason"]
            .as_str()
            .unwrap()
            .contains("no UI")
    );
}

#[test]
fn inspect_bounds_rendering_but_preserves_totals() {
    let repo = tracked_repo();
    let ui = (0..12)
        .map(|index| format!("export const B{index} = () => <button>{index}</button>;"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(repo.path().join("src/buttons.tsx"), ui).unwrap();
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["add", "src/buttons.tsx"])
            .status()
            .unwrap()
            .success()
    );
    let result = invoke_aethyme([
        "quality",
        "inspect",
        "--repo",
        &repo.path().display().to_string(),
        "--detectors",
        "data-ui-coverage",
        "--limit",
        "3",
        "--format",
        "json",
    ]);
    result.ok();
    let report: Value = serde_json::from_str(result.output.trim()).unwrap();
    assert_eq!(report["totals"]["findings"], 12);
    assert_eq!(report["totals"]["rendered"], 3);
    assert_eq!(report["totals"]["omitted"], 9);
    assert_eq!(report["findings"].as_array().unwrap().len(), 3);
}
