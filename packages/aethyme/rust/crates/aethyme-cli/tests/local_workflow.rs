//! Local-first CLI tests for the Rust-backed repository workflow.
//!
//! Ported from `tests/local/test_local_workflow.py` (python-retirement
//! Phase 7). Each case indexes a two-module demo repo the same way
//! `scripts/eval/setup-playground.sh` does — fragments via
//! `aethyme-graph-index`, then the redb store via
//! `aethyme-engine-cli index` — and drives the router over it.
//!
//! The Python suite's two remaining cases exercised its own
//! `require_local_engine_or_skip()` helper: whether a missing engine
//! skipped, or failed when `AETHYME_REQUIRE_LOCAL_ENGINE=1`. They have no
//! counterpart here on purpose. The Rust harness has no skip path at all
//! — `aethyme_testkit::bins` asserts on a failed build — so the strict
//! mode those cases guarded is now the only mode, and a helper that
//! cannot skip needs no test proving it skips.

use std::path::Path;

use aethyme_testkit::repos::{bootstrap_repo_fragments, build_demo_source_repo};
use aethyme_testkit::{invoke_aethyme, tmp_dir};

fn demo_repo(tmp: &Path) -> std::path::PathBuf {
    let repo = build_demo_source_repo(&tmp.join("demo-repo"));
    bootstrap_repo_fragments(&repo);
    repo
}

#[test]
fn local_repo_inspect_and_pack() {
    let tmp = tmp_dir();
    let repo = demo_repo(tmp.path());
    let repo_arg = repo.display().to_string();

    let inspect = invoke_aethyme(["repo", "inspect", &repo_arg, "--json-output"]);
    inspect.ok();
    let payload = inspect.json();
    assert_eq!(payload["snapshot"]["readme_path"], "README.md");
    assert!(payload.get("signals").is_some());
    assert!(payload["signals"]["boundary_clarity"]["score"].as_f64().unwrap() >= 0.0);
    assert!(!payload["symbols"].as_array().unwrap().is_empty());
    assert!(!payload["areas"].as_array().unwrap().is_empty());
    assert!(!payload["files"].as_array().unwrap().is_empty());
    assert!(!payload["graph"]["nodes"].as_array().unwrap().is_empty());

    let pack = invoke_aethyme([
        "task", "pack", "--repo", &repo_arg, "--task", "Explain this repo", "--json-output",
    ]);
    pack.ok();
    let payload = pack.json();
    assert_eq!(payload["task"]["kind"], "explain_repo");
    assert!(!payload["anchors"].as_array().unwrap().is_empty());
    assert!(
        payload["navigation_order"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry == "README.md")
    );
    assert!(!payload["in_scope"]["areas"].as_array().unwrap().is_empty());
}

#[test]
fn local_graph_navigation_commands() {
    let tmp = tmp_dir();
    let repo = demo_repo(tmp.path());
    let repo_arg = repo.display().to_string();

    let node = invoke_aethyme(["graph", "node", &repo_arg, "src/main.py", "--json-output"]);
    node.ok();
    assert_eq!(node.json()["kind"], "file");

    let children = invoke_aethyme(["graph", "children", &repo_arg, "src", "--json-output"]);
    children.ok();
    assert!(!children.json()["items"].as_array().unwrap().is_empty());

    let overview = invoke_aethyme(["graph", "overview", &repo_arg, "--json-output"]);
    overview.ok();
    let payload = overview.json();
    assert!(payload.get("signals").is_some());
    assert!(payload["signals"]["parser_visibility"]["score"].as_f64().unwrap() >= 0.0);
}

/// Coverage for the redb deps/impact contract.
///
/// Added 2026-07-27 after `query deps` shipped broken for 12 days (an
/// engine flag plus an output-format change with no test to notice).
/// Asserts the cross-process contract holds end-to-end, not result
/// richness — the demo repo has no resolved file-to-file adjacency, so
/// empty lists are valid.
#[test]
fn local_query_deps_and_impact_commands() {
    let tmp = tmp_dir();
    let repo = demo_repo(tmp.path());
    let repo_arg = repo.display().to_string();

    invoke_aethyme(["query", "deps", &repo_arg, "src/main.py"]).ok();
    invoke_aethyme(["query", "impact", &repo_arg, "src/auth.py"]).ok();
}

#[test]
fn local_task_navigation_commands() {
    let tmp = tmp_dir();
    let repo = demo_repo(tmp.path());
    let repo_arg = repo.display().to_string();
    let task = "Update validate_token flow";

    let anchors = invoke_aethyme([
        "task", "anchors", "--repo", &repo_arg, "--task", task, "--json-output",
    ]);
    anchors.ok();
    assert!(!anchors.json()["anchors"].as_array().unwrap().is_empty());

    let scope = invoke_aethyme([
        "task", "scope", "--repo", &repo_arg, "--task", task, "--json-output",
    ]);
    scope.ok();
    let payload = scope.json();
    let in_scope_files: Vec<&str> = payload["in_scope_files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect();
    assert!(!in_scope_files.is_empty());
    assert!(in_scope_files.contains(&"src/auth.py"));
    assert!(in_scope_files.contains(&"src/main.py"));
    assert!(
        payload["in_scope_symbols"]
            .as_array()
            .unwrap()
            .iter()
            .any(|symbol| symbol.as_str().unwrap().starts_with("src/auth.py::"))
    );

    let next = invoke_aethyme([
        "task", "next", "--repo", &repo_arg, "--task", task, "--json-output",
    ]);
    next.ok();
    let payload = next.json();
    let displays: Vec<&str> = payload["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["display"].as_str().unwrap())
        .collect();
    assert!(!displays.is_empty());
    assert!(displays.contains(&"src/auth.py"));
    assert!(displays.contains(&"src/main.py"));

    let expand = invoke_aethyme([
        "task", "expand", "--repo", &repo_arg, "--node", "src/auth.py", "--json-output",
    ]);
    expand.ok();
    assert!(expand.json().get("dependencies").is_some());
}
