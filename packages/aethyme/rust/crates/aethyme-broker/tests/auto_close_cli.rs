//! A command run on a session the broker closed automatically names that
//! close and says how to continue, instead of a bare refusal.

mod common;

use std::path::Path;
use std::process::Command;

use aethyme_broker::Broker;
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
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn child_command(broker: &Broker, dir: &Path, args: &[&str]) -> Command {
    let mut command = common::broker_cli(CLI, args);
    command.current_dir(dir);
    broker
        .configure_storage_context_for_child(&mut command)
        .unwrap();
    command
}

#[test]
fn a_command_on_an_automatically_closed_session_names_the_close() {
    let fixture = FixtureRepository::new();
    let repo = fixture.path();
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-qm", "init"]);
    let worktree = repo.join(".aethyme/worktrees/closed");
    std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    git(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent/closed",
            worktree.to_str().unwrap(),
            "main",
        ],
    );
    let mut broker = open_fixture_repository_broker(&fixture).unwrap();
    let session = broker
        .adopt(&worktree, Some("closed by the broker"))
        .unwrap();
    broker.store().finish_session(session.id, "{}").unwrap();
    broker
        .store()
        .append_event(
            "session.abandoned",
            Some(session.id),
            Some(
                &serde_json::json!({
                    "idle_ms": 80 * 3_600_000_i64,
                    "abandon_after_ms": 72 * 3_600_000_i64,
                    "close_reason": "inactive_no_pull_request",
                    "unlanded_commits": false,
                })
                .to_string(),
            ),
        )
        .unwrap();
    let id = session.id.to_string();
    let mut explicit = child_command(&broker, repo, &["sync", "--session", &id]);
    let mut from_checkout = child_command(&broker, &worktree, &["sync"]);
    drop(broker);

    let refused = explicit.output().unwrap();
    assert!(!refused.status.success());
    // The refusal is printed on stderr, or inside the JSON error envelope.
    let stderr = format!(
        "{}{}",
        String::from_utf8_lossy(&refused.stderr),
        String::from_utf8_lossy(&refused.stdout)
    );
    assert!(
        stderr.contains(&format!("the broker closed session {id} automatically")),
        "{stderr}"
    );
    assert!(stderr.contains("no pull request was recorded"), "{stderr}");
    assert!(stderr.contains("from inside that checkout"), "{stderr}");
    assert!(stderr.contains("broker start --reuse"), "{stderr}");

    // Without `--session`, the closed session does not resolve from its
    // checkout; the command runs and a note explains the close.
    let observed = from_checkout.output().unwrap();
    let stderr = format!(
        "{}{}",
        String::from_utf8_lossy(&observed.stderr),
        String::from_utf8_lossy(&observed.stdout)
    );
    assert!(
        stderr.contains(&format!("this checkout's session {id} is closed")),
        "{stderr}"
    );
}
