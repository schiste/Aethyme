//! CLI regression test for completeness and confidence signal surfacing.
//!
//! Ported from `tests/local/test_cli_completeness_signals.py`
//! (python-retirement Phase 7). That file lost its in-process
//! monkeypatch/CliRunner cases in Phase 6 along with the code they
//! tested: `test_removed_python_explore_command_prints_native_recovery_hint`
//! exercised `src/cli.py`'s explore tombstone, a Click `UsageError` that
//! pointed operators at the native binary after the 2026-05-08
//! hard-delete. `src/cli.py` is gone, so `python -m src.cli explore` now
//! produces "No module named src" rather than a recovery hint — a break
//! announced in README + AGENTS.md rather than shimmed. What remains is
//! the surface-level test, which drives the router subprocess.

use aethyme_testkit::invoke_aethyme;

#[test]
fn intents_compact_json_lists_default_task_localization_query() {
    let result = invoke_aethyme(["intents", "--format", "compact-json"]);
    result.ok();
    let payload = result.json();

    let explore_mode = payload["modes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|mode| mode["mode"] == "explore")
        .expect("explore mode is listed");

    let intents = explore_mode["intents"].as_array().unwrap();
    let intent = &intents[0];
    assert_eq!(intent["intent"], "task_localization_query");
    assert_eq!(intent["required_params"], serde_json::json!([]));
    assert_eq!(intent["default_for_explore"], true);
    assert!(intent.get("answer_schema").is_some());
    assert!(intent.get("observability").is_some());

    let names: Vec<&str> = intents
        .iter()
        .map(|item| item["intent"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"behavior_localization_query"));
    assert!(names.contains(&"usage_boundary_query"));
}
