//! Integration tests for the `aethyme-graph-query` CLI.
//! Builds a tiny indexed repo via `aethyme-graph-index`, then
//! exercises the query CLI against it.

use std::path::Path;
use std::process::Command;

fn index_bin() -> &'static str {
    env!("CARGO_BIN_EXE_aethyme-graph-index")
}

fn query_bin() -> &'static str {
    env!("CARGO_BIN_EXE_aethyme-graph-query")
}

fn write(root: &Path, rel: &str, content: &[u8]) {
    let full = root.join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, content).unwrap();
}

fn build_indexed_repo() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/cli.py",
        b"def explore_command():\n    return 'hi'\n\nclass Helper:\n    def run(self):\n        pass\n",
    );
    write(tmp.path(), "src/util.py", b"def util_fn():\n    return 1\n");
    let status = Command::new(index_bin())
        .arg("--repo-root")
        .arg(tmp.path())
        .arg("--repo-name")
        .arg("testrepo")
        .arg("--engine-version")
        .arg("0.1.0")
        .status()
        .unwrap();
    assert!(status.success(), "indexer failed");
    tmp
}

#[test]
fn find_symbol_finds_a_known_function() {
    let tmp = build_indexed_repo();
    let output = Command::new(query_bin())
        .arg("--repo-root")
        .arg(tmp.path())
        .arg("find-symbol")
        .arg("--name")
        .arg("explore_command")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("explore_command"), "stdout = {stdout}");
    assert!(stdout.contains("src/cli.py"), "stdout = {stdout}");
}

#[test]
fn find_symbol_kind_filter_works() {
    let tmp = build_indexed_repo();
    // Helper is a Class, not a Function — kind=function should
    // produce zero hits.
    let output = Command::new(query_bin())
        .arg("--repo-root")
        .arg(tmp.path())
        .arg("find-symbol")
        .arg("--name")
        .arg("Helper")
        .arg("--kind")
        .arg("function")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("no matches"), "stdout = {stdout}");
}

#[test]
fn find_symbol_reports_class_with_kind_filter() {
    let tmp = build_indexed_repo();
    let output = Command::new(query_bin())
        .arg("--repo-root")
        .arg(tmp.path())
        .arg("find-symbol")
        .arg("--name")
        .arg("Helper")
        .arg("--kind")
        .arg("class")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Helper"));
    assert!(stdout.contains("class "));
}

#[test]
fn list_modules_returns_sorted_modules() {
    let tmp = build_indexed_repo();
    let output = Command::new(query_bin())
        .arg("--repo-root")
        .arg(tmp.path())
        .arg("list-modules")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let modules: Vec<&str> = stdout.lines().collect();
    assert!(modules.contains(&"src.cli"));
    assert!(modules.contains(&"src.util"));
}

#[test]
fn list_files_returns_indexed_paths() {
    let tmp = build_indexed_repo();
    let output = Command::new(query_bin())
        .arg("--repo-root")
        .arg(tmp.path())
        .arg("list-files")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("src/cli.py"));
    assert!(stdout.contains("src/util.py"));
}

#[test]
fn list_languages_returns_python() {
    let tmp = build_indexed_repo();
    let output = Command::new(query_bin())
        .arg("--repo-root")
        .arg(tmp.path())
        .arg("list-languages")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("python"));
}

#[test]
fn counts_aggregates_across_files() {
    let tmp = build_indexed_repo();
    let output = Command::new(query_bin())
        .arg("--repo-root")
        .arg(tmp.path())
        .arg("counts")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // 2 Python files indexed → 2 File nodes
    assert!(stdout.contains("file = 2"), "stdout = {stdout}");
    // 1 top-level + 1 nested function (in `Helper.run` is a method, not function)
    // explore_command + util_fn = 2 functions
    assert!(stdout.contains("function = 2"));
    assert!(stdout.contains("class = 1"));
    assert!(stdout.contains("method = 1"));
}

#[test]
fn query_against_missing_layout_exits_nonzero() {
    let tmp = tempfile::tempdir().unwrap();
    let output = Command::new(query_bin())
        .arg("--repo-root")
        .arg(tmp.path())
        .arg("list-modules")
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[test]
fn invalid_kind_filter_exits_nonzero() {
    let tmp = build_indexed_repo();
    let output = Command::new(query_bin())
        .arg("--repo-root")
        .arg(tmp.path())
        .arg("find-symbol")
        .arg("--name")
        .arg("anything")
        .arg("--kind")
        .arg("not-a-real-kind")
        .output()
        .unwrap();
    assert!(!output.status.success());
}
