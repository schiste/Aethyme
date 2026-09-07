//! `broker status` must show a wedged coordinated operation.
//!
//! Issue #147: an operation held a repository's write lock for 47 minutes while
//! three sessions' writes parked behind it. The blocked callers could not say
//! so -- each was inside a command that never returned, having printed its one
//! wait notice to stderr before hanging. Status was the surface the reporter
//! checked, and it showed nothing, while offering a `queue` field that means
//! the merge queue. They read it as authoritative and concluded the operation
//! was merely slow.

use std::path::Path;
use std::process::{Command, Output};

use aethyme_broker::{
    Broker, NewCoordinatedOperation, OperationEffect, OperationIdentityProvenance,
    OperationProvider, OperationStatus,
};

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

fn run(repo: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap()
}

#[test]
fn status_shows_a_wedged_operation_and_what_is_parked_behind_it() {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "init"]);

    let worktree = tmp.path().join("wt");
    git(
        tmp.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "wedge",
            worktree.to_str().unwrap(),
        ],
    );

    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.adopt(&worktree, None).unwrap();
    let repository = format!("local:{}", broker.main_root().display());

    let make = |scope: &str, command: &str| NewCoordinatedOperation {
        session_id: session.id,
        provider: OperationProvider::Git,
        repository: repository.clone(),
        scope: scope.into(),
        effect: OperationEffect::Write,
        authorization_reason: Some("test".into()),
        command_json: command.into(),
        pid: 999_999,
        host_operation_id: None,
        identity_provenance: OperationIdentityProvenance::LocalRepository,
    };

    let holder = broker
        .store()
        .create_coordinated_operation(&make("repository", r#"["git","push"]"#))
        .unwrap();
    broker
        .store()
        .transition_coordinated_operation(holder.id, OperationStatus::Running, None, None)
        .unwrap();
    broker
        .store()
        .create_coordinated_operation(&make("issues/1/comments", r#"["gh","api"]"#))
        .unwrap();
    drop(broker);

    let output = run(tmp.path(), &["status"]);
    let text = String::from_utf8_lossy(&output.stdout);

    assert!(
        text.contains("Coordinated operations:"),
        "status must surface pending operations; got:\n{text}"
    );
    assert!(
        text.contains("holding"),
        "status must name the operation holding the lock; got:\n{text}"
    );
    assert!(
        text.contains(&format!("blocked by {}", holder.id)),
        "status must attribute the parked operation to its blocker; got:\n{text}"
    );
    assert!(
        text.contains("aethyme broker operations list"),
        "status must point at the surface with the full detail; got:\n{text}"
    );
}
