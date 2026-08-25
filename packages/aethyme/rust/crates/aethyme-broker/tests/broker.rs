//! End-to-end tests for the session-facing broker API (issues #7-#10):
//! adopt on hand-made worktrees, spawn with log capture, liveness
//! derivation, and cleanup guards — against real git repos and processes.

use std::path::Path;
use std::process::Command;

use aethyme_broker::{
    AdoptIntegrationRelation, AdoptIntegrationSyncOutcome, AdoptMode, AdoptOptions, Broker,
    BrokerOpError, FinishGateCacheSource, FinishLeaseState, FinishStatus, GateStatus,
    NewGateResult, NewSession, SessionOrigin, SessionStatus, VersionDriftStatus, events,
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

fn rev(cwd: &Path, reference: &str) -> String {
    let output = Command::new("git")
        .args(["rev-parse", reference])
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn init_repo(root: &Path) {
    sh(root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("README.md"), "hello\n").unwrap();
    std::fs::write(root.join(".gitignore"), "/.aethyme/\n").unwrap();
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
fn compatibility_status_snapshot_derives_without_reconciling_state() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.adopt(tmp.path(), Some("snapshot task")).unwrap();
    std::fs::write(tmp.path().join("uncommitted.txt"), "snapshot only\n").unwrap();
    let event_count = broker.store().events_after(0, 10_000).unwrap().len();
    assert!(broker.store().active_leases().unwrap().is_empty());
    drop(broker);

    let mut snapshot = Broker::open_snapshot(tmp.path()).unwrap();
    let report = snapshot
        .status_snapshot(now_ms() + 7 * 24 * 60 * 60 * 1_000)
        .unwrap();
    let agent = report
        .agents
        .iter()
        .find(|agent| agent.session.id == session.id)
        .unwrap();
    assert_eq!(agent.derived_status, SessionStatus::Stale);
    assert_eq!(
        snapshot.store().session(session.id).unwrap().status,
        SessionStatus::Active
    );
    assert!(snapshot.store().active_leases().unwrap().is_empty());
    assert_eq!(
        snapshot.store().events_after(0, 10_000).unwrap().len(),
        event_count
    );
    assert!(
        !Command::new("git")
            .args(["show-ref", "--verify", "refs/heads/aethyme/integration"])
            .current_dir(tmp.path())
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn session_creation_pins_repository_contract_and_reuse_does_not_refresh_it() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/repository.json"),
        r#"{"schema_version":1,"applied_migrations":["repository-deployment-v1"]}"#,
    )
    .unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/gates.toml"),
        "[[gate]]\nname = \"first\"\ncommand = \"true\"\n",
    )
    .unwrap();

    let mut broker = Broker::open(tmp.path()).unwrap();
    let created = broker.adopt(tmp.path(), Some("pinned task")).unwrap();
    let original_contract = created
        .repository_contract
        .clone()
        .expect("new sessions pin a repository contract");
    assert_eq!(created.adoption_base, created.diff_base);
    assert_eq!(original_contract.repository_schema, Some(1));
    assert_eq!(original_contract.aethyme_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(
        original_contract
            .gate_definition_digest
            .as_deref()
            .map(str::len),
        Some(64)
    );
    assert!(!original_contract.backfilled);
    let json = serde_json::to_value(&created).unwrap();
    assert_eq!(json["adoption_base"], json["diff_base"]);
    assert_eq!(json["repository_contract"]["repository_schema"], 1);
    assert_eq!(
        json["repository_contract"]["deployment_state_digest"]
            .as_str()
            .map(str::len),
        Some(64)
    );

    std::fs::write(
        tmp.path().join(".aethyme/gates.toml"),
        "[[gate]]\nname = \"changed\"\ncommand = \"false\"\n",
    )
    .unwrap();
    let reused = broker
        .adopt_with(tmp.path(), Some("follow-up"), AdoptMode::Reuse)
        .unwrap()
        .session;
    assert_eq!(reused.id, created.id);
    assert_eq!(reused.repository_contract, Some(original_contract));
    assert_eq!(reused.adoption_base, created.adoption_base);
}

#[test]
fn opening_the_broker_backfills_live_pre_contract_sessions() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let head = rev(tmp.path(), "HEAD");
    let session_id = {
        let mut broker = Broker::open(tmp.path()).unwrap();
        broker
            .store()
            .register_session(&NewSession {
                worktree_path: tmp.path().to_string_lossy().into_owned(),
                branch: "main".into(),
                origin: SessionOrigin::Adopted,
                task: Some("pre-contract session".into()),
                diff_base: Some(head.clone()),
                adoption_base: None,
                repository_contract: None,
                pid: None,
                command: None,
                log_path: None,
            })
            .unwrap()
            .id
    };

    let mut reopened = Broker::open(tmp.path()).unwrap();
    let backfilled = reopened.store().session(session_id).unwrap();
    assert_eq!(backfilled.adoption_base.as_deref(), Some(head.as_str()));
    let contract = backfilled
        .repository_contract
        .expect("live session contract was backfilled");
    assert!(contract.backfilled);
    assert_eq!(contract.aethyme_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(contract.deployment_state_digest.len(), 64);
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

    let first = broker
        .adopt_with(
            tmp.path(),
            Some("first task"),
            aethyme_broker::AdoptMode::New,
        )
        .unwrap();
    assert_eq!(first.outcome, aethyme_broker::AdoptOutcome::Created);
    let first = first.session;

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

    // --reuse keeps the identity and updates the task, but the ownership
    // baseline remains immutable while the session is live.
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
    assert_eq!(reused.outcome, aethyme_broker::AdoptOutcome::Reused);
    let reused = reused.session;
    assert_eq!(reused.id, first.id);
    assert_eq!(reused.task.as_deref(), Some("follow-up task"));
    assert_eq!(
        reused.diff_base, first.diff_base,
        "plain active reuse must not absorb pending commits into the baseline"
    );
    assert_eq!(reused.adoption_base, first.adoption_base);
    assert_eq!(reused.repository_contract, first.repository_contract);

    // close is state-only: session cleaned, worktree untouched.
    broker.close(first.id).unwrap();
    let closed = broker.store().session(first.id).unwrap();
    assert_eq!(closed.status, SessionStatus::Cleaned);
    assert!(tmp.path().join("README.md").exists());

    // After close, --reuse creates a fresh session because no live identity remains.
    let second = broker
        .adopt_with(
            tmp.path(),
            Some("third task"),
            aethyme_broker::AdoptMode::Reuse,
        )
        .unwrap();
    assert_eq!(second.outcome, aethyme_broker::AdoptOutcome::Created);
    let second = second.session;
    assert_ne!(second.id, first.id);

    // --replace-stale swaps a lingering session for a fresh one.
    let replaced = broker
        .adopt_with(
            tmp.path(),
            Some("fourth task"),
            aethyme_broker::AdoptMode::ReplaceStale,
        )
        .unwrap();
    assert_eq!(replaced.outcome, aethyme_broker::AdoptOutcome::Replaced);
    let replaced = replaced.session;
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
fn adopt_reuse_reports_behind_drift_and_dirty_path_overlap() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.adopt(tmp.path(), Some("first task")).unwrap();

    sh(tmp.path(), &["checkout", "-qb", "integration-work"]);
    std::fs::write(tmp.path().join("shared.txt"), "integration\n").unwrap();
    sh(tmp.path(), &["add", "shared.txt"]);
    sh(tmp.path(), &["commit", "-qm", "integration advances"]);
    let integration_head = rev(tmp.path(), "HEAD");
    sh(tmp.path(), &["branch", "aethyme/integration", "HEAD"]);
    sh(tmp.path(), &["checkout", "-q", "main"]);
    std::fs::write(tmp.path().join("shared.txt"), "dirty session edit\n").unwrap();

    let report = broker
        .adopt_with(tmp.path(), Some("follow-up"), AdoptMode::Reuse)
        .unwrap();
    assert_eq!(report.session.id, session.id);
    let drift = report.integration_drift.expect("reuse drift");
    assert_eq!(drift.relation, AdoptIntegrationRelation::Behind);
    assert_eq!(drift.session_head, rev(tmp.path(), "main"));
    assert_eq!(drift.integration_head, integration_head);
    assert_eq!(drift.ahead_commits, 0);
    assert_eq!(drift.behind_commits, 1);
    assert_eq!(drift.overlapping_changed_paths, vec!["shared.txt"]);
    assert!(drift.warning.as_deref().unwrap().contains("behind"));
    assert_eq!(drift.safe_next_action, "aethyme broker integration status");
}

#[test]
fn adopt_reuse_reports_diverged_commit_counts_and_overlap() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.adopt(tmp.path(), Some("first task")).unwrap();

    sh(tmp.path(), &["checkout", "-qb", "integration-work"]);
    std::fs::write(tmp.path().join("README.md"), "integration\n").unwrap();
    sh(tmp.path(), &["commit", "-qam", "integration advances"]);
    let integration_head = rev(tmp.path(), "HEAD");
    sh(tmp.path(), &["branch", "aethyme/integration", "HEAD"]);
    sh(tmp.path(), &["checkout", "-q", "main"]);
    std::fs::write(tmp.path().join("README.md"), "session\n").unwrap();
    sh(tmp.path(), &["commit", "-qam", "session advances"]);
    let session_head = rev(tmp.path(), "HEAD");

    let report = broker
        .adopt_with(tmp.path(), Some("follow-up"), AdoptMode::Reuse)
        .unwrap();
    assert_eq!(report.session.id, session.id);
    let drift = report.integration_drift.expect("reuse drift");
    assert_eq!(drift.relation, AdoptIntegrationRelation::Diverged);
    assert_eq!(drift.session_head, session_head);
    assert_eq!(drift.integration_head, integration_head);
    assert_eq!(drift.ahead_commits, 1);
    assert_eq!(drift.behind_commits, 1);
    assert_eq!(drift.overlapping_changed_paths, vec!["README.md"]);
    assert!(drift.warning.as_deref().unwrap().contains("diverged"));
}

#[test]
fn adopt_reuse_reports_ahead_work_and_routes_it_to_submit() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let worktree = tmp.path().join("agent-wt");
    sh(
        tmp.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent/ahead",
            worktree.to_str().unwrap(),
            "main",
        ],
    );
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.adopt(&worktree, Some("first task")).unwrap();
    std::fs::write(worktree.join("ahead.txt"), "session work\n").unwrap();
    sh(&worktree, &["add", "ahead.txt"]);
    sh(&worktree, &["commit", "-qm", "session advances"]);

    let report = broker
        .adopt_with(&worktree, Some("follow-up"), AdoptMode::Reuse)
        .unwrap();
    let drift = report.integration_drift.expect("reuse drift");
    assert_eq!(drift.relation, AdoptIntegrationRelation::Ahead);
    assert_eq!(drift.ahead_commits, 1);
    assert_eq!(drift.behind_commits, 0);
    assert_eq!(
        drift.safe_next_action,
        format!("aethyme broker submit --session {}", session.id)
    );
}

#[test]
fn adopt_reuse_sync_fast_forwards_before_refreshing_the_baseline() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.adopt(tmp.path(), Some("first task")).unwrap();
    let original_head = rev(tmp.path(), "HEAD");

    sh(tmp.path(), &["checkout", "-qb", "integration-work"]);
    std::fs::write(tmp.path().join("integration.txt"), "advance\n").unwrap();
    sh(tmp.path(), &["add", "integration.txt"]);
    sh(tmp.path(), &["commit", "-qm", "integration advances"]);
    let integration_head = rev(tmp.path(), "HEAD");
    sh(tmp.path(), &["branch", "aethyme/integration", "HEAD"]);
    sh(tmp.path(), &["checkout", "-q", "main"]);

    let report = broker
        .adopt_with_options(
            tmp.path(),
            Some("synchronized follow-up"),
            AdoptOptions {
                mode: AdoptMode::Reuse,
                sync_integration: true,
            },
        )
        .unwrap();

    assert_eq!(report.session.id, session.id);
    assert_eq!(rev(tmp.path(), "HEAD"), integration_head);
    assert_eq!(
        report.session.diff_base.as_deref(),
        Some(integration_head.as_str())
    );
    assert_eq!(
        report.session.adoption_base.as_deref(),
        Some(original_head.as_str()),
        "guarded synchronization may refresh diff_base but not adoption_base"
    );
    let sync = report.integration_sync.expect("synchronization result");
    assert_eq!(sync.outcome, AdoptIntegrationSyncOutcome::FastForwarded);
    assert_eq!(sync.before_head, original_head);
    assert_eq!(sync.after_head, integration_head);
    assert_eq!(
        report.integration_drift.expect("post-sync drift").relation,
        AdoptIntegrationRelation::Current
    );
}

#[test]
fn adopt_reuse_sync_reports_an_already_current_checkout() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.adopt(tmp.path(), Some("first task")).unwrap();
    let head = rev(tmp.path(), "HEAD");

    let report = broker
        .adopt_with_options(
            tmp.path(),
            Some("current follow-up"),
            AdoptOptions {
                mode: AdoptMode::Reuse,
                sync_integration: true,
            },
        )
        .unwrap();

    assert_eq!(report.session.id, session.id);
    assert_eq!(report.session.diff_base.as_deref(), Some(head.as_str()));
    let sync = report.integration_sync.expect("synchronization result");
    assert_eq!(sync.outcome, AdoptIntegrationSyncOutcome::AlreadyCurrent);
    assert_eq!(sync.before_head, head);
    assert_eq!(sync.after_head, head);
}

#[test]
fn adopt_reuse_sync_refuses_dirty_state_without_changing_head_or_session() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.adopt(tmp.path(), Some("first task")).unwrap();
    let original_head = rev(tmp.path(), "HEAD");

    sh(tmp.path(), &["checkout", "-qb", "integration-work"]);
    std::fs::write(tmp.path().join("integration.txt"), "advance\n").unwrap();
    sh(tmp.path(), &["add", "integration.txt"]);
    sh(tmp.path(), &["commit", "-qm", "integration advances"]);
    sh(tmp.path(), &["branch", "aethyme/integration", "HEAD"]);
    sh(tmp.path(), &["checkout", "-q", "main"]);
    std::fs::write(tmp.path().join("dirty.txt"), "wip\n").unwrap();

    let err = broker
        .adopt_with_options(
            tmp.path(),
            Some("must not persist"),
            AdoptOptions {
                mode: AdoptMode::Reuse,
                sync_integration: true,
            },
        )
        .unwrap_err();

    assert!(matches!(err, BrokerOpError::ReuseSyncDirty { .. }));
    assert_eq!(rev(tmp.path(), "HEAD"), original_head);
    let persisted = broker.store().session(session.id).unwrap();
    assert_eq!(persisted.task, session.task);
    assert_eq!(persisted.diff_base, session.diff_base);
}

#[test]
fn adopt_reuse_sync_refuses_divergence_without_changing_head_or_session() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.adopt(tmp.path(), Some("first task")).unwrap();

    sh(tmp.path(), &["checkout", "-qb", "integration-work"]);
    std::fs::write(tmp.path().join("integration.txt"), "advance\n").unwrap();
    sh(tmp.path(), &["add", "integration.txt"]);
    sh(tmp.path(), &["commit", "-qm", "integration advances"]);
    sh(tmp.path(), &["branch", "aethyme/integration", "HEAD"]);
    sh(tmp.path(), &["checkout", "-q", "main"]);
    std::fs::write(tmp.path().join("session.txt"), "advance\n").unwrap();
    sh(tmp.path(), &["add", "session.txt"]);
    sh(tmp.path(), &["commit", "-qm", "session advances"]);
    let session_head = rev(tmp.path(), "HEAD");

    let err = broker
        .adopt_with_options(
            tmp.path(),
            Some("must not persist"),
            AdoptOptions {
                mode: AdoptMode::Reuse,
                sync_integration: true,
            },
        )
        .unwrap_err();

    assert!(matches!(
        err,
        BrokerOpError::ReuseSyncNotFastForward {
            relation: "diverged",
            ..
        }
    ));
    assert_eq!(rev(tmp.path(), "HEAD"), session_head);
    let persisted = broker.store().session(session.id).unwrap();
    assert_eq!(persisted.task, session.task);
    assert_eq!(persisted.diff_base, session.diff_base);
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
    assert!(dirty.pending_work.present);
    assert_eq!(dirty.pending_work.dirty_path_count, 1);
    assert_eq!(dirty.pending_work.unsubmitted_commits, 0);
    assert!(!dirty.delivery.submitted);
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
    assert!(
        broker
            .store()
            .events_after(0, i64::MAX)
            .unwrap()
            .iter()
            .all(|event| event.kind != events::SESSION_FINISHED),
        "refused finishes must not persist a completed handoff"
    );

    sh(&wt, &["add", "-A"]);
    sh(&wt, &["commit", "-qm", "finish wip"]);
    let unsubmitted = broker.finish(session.id).unwrap();
    assert_eq!(unsubmitted.status, FinishStatus::Blocked);
    assert_eq!(unsubmitted.unsubmitted_commits, 1);
    assert!(unsubmitted.pending_work.present);
    assert_eq!(unsubmitted.pending_work.dirty_path_count, 0);
    assert_eq!(unsubmitted.pending_work.unsubmitted_commits, 1);
    assert_eq!(
        unsubmitted.next_commands,
        vec![format!("aethyme broker submit --session {}", session.id)]
    );
    assert_ne!(
        broker.store().session(session.id).unwrap().status,
        SessionStatus::Cleaned
    );
    assert!(
        broker
            .store()
            .events_after(0, i64::MAX)
            .unwrap()
            .iter()
            .all(|event| event.kind != events::SESSION_FINISHED)
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
    let queue_entry = broker
        .store()
        .merge_queue()
        .unwrap()
        .into_iter()
        .last()
        .unwrap();
    broker
        .store()
        .claim_lease(session.id, "src/", None)
        .unwrap();
    broker
        .store()
        .claim_lease(session.id, "released.txt", None)
        .unwrap();
    broker
        .store()
        .release_lease(session.id, "released.txt")
        .unwrap();
    broker
        .store()
        .claim_lease(session.id, "expired.txt", Some(-1))
        .unwrap();
    broker
        .store()
        .claim_lease(session.id, "/must/not/enter/lease", None)
        .unwrap();
    let gate_tree = rev(&wt, "HEAD^{tree}");
    broker
        .store()
        .record_gate_result(&NewGateResult {
            gate_name: "handoff-gate".into(),
            tree_hash: gate_tree.clone(),
            definition_hash: "test-definition".into(),
            status: GateStatus::Pass,
            failure_class: None,
            exit_code: Some(0),
            duration_ms: Some(12),
            log_path: Some("/must/not/enter/handoff.log".into()),
            session_id: Some(session.id),
        })
        .unwrap();
    broker
        .store()
        .append_event(
            events::GATE_CACHED,
            Some(session.id),
            Some(&events::gate_cached_payload(
                "handoff-gate",
                &gate_tree,
                12,
                GateStatus::Pass,
                None,
            )),
        )
        .unwrap();

    let closed = broker.finish(session.id).unwrap();
    assert_eq!(closed.status, FinishStatus::Closed);
    assert!(closed.closed);
    assert!(!closed.cleanup_safe);
    assert!(closed.next_commands.is_empty());
    assert!(closed.delivery.submitted);
    assert!(closed.delivery.promoted);
    assert!(!closed.delivery.published);
    assert!(!closed.pending_work.present);
    assert_eq!(closed.pending_work.dirty_path_count, 0);
    assert_eq!(closed.pending_work.unsubmitted_commits, 0);
    assert_eq!(
        closed.recommended_next_action,
        Some(format!(
            "aethyme broker ship plan --entry {}",
            queue_entry.id
        ))
    );
    assert!(
        closed
            .leases_held
            .iter()
            .any(|lease| { lease.path == "src/" && lease.state == FinishLeaseState::Active })
    );
    assert!(closed.leases_held.iter().any(|lease| {
        lease.path == "released.txt" && lease.state == FinishLeaseState::Released
    }));
    assert!(
        closed.leases_held.iter().any(|lease| {
            lease.path == "expired.txt" && lease.state == FinishLeaseState::Expired
        })
    );
    assert!(
        closed
            .leases_held
            .iter()
            .any(|lease| lease.path == "<absolute-path-redacted>")
    );
    let last_gate = closed.last_gate.as_ref().unwrap();
    assert_eq!(last_gate.gate, "handoff-gate");
    assert_eq!(last_gate.tree_hash, gate_tree);
    assert_eq!(last_gate.status, GateStatus::Pass);
    assert_eq!(last_gate.cache_source, FinishGateCacheSource::CacheHit);
    assert_eq!(
        broker.store().session(session.id).unwrap().status,
        SessionStatus::Cleaned
    );
    assert!(
        broker
            .store()
            .session_leases(session.id)
            .unwrap()
            .is_empty()
    );
    let events = broker.store().events_after(0, i64::MAX).unwrap();
    let finished = events
        .iter()
        .filter(|event| event.kind == events::SESSION_FINISHED)
        .collect::<Vec<_>>();
    assert_eq!(finished.len(), 1);
    let cleaned_index = events
        .iter()
        .position(|event| event.kind == "session.cleaned")
        .unwrap();
    let finished_index = events
        .iter()
        .position(|event| event.kind == events::SESSION_FINISHED)
        .unwrap();
    assert_eq!(finished_index, cleaned_index + 1);
    assert_eq!(events[cleaned_index].ts, events[finished_index].ts);
    let payload = finished[0].payload_json.as_deref().unwrap();
    let handoff: serde_json::Value = serde_json::from_str(payload).unwrap();
    assert_eq!(handoff["session_id"], session.id);
    assert_eq!(handoff["delivery"]["promoted"], true);
    assert_eq!(handoff["delivery"]["published"], false);
    assert_eq!(handoff["last_gate"]["cache_source"], "cache_hit");
    assert!(handoff["leases_held"].as_array().unwrap().len() >= 3);
    assert!(handoff.get("worktree_path").is_none());
    assert!(!payload.contains(tmp.path().to_str().unwrap()));
    assert!(!payload.contains("must/not/enter"));

    sh(tmp.path(), &["merge", "--ff-only", "aethyme/integration"]);
    let already = broker.finish(session.id).unwrap();
    assert_eq!(already.status, FinishStatus::AlreadyClosed);
    assert!(already.cleanup_safe);
    assert_eq!(
        already.next_commands,
        vec![format!("aethyme broker cleanup {}", session.id)]
    );
    assert_eq!(
        broker
            .store()
            .events_after(0, i64::MAX)
            .unwrap()
            .iter()
            .filter(|event| event.kind == events::SESSION_FINISHED)
            .count(),
        1,
        "already-closed finish must not emit a duplicate handoff"
    );
}

#[test]
fn finish_distinguishes_fully_published_work_from_local_promotion() {
    let tmp = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    sh(remote.path(), &["init", "--bare", "-q"]);
    sh(
        tmp.path(),
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    );
    sh(tmp.path(), &["push", "-qu", "origin", "main"]);

    let wt = tmp.path().join("published-finish-wt");
    sh(
        tmp.path(),
        &[
            "worktree",
            "add",
            "-b",
            "agent/published-finish",
            wt.to_str().unwrap(),
            "main",
        ],
    );
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.adopt(&wt, Some("published finish task")).unwrap();
    std::fs::write(wt.join("published.txt"), "published\n").unwrap();
    sh(&wt, &["add", "-A"]);
    sh(&wt, &["commit", "-qm", "published"]);
    assert!(broker.submit(session.id).unwrap().promoted);
    sh(
        tmp.path(),
        &["push", "-q", "origin", "aethyme/integration:main"],
    );
    sh(tmp.path(), &["fetch", "-q", "origin", "main"]);

    let closed = broker.finish(session.id).unwrap();
    assert_eq!(closed.status, FinishStatus::Closed);
    assert!(closed.delivery.submitted);
    assert!(closed.delivery.promoted);
    assert!(closed.delivery.published);
    assert!(closed.last_gate.is_none());
    assert_eq!(
        closed.recommended_next_action.as_deref(),
        Some("aethyme broker integration status")
    );
    let event = broker
        .store()
        .events_after(0, i64::MAX)
        .unwrap()
        .into_iter()
        .find(|event| event.kind == events::SESSION_FINISHED)
        .unwrap();
    let handoff: serde_json::Value =
        serde_json::from_str(event.payload_json.as_deref().unwrap()).unwrap();
    assert_eq!(handoff["delivery"]["published"], true);
    assert!(handoff["last_gate"].is_null());
}

#[test]
fn finish_missing_worktree_persists_an_explicitly_incomplete_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let wt = tmp.path().join("missing-finish-wt");
    sh(
        tmp.path(),
        &[
            "worktree",
            "add",
            "-b",
            "agent/missing-finish",
            wt.to_str().unwrap(),
            "main",
        ],
    );
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.adopt(&wt, Some("missing finish task")).unwrap();
    sh(
        tmp.path(),
        &["worktree", "remove", "--force", wt.to_str().unwrap()],
    );

    let closed = broker.finish(session.id).unwrap();
    assert_eq!(closed.status, FinishStatus::Closed);
    assert!(closed.closed);
    assert!(closed.pending_work.worktree_missing);
    assert!(!closed.pending_work.present);
    assert!(!closed.delivery.submitted);
    assert!(!closed.cleanup_safe);
    let event = broker
        .store()
        .events_after(0, i64::MAX)
        .unwrap()
        .into_iter()
        .find(|event| event.kind == events::SESSION_FINISHED)
        .unwrap();
    let handoff: serde_json::Value =
        serde_json::from_str(event.payload_json.as_deref().unwrap()).unwrap();
    assert_eq!(handoff["pending_work"]["worktree_missing"], true);
}
