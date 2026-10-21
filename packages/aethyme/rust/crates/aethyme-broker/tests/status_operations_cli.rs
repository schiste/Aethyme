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
use std::process::Command;

mod common;

use aethyme_broker::{
    NewCoordinatedOperation, OperationEffect, OperationIdentityProvenance, OperationProvider,
    OperationStatus,
};
use common::{FixtureRepository, open_fixture_repository_broker};

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

#[test]
fn status_shows_a_wedged_operation_and_what_is_parked_behind_it() {
    let tmp = FixtureRepository::new();
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

    let mut broker = open_fixture_repository_broker(&tmp).unwrap();
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
        .create_coordinated_operation(&NewCoordinatedOperation {
            // Keep the parked row alive long enough for the status command
            // to observe it; broker-open reaps genuinely abandoned clients.
            pid: std::process::id() as i64,
            ..make("issues/1/comments", r#"["gh","api"]"#)
        })
        .unwrap();
    let owner = tmp
        .owner_path()
        .canonicalize()
        .expect("canonicalize fixture owner");
    let repository = tmp
        .path()
        .canonicalize()
        .expect("canonicalize fixture repository");
    let host_state = common::fixture_host_state_dir(&owner);
    let mut command = common::broker_cli_for_fixture(CLI, &["status"], &owner);
    command
        .env("AETHYME_BROKER_DB", repository.join(".aethyme/broker.db"))
        .env(
            "AETHYME_BROKER_OPEN_HOST_OPERATION_DB",
            host_state.join("host-operations.db"),
        )
        .env("AETHYME_BROKER_OPEN_HOST_STATE_EXPLICIT", "true")
        .env("AETHYME_BROKER_OPEN_WORKTREE_ROOT", owner.join("worktrees"))
        .env(
            "AETHYME_BROKER_OPEN_COLLABORATION_ROOT",
            host_state.join("collaboration"),
        );
    command.current_dir(tmp.path());
    drop(broker);

    let output = command.output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout);

    assert!(
        text.contains("Coordinated operations:"),
        "status must surface pending operations; got:\n{text}"
    );
    assert!(
        text.contains("2 unresolved, 1 holding a write lock, 1 waiting"),
        "status must distinguish the lock holder from the current waiter; got:\n{text}"
    );
    assert!(
        text.contains(&format!("lock {}", repository.display())),
        "status must name the lock resource; got:\n{text}"
    );
    assert!(
        text.contains(&format!("blocked by {}", holder.id)),
        "status must attribute the parked operation to its blocker; got:\n{text}"
    );
    assert!(
        text.contains("aethyme broker advanced operations list"),
        "status must point at the surface with the full detail; got:\n{text}"
    );
}
