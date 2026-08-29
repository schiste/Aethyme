//! Implementation-blind CLI tests for `aethyme autofix`.
//!
//! Ported verbatim from `tests/local/test_autofix_cli.py`
//! (python-retirement Phase 7), which itself replaced `tests/autofixers/`
//! after the Phase 5 native flip. Safety-engine, patch, fixer, and
//! PR-helper behaviour is covered by the Rust unit tests in
//! `aethyme-quality`.
//!
//! Nothing here touches a real remote: the PR-mode test runs outside a
//! git repository.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use aethyme_testkit::repos::{build_clean_repo, build_fixable_repo, read};
use aethyme_testkit::{invoke_aethyme, tmp_dir};

/// Every file in `root`, keyed by path — the Rust equivalent of the
/// Python `{p: p.read_bytes() for p in repo.rglob("*") if p.is_file()}`
/// before/after snapshot.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read fixture dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.insert(
                    path.clone(),
                    std::fs::read(&path).expect("read fixture file"),
                );
            }
        }
    }
    files
}

#[test]
fn dry_run_is_the_default_mode() {
    let tmp = tmp_dir();
    let result = invoke_aethyme([
        "autofix",
        &build_fixable_repo(tmp.path()).display().to_string(),
    ]);
    result.ok();
    result.assert_contains("Mode: DRY RUN");
    result.assert_contains("Changes Preview (Dry Run)");
    result.assert_contains("Run with --apply to apply these changes");
}

#[test]
fn dry_run_writes_nothing() {
    let tmp = tmp_dir();
    let repo = build_fixable_repo(tmp.path());
    let before = snapshot(&repo);
    invoke_aethyme(["autofix", &repo.display().to_string(), "--dry-run"]).ok();
    assert_eq!(before, snapshot(&repo));
}

#[test]
fn summary_reports_risk_buckets() {
    let tmp = tmp_dir();
    let result = invoke_aethyme([
        "autofix",
        &build_fixable_repo(tmp.path()).display().to_string(),
    ]);
    for line in ["Total files:", "Low risk:", "Medium risk:", "High risk:"] {
        result.assert_contains(line);
    }
}

#[test]
fn clean_repo_reports_no_fixes() {
    let tmp = tmp_dir();
    let result = invoke_aethyme([
        "autofix",
        &build_clean_repo(tmp.path()).display().to_string(),
    ]);
    result.ok();
    result.assert_contains("No fixes needed!");
}

#[test]
fn fix_type_selects_a_single_scanner() {
    let tmp = tmp_dir();
    let repo = build_fixable_repo(tmp.path());
    let result = invoke_aethyme(["autofix", &repo.display().to_string(), "--fix-type", "docs"]);
    result.ok();
    result.assert_contains("Scanning for documentation issues...");
    result.assert_lacks("Scanning for link issues...");
}

#[test]
fn every_fix_type_choice_runs() {
    let tmp = tmp_dir();
    let repo = build_fixable_repo(tmp.path());
    for choice in ["all", "docs", "links", "selectors", "i18n", "format"] {
        let result = invoke_aethyme(["autofix", &repo.display().to_string(), "--fix-type", choice]);
        assert_eq!(
            result.exit_code, 0,
            "--fix-type {choice}:\n{}",
            result.output
        );
    }
}

/// The gate: newly created docs are medium risk, so `--apply` stops.
#[test]
fn apply_requires_approval_and_writes_nothing() {
    let tmp = tmp_dir();
    let repo = build_fixable_repo(tmp.path());
    let before = snapshot(&repo);
    let result = invoke_aethyme([
        "autofix",
        &repo.display().to_string(),
        "--apply",
        "--fix-type",
        "docs",
    ]);
    // stdin is closed, so the confirmation prompt aborts.
    result.expect_code(1);
    result.assert_contains("files require approval");
    result.assert_contains("Aborted!");
    assert_eq!(before, snapshot(&repo));
}

#[test]
fn skip_approval_applies_the_patches() {
    let tmp = tmp_dir();
    let repo = build_fixable_repo(tmp.path());
    let result = invoke_aethyme([
        "autofix",
        &repo.display().to_string(),
        "--apply",
        "--skip-approval",
        "--fix-type",
        "docs",
    ]);
    result.ok();
    result.assert_contains("Applied ");
    assert!(repo.join("src/FOLDER.md").exists());
}

#[test]
fn protected_paths_are_never_patched() {
    let tmp = tmp_dir();
    let repo = build_fixable_repo(tmp.path());
    invoke_aethyme([
        "autofix",
        &repo.display().to_string(),
        "--apply",
        "--skip-approval",
    ])
    .ok();
    // A lockfile, a manifest, and a build directory: no FOLDER.md may
    // appear next to them, and their contents are untouched.
    assert_eq!(read(repo.join("package.json")), "{}\n");
    assert_eq!(read(repo.join("Cargo.lock")), "# lock\n");
    assert!(!repo.join("node_modules/pkg/FOLDER.md").exists());
}

#[test]
fn dry_run_wins_over_apply() {
    let tmp = tmp_dir();
    let repo = build_fixable_repo(tmp.path());
    let result = invoke_aethyme([
        "autofix",
        &repo.display().to_string(),
        "--dry-run",
        "--apply",
    ]);
    result.ok();
    result.assert_contains("Mode: DRY RUN");
    assert!(!repo.join("src/FOLDER.md").exists());
}

#[test]
fn pr_mode_refuses_a_dirty_working_tree() {
    // Not a git repository at all: `git status` fails, which the command
    // reports as an unclean tree.
    let tmp = tmp_dir();
    let result = invoke_aethyme([
        "autofix",
        &build_fixable_repo(tmp.path()).display().to_string(),
        "--pr",
    ]);
    result.ok();
    result.assert_contains("Working tree has uncommitted changes");
}

#[test]
fn missing_argument_is_a_usage_error() {
    let result = invoke_aethyme(["autofix"]);
    result.expect_code(2);
    result.assert_contains("Missing argument 'REPO_PATH'.");
}

#[test]
fn invalid_repo_path() {
    let result = invoke_aethyme(["autofix", "/nonexistent/path"]);
    result.expect_code(2);
    result.assert_contains("does not exist");
}

#[test]
fn invalid_fix_type() {
    let tmp = tmp_dir();
    let result = invoke_aethyme([
        "autofix",
        &build_clean_repo(tmp.path()).display().to_string(),
        "--fix-type",
        "bogus",
    ]);
    result.expect_code(2);
    result.assert_contains("is not one of 'all', 'docs', 'links', 'selectors', 'i18n', 'format'.");
}

#[test]
fn help_names_the_options() {
    let result = invoke_aethyme(["autofix", "--help"]);
    result.ok();
    for flag in [
        "--dry-run",
        "--apply",
        "--pr",
        "--fix-type",
        "--skip-approval",
    ] {
        result.assert_contains(flag);
    }
}
