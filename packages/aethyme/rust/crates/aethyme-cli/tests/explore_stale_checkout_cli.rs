//! `aethyme explore` on a checkout behind the code that ships (#232).
//!
//! The primary checkout trails `aethyme/integration` and the fetched
//! upstream by construction, and an agent that researches there reports
//! already-fixed work as missing. What this holds: a checkout behind a local
//! fresher ref gets one stderr warning, a `source_staleness` object and a
//! withdrawn `safe_to_use_as_answer`; the brief says so on its second line; a
//! current checkout, or one with nothing to compare against, is untouched.

use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::Value;

const REQUEST: &str = "where is handleSessionStart";

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// A repository on `main` with two commits; returns (dir, first, second).
fn repository() -> (tempfile::TempDir, String, String) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path();
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(
        repo.join("src/session_hook.ts"),
        "export function handleSessionStart(event: string) {\n  return event;\n}\n",
    )
    .unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "first"]);
    let first = git(repo, &["rev-parse", "HEAD"]);
    std::fs::write(repo.join("src/fixed.ts"), "export const fixed = true;\n").unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-q", "-m", "second"]);
    let second = git(repo, &["rev-parse", "HEAD"]);
    (tmp, first, second)
}

fn explore(repo: &Path, format: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aethyme"))
        .arg("explore")
        .arg("--repo")
        .arg(repo)
        .args(["--request", REQUEST, "--format", format])
        .current_dir(repo)
        .env_remove("AETHYME_REPO")
        .stdin(Stdio::null())
        .output()
        .expect("run aethyme explore")
}

fn answer(repo: &Path) -> (Value, String) {
    let output = explore(repo, "answer-json");
    assert!(
        output.status.success(),
        "explore failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value = serde_json::from_slice(&output.stdout).expect("answer-json");
    (value, String::from_utf8_lossy(&output.stderr).into_owned())
}

#[test]
fn a_primary_checkout_behind_the_upstream_is_marked_stale() {
    let (tmp, first, second) = repository();
    let repo = tmp.path();
    git(repo, &["update-ref", "refs/remotes/origin/main", &second]);
    git(repo, &["reset", "-q", "--hard", &first]);

    let (value, stderr) = answer(repo);
    assert!(
        stderr.contains("warning: this checkout is 1 commit(s) behind origin/main"),
        "{stderr}"
    );
    let stale = &value["source_staleness"];
    assert_eq!(stale["behind"], 1, "{value:#}");
    assert_eq!(stale["reference"], "origin/main");
    assert_eq!(stale["head"], first.as_str());
    assert_eq!(stale["linked_worktree"], false);
    assert!(
        stale["suggestion"]
            .as_str()
            .unwrap()
            .contains("aethyme broker start"),
        "{stale:#}"
    );
    assert_eq!(value["safe_to_use_as_answer"], false);
    assert_eq!(value["trust_policy"]["safe_to_use_as_answer"], false);

    let brief = explore(repo, "brief");
    assert!(brief.status.success());
    let text = String::from_utf8_lossy(&brief.stdout);
    let second_line = text.lines().nth(1).unwrap_or_default();
    assert!(
        second_line.starts_with("Stale checkout: 1 commit(s) behind origin/main"),
        "{text}"
    );
}

#[test]
fn a_session_worktree_behind_integration_is_told_to_sync() {
    let (tmp, first, second) = repository();
    let repo = tmp.path();
    git(repo, &["branch", "aethyme/integration", &second]);
    let worktree = tmp.path().join("session");
    git(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            worktree.to_str().unwrap(),
            &first,
        ],
    );

    let (value, stderr) = answer(&worktree);
    assert!(stderr.contains("behind aethyme/integration"), "{stderr}");
    let stale = &value["source_staleness"];
    assert_eq!(stale["reference"], "aethyme/integration", "{value:#}");
    assert_eq!(stale["linked_worktree"], true);
    assert!(
        stale["suggestion"]
            .as_str()
            .unwrap()
            .contains("aethyme broker sync"),
        "{stale:#}"
    );
}

#[test]
fn a_current_checkout_is_left_alone() {
    let (tmp, _, second) = repository();
    let repo = tmp.path();
    git(repo, &["update-ref", "refs/remotes/origin/main", &second]);
    git(repo, &["branch", "aethyme/integration", &second]);

    let (value, stderr) = answer(repo);
    assert!(!stderr.contains("behind"), "{stderr}");
    assert!(value.get("source_staleness").is_none(), "{value:#}");
}

#[test]
fn a_checkout_with_nothing_to_compare_against_is_left_alone() {
    let (tmp, _, _) = repository();
    let (value, stderr) = answer(tmp.path());
    assert!(!stderr.contains("behind"), "{stderr}");
    assert!(value.get("source_staleness").is_none(), "{value:#}");

    // Not a Git checkout at all: the check stays silent rather than failing.
    let plain = tempfile::tempdir().unwrap();
    std::fs::write(plain.path().join("a.ts"), "export const a = 1;\n").unwrap();
    let (value, stderr) = answer(plain.path());
    assert!(!stderr.contains("behind"), "{stderr}");
    assert!(value.get("source_staleness").is_none());
}
