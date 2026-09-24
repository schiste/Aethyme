//! Graph-free Explore: bounded full-content search plus the on-demand
//! symbol index, driven through the built engine CLI on generated fixtures.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use aethyme_engine::explore::{SourceSearchOptions, SymbolCache, graph_unavailable_response_with};

fn engine_bin() -> &'static str {
    env!("CARGO_BIN_EXE_aethyme-engine-cli")
}

fn write(root: &Path, rel: &str, content: &str) {
    let full = root.join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, content).unwrap();
}

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

/// About 2,000 files of ordinary-looking source, tests, docs and vendored
/// code. Exactly one file defines the behavior the request names; many
/// others mention the same words.
fn generated_repo(root: &Path, modules: usize) {
    git(root, &["init", "-q"]);
    let words = [
        "account",
        "billing",
        "catalog",
        "delivery",
        "export",
        "gateway",
        "inventory",
        "ledger",
        "metrics",
        "notify",
        "order",
        "payment",
        "quota",
        "report",
        "search",
        "tenant",
    ];
    for index in 0..modules {
        let area = words[index % words.len()];
        let other = words[(index / words.len()) % words.len()];
        let body = (0..40)
            .map(|line| format!("    total = total + {line}  # {area} {other} step\n"))
            .collect::<String>();
        write(
            root,
            &format!("src/{area}/{other}_{index}.py"),
            &format!(
                "def handle_{area}_{other}_{index}(request):\n    total = 0\n{body}    return total\n\n\
                 class {Area}{index}Service:\n    def run(self):\n        return handle_{area}_{other}_{index}(None)\n",
                Area = area[..1].to_uppercase() + &area[1..],
            ),
        );
        if index % 4 == 0 {
            write(
                root,
                &format!("web/{area}/{other}_{index}.ts"),
                &format!(
                    "export function render{index}(items: string[]): number {{\n  // throttle retry for {area}\n  return items.length;\n}}\n"
                ),
            );
        }
        if index % 5 == 0 {
            write(
                root,
                &format!("tests/{area}/test_{other}_{index}.py"),
                &format!(
                    "def test_{area}_{index}():\n    # retry throttle window is exercised elsewhere\n    assert True\n"
                ),
            );
        }
        if index % 10 == 0 {
            write(
                root,
                &format!("docs/{area}/{other}_{index}.md"),
                "# Notes\n\nThe retry throttle window limits retry storms. Retry throttle.\n",
            );
            write(
                root,
                &format!("vendor/lib{index}/throttle.py"),
                "def retry_throttle_window(n):\n    # retry throttle window retry throttle\n    return n\n",
            );
        }
    }
    write(
        root,
        "src/net/limits.py",
        "import time\n\n\n\
         class RetryThrottle:\n\
         \x20   \"\"\"Caps how often a failing call may be retried.\"\"\"\n\n\
         \x20   def __init__(self, window_seconds):\n\
         \x20       self.window_seconds = window_seconds\n\n\
         \x20   def retry_throttle_window(self, attempts):\n\
         \x20       return min(self.window_seconds * attempts, 60)\n",
    );
    git(root, &["add", "--all"]);
}

fn explore(root: &Path, cache: &Path, request: &str) -> (serde_json::Value, Duration) {
    let started = Instant::now();
    let output = Command::new(engine_bin())
        .args([
            "explore",
            "--repo",
            root.to_str().unwrap(),
            "--request",
            request,
            "--format",
            "answer-json",
            "--show-observability",
        ])
        .env("AETHYME_HOST_CACHE_DIR", cache)
        .output()
        .unwrap();
    let elapsed = started.elapsed();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    (serde_json::from_slice(&output.stdout).unwrap(), elapsed)
}

fn hint_paths(response: &serde_json::Value) -> Vec<String> {
    response["navigation_hints"]
        .as_array()
        .unwrap()
        .iter()
        .map(|hint| hint["path"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn two_thousand_file_repo_is_searched_completely_within_budget_cold_and_warm() {
    let repo = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let root = repo.path();
    generated_repo(root, 1_440);
    let request = "Where is the retry throttle window computed?";

    let (cold, cold_elapsed) = explore(root, cache.path(), request);
    let (warm, warm_elapsed) = explore(root, cache.path(), request);
    eprintln!(
        "source search on {} files: cold {cold_elapsed:?} (search {}ms, parsed {}), warm {warm_elapsed:?} (search {}ms, cache hits {})",
        cold["observability"]["source_fallback"]["listed_files"],
        cold["observability"]["source_fallback"]["elapsed_ms"],
        cold["observability"]["source_fallback"]["symbol_index"]["parsed_files"],
        warm["observability"]["source_fallback"]["elapsed_ms"],
        warm["observability"]["source_fallback"]["symbol_index"]["cache_hits"],
    );

    for response in [&cold, &warm] {
        let source = &response["observability"]["source_fallback"];
        assert!(
            source["listed_files"].as_u64().unwrap() >= 2_000,
            "{source}"
        );
        assert_eq!(source["complete"], true, "{source}");
        assert!(source["reason"].is_null());
        assert_eq!(response["observability"]["readiness"]["status"], "ready");
        assert_eq!(response["safe_to_use_as_answer"], false);
        assert_eq!(response["truncated"], true, "more than 8 files match");
        let paths = hint_paths(response);
        assert_eq!(paths.len(), 8);
        assert_eq!(paths[0], "src/net/limits.py", "{paths:?}");
        let first = &response["navigation_hints"][0]["evidence"];
        assert_eq!(first["symbol_match"]["name"], "retry_throttle_window");
        assert_eq!(first["line_refs"][0]["line"], 10);
        for (rank, path) in paths.iter().enumerate() {
            if path.starts_with("vendor/") || path.starts_with("docs/") {
                assert!(rank > 0, "{paths:?}");
            }
        }
    }
    assert_eq!(hint_paths(&cold), hint_paths(&warm));
    let warm_index = &warm["observability"]["source_fallback"]["symbol_index"];
    assert_eq!(warm_index["parsed_files"], 0, "{warm_index}");
    assert!(warm_index["cache_hits"].as_u64().unwrap() > 0);
    // The index lives under the host cache directory, never in the repo.
    assert!(!root.join(".aethyme").exists());
    assert!(cache.path().join("symbol-index").is_dir());
}

#[test]
fn a_tiny_budget_reports_incomplete_with_its_reason() {
    let repo = tempfile::tempdir().unwrap();
    generated_repo(repo.path(), 200);
    let response = graph_unavailable_response_with(
        repo.path(),
        "retry throttle window",
        "task_localization_query",
        "explicit",
        "missing",
        "fixture".into(),
        &SourceSearchOptions {
            budget: Duration::from_millis(1),
            symbol_cache: SymbolCache::Disabled,
            ..SourceSearchOptions::default()
        },
    );
    let json = serde_json::to_value(&response).unwrap();
    let source = &json["observability"]["source_fallback"];
    assert_eq!(source["complete"], false, "{source}");
    assert_eq!(source["reason"], "time_budget_exhausted");
    assert_eq!(json["observability"]["readiness"]["status"], "partial");
    assert_eq!(json["truncated"], true);
}

#[test]
fn answer_json_feeds_verify_targets_with_definition_spans() {
    let repo = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let root = repo.path();
    generated_repo(root, 40);
    let (response, _) = explore(root, cache.path(), "retry throttle window");
    let saved = cache.path().join("explore.json");
    std::fs::write(&saved, serde_json::to_vec(&response).unwrap()).unwrap();
    let output = Command::new(engine_bin())
        .args([
            "verify-targets",
            "--repo",
            root.to_str().unwrap(),
            "--from",
            saved.to_str().unwrap(),
            "--max-targets",
            "1",
            "--max-lines",
            "40",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let target = &report["targets"][0];
    assert_eq!(target["path"], "src/net/limits.py", "{report}");
    let lines = target["lines"].as_array().unwrap();
    assert!(
        lines.iter().any(|line| line["text"]
            .as_str()
            .unwrap()
            .contains("def retry_throttle_window")),
        "{report}"
    );
}
