//! End-to-end tests for the session-facing broker API (issues #7-#10):
//! adopt on hand-made worktrees, spawn with log capture, liveness
//! derivation, and cleanup guards — against real git repos and processes.

use std::path::Path;
use std::process::Command;

use aethyme_broker::{Broker, BrokerOpError, SessionOrigin, SessionStatus};

fn sh(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap()
        .status;
    assert!(status.success(), "git {args:?} failed");
}

fn init_repo(root: &Path) {
    sh(root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("README.md"), "hello\n").unwrap();
    sh(root, &["add", "-A"]);
    sh(root, &["commit", "-qm", "init"]);
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

#[test]
fn adopted_hand_made_worktree_and_spawned_session_are_indistinguishable() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());

    // The user makes a worktree by hand — the broker never saw it happen.
    let hand_made = tmp.path().join("hand-made-wt");
    sh(
        tmp.path(),
        &[
            "worktree",
            "add",
            "-b",
            "agent/hand",
            hand_made.to_str().unwrap(),
            "main",
        ],
    );

    // Broker opened from INSIDE the hand-made worktree still resolves to
    // the main checkout's database (attach-first, one shared db).
    let mut broker = Broker::open(&hand_made).unwrap();
    assert_eq!(
        broker.main_root().canonicalize().unwrap(),
        tmp.path().canonicalize().unwrap()
    );
    let adopted = broker.adopt(&hand_made, Some("hand task")).unwrap();
    assert_eq!(adopted.origin, SessionOrigin::Adopted);
    assert_eq!(adopted.branch, "agent/hand");
    assert!(adopted.pid.is_none());

    // Spawned convenience on the same model.
    let spawned = broker
        .start_agent("spawned task", "echo out; echo err >&2")
        .unwrap();
    assert_eq!(spawned.origin, SessionOrigin::Spawned);
    assert!(spawned.pid.is_some());
    assert!(
        spawned
            .worktree_path
            .contains(".aethyme/worktrees/spawned-task")
    );

    // Downstream (agents view) treats both identically.
    let views = broker.agents(now_ms()).unwrap();
    assert_eq!(views.len(), 2);
    for view in &views {
        assert!(matches!(
            view.derived_status,
            SessionStatus::Active | SessionStatus::Exited
        ));
    }

    // Log capture worked (stdout + stderr).
    std::thread::sleep(std::time::Duration::from_millis(300));
    let log = std::fs::read_to_string(spawned.log_path.unwrap()).unwrap();
    assert!(log.contains("out"));
    assert!(log.contains("err"));
}

#[test]
fn dead_spawned_pid_reconciles_to_exited_and_activity_drives_idle() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();

    let session = broker.start_agent("quick", "true").unwrap();
    // Let the child exit.
    std::thread::sleep(std::time::Duration::from_millis(400));

    let views = broker.agents(now_ms()).unwrap();
    let view = views.iter().find(|v| v.session.id == session.id).unwrap();
    assert_eq!(view.pid_alive, Some(false));
    assert_eq!(view.derived_status, SessionStatus::Exited);
    // Reconciled into the store, not just the view.
    let views_again = broker.agents(now_ms()).unwrap();
    let again = views_again
        .iter()
        .find(|v| v.session.id == session.id)
        .unwrap();
    assert_eq!(again.session.status, SessionStatus::Exited);

    // An adopted session with no PID transitions on activity age alone:
    // pretend 30 minutes pass.
    let adopted_wt = tmp.path().join("adopted-wt");
    sh(
        tmp.path(),
        &[
            "worktree",
            "add",
            "-b",
            "agent/a",
            adopted_wt.to_str().unwrap(),
            "main",
        ],
    );
    let adopted = broker.adopt(&adopted_wt, None).unwrap();
    let future = now_ms() + 30 * 60 * 1000;
    let views = broker.agents(future).unwrap();
    let view = views.iter().find(|v| v.session.id == adopted.id).unwrap();
    assert_eq!(view.pid_alive, None);
    assert_eq!(view.derived_status, SessionStatus::Idle);
}

#[test]
fn cleanup_refuses_dirty_and_unmerged_then_force_discards() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();

    let session = broker.start_agent("doomed", "true").unwrap();
    let wt = std::path::PathBuf::from(&session.worktree_path);

    // Uncommitted file → refuse.
    std::fs::write(wt.join("wip.rs"), "x\n").unwrap();
    assert!(matches!(
        broker.cleanup(session.id, false),
        Err(BrokerOpError::DirtyWorktree { .. })
    ));

    // Committed but unmerged → still refuse (work would be lost).
    sh(&wt, &["add", "-A"]);
    sh(&wt, &["commit", "-qm", "wip"]);
    let err = broker.cleanup(session.id, false).unwrap_err();
    assert!(err.to_string().contains("not reachable"));

    // Force discards, marks cleaned, frees the worktree slot.
    broker.cleanup(session.id, true).unwrap();
    assert!(!wt.exists());
    let views = broker.agents(now_ms()).unwrap();
    assert!(views.iter().all(|v| v.session.id != session.id));
}
