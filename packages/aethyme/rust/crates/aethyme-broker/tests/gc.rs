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
    let delivered = broker.start_worktree("old delivered work", None).unwrap();
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
    let active = broker.start_worktree("live blocker", None).unwrap();
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
    // No journal yet, so this is ordinary staleness: send the operator back to
    // `gc plan` rather than offering a digest to paste (issue #140).
    let mismatch = broker.gc_apply(&"0".repeat(64)).unwrap_err().to_string();
    assert!(
        mismatch.contains("no longer matches current state")
            && mismatch.contains("aethyme broker gc plan"),
        "stale confirmation must direct to a fresh plan: {mismatch}"
    );
    assert!(
        !mismatch.contains("expected"),
        "a freshly computed digest must not be offered as a value to confirm: {mismatch}"
    );
    assert!(gate_log.exists());

    let paused = broker.gc_apply_bounded(&plan.digest, Some(0)).unwrap();
    assert!(!paused.complete);
    assert!(paused.deadline_reached);
    assert!(main_root.join(".aethyme/gc-journal.json").exists());

    // With a journal present the expectation comes from the interrupted run, and
    // no fresh plan can reproduce it -- so the message must say so instead of
    // sending the operator to `gc plan`, which would loop them (issue #140).
    let resume = broker.gc_apply(&"1".repeat(64)).unwrap_err().to_string();
    assert!(
        resume.contains("interrupted GC run is pending") && resume.contains(&plan.digest),
        "a pending run must be named with its own digest: {resume}"
    );
    assert!(
        resume.contains("cannot reproduce"),
        "the message must explain why re-planning will not help: {resume}"
    );

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

#[test]
fn ephemeral_repositories_never_anchor_worktrees_in_durable_host_state() {
    let (tmp, _broker, _id, worktree) = fixture();
    let repo = tmp.path().canonicalize().unwrap();
    let worktree = worktree.canonicalize().unwrap();
    assert!(
        worktree.starts_with(&repo),
        "a temp-dir repository must keep its worktrees repo-local so they die with it, got {}",
        worktree.display()
    );
}

#[test]
fn a_resumed_candidate_that_stopped_qualifying_is_retained_without_stranding_the_rest() {
    let (tmp, broker, _delivered_id, worktree) = fixture();
    drop(broker);
    // Reclaim build caches immediately and leave the autonomous sweep off, so
    // the authorized plan is the only thing that touches them.
    std::fs::write(
        tmp.path().join(".aethyme/broker.toml"),
        "[retention]\nclosed_worktrees_days = 30\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n",
    )
    .unwrap();
    std::fs::write(tmp.path().join(".git/info/exclude"), "target/\n").unwrap();
    let caches = ["one/target", "two/target"].map(|relative| {
        let dir = worktree.join(relative);
        std::fs::create_dir_all(dir.join("debug")).unwrap();
        std::fs::write(dir.join("CACHEDIR.TAG"), "Signature: 8a477f597d28d172\n").unwrap();
        std::fs::write(dir.join("debug/artifact.bin"), vec![0_u8; 4096]).unwrap();
        dir
    });

    let mut broker = Broker::open(tmp.path()).unwrap();
    let plan = broker.gc_plan().unwrap();
    assert_eq!(
        plan.artifacts.len(),
        2,
        "expected two build caches: {:?}",
        plan.artifacts
    );
    let first = worktree.join(&plan.artifacts[0].relative_dir);
    let second = worktree.join(&plan.artifacts[1].relative_dir);

    // Pause immediately: what follows can only be reached by resuming a
    // journal, because a fresh plan simply would not name a candidate that no
    // longer qualifies.
    assert!(
        !broker
            .gc_apply_bounded(&plan.digest, Some(0))
            .unwrap()
            .complete
    );
    assert!(tmp.path().join(".aethyme/gc-journal.json").exists());

    // Take the witness off the candidate the resume reaches first. This is the
    // state an interrupted removal used to leave behind, and the state a stray
    // `.DS_Store` produced once the final `rmdir` failed partway through.
    std::fs::remove_file(first.join("CACHEDIR.TAG")).unwrap();

    let resumed = broker.gc_apply(&plan.digest).unwrap();
    assert!(
        resumed.complete,
        "one disqualified candidate must not leave the run unfinished: {resumed:?}"
    );
    assert!(
        resumed
            .failures
            .iter()
            .any(|failure| failure.contains(&plan.artifacts[0].relative_dir)),
        "the candidate left in place must be reported: {:?}",
        resumed.failures
    );
    assert!(
        first.exists(),
        "a directory that no longer proves it is a build cache must be left alone"
    );
    assert!(
        !second.exists(),
        "the other candidate must still be reclaimed"
    );
    assert!(caches.iter().any(|cache| cache.exists()));

    // The journal is gone, so planning works again. While it survived, a
    // disqualified candidate pinned every later plan and no command released it.
    assert!(!tmp.path().join(".aethyme/gc-journal.json").exists());
    let replanned = broker.gc_plan().unwrap();
    assert_ne!(replanned.digest, plan.digest);
    assert!(
        broker.gc_apply(&replanned.digest).is_ok(),
        "a fresh plan must be runnable"
    );
}
