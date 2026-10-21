//! `broker exec` must distinguish its two failure causes (#133).
//!
//! `ok` is `command_success && audit.ok`, so a wrapped command that exits
//! non-zero and a guard that refuses an out-of-ownership write are separate
//! events. Reporting both as an ownership failure sends the reader to debug
//! leases when the command simply failed on its own.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
mod common;

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

fn run_at(broker: &common::FixtureBroker, cwd: &Path, args: &[&str]) -> Output {
    let mut command = common::broker_cli(CLI, args);
    broker
        .configure_storage_context_for_child(&mut command)
        .unwrap();
    command.current_dir(cwd).output().unwrap()
}

fn fixture() -> (
    common::FixtureRepository,
    common::FixtureBroker,
    i64,
    PathBuf,
) {
    let repository = common::FixtureRepository::from_tempdir(tempfile::tempdir().unwrap());
    let repo = repository.path();
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    std::fs::write(repo.join("owned.txt"), "owned\n").unwrap();
    std::fs::write(repo.join("other.txt"), "other\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-qm", "init"]);

    let mut broker = common::open_fixture_repository_broker(&repository).unwrap();
    let session = broker
        .start_worktree_with_planned_paths("exec guard", &["owned.txt".to_owned()], None)
        .unwrap()
        .session;
    let worktree = PathBuf::from(&session.worktree_path);
    assert!(
        worktree.starts_with(broker.worktree_root()),
        "library-created checkout must use the fixture's serialized worktree root"
    );
    (repository, broker, session.id, worktree)
}

/// The command fails on its own and touches nothing: the message must not
/// blame ownership, because the guard rejected nothing.
#[test]
fn a_command_that_fails_on_its_own_is_not_reported_as_an_ownership_failure() {
    let (_repository, broker, session_id, worktree) = fixture();
    let id = session_id.to_string();
    let out = run_at(
        &broker,
        &worktree,
        &[
            "advanced",
            "exec",
            "--session",
            &id,
            "--",
            "sh",
            "-c",
            "exit 3",
        ],
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "the command failed, so exec must fail: {text}"
    );
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
}

/// The command succeeds but writes outside the session's leases: that is a
/// genuine guard refusal and must say so.
#[test]
fn a_write_outside_ownership_is_reported_as_a_refusal() {
    let (_repository, broker, session_id, worktree) = fixture();
    let id = session_id.to_string();
    let out = run_at(
        &broker,
        &worktree,
        &[
            "advanced",
            "exec",
            "--session",
            &id,
            "--",
            "sh",
            "-c",
            "printf changed >> other.txt; touch runtime.sock",
        ],
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "an out-of-ownership write must fail: {text}"
    );
    assert!(
        text.contains("outside") && text.contains("ownership"),
        "a real refusal must name ownership: {text}"
    );
    assert!(
        !text.contains("no ownership violation"),
        "this one really is an ownership violation: {text}"
    );
    let events = broker.store().events_after(0, i64::MAX).unwrap();
    let out_of_lease = events
        .iter()
        .find(|event| event.kind == "guard.out_of_lease_write")
        .expect("out-of-lease evidence is durable");
    assert!(
        out_of_lease
            .payload_json
            .as_deref()
            .is_some_and(|payload| payload.contains("other.txt") && !payload.contains("printf"))
    );
    let worktree = worktree.to_string_lossy();
    let untracked = events
        .iter()
        .find(|event| event.kind == "guard.untracked_artifact")
        .expect("new untracked artifact evidence is durable");
    assert!(untracked.payload_json.as_deref().is_some_and(
        |payload| payload.contains("runtime.sock") && !payload.contains(worktree.as_ref())
    ));
}
