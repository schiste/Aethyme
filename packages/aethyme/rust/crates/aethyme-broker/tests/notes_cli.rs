use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::Broker;

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

fn run(repo: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap()
}

fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "init"]);
    let sender = tmp.path().join(".aethyme/worktrees/sender");
    let recipient = tmp.path().join(".aethyme/worktrees/recipient");
    std::fs::create_dir_all(sender.parent().unwrap()).unwrap();
    git(
        tmp.path(),
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
        tmp.path(),
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
    (tmp, sender, recipient)
}

#[test]
fn notes_are_local_bounded_recipient_scoped_and_redacted_from_events() {
    let (tmp, sender_worktree, recipient_worktree) = fixture();
    let mut broker = Broker::open(tmp.path()).unwrap();
    let sender = broker.adopt(&sender_worktree, Some("sender")).unwrap();
    let recipient = broker
        .adopt(&recipient_worktree, Some("recipient"))
        .unwrap();
    drop(broker);

    let sender_id = sender.id.to_string();
    let recipient_id = recipient.id.to_string();
    let sent = run(
        &sender_worktree,
        &[
            "note",
            "send",
            "--session",
            &sender_id,
            "--to-session",
            &recipient_id,
            "--message",
            "coordinate src/router.rs before editing",
            "--json",
        ],
    );
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

    let mut broker = Broker::open(tmp.path()).unwrap();
    let event = broker
        .store()
        .events_after_filtered(0, 10, Some("session.note.sent"))
        .unwrap()
        .pop()
        .unwrap();
    let payload = event.payload_json.unwrap();
    assert!(payload.contains(&format!("\"note_id\":{note_id}")));
    assert!(!payload.contains("coordinate"), "{payload}");
    drop(broker);

    let surfaced = run(&recipient_worktree, &["agents", "--json"]);
    let surfaced_stderr = String::from_utf8_lossy(&surfaced.stderr);
    assert!(surfaced.status.success(), "{surfaced_stderr}");
    assert!(surfaced_stderr.contains(&format!("Unread broker note {note_id}")));
    assert!(surfaced_stderr.contains("coordinate src/router.rs before editing"));
    assert!(surfaced_stderr.contains(&format!("--session {} --id {note_id}", recipient.id)));

    let listed = run(
        &recipient_worktree,
        &["note", "list", "--session", &recipient_id, "--json"],
    );
    let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(listed["unread_count"], 1);
    assert_eq!(listed["notes"][0]["id"], note_id);

    let wrong_recipient = run(
        &sender_worktree,
        &[
            "note",
            "ack",
            "--session",
            &sender_id,
            "--id",
            &note_id.to_string(),
        ],
    );
    assert!(!wrong_recipient.status.success());
    assert!(String::from_utf8_lossy(&wrong_recipient.stderr).contains("belongs to session"));

    let acknowledged = run(
        &recipient_worktree,
        &[
            "note",
            "ack",
            "--session",
            &recipient_id,
            "--id",
            &note_id.to_string(),
            "--json",
        ],
    );
    assert!(acknowledged.status.success());
    let acknowledged: serde_json::Value = serde_json::from_slice(&acknowledged.stdout).unwrap();
    assert!(acknowledged["acknowledged_at"].as_i64().is_some());

    let quiet = run(&recipient_worktree, &["agents", "--json"]);
    assert!(!String::from_utf8_lossy(&quiet.stderr).contains("Unread broker note"));

    let multiline = run(
        &sender_worktree,
        &[
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
    assert!(!multiline.status.success());
    assert!(String::from_utf8_lossy(&multiline.stderr).contains("one line of plain text"));

    let mut broker = Broker::open(tmp.path()).unwrap();
    broker.close(recipient.id).unwrap();
    let closed = broker
        .send_session_note(sender.id, recipient.id, "too late")
        .unwrap_err()
        .to_string();
    assert!(closed.contains("recipient session") && closed.contains("is closed"));
}
