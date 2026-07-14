//! Integration tests for the `aethyme-graph-index` CLI binary.
//!
//! These spawn the binary as a subprocess so they exercise the
//! full argv → exit-code path that downstream consumers (shells,
//! CI, eval harnesses) will see. The binary path comes from
//! Cargo's standard `CARGO_BIN_EXE_<name>` env var which is set
//! during `cargo test`.

use std::path::Path;
use std::process::Command;

fn bin_path() -> &'static str {
    env!("CARGO_BIN_EXE_aethyme-graph-index")
}

fn write(root: &Path, rel: &str, content: &[u8]) {
    let full = root.join(rel);
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(full, content).unwrap();
}

#[test]
fn binary_exits_zero_on_success() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/cli.py", b"def hello(): pass\n");

    let output = Command::new(bin_path())
        .arg("--repo-root")
        .arg(tmp.path())
        .arg("--repo-name")
        .arg("testrepo")
        .arg("--engine-version")
        .arg("0.1.0")
        .output()
        .expect("failed to spawn aethyme-graph-index");

    assert!(
        output.status.success(),
        "binary failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn binary_creates_aethyme_layout() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/cli.py", b"def hello(): pass\n");

    let output = Command::new(bin_path())
        .arg("--repo-root")
        .arg(tmp.path())
        .arg("--repo-name")
        .arg("testrepo")
        .arg("--engine-version")
        .arg("0.1.0")
        .output()
        .unwrap();
    assert!(output.status.success());

    // Expected layout per the Phase 2 storage spec.
    assert!(tmp.path().join(".aethyme/graph").is_dir());
    assert!(tmp.path().join(".aethyme/graph/_index").is_dir());
    assert!(tmp.path().join(".aethyme/graph/.gitattributes").is_file());
    assert!(tmp.path().join(".aethyme/engine-version").is_file());
    assert!(tmp.path().join(".aethyme/graph/src/cli.py.bin").is_file());
}

#[test]
fn binary_text_output_includes_per_kind_counts() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/cli.py",
        b"def f(): pass\nclass C:\n    def m(self): pass\n",
    );

    let output = Command::new(bin_path())
        .arg("--repo-root")
        .arg(tmp.path())
        .arg("--repo-name")
        .arg("testrepo")
        .arg("--engine-version")
        .arg("0.1.0")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("indexed 1 files"));
    assert!(stdout.contains("nodes by kind"));
    assert!(stdout.contains("function = "));
    assert!(stdout.contains("class = "));
    assert!(stdout.contains("method = "));
}

#[test]
fn binary_json_output_parses_as_object() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/cli.py", b"def f(): pass\n");

    let output = Command::new(bin_path())
        .arg("--repo-root")
        .arg(tmp.path())
        .arg("--repo-name")
        .arg("testrepo")
        .arg("--engine-version")
        .arg("0.1.0")
        .arg("--json")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Hand-rolled JSON must round-trip through serde_json.
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("CLI --json output must parse as JSON");
    assert_eq!(parsed["total_files"], 1);
    assert!(parsed["counts_by_kind"]["function"].as_u64().unwrap() >= 1);
}

#[test]
fn binary_skip_bootstrap_does_not_overwrite_engine_version() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/engine-version"),
        b"PRE-EXISTING-VERSION\n",
    )
    .unwrap();
    std::fs::create_dir_all(tmp.path().join(".aethyme/graph/_index")).unwrap();

    write(tmp.path(), "src/cli.py", b"def f(): pass\n");

    let output = Command::new(bin_path())
        .arg("--repo-root")
        .arg(tmp.path())
        .arg("--repo-name")
        .arg("testrepo")
        .arg("--engine-version")
        .arg("0.1.0")
        .arg("--skip-bootstrap")
        .output()
        .unwrap();
    assert!(output.status.success());

    // Pre-existing engine-version file is preserved (we didn't
    // re-bootstrap).
    let v = std::fs::read_to_string(tmp.path().join(".aethyme/engine-version")).unwrap();
    assert_eq!(v.trim(), "PRE-EXISTING-VERSION");
}

#[test]
fn binary_extra_ignore_dirs_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "src/keep.py", b"def keep(): pass\n");
    write(tmp.path(), "vendor/skip.py", b"def skip(): pass\n");

    let output = Command::new(bin_path())
        .arg("--repo-root")
        .arg(tmp.path())
        .arg("--repo-name")
        .arg("testrepo")
        .arg("--engine-version")
        .arg("0.1.0")
        .arg("--extra-ignore")
        .arg("vendor")
        .arg("--json")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["total_files"], 1);
}

#[test]
fn binary_rejects_relative_repo_root() {
    // IndexerContext requires an absolute path. The binary should
    // surface that as a non-zero exit, not crash.
    let output = Command::new(bin_path())
        .arg("--repo-root")
        .arg("relative/path")
        .arg("--repo-name")
        .arg("testrepo")
        .arg("--engine-version")
        .arg("0.1.0")
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "expected non-zero exit, got {output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.to_lowercase().contains("relative") || stderr.contains("absolute"),
        "expected an absolute-path complaint in stderr, got: {stderr}"
    );
}
