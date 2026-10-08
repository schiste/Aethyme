//! A running `broker submit` is visible from another process: `status` lists
//! it with its phase, place in line and last progress, and flags one that has
//! stopped reporting, so a queued submit can be told from a stuck one. A
//! process blocked on a contested lock or lease is listed the same way (#494).

use std::path::{Path, PathBuf};
use std::process::Command;

use aethyme_broker::{Broker, SUBMIT_STALL_AFTER, SubmitProgressRecord, WaitRecord};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn sh(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn repo(tmp: &Path) -> PathBuf {
    let repo = tmp.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    sh(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    sh(&repo, &["add", "-A"]);
    sh(&repo, &["commit", "-qm", "init"]);
    repo
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// What a running submit writes: this test process stands in for it, so the
/// record's PID is alive.
fn write_record(repo: &Path, session_id: i64, phase: &str, phase_started: i64, progress_at: i64) {
    let record = SubmitProgressRecord {
        session_id,
        pid: i64::from(std::process::id()),
        started_at_ms: phase_started - 5_000,
        phase: phase.into(),
        phase_started_at_ms: phase_started,
        last_progress_at_ms: progress_at,
        last_progress: format!("gate cargo-test running... (session {session_id})"),
    };
    let dir = repo.join(".aethyme/run/submits");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("session-{session_id}.json")),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
}

#[test]
fn status_lists_a_waiting_submit_with_its_position_and_the_slot_holder() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo(tmp.path());
    let now = now_ms();
    write_record(
        &repo,
        812,
        "verifying the merged tree",
        now - 190_000,
        now - 2_000,
    );
    write_record(
        &repo,
        813,
        "waiting for a verification slot",
        now - 60_000,
        now - 1_000,
    );

    let mut broker = Broker::open(&repo).unwrap();
    let status = broker.status(now_ms()).unwrap();
    let json = serde_json::to_value(&status).unwrap();
    let submits = json["in_flight_submits"]
        .as_array()
        .expect("in_flight_submits");
    assert_eq!(submits.len(), 2);
    let waiting = submits
        .iter()
        .find(|submit| submit["session_id"] == 813)
        .unwrap();
    assert_eq!(waiting["phase"], "waiting for a verification slot");
    assert_eq!(waiting["alive"], true);
    assert_eq!(waiting["possibly_stalled"], false);
    assert_eq!(waiting["position"]["position"], 1);
    assert_eq!(waiting["position"]["waiting"], 1);
    assert_eq!(waiting["position"]["holders"][0]["session_id"], 812);
    assert!(
        status
            .advice
            .iter()
            .all(|row| row.id != "submit.possibly-stalled"),
        "a submit that is only waiting was flagged"
    );
}

#[test]
fn a_silent_submit_is_flagged_possibly_stalled_with_advice() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo(tmp.path());
    let now = now_ms();
    let silent_since = now - SUBMIT_STALL_AFTER.as_millis() as i64 - 1_000;
    write_record(&repo, 7, "simulating the merge", silent_since, silent_since);

    let mut broker = Broker::open(&repo).unwrap();
    let status = broker.status(now_ms()).unwrap();
    let submit = &status.in_flight_submits[0];
    assert!(submit.alive && submit.possibly_stalled);
    let advice = status
        .advice
        .iter()
        .find(|row| row.id == "submit.possibly-stalled")
        .expect("stalled submit advice");
    assert_eq!(advice.session_id, Some(7));
    assert!(
        advice.summary.contains("simulating the merge"),
        "{}",
        advice.summary
    );
}

#[test]
fn status_omits_the_field_when_no_submit_is_running() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo(tmp.path());
    let mut broker = Broker::open(&repo).unwrap();
    let json = serde_json::to_value(broker.status(now_ms()).unwrap()).unwrap();
    assert!(json.get("in_flight_submits").is_none());
}

/// A finished submit takes its record with it: only a crashed one can leave
/// a record behind, and that one is reported as gone, not as running.
#[test]
fn a_completed_submit_leaves_no_progress_record() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo(tmp.path());
    let mut broker = Broker::open(&repo).unwrap();
    let session = broker.start_worktree("work", None).unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    std::fs::write(worktree.join("work.txt"), "landed\n").unwrap();
    sh(&worktree, &["add", "-A"]);
    sh(&worktree, &["commit", "-qm", "feat: work"]);

    broker.submit(session.id).unwrap();
    let left = std::fs::read_dir(repo.join(".aethyme/run/submits"))
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(left, 0, "the submit left its progress record behind");
    assert!(
        broker
            .status(now_ms())
            .unwrap()
            .in_flight_submits
            .is_empty()
    );
}

/// What a process blocked on a contested lock writes while it waits.
fn write_wait(repo: &Path, pid: i64, session_id: i64, started_at_ms: i64) {
    let record = WaitRecord {
        pid,
        session_id: Some(session_id),
        kind: "coordinated_write_lock".into(),
        resource: "owner/repo".into(),
        holder: "operation 41 (session 700)".into(),
        started_at_ms,
    };
    let dir = repo.join(".aethyme/run/waits");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{pid}-1.json")),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
}

#[test]
fn status_and_its_summary_list_who_waits_on_which_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo(tmp.path());
    let now = now_ms();
    write_wait(&repo, i64::from(std::process::id()), 812, now - 65_000);

    let mut broker = Broker::open(&repo).unwrap();
    for json in [
        serde_json::to_value(broker.status(now_ms()).unwrap()).unwrap(),
        serde_json::to_value(broker.status_brief(now_ms()).unwrap()).unwrap(),
    ] {
        let waiter = &json["waiters"][0];
        assert_eq!(waiter["session_id"], 812, "{json}");
        assert_eq!(waiter["kind"], "coordinated_write_lock");
        assert_eq!(waiter["resource"], "owner/repo");
        assert_eq!(waiter["holder"], "operation 41 (session 700)");
        assert_eq!(waiter["alive"], true);
        assert!(waiter["waited_ms"].as_i64().unwrap() >= 65_000);
    }
    drop(broker);

    let output = Command::new(CLI)
        .args(["status", "--summary"])
        .current_dir(&repo)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    // The wait keeps aging while the command runs; only its shape is fixed.
    let line = stdout
        .lines()
        .find(|line| line.trim_start().starts_with("session 812 waiting "))
        .unwrap_or_else(|| panic!("no waiter line in {stdout}"));
    assert!(
        line.ends_with(" for the coordinated write lock on owner/repo: operation 41 (session 700)"),
        "{line}"
    );
}

#[test]
fn status_omits_waiters_when_nobody_waits() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo(tmp.path());
    let mut broker = Broker::open(&repo).unwrap();
    let json = serde_json::to_value(broker.status(now_ms()).unwrap()).unwrap();
    assert!(json.get("waiters").is_none());
    let json = serde_json::to_value(broker.status_brief(now_ms()).unwrap()).unwrap();
    assert!(json.get("waiters").is_none());
}
