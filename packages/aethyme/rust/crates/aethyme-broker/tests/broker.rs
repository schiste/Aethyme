//! End-to-end tests for the session-facing broker API (issues #7-#10):
//! adopt on hand-made worktrees, spawn with log capture, liveness
//! derivation, and cleanup guards — against real git repos and processes.

use std::path::Path;
use std::process::Command;

use aethyme_broker::{
    Broker, BrokerOpError, FinishStatus, SessionOrigin, SessionStatus, VersionDriftStatus,
};

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
fn start_worktree_creates_broker_managed_session_without_process() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();

    let session = broker.start_worktree("isolated edits").unwrap();
    let wt = std::path::PathBuf::from(&session.worktree_path);

    assert_eq!(session.origin, SessionOrigin::Spawned);
    assert!(session.pid.is_none());
    assert_eq!(session.branch, "agent/isolated-edits");
    assert!(wt.exists());
    assert!(wt.ends_with(".aethyme/worktrees/isolated-edits"));
    assert!(
        broker
            .store()
            .session_foreign_files(session.id)
            .unwrap()
            .is_empty()
    );
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

#[test]
fn doctor_reports_healthy_then_finds_missing_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();

    let report = broker.doctor().unwrap();
    assert_eq!(report.integrity, "ok");
    assert_eq!(report.version.status, VersionDriftStatus::NotAethymeSource);
    assert!(report.healthy());

    // A session whose worktree vanishes out-of-band is a finding.
    let wt = tmp.path().join("doomed-wt");
    sh(
        tmp.path(),
        &[
            "worktree",
            "add",
            "-b",
            "agent/doomed",
            wt.to_str().unwrap(),
            "main",
        ],
    );
    let session = broker.adopt(&wt, None).unwrap();
    std::fs::remove_dir_all(&wt).unwrap();
    let report = broker.doctor().unwrap();
    assert_eq!(report.missing_worktrees, vec![session.id]);
    assert!(!report.healthy());
}

// ── session lifecycle UX (dogfood feedback 2026-07-14) ─────────────────

#[test]
fn adopt_conflict_close_reuse_and_replace_stale_lifecycle() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();

    let first = broker.adopt(tmp.path(), Some("first task")).unwrap();

    // Bare re-adopt fails with guidance naming the session and options.
    let err = broker.adopt(tmp.path(), Some("second task")).unwrap_err();
    let msg = err.to_string();
    assert!(
        matches!(err, BrokerOpError::SessionExistsForWorktree { id, .. } if id == first.id),
        "expected guidance error, got: {msg}"
    );
    for needle in [
        "--reuse",
        "close --session",
        "--replace-stale",
        "first task",
    ] {
        assert!(msg.contains(needle), "guidance missing {needle:?}: {msg}");
    }

    // --reuse keeps the identity, updates the task, refreshes the baseline.
    std::fs::write(tmp.path().join("f.txt"), "x\n").unwrap();
    sh(tmp.path(), &["add", "-A"]);
    sh(tmp.path(), &["commit", "-qm", "advance head"]);
    let reused = broker
        .adopt_with(
            tmp.path(),
            Some("follow-up task"),
            aethyme_broker::AdoptMode::Reuse,
        )
        .unwrap();
    assert_eq!(reused.id, first.id);
    assert_eq!(reused.task.as_deref(), Some("follow-up task"));
    assert_ne!(reused.diff_base, first.diff_base, "baseline must refresh");

    // close is state-only: session cleaned, worktree untouched.
    broker.close(first.id).unwrap();
    let closed = broker.store().session(first.id).unwrap();
    assert_eq!(closed.status, SessionStatus::Cleaned);
    assert!(tmp.path().join("README.md").exists());

    // After close, plain adopt works again (no --replace-stale needed).
    let second = broker.adopt(tmp.path(), Some("third task")).unwrap();
    assert_ne!(second.id, first.id);

    // --replace-stale swaps a lingering session for a fresh one.
    let replaced = broker
        .adopt_with(
            tmp.path(),
            Some("fourth task"),
            aethyme_broker::AdoptMode::ReplaceStale,
        )
        .unwrap();
    assert_ne!(replaced.id, second.id);
    assert_eq!(
        broker.store().session(second.id).unwrap().status,
        SessionStatus::Cleaned
    );

    // The reuse left an audit trail.
    let events = broker.store().events_after(0, i64::MAX).unwrap();
    assert!(events.iter().any(|e| e.kind == "session.reused"));
}

#[test]
fn finish_blocks_dirty_then_unsubmitted_commits() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let wt = tmp.path().join("finish-wt");
    sh(
        tmp.path(),
        &[
            "worktree",
            "add",
            "-b",
            "agent/finish",
            wt.to_str().unwrap(),
            "main",
        ],
    );
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.adopt(&wt, Some("finish task")).unwrap();

    std::fs::write(wt.join("wip.txt"), "dirty\n").unwrap();
    let dirty = broker.finish(session.id).unwrap();
    assert_eq!(dirty.status, FinishStatus::Blocked);
    assert!(!dirty.closed);
    assert_eq!(dirty.dirty_paths, vec!["wip.txt"]);
    assert!(
        dirty
            .next_commands
            .iter()
            .any(|command| command.contains("status --short"))
    );
    assert_ne!(
        broker.store().session(session.id).unwrap().status,
        SessionStatus::Cleaned
    );

    sh(&wt, &["add", "-A"]);
    sh(&wt, &["commit", "-qm", "finish wip"]);
    let unsubmitted = broker.finish(session.id).unwrap();
    assert_eq!(unsubmitted.status, FinishStatus::Blocked);
    assert_eq!(unsubmitted.unsubmitted_commits, 1);
    assert_eq!(
        unsubmitted.next_commands,
        vec![format!("aethyme broker submit --session {}", session.id)]
    );
    assert_ne!(
        broker.store().session(session.id).unwrap().status,
        SessionStatus::Cleaned
    );
}

#[test]
fn finish_closes_promoted_session_and_suggests_cleanup_only_when_main_contains_it() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let wt = tmp.path().join("promoted-finish-wt");
    sh(
        tmp.path(),
        &[
            "worktree",
            "add",
            "-b",
            "agent/promoted-finish",
            wt.to_str().unwrap(),
            "main",
        ],
    );
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.adopt(&wt, Some("promoted finish task")).unwrap();

    std::fs::write(wt.join("done.txt"), "done\n").unwrap();
    sh(&wt, &["add", "-A"]);
    sh(&wt, &["commit", "-qm", "done"]);
    assert!(broker.submit(session.id).unwrap().promoted);

    let closed = broker.finish(session.id).unwrap();
    assert_eq!(closed.status, FinishStatus::Closed);
    assert!(closed.closed);
    assert!(!closed.cleanup_safe);
    assert!(closed.next_commands.is_empty());
    assert_eq!(
        broker.store().session(session.id).unwrap().status,
        SessionStatus::Cleaned
    );

    sh(tmp.path(), &["merge", "--ff-only", "aethyme/integration"]);
    let already = broker.finish(session.id).unwrap();
    assert_eq!(already.status, FinishStatus::AlreadyClosed);
    assert!(already.cleanup_safe);
    assert_eq!(
        already.next_commands,
        vec![format!("aethyme broker cleanup {}", session.id)]
    );
}
