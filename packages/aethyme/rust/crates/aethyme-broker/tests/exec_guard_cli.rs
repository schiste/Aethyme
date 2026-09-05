//! `broker exec` must distinguish its two failure causes (#133).
//!
//! `ok` is `command_success && audit.ok`, so a wrapped command that exits
//! non-zero and a guard that refuses an out-of-ownership write are separate
//! events. Reporting both as an ownership failure sends the reader to debug
//! leases when the command simply failed on its own.

use std::path::Path;
use std::process::{Command, Output};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}");
}

fn run(repo: &Path, state: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
        .current_dir(repo)
        .env("AETHYME_HOST_STATE_DIR", state)
        .output()
        .unwrap()
}

fn fixture() -> (tempfile::TempDir, tempfile::TempDir, String) {
    let repo = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(repo.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    std::fs::write(repo.path().join("owned.txt"), "owned\n").unwrap();
    std::fs::write(repo.path().join("other.txt"), "other\n").unwrap();
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-qm", "init"]);
    let started = run(
        repo.path(),
        state.path(),
        &["start", "--task", "exec guard", "--path", "owned.txt", "--json"],
    );
    assert!(started.status.success(), "start: {}", String::from_utf8_lossy(&started.stderr));
    let session: serde_json::Value = serde_json::from_slice(&started.stdout).unwrap();
    let id = session["id"].as_i64().unwrap().to_string();
    let worktree = session["worktree_path"].as_str().unwrap().to_string();
    (repo, state, format!("{id}\u{1}{worktree}"))
}

fn split(v: &str) -> (&str, &str) {
    let mut parts = v.split('\u{1}');
    (parts.next().unwrap(), parts.next().unwrap())
}

/// The command fails on its own and touches nothing: the message must not
/// blame ownership, because the guard rejected nothing.
#[test]
fn a_command_that_fails_on_its_own_is_not_reported_as_an_ownership_failure() {
    let (repo, state, packed) = fixture();
    let (id, worktree) = split(&packed);
    let out = Command::new(CLI)
        .args(["exec", "--session", id, "--", "sh", "-c", "exit 3"])
        .current_dir(worktree)
        .env("AETHYME_HOST_STATE_DIR", state.path())
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "the command failed, so exec must fail: {text}");
    assert!(
        text.contains("exited 3"),
        "the wrapped command's exit code is the useful fact: {text}"
    );
    assert!(
        text.contains("no ownership violation"),
        "the message must clear the guard rather than implicate it: {text}"
    );
    assert!(
        !text.contains("failed ownership or command checks"),
        "the conflated message must be gone: {text}"
    );
    drop(repo);
}

/// The command succeeds but writes outside the session's leases: that is a
/// genuine guard refusal and must say so.
#[test]
fn a_write_outside_ownership_is_reported_as_a_refusal() {
    let (repo, state, packed) = fixture();
    let (id, worktree) = split(&packed);
    let out = Command::new(CLI)
        .args([
            "exec", "--session", id, "--", "sh", "-c",
            "printf changed >> other.txt",
        ])
        .current_dir(worktree)
        .env("AETHYME_HOST_STATE_DIR", state.path())
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "an out-of-ownership write must fail: {text}");
    assert!(
        text.contains("outside") && text.contains("ownership"),
        "a real refusal must name ownership: {text}"
    );
    assert!(
        !text.contains("no ownership violation"),
        "this one really is an ownership violation: {text}"
    );
    drop(repo);
}
