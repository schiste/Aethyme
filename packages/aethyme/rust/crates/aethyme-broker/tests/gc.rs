use std::path::{Path, PathBuf};
use std::process::Command;

use aethyme_broker::{Broker, FinishOptions, GateStatus, GcFileAction, GcRowKind, NewGateResult};

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

fn fixture() -> (tempfile::TempDir, Broker, i64, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("README.md"), "fixture\n").unwrap();
    std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/\n").unwrap();
    git(tmp.path(), &["add", "-A"]);
    git(tmp.path(), &["commit", "-qm", "init"]);
    std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/broker.toml"),
        "[retention]\nterminal_events_days = 1\ngate_results_days = 1\nterminal_merge_queue_days = 1\ncommand_metrics_days = 1\nclosed_worktrees_days = 1\nstartup_budget_ms = 5\n",
    )
    .unwrap();

    let mut broker = Broker::open(tmp.path()).unwrap();
    let delivered = broker.start_worktree("old delivered work").unwrap();
    let worktree = PathBuf::from(&delivered.worktree_path);
    std::fs::write(worktree.join("done.txt"), "done\n").unwrap();
    git(&worktree, &["add", "done.txt"]);
    git(&worktree, &["commit", "-qm", "done"]);
    assert!(broker.submit(delivered.id).unwrap().promoted);
    assert!(
        broker
            .finish_with_options(
                delivered.id,
                FinishOptions {
                    keep_worktree: true,
                },
            )
            .unwrap()
            .closed
    );
    (tmp, broker, delivered.id, worktree)
}

#[test]
fn plan_is_exact_deterministic_and_protects_live_or_unresolved_state() {
    let (tmp, mut broker, delivered_id, worktree) = fixture();
    let active = broker.start_worktree("live blocker").unwrap();
    let main_root = broker.main_root().to_path_buf();
    let gate_dir = main_root.join(".aethyme/logs/gates");
    std::fs::create_dir_all(&gate_dir).unwrap();
    let gate_log = gate_dir.join("old.log");
    std::fs::write(&gate_log, "old gate output\n").unwrap();
    broker
        .store()
        .record_gate_result(&NewGateResult {
            gate_name: "old-gate".into(),
            tree_hash: "tree".into(),
            definition_hash: "definition".into(),
            status: GateStatus::Pass,
            failure_class: None,
            exit_code: Some(0),
            duration_ms: Some(1),
            wait_duration_ms: None,
            first_output_ms: None,
            output_bytes: Some(16),
            log_path: Some(gate_log.to_string_lossy().into_owned()),
            session_id: Some(delivered_id),
        })
        .unwrap();
    broker
        .store()
        .append_event("test.old", Some(delivered_id), None)
        .unwrap();
    drop(broker);

    let old = 1_i64;
    let db = rusqlite::Connection::open(tmp.path().join(".aethyme/broker.db")).unwrap();
    db.execute("UPDATE events SET ts = ?1", [old]).unwrap();
    db.execute("UPDATE gate_results SET created_at = ?1", [old])
        .unwrap();
    db.execute(
        "UPDATE merge_queue SET created_at = ?1, updated_at = ?1",
        [old],
    )
    .unwrap();
    db.execute(
        "UPDATE sessions SET closed_at = ?1, updated_at = ?1 WHERE id = ?2",
        rusqlite::params![old, delivered_id],
    )
    .unwrap();
    db.execute(
        "UPDATE entry_path_exposures
         SET state = 'resolved', resolved_at = ?1,
             resolution_kind = 'ship_verified', resolution_sha = 'remote',
             resolution_evidence = 'fixture'
         WHERE queue_entry_id IN (SELECT id FROM merge_queue WHERE session_id = ?2)",
        rusqlite::params![old, delivered_id],
    )
    .unwrap();
    db.execute(
        "INSERT INTO merge_queue (
             session_id, head_commit, base_commit, status, created_at, updated_at
         ) VALUES (?1, 'superseded-head', 'superseded-base', 'superseded', 1, 1)",
        [delivered_id],
    )
    .unwrap();
    drop(db);

    let metrics = main_root.join(".aethyme/logs/command-metrics.jsonl");
    std::fs::create_dir_all(metrics.parent().unwrap()).unwrap();
    std::fs::write(
        &metrics,
        "{\"ts\":1,\"command\":\"old\"}\n{\"ts\":9999999999999,\"command\":\"new\"}\nmalformed\n",
    )
    .unwrap();
    std::fs::write(
        main_root.join(".aethyme/broker.toml"),
        "[retention]\nstartup_budget_ms = 5000\n",
    )
    .unwrap();

    let mut broker = Broker::open(tmp.path()).unwrap();
    let first = broker.gc_plan().unwrap();
    let second = broker.gc_plan().unwrap();
    assert_eq!(first.digest, second.digest);
    assert!(
        first
            .rows
            .iter()
            .any(|row| row.kind == GcRowKind::GateResult)
    );
    assert!(
        first
            .rows
            .iter()
            .any(|row| row.kind == GcRowKind::MergeQueue)
    );
    assert!(first.files.iter().any(|file| {
        file.path == ".aethyme/logs/gates/old.log" && file.action == GcFileAction::Delete
    }));
    assert!(first.files.iter().any(|file| {
        file.path == ".aethyme/logs/command-metrics.jsonl"
            && file.action == GcFileAction::Rewrite
            && file.bytes_after < file.bytes_before
    }));
    assert!(
        first
            .worktrees
            .iter()
            .any(|candidate| candidate.session_id == delivered_id)
    );
    assert!(
        first
            .blockers
            .iter()
            .any(|blocker| { blocker.kind == "live_session" && blocker.id == Some(active.id) })
    );
    assert!(
        first
            .blockers
            .iter()
            .any(|blocker| { blocker.kind == "command_metric_line" && blocker.id.is_none() })
    );
    assert!(first.files.iter().all(|file| !file.path.starts_with('/')));
    assert!(worktree.exists(), "planning must not remove the worktree");
    assert!(gate_log.exists(), "planning must not remove gate logs");
    assert!(metrics.exists(), "planning must not rewrite metrics");
}

#[test]
fn digest_confirmed_apply_resumes_a_deadline_and_preserves_monotonic_ids() {
    let (tmp, mut broker, delivered_id, worktree) = fixture();
    let main_root = broker.main_root().to_path_buf();
    let gate_dir = main_root.join(".aethyme/logs/gates");
    std::fs::create_dir_all(&gate_dir).unwrap();
    let gate_log = gate_dir.join("old.log");
    std::fs::write(&gate_log, "old gate output\n").unwrap();
    broker
        .store()
        .record_gate_result(&NewGateResult {
            gate_name: "old-gate".into(),
            tree_hash: "tree".into(),
            definition_hash: "definition".into(),
            status: GateStatus::Pass,
            failure_class: None,
            exit_code: Some(0),
            duration_ms: Some(1),
            wait_duration_ms: None,
            first_output_ms: None,
            output_bytes: Some(16),
            log_path: Some(gate_log.to_string_lossy().into_owned()),
            session_id: Some(delivered_id),
        })
        .unwrap();
    broker
        .store()
        .append_event("test.old", Some(delivered_id), None)
        .unwrap();
    drop(broker);

    let db_path = tmp.path().join(".aethyme/broker.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute("UPDATE events SET ts = 1", []).unwrap();
    db.execute("UPDATE gate_results SET created_at = 1", [])
        .unwrap();
    db.execute("UPDATE merge_queue SET created_at = 1, updated_at = 1", [])
        .unwrap();
    db.execute(
        "UPDATE sessions SET closed_at = 1, updated_at = 1 WHERE id = ?1",
        [delivered_id],
    )
    .unwrap();
    db.execute(
        "UPDATE entry_path_exposures
         SET state = 'resolved', resolved_at = 1,
             resolution_kind = 'ship_verified', resolution_sha = 'remote',
             resolution_evidence = 'fixture'",
        [],
    )
    .unwrap();
    db.execute(
        "INSERT INTO merge_queue (
             session_id, head_commit, base_commit, status, created_at, updated_at
         ) VALUES (?1, 'superseded-head', 'superseded-base', 'superseded', 1, 1)",
        [delivered_id],
    )
    .unwrap();
    let max_event_before: i64 = db
        .query_row("SELECT MAX(id) FROM events", [], |row| row.get(0))
        .unwrap();
    drop(db);
    let metrics = main_root.join(".aethyme/logs/command-metrics.jsonl");
    std::fs::create_dir_all(metrics.parent().unwrap()).unwrap();
    std::fs::write(
        &metrics,
        "{\"ts\":1,\"command\":\"old\"}\n{\"ts\":9999999999999,\"command\":\"new\"}\nmalformed\n",
    )
    .unwrap();

    let mut broker = Broker::open(tmp.path()).unwrap();
    let plan = broker.gc_plan().unwrap();
    let mismatch = broker.gc_apply(&"0".repeat(64)).unwrap_err();
    assert!(mismatch.to_string().contains("confirmation mismatch"));
    assert!(gate_log.exists());

    let paused = broker.gc_apply_bounded(&plan.digest, Some(0)).unwrap();
    assert!(!paused.complete);
    assert!(paused.deadline_reached);
    assert!(main_root.join(".aethyme/gc-journal.json").exists());

    std::fs::write(&gate_log, "changed after confirmation\n").unwrap();
    let drift = broker.gc_apply(&plan.digest).unwrap_err();
    assert!(drift.to_string().contains("reviewed artifact drifted"));
    assert!(gate_log.exists());
    assert!(main_root.join(".aethyme/gc-journal.json").exists());
    std::fs::write(&gate_log, "old gate output\n").unwrap();

    drop(broker);
    let mut broker = Broker::open(tmp.path()).unwrap();
    assert!(!gate_log.exists());
    if main_root.join(".aethyme/gc-journal.json").exists() {
        let applied = broker.gc_apply(&plan.digest).unwrap();
        assert!(applied.complete);
    }
    assert!(!worktree.exists());
    let metrics = std::fs::read_to_string(&metrics).unwrap();
    assert!(!metrics.contains("\"old\""));
    assert!(metrics.contains("\"new\""));
    assert!(metrics.contains("malformed"));
    assert!(!main_root.join(".aethyme/gc-journal.json").exists());

    let db = rusqlite::Connection::open(&db_path).unwrap();
    let max_event_after: i64 = db
        .query_row("SELECT MAX(id) FROM events", [], |row| row.get(0))
        .unwrap();
    assert!(max_event_after > max_event_before);
    let gate_rows: i64 = db
        .query_row("SELECT COUNT(*) FROM gate_results", [], |row| row.get(0))
        .unwrap();
    assert_eq!(gate_rows, 0);
    let queue_rows: i64 = db
        .query_row("SELECT COUNT(*) FROM merge_queue", [], |row| row.get(0))
        .unwrap();
    assert_eq!(queue_rows, 1, "the accepted checkpoint remains protected");
}

#[cfg(unix)]
#[test]
fn locked_runtime_file_is_retained_and_resumable() {
    use std::os::unix::fs::PermissionsExt;

    let (tmp, mut broker, _delivered_id, _worktree) = fixture();
    let metrics = tmp.path().join(".aethyme/logs/command-metrics.jsonl");
    std::fs::create_dir_all(metrics.parent().unwrap()).unwrap();
    std::fs::write(
        &metrics,
        "{\"ts\":1,\"command\":\"old\"}\n{\"ts\":9999999999999,\"command\":\"new\"}\n",
    )
    .unwrap();
    let plan = broker.gc_plan().unwrap();
    assert!(
        plan.files
            .iter()
            .any(|file| file.path.ends_with("command-metrics.jsonl"))
    );

    let directory = metrics.parent().unwrap();
    let original_mode = std::fs::metadata(directory).unwrap().permissions().mode();
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = broker.gc_apply(&plan.digest);
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(original_mode)).unwrap();

    let blocked = result.unwrap();
    assert!(!blocked.complete);
    assert!(!blocked.failures.is_empty());
    assert!(metrics.exists());
    assert!(tmp.path().join(".aethyme/gc-journal.json").exists());

    let resumed = broker.gc_apply(&plan.digest).unwrap();
    assert!(resumed.complete);
    assert!(
        !std::fs::read_to_string(metrics)
            .unwrap()
            .contains("\"old\"")
    );
}
