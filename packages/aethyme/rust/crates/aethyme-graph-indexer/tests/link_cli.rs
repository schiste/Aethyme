//! Integration tests for the `aethyme-graph-link` CLI binary.

use std::path::Path;
use std::process::Command;

fn index_bin() -> &'static str {
    env!("CARGO_BIN_EXE_aethyme-graph-index")
}

fn link_bin() -> &'static str {
    env!("CARGO_BIN_EXE_aethyme-graph-link")
}

fn write(root: &Path, rel: &str, content: &[u8]) {
    let full = root.join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, content).unwrap();
}

fn build_indexed_repo_skipping_link() -> tempfile::TempDir {
    // Index but DON'T run the auto-linker — so placeholders remain
    // on disk and the standalone link binary has work to do.
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "cli.py", b"from util import helper\n");
    write(tmp.path(), "util.py", b"def helper():\n    return 1\n");
    let status = Command::new(index_bin())
        .arg("--repo-root")
        .arg(tmp.path())
        .arg("--repo-name")
        .arg("test")
        .arg("--engine-version")
        .arg("0.1.0")
        .arg("--skip-link")
        .status()
        .unwrap();
    assert!(status.success(), "index --skip-link should succeed");
    tmp
}

#[test]
fn link_cli_resolves_placeholders_text_output() {
    let tmp = build_indexed_repo_skipping_link();
    let output = Command::new(link_bin())
        .arg("--repo-root")
        .arg(tmp.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "link CLI failed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("resolved 1/1 placeholders"),
        "stdout = {stdout}"
    );
    assert!(stdout.contains("rewrote 1 edges"), "stdout = {stdout}");
}

#[test]
fn link_cli_json_output_carries_summary() {
    let tmp = build_indexed_repo_skipping_link();
    let output = Command::new(link_bin())
        .arg("--repo-root")
        .arg(tmp.path())
        .arg("--json")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"placeholders_resolved\":1"));
    assert!(stdout.contains("\"edges_rewritten\":1"));
    assert!(stdout.contains("\"orphans_removed\":1"));
}

#[test]
fn link_cli_against_missing_layout_exits_nonzero() {
    let tmp = tempfile::tempdir().unwrap();
    // No .aethyme/graph/ here — link must fail cleanly.
    let output = Command::new(link_bin())
        .arg("--repo-root")
        .arg(tmp.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("aethyme-graph-link:"));
}

#[test]
fn index_binary_auto_links_by_default() {
    // Without --skip-link, indexing should produce resolved
    // fragments. We verify this by checking the auto-link stderr
    // message.
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "cli.py", b"from util import helper\n");
    write(tmp.path(), "util.py", b"def helper():\n    return 1\n");
    let output = Command::new(index_bin())
        .arg("--repo-root")
        .arg(tmp.path())
        .arg("--repo-name")
        .arg("test")
        .arg("--engine-version")
        .arg("0.1.0")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("linker resolved 1/1 placeholders"),
        "stderr = {stderr}"
    );
}

#[test]
fn index_binary_skip_link_leaves_placeholders() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "cli.py", b"from util import helper\n");
    write(tmp.path(), "util.py", b"def helper():\n    return 1\n");
    let output = Command::new(index_bin())
        .arg("--repo-root")
        .arg(tmp.path())
        .arg("--repo-name")
        .arg("test")
        .arg("--engine-version")
        .arg("0.1.0")
        .arg("--skip-link")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    // No "linker resolved" message when skip is set.
    assert!(
        !stderr.contains("linker resolved"),
        "skip-link should suppress link output; stderr = {stderr}"
    );
}

#[test]
fn index_binary_json_includes_link_section_when_linking() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "cli.py", b"from util import helper\n");
    write(tmp.path(), "util.py", b"def helper():\n    return 1\n");
    let output = Command::new(index_bin())
        .arg("--repo-root")
        .arg(tmp.path())
        .arg("--repo-name")
        .arg("test")
        .arg("--engine-version")
        .arg("0.1.0")
        .arg("--json")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"link\":"));
    assert!(stdout.contains("\"placeholders_resolved\":1"));
}

#[test]
fn index_binary_skip_link_omits_link_section_in_json() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "cli.py", b"from util import helper\n");
    write(tmp.path(), "util.py", b"def helper():\n    return 1\n");
    let output = Command::new(index_bin())
        .arg("--repo-root")
        .arg(tmp.path())
        .arg("--repo-name")
        .arg("test")
        .arg("--engine-version")
        .arg("0.1.0")
        .arg("--json")
        .arg("--skip-link")
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The JSON must not include a "link" section when linking is
    // suppressed.
    assert!(!stdout.contains("\"link\":"));
}
