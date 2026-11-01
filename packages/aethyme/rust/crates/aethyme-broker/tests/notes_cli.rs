mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use aethyme_broker::Broker;
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

fn child_command(broker: &Broker, repo: &Path, args: &[&str]) -> Command {
    let mut command = common::broker_cli(CLI, args);
    command.current_dir(repo);
    broker
        .configure_storage_context_for_child(&mut command)
        .unwrap();
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
        &broker,
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
        &broker,
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
        &broker,
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
        &broker,
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
        &broker,
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
        &broker,
        &recipient_worktree,
        &["advanced", "agents", "--json"],
    );
    let mut multiline_command = child_command(
        &broker,
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

fn output_json(command: &mut Command) -> serde_json::Value {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// #1238: an automation has no session, so it sends as `automation:<name>`
/// from the repository itself. A fingerprint keeps one unread note per
/// recipient; the note surfaces on the recipient's next command with its own
/// acknowledge command, lists beside session notes, and once acknowledged a
/// repeat is queued again.
#[test]
fn automation_notes_need_no_session_and_repeat_only_once_read() {
    let (fixture, _sender_worktree, recipient_worktree) = fixture();
    let repo = fixture.path().to_path_buf();
    let mut broker = open_fixture_repository_broker(&fixture).unwrap();
    let recipient = broker
        .adopt(&recipient_worktree, Some("recipient"))
        .unwrap();
    let recipient_id = recipient.id.to_string();
    let send = |broker: &Broker, extra: &[&str]| {
        let mut args = vec![
            "advanced",
            "note",
            "send",
            "--to-session",
            recipient_id.as_str(),
            "--message",
            "prebuild of db-template failed; see #77",
        ];
        args.extend_from_slice(extra);
        child_command(broker, &repo, &args)
    };
    let fingerprinted = [
        "--from",
        "automation:prebuild",
        "--fingerprint",
        "db-template",
        "--json",
    ];
    let mut first = send(&broker, &fingerprinted);
    let mut repeat = send(&broker, &fingerprinted);
    let mut bad_sender = send(&broker, &["--from", "prebuild"]);
    let mut both = send(
        &broker,
        &["--from", "automation:prebuild", "--session", &recipient_id],
    );
    let mut session_fingerprint =
        send(&broker, &["--session", &recipient_id, "--fingerprint", "x"]);
    let mut surfaced = child_command(
        &broker,
        &recipient_worktree,
        &["advanced", "agents", "--json"],
    );
    let mut listed = child_command(
        &broker,
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
    drop(broker);

    let first = output_json(&mut first);
    assert_eq!(first["deduplicated"], false);
    assert_eq!(first["note"]["sender"], "automation:prebuild");
    assert_eq!(first["note"]["fingerprint"], "db-template");
    let note_id = first["note"]["id"].as_i64().unwrap();
    let repeat = output_json(&mut repeat);
    assert_eq!(repeat["deduplicated"], true);
    assert_eq!(repeat["note"]["id"], note_id);

    for (command, expected) in [
        (&mut bad_sender, "must be automation:<name>"),
        (&mut both, "not both"),
        (&mut session_fingerprint, "--fingerprint applies only"),
    ] {
        let output = command.output().unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(expected), "{stderr}");
    }

    let surfaced = surfaced.output().unwrap();
    let stderr = String::from_utf8_lossy(&surfaced.stderr);
    assert!(
        stderr.contains(&format!(
            "Unread note {note_id} from automation:prebuild: prebuild of db-template failed; see #77"
        )),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!(
            "note ack --session {recipient_id} --automation-id {note_id}"
        )),
        "{stderr}"
    );
    let listed = output_json(&mut listed);
    assert_eq!(listed["unread_count"], 1);
    assert_eq!(listed["automation_notes"][0]["id"], note_id);
    assert_eq!(listed["notes"].as_array().unwrap().len(), 0);

    let mut broker = open_fixture_repository_broker(&fixture).unwrap();
    let event = broker
        .store()
        .events_after_filtered(0, 10, Some("automation.note.sent"))
        .unwrap();
    assert_eq!(
        event.len(),
        1,
        "a deduplicated repeat records no second send"
    );
    assert!(
        !event[0]
            .payload_json
            .as_deref()
            .unwrap()
            .contains("db-template failed")
    );
    let mut acked = child_command(
        &broker,
        &recipient_worktree,
        &[
            "advanced",
            "note",
            "ack",
            "--session",
            &recipient_id,
            "--automation-id",
            &note_id.to_string(),
            "--json",
        ],
    );
    let mut again = send(&broker, &fingerprinted);
    drop(broker);
    assert!(
        output_json(&mut acked)["acknowledged_at"]
            .as_i64()
            .is_some()
    );
    let again = output_json(&mut again);
    assert_eq!(again["deduplicated"], false);
    assert_ne!(again["note"]["id"], note_id);

    let mut broker = open_fixture_repository_broker(&fixture).unwrap();
    broker.close(recipient.id).unwrap();
    let closed = broker
        .send_automation_note("automation:prebuild", recipient.id, "too late", None)
        .unwrap_err()
        .to_string();
    assert!(closed.contains("is closed"), "{closed}");
}
