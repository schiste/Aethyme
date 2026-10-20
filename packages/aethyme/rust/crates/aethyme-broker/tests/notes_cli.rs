mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{FixtureRepository, open_fixture_repository_broker};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn git(repo: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn child_command(fixture: &FixtureRepository, repo: &Path, args: &[&str]) -> Command {
    let owner = fixture
        .owner_path()
        .canonicalize()
        .expect("canonicalize fixture owner");
    let repository = fixture
        .path()
        .canonicalize()
        .expect("canonicalize fixture repository");
    let host_state = common::fixture_host_state_dir(&owner);
    let mut command = common::broker_cli_for_fixture(CLI, args, &owner);
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
    command.current_dir(repo);
    command
}

fn fixture() -> (FixtureRepository, PathBuf, PathBuf) {
    let fixture = FixtureRepository::new();
    let repo = fixture.path();
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-qm", "init"]);
    let sender = repo.join(".aethyme/worktrees/sender");
    let recipient = repo.join(".aethyme/worktrees/recipient");
    std::fs::create_dir_all(sender.parent().unwrap()).unwrap();
    git(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent/sender",
            sender.to_str().unwrap(),
            "main",
        ],
    );
    git(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent/recipient",
            recipient.to_str().unwrap(),
            "main",
        ],
    );
    (fixture, sender, recipient)
}

#[test]
fn notes_are_local_bounded_recipient_scoped_and_redacted_from_events() {
    let (fixture, sender_worktree, recipient_worktree) = fixture();
    let mut broker = open_fixture_repository_broker(&fixture).unwrap();
    let sender = broker.adopt(&sender_worktree, Some("sender")).unwrap();
    let recipient = broker
        .adopt(&recipient_worktree, Some("recipient"))
        .unwrap();
    let mut send_command = child_command(
        &fixture,
        &sender_worktree,
        &[
            "advanced",
            "note",
            "send",
            "--session",
            &sender.id.to_string(),
            "--to-session",
            &recipient.id.to_string(),
            "--message",
            "coordinate src/router.rs before editing",
            "--json",
        ],
    );
    drop(broker);

    let sender_id = sender.id.to_string();
    let recipient_id = recipient.id.to_string();
    let sent = send_command.output().unwrap();
    assert!(
        sent.status.success(),
        "{}",
        String::from_utf8_lossy(&sent.stderr)
    );
    let sent: serde_json::Value = serde_json::from_slice(&sent.stdout).unwrap();
    let note_id = sent["id"].as_i64().unwrap();
    assert_eq!(sent["sender_session_id"], sender.id);
    assert_eq!(sent["recipient_session_id"], recipient.id);
    assert_eq!(sent["acknowledged_at"], serde_json::Value::Null);

    let mut broker = open_fixture_repository_broker(&fixture).unwrap();
    let event = broker
        .store()
        .events_after_filtered(0, 10, Some("session.note.sent"))
        .unwrap()
        .pop()
        .unwrap();
    let payload = event.payload_json.unwrap();
    assert!(payload.contains(&format!("\"note_id\":{note_id}")));
    assert!(!payload.contains("coordinate"), "{payload}");
    let mut agents_command = child_command(
        &fixture,
        &recipient_worktree,
        &["advanced", "agents", "--json"],
    );
    drop(broker);

    let surfaced = agents_command.output().unwrap();
    let surfaced_stderr = String::from_utf8_lossy(&surfaced.stderr);
    assert!(surfaced.status.success(), "{surfaced_stderr}");
    assert!(surfaced_stderr.contains(&format!("Unread broker note {note_id}")));
    assert!(surfaced_stderr.contains("coordinate src/router.rs before editing"));
    assert!(surfaced_stderr.contains(&format!("--session {} --id {note_id}", recipient.id)));

    let broker = open_fixture_repository_broker(&fixture).unwrap();
    let mut listed_command = child_command(
        &fixture,
        &recipient_worktree,
        &[
            "advanced",
            "note",
            "list",
            "--session",
            &recipient_id,
            "--json",
        ],
    );
    let mut wrong_recipient_command = child_command(
        &fixture,
        &sender_worktree,
        &[
            "advanced",
            "note",
            "ack",
            "--session",
            &sender_id,
            "--id",
            &note_id.to_string(),
        ],
    );
    let mut acknowledged_command = child_command(
        &fixture,
        &recipient_worktree,
        &[
            "advanced",
            "note",
            "ack",
            "--session",
            &recipient_id,
            "--id",
            &note_id.to_string(),
            "--json",
        ],
    );
    let mut quiet_command = child_command(
        &fixture,
        &recipient_worktree,
        &["advanced", "agents", "--json"],
    );
    let mut multiline_command = child_command(
        &fixture,
        &sender_worktree,
        &[
            "advanced",
            "note",
            "send",
            "--session",
            &sender_id,
            "--to-session",
            &recipient_id,
            "--message",
            "forged\noutput",
        ],
    );
    drop(broker);
    let listed = listed_command.output().unwrap();
    let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed["unread_count"], 1);
    assert_eq!(listed["notes"][0]["id"], note_id);

    let wrong_recipient = wrong_recipient_command.output().unwrap();
    assert!(!wrong_recipient.status.success());
    assert!(String::from_utf8_lossy(&wrong_recipient.stderr).contains("belongs to session"));

    let acknowledged = acknowledged_command.output().unwrap();
    assert!(acknowledged.status.success());
    let acknowledged: serde_json::Value = serde_json::from_slice(&acknowledged.stdout).unwrap();
    assert!(acknowledged["acknowledged_at"].as_i64().is_some());

    let quiet = quiet_command.output().unwrap();
    assert!(!String::from_utf8_lossy(&quiet.stderr).contains("Unread broker note"));

    let multiline = multiline_command.output().unwrap();
    assert!(!multiline.status.success());
    assert!(String::from_utf8_lossy(&multiline.stderr).contains("one line of plain text"));

    let mut broker = open_fixture_repository_broker(&fixture).unwrap();
    broker.close(recipient.id).unwrap();
    let closed = broker
        .send_session_note(sender.id, recipient.id, "too late")
        .unwrap_err()
        .to_string();
    assert!(closed.contains("recipient session") && closed.contains("is closed"));
}
