//! End-to-end tests for the session-facing broker API (issues #7-#10):
//! adopt on hand-made worktrees, spawn with log capture, liveness
//! derivation, and cleanup guards — against real git repos and processes.

use std::path::Path;
use std::process::Command;

mod common;

use aethyme_broker::{
    AdoptIntegrationRelation, AdoptIntegrationSyncOutcome, AdoptMode, AdoptOptions, Broker,
    BrokerOpError, CleanupDisposition, CleanupRepresentation, FinishGateCacheSource,
    FinishLeaseState, FinishOptions, FinishStatus, GateStatus, NewGateResult, NewSession,
    SessionOrigin, SessionStatus, VersionDriftStatus, events,
};

fn open_fixture_broker(path: &Path) -> Result<common::FixtureBroker, BrokerOpError> {
    let fixture_root = path.parent().unwrap_or(path);
    common::open_fixture_broker(path, fixture_root)
}

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

fn ref_exists(cwd: &Path, reference: &str) -> bool {
    Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", reference])
        .current_dir(cwd)
        .output()
        .unwrap()
        .status
        .success()
}

fn init_repo(root: &Path) {
    sh(root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("README.md"), "hello\n").unwrap();
    std::fs::write(root.join(".gitignore"), "/.aethyme/\n").unwrap();
    sh(root, &["add", "-A"]);
    sh(root, &["commit", "-qm", "init"]);
}

fn set_integration_at_head(root: &Path) {
    let head = rev(root, "HEAD");
    sh(
        root,
        &[
            "update-ref",
            "refs/heads/aethyme/integration",
            head.as_str(),
        ],
    );
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn wait_for_agent_pid_exit(broker: &Broker, session_id: i64) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let view = broker
            .agents_snapshot(now_ms())
            .unwrap()
            .into_iter()
            .find(|view| view.session.id == session_id);
        if view
            .as_ref()
            .is_some_and(|view| view.pid_alive == Some(false))
        {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "spawned process for session {session_id} did not exit before the deadline"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[test]
fn adopted_hand_made_worktree_and_spawned_session_are_indistinguishable() {
    let tmp = common::FixtureRepository::new();
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
    let external_root = tempfile::tempdir().unwrap();
    let worktree_root = external_root.path().join("worktrees");
    let mut broker = common::open_fixture_broker(&hand_made, tmp.owner_path())
        .unwrap()
        .with_worktree_root(&worktree_root);
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
        .start_agent("spawned task", "echo out; echo err >&2", None)
        .unwrap();
    assert_eq!(spawned.origin, SessionOrigin::Spawned);
    assert!(spawned.pid.is_some());
    let spawned_path = std::path::PathBuf::from(&spawned.worktree_path);
    assert_eq!(
        spawned_path.parent(),
        Some(worktree_root.canonicalize().unwrap().as_path())
    );
    assert!(spawned_path.ends_with("spawned-task"));

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
    let tmp = common::FixtureRepository::new();
    let external_root = tempfile::tempdir().unwrap();
    let worktree_root = external_root.path().join("worktrees");
    init_repo(tmp.path());
    let mut broker = open_fixture_broker(tmp.path())
        .unwrap()
        .with_worktree_root(&worktree_root);

    let session = broker
        .start_worktree(
            "isolated edits",
            Some("Claude Opus 5 <noreply@anthropic.com>"),
        )
        .unwrap();
    let wt = std::path::PathBuf::from(&session.worktree_path);

    assert_eq!(session.origin, SessionOrigin::Spawned);
    assert!(session.pid.is_none());
    assert_eq!(session.branch, "agent/isolated-edits");
    // `start` is the primary agent entrypoint and never calls adopt, so it
    // has to be able to record the identity itself or the promote commit
    // would never credit an agent on this path.
    assert_eq!(
        session.agent_identity.as_deref(),
        Some("Claude Opus 5 <noreply@anthropic.com>")
    );
    assert!(wt.exists());
    assert_eq!(
        wt.parent(),
        Some(worktree_root.canonicalize().unwrap().as_path())
    );
    assert!(wt.ends_with("isolated-edits"));
    // Build discipline sits one directory above the worktree: cargo finds it
    // by walking up, it is never an untracked file inside the checkout, and a
    // repository shipping its own `.cargo/config.toml` still outranks it.
    let cargo_config = worktree_root.join(".cargo/config.toml");
    let defaults = std::fs::read_to_string(&cargo_config).unwrap();
    assert!(defaults.contains("incremental = false"), "{defaults}");
    assert!(defaults.contains("line-tables-only"), "{defaults}");
    assert!(!wt.join(".cargo").exists());
    assert!(
        broker
            .store()
            .session_foreign_files(session.id)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn two_session_worktrees_are_distinct_and_registered() {
    let tmp = common::FixtureRepository::new();
    let external_root = tempfile::tempdir().unwrap();
    let worktree_root = external_root.path().join("worktrees");
    init_repo(tmp.path());
    let mut broker = open_fixture_broker(tmp.path())
        .unwrap()
        .with_worktree_root(&worktree_root);

    let first = broker
        .start_worktree("first isolated session", None)
        .unwrap();
    let second = broker
        .start_worktree("second isolated session", None)
        .unwrap();
    assert_ne!(first.id, second.id);
    assert_ne!(first.branch, second.branch);
    assert_ne!(first.worktree_path, second.worktree_path);
    assert!(Path::new(&first.worktree_path).is_dir());
    assert!(Path::new(&second.worktree_path).is_dir());

    let sessions = broker.agents(now_ms()).unwrap();
    assert_eq!(sessions.len(), 2);
    assert!(sessions.iter().any(
        |view| view.session.id == first.id && view.session.worktree_path == first.worktree_path
    ));
    assert!(
        sessions.iter().any(|view| view.session.id == second.id
            && view.session.worktree_path == second.worktree_path)
    );
}

#[test]
fn compatibility_status_snapshot_derives_without_reconciling_state() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = open_fixture_broker(tmp.path()).unwrap();
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
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    set_integration_at_head(tmp.path());
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

    let mut broker = open_fixture_broker(tmp.path()).unwrap();
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
        .adopt_with(tmp.path(), Some("follow-up"), AdoptMode::Reuse, None)
        .unwrap()
        .session;
    assert_eq!(reused.id, created.id);
    assert_eq!(reused.repository_contract, Some(original_contract));
    assert_eq!(reused.adoption_base, created.adoption_base);
}

#[test]
fn opening_the_broker_backfills_live_pre_contract_sessions() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let head = rev(tmp.path(), "HEAD");
    let session_id = {
        let mut broker = open_fixture_broker(tmp.path()).unwrap();
        let root_setup = broker
            .start_worktree("bind root before backfill", None)
            .unwrap();
        broker.close(root_setup.id).unwrap();
        broker.cleanup(root_setup.id, false).unwrap();
        broker
            .store()
            .register_session(&NewSession {
                worktree_path: tmp.path().to_string_lossy().into_owned(),
                branch: "main".into(),
                origin: SessionOrigin::Adopted,
                task: Some("pre-contract session".into()),
                diff_base: Some(head.clone()),
                adoption_base: None,
                adopted_head: None,
                repository_contract: None,
                pid: None,
                command: None,
                log_path: None,
                agent_identity: None,
            })
            .unwrap()
            .id
    };

    let mut reopened = open_fixture_broker(tmp.path()).unwrap();
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
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = open_fixture_broker(tmp.path()).unwrap();

    let session = broker.start_agent("quick", "true", None).unwrap();
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
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = open_fixture_broker(tmp.path()).unwrap();

    let session = broker.start_agent("doomed", "true", None).unwrap();
    let wt = std::path::PathBuf::from(&session.worktree_path);
    wait_for_agent_pid_exit(&broker, session.id);
    broker.close(session.id).unwrap();
    assert!(
        broker
            .store()
            .session(session.id)
            .unwrap()
            .status
            .is_closed()
    );
    assert_eq!(
        broker.store().session(session.id).unwrap().cleanup_state,
        aethyme_broker::SessionCleanupState::Closed
    );

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
    assert!(err.to_string().contains("never been accepted"));

    // Force discards, marks cleaned, frees the worktree slot.
    broker.cleanup(session.id, true).unwrap();
    assert!(!wt.exists());
    let views = broker.agents(now_ms()).unwrap();
    assert!(views.iter().all(|v| v.session.id != session.id));
}

#[test]
fn cleanup_plan_and_apply_reclaim_only_safe_closed_broker_worktrees() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/\n/target/\n").unwrap();
    sh(tmp.path(), &["add", ".gitignore"]);
    sh(tmp.path(), &["commit", "-qm", "ignore build output"]);
    let mut broker = open_fixture_broker(tmp.path()).unwrap();

    let safe = broker.start_worktree("safe cleanup", None).unwrap();
    let safe_path = std::path::PathBuf::from(&safe.worktree_path);
    std::fs::write(safe_path.join("safe.txt"), "safe\n").unwrap();
    sh(&safe_path, &["add", "safe.txt"]);
    sh(&safe_path, &["commit", "-qm", "safe work"]);
    aethyme_testkit::ensure_origin(broker.main_root());
    broker.submit(safe.id).unwrap();
    aethyme_testkit::land_as_pull_request(
        broker.main_root(),
        &safe.branch,
        aethyme_testkit::MergeStyle::MergeCommit,
    );
    let finished = broker
        .finish_with_options(
            safe.id,
            FinishOptions {
                keep_worktree: true,
            },
        )
        .unwrap();
    assert!(finished.cleanup_safe);
    std::fs::create_dir_all(safe_path.join("target/debug")).unwrap();
    std::fs::write(
        safe_path.join("target/CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n",
    )
    .unwrap();
    std::fs::write(safe_path.join("target/debug/cache.bin"), vec![7_u8; 4096]).unwrap();

    let dirty = broker.start_worktree("dirty cleanup", None).unwrap();
    let dirty_path = std::path::PathBuf::from(&dirty.worktree_path);
    std::fs::write(dirty_path.join("dirty.txt"), "committed\n").unwrap();
    sh(&dirty_path, &["add", "dirty.txt"]);
    sh(&dirty_path, &["commit", "-qm", "delivered work"]);
    aethyme_testkit::ensure_origin(broker.main_root());
    broker.submit(dirty.id).unwrap();
    aethyme_testkit::land_as_pull_request(
        broker.main_root(),
        &dirty.branch,
        aethyme_testkit::MergeStyle::MergeCommit,
    );
    assert_eq!(
        broker
            .finish_with_options(
                dirty.id,
                FinishOptions {
                    keep_worktree: true,
                },
            )
            .unwrap()
            .status,
        FinishStatus::Closed
    );
    std::fs::write(dirty_path.join("untracked.txt"), "keep me\n").unwrap();

    let status = broker.status(now_ms()).unwrap();
    assert_eq!(status.cleanup_retention.broker_owned_worktree_count, 2);
    assert_eq!(status.cleanup_retention.retained_session_branch_count, 2);
    assert_eq!(status.cleanup_retention.eligible_worktree_count, 1);
    assert!(status.cleanup_retention.estimated_retained_bytes >= 4096);
    assert_eq!(
        status.cleanup_retention.estimated_blocked_bytes,
        status
            .cleanup_retention
            .estimated_retained_bytes
            .saturating_sub(status.cleanup_retention.estimated_reclaimable_bytes)
    );
    assert_eq!(
        status.cleanup_retention.retained_bytes_budget,
        1_073_741_824
    );
    assert!(!status.cleanup_retention.over_retained_bytes_budget);
    assert_eq!(
        status.cleanup_retention.severity,
        aethyme_broker::StatusAdviceSeverity::Notice
    );
    let cleanup_advice = status
        .advice
        .iter()
        .find(|advice| advice.id == "cleanup.retained-worktrees")
        .unwrap();
    assert!(
        cleanup_advice
            .commands
            .iter()
            .any(|command| command.contains("cleanup --all-cleaned"))
    );

    std::fs::write(
        tmp.path().join(".aethyme/broker.toml"),
        "[retention]\nretained_bytes_budget = 1\nartifact_sweep_budget_ms = 0\n",
    )
    .unwrap();
    let pressured = broker.status(now_ms()).unwrap();
    let pressured_advice = pressured
        .advice
        .iter()
        .find(|advice| advice.id == "cleanup.retained-worktrees")
        .unwrap();
    assert!(pressured.cleanup_retention.over_retained_bytes_budget);
    assert_eq!(
        pressured_advice.commands,
        vec![
            "aethyme broker gc plan",
            "aethyme broker gc apply --confirm <sha256-from-plan>",
        ]
    );

    let dry_run = broker.cleanup_cleaned_worktrees(false, None).unwrap();
    assert!(!dry_run.applied);
    assert_eq!(dry_run.plan.retained_worktree_count, 2);
    assert_eq!(dry_run.plan.eligible_worktree_count, 1);
    assert!(dry_run.plan.estimated_reclaimable_bytes >= 4096);
    assert!(dry_run.removed_session_ids.is_empty());
    assert!(safe_path.exists());
    assert!(dirty_path.exists());
    let safe_item = dry_run
        .plan
        .worktrees
        .iter()
        .find(|item| item.session_id == safe.id)
        .unwrap();
    let safe_provenance = safe_item.provenance.as_ref().unwrap();
    assert_eq!(
        safe_provenance.representation,
        CleanupRepresentation::Represented
    );
    assert_eq!(
        safe_provenance.accepted_session_head,
        broker
            .store()
            .session(safe.id)
            .unwrap()
            .accepted_session_head
    );
    // Nothing is accepted locally: the merged pull request is the proof.
    assert!(safe_provenance.accepted_queue_entry_id.is_none());
    assert!(safe_provenance.represented_on.is_some());
    assert_eq!(
        dry_run
            .plan
            .worktrees
            .iter()
            .find(|item| item.session_id == dirty.id)
            .unwrap()
            .disposition,
        CleanupDisposition::Dirty
    );

    let digest = dry_run.plan.digest.clone();
    let applied = broker
        .cleanup_cleaned_worktrees(true, Some(&digest))
        .unwrap();
    assert!(applied.applied);
    assert_eq!(applied.removed_session_ids, vec![safe.id]);
    assert!(applied.failures.is_empty());
    assert!(!safe_path.exists());
    assert!(!ref_exists(
        tmp.path(),
        &format!("refs/heads/{}", safe.branch)
    ));
    assert!(dirty_path.exists());
    assert!(ref_exists(
        tmp.path(),
        &format!("refs/heads/{}", dirty.branch)
    ));
}

#[test]
fn bulk_cleanup_apply_reclaims_build_output_of_closed_kept_worktrees_only() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    std::fs::write(
        tmp.path().join(".gitignore"),
        "/.aethyme/\n/target/\n/.claude/settings.local.json\n",
    )
    .unwrap();
    sh(tmp.path(), &["add", ".gitignore"]);
    sh(tmp.path(), &["commit", "-qm", "ignore build output"]);
    std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/broker.toml"),
        "[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 0\n",
    )
    .unwrap();
    let mut broker = open_fixture_broker(tmp.path()).unwrap();
    let populate = |worktree: &Path| {
        std::fs::create_dir_all(worktree.join(".claude")).unwrap();
        std::fs::write(worktree.join(".claude/settings.local.json"), "{}\n").unwrap();
        std::fs::create_dir_all(worktree.join("target/debug")).unwrap();
        std::fs::write(
            worktree.join("target/CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55\n",
        )
        .unwrap();
        std::fs::write(worktree.join("target/debug/cache.bin"), vec![7_u8; 4096]).unwrap();
    };

    let closed = broker.start_worktree("already closed kept", None).unwrap();
    let closed_path = std::path::PathBuf::from(&closed.worktree_path);
    broker.close(closed.id).unwrap();
    // Build output that outlived the close models checkouts that were
    // already closed, caches intact, before this sweep reclaimed them.
    populate(&closed_path);
    let live = broker.start_worktree("live with a build", None).unwrap();
    let live_path = std::path::PathBuf::from(&live.worktree_path);
    populate(&live_path);
    std::fs::write(
        tmp.path().join(".aethyme/broker.toml"),
        "[retention]\nartifact_reclaim_days = 0\nartifact_sweep_budget_ms = 5000\n",
    )
    .unwrap();

    let plan = broker.cleanup_cleaned_worktrees(false, None).unwrap();
    assert_eq!(plan.artifact_bytes_reclaimed, 0, "a plan is read-only");
    assert!(closed_path.join("target/debug/cache.bin").is_file());
    let applied = broker
        .cleanup_cleaned_worktrees(true, Some(&plan.plan.digest))
        .unwrap();

    assert!(closed_path.exists(), "the kept checkout must stay");
    assert!(
        !closed_path.join("target").exists(),
        "apply must reclaim a closed kept worktree's build output: {:?}",
        applied.failures
    );
    assert!(applied.artifact_directories_reclaimed >= 1);
    assert!(applied.artifact_bytes_reclaimed >= 4096);
    assert!(closed_path.join(".claude/settings.local.json").is_file());
    assert!(
        live_path.join("target/debug/cache.bin").is_file(),
        "a live session's build output must never be touched"
    );
}

/// A session whose one commit landed as a merged pull request, finished with
/// its checkout kept, as an agent leaves it.
fn landed_kept_session(broker: &mut Broker, task: &str, file: &str) -> (i64, std::path::PathBuf) {
    let session = broker.start_worktree(task, None).unwrap();
    let path = std::path::PathBuf::from(&session.worktree_path);
    std::fs::write(path.join(file), "landed\n").unwrap();
    sh(&path, &["add", file]);
    sh(&path, &["commit", "-qm", task]);
    aethyme_testkit::ensure_origin(broker.main_root());
    broker.submit(session.id).unwrap();
    aethyme_testkit::land_as_pull_request(
        broker.main_root(),
        &session.branch,
        aethyme_testkit::MergeStyle::MergeCommit,
    );
    let finished = broker
        .finish_with_options(
            session.id,
            FinishOptions {
                keep_worktree: true,
            },
        )
        .unwrap();
    assert_eq!(finished.status, FinishStatus::Closed);
    (session.id, path)
}

/// Write the bytes Aethyme's deploy writes at `relative`.
fn write_generated(worktree: &Path, relative: &str) -> Vec<u8> {
    let content = aethyme_enhance::deploy::generated_artifact_contents(relative)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("{relative} is not a generated artifact"));
    let path = worktree.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, &content).unwrap();
    content.into_bytes()
}

#[test]
fn bulk_cleanup_removes_checkouts_whose_only_ignored_content_is_unmodified_generated_files() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    // Claude Code's runtime files are ignored the way deployed repositories
    // ignore them; the tracked skill keeps `.claude/` from collapsing into
    // one ignored root, so each runtime path is classified on its own.
    std::fs::write(
        tmp.path().join(".gitignore"),
        "/.aethyme/\n.claude/*\n!.claude/skills/\n",
    )
    .unwrap();
    std::fs::create_dir_all(tmp.path().join(".claude/skills/team")).unwrap();
    std::fs::write(tmp.path().join(".claude/skills/team/SKILL.md"), "team\n").unwrap();
    sh(
        tmp.path(),
        &["add", ".gitignore", ".claude/skills/team/SKILL.md"],
    );
    sh(
        tmp.path(),
        &["commit", "-qm", "ignore harness runtime files"],
    );
    let mut broker = open_fixture_broker(tmp.path()).unwrap();

    let (generated_id, generated_path) =
        landed_kept_session(&mut broker, "generated only", "generated.txt");
    write_generated(&generated_path, ".claude/settings.local.json");
    write_generated(&generated_path, ".claude/hooks/aethyme-load-context.sh");

    let (edited_id, edited_path) =
        landed_kept_session(&mut broker, "edited settings", "edited.txt");
    let mut edited = write_generated(&edited_path, ".claude/settings.local.json");
    write_generated(&edited_path, ".claude/hooks/aethyme-load-context.sh");
    // One byte differs, as when a harness records a granted permission.
    let last = edited.len() - 2;
    edited[last] = b' ';
    std::fs::write(edited_path.join(".claude/settings.local.json"), &edited).unwrap();

    let (unrelated_id, unrelated_path) =
        landed_kept_session(&mut broker, "unrelated ignored", "unrelated.txt");
    write_generated(&unrelated_path, ".claude/settings.local.json");
    std::fs::write(unrelated_path.join(".claude/notes.md"), "mine\n").unwrap();

    let plan = broker.cleanup_cleaned_worktrees(false, None).unwrap();
    let item = |id: i64| {
        plan.plan
            .worktrees
            .iter()
            .find(|item| item.session_id == id)
            .unwrap()
            .clone()
    };
    assert_eq!(
        item(generated_id).disposition,
        CleanupDisposition::Eligible,
        "{}",
        item(generated_id).reason
    );
    let edited_item = item(edited_id);
    assert_eq!(edited_item.disposition, CleanupDisposition::Dirty);
    assert!(
        edited_item.reason.contains(
            "ignored content requires explicit cleanup: ignored path .claude/settings.local.json is not inside a recognised build-output directory"
        ),
        "{}",
        edited_item.reason
    );
    let unrelated_item = item(unrelated_id);
    assert_eq!(unrelated_item.disposition, CleanupDisposition::Dirty);
    assert!(
        unrelated_item.reason.contains(".claude/notes.md"),
        "{}",
        unrelated_item.reason
    );

    let applied = broker
        .cleanup_cleaned_worktrees(true, Some(&plan.plan.digest))
        .unwrap();
    assert!(applied.applied);
    assert_eq!(applied.removed_session_ids, vec![generated_id]);
    assert!(!generated_path.exists());
    assert_eq!(
        std::fs::read(edited_path.join(".claude/settings.local.json")).unwrap(),
        edited,
        "a modified settings file is never discarded"
    );
    assert!(unrelated_path.join(".claude/notes.md").is_file());
}

#[test]
fn status_warns_before_the_linked_checkout_limit_refuses_start() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
    std::fs::write(
        tmp.path().join(".aethyme/config.toml"),
        "[worktrees]\nmax_checkouts = 4\n",
    )
    .unwrap();
    let mut broker = open_fixture_broker(tmp.path()).unwrap();
    let first = broker.start_worktree("first", None).unwrap();
    broker.start_worktree("second", None).unwrap();
    let below = broker.status(now_ms()).unwrap();
    assert!(
        below
            .advice
            .iter()
            .all(|advice| advice.id != "cleanup.checkout-capacity"),
        "two of four checkouts is below three quarters"
    );

    broker.close(first.id).unwrap();
    broker.start_worktree("third", None).unwrap();
    for (surface, advice) in [
        ("refresh", broker.status(now_ms()).unwrap().advice),
        ("routine", broker.status_current(now_ms()).unwrap().advice),
        ("compact", broker.status_brief(now_ms()).unwrap().advice),
    ] {
        let row = advice
            .iter()
            .find(|advice| advice.id == "cleanup.checkout-capacity")
            .unwrap_or_else(|| panic!("{surface} status has no capacity row: {advice:#?}"));
        assert_eq!(
            row.severity,
            aethyme_broker::StatusAdviceSeverity::Warning,
            "{surface}"
        );
        assert_eq!(
            row.summary,
            "3 of 4 linked checkouts are registered; 1 are not held by an open session",
            "{surface}"
        );
        assert!(
            row.evidence.contains(&"retained checkouts: 1".to_string()),
            "{surface}: {:?}",
            row.evidence
        );
        assert_eq!(
            row.commands,
            vec![
                "aethyme broker gc sweep --json",
                "aethyme broker finish cleanup --all-cleaned --json",
            ],
            "{surface}"
        );
    }
}

#[test]
fn cleanup_recovers_after_worktree_removal_and_prunes_only_the_exact_branch_tip() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = open_fixture_broker(tmp.path()).unwrap();

    let session = broker.start_worktree("interrupted cleanup", None).unwrap();
    let worktree = std::path::PathBuf::from(&session.worktree_path);
    std::fs::write(worktree.join("renamed.bin"), [0_u8, 7, 255]).unwrap();
    sh(&worktree, &["add", "renamed.bin"]);
    sh(&worktree, &["commit", "-qm", "represented binary"]);
    aethyme_testkit::ensure_origin(broker.main_root());
    broker.submit(session.id).unwrap();
    aethyme_testkit::land_as_pull_request(
        broker.main_root(),
        &session.branch,
        aethyme_testkit::MergeStyle::MergeCommit,
    );
    assert!(
        broker
            .finish_with_options(
                session.id,
                FinishOptions {
                    keep_worktree: true,
                },
            )
            .unwrap()
            .closed
    );
    let branch_ref = format!("refs/heads/{}", session.branch);
    let branch_tip = rev(tmp.path(), &branch_ref);

    sh(
        tmp.path(),
        &["worktree", "remove", worktree.to_str().unwrap()],
    );
    assert!(!worktree.exists());
    assert_eq!(rev(tmp.path(), &branch_ref), branch_tip);

    let plan = broker.cleanup_plan().unwrap();
    assert_eq!(plan.retained_worktree_count, 0);
    assert_eq!(plan.retained_branch_count, 1);
    let item = plan
        .worktrees
        .iter()
        .find(|item| item.session_id == session.id)
        .unwrap();
    assert!(!item.worktree_present);
    assert_eq!(item.branch_tip.as_deref(), Some(branch_tip.as_str()));
    assert!(item.delete_branch);
    assert!(item.eligible());

    broker.cleanup(session.id, false).unwrap();
    assert!(!ref_exists(tmp.path(), &branch_ref));
    assert!(broker.cleanup_plan().unwrap().worktrees.is_empty());
}

#[test]
fn cleanup_distinguishes_pending_commits_from_missing_acceptance_provenance() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = open_fixture_broker(tmp.path()).unwrap();

    let pending = broker.start_worktree("pending cleanup", None).unwrap();
    let pending_path = std::path::PathBuf::from(&pending.worktree_path);
    std::fs::write(pending_path.join("pending.bin"), [0_u8, 255, 17]).unwrap();
    sh(&pending_path, &["add", "pending.bin"]);
    sh(&pending_path, &["commit", "-qm", "pending binary"]);
    broker.close(pending.id).unwrap();

    // An accepted checkpoint whose queue, integration commit and tree
    // provenance are all missing, for work that reached no delivery target:
    // nothing can prove it landed, so it must stay unproven.
    let unproven = broker.start_worktree("unproven cleanup", None).unwrap();
    let unproven_path = std::path::PathBuf::from(&unproven.worktree_path);
    std::fs::write(unproven_path.join("renamed-before.txt"), "accepted\n").unwrap();
    sh(&unproven_path, &["add", "renamed-before.txt"]);
    sh(&unproven_path, &["commit", "-qm", "accepted rename source"]);
    let unproven_head = rev(&unproven_path, "HEAD");
    broker.close(unproven.id).unwrap();
    drop(broker);

    let database = tmp.path().join(aethyme_broker::BROKER_DB_RELPATH);
    let connection = rusqlite::Connection::open(database).unwrap();
    connection
        .execute(
            "UPDATE sessions SET accepted_session_head = ?1, accepted_queue_entry_id = NULL \
             WHERE id = ?2",
            rusqlite::params![unproven_head, unproven.id],
        )
        .unwrap();
    drop(connection);

    let broker = open_fixture_broker(tmp.path()).unwrap();
    let plan = broker.cleanup_plan().unwrap();
    let pending_item = plan
        .worktrees
        .iter()
        .find(|item| item.session_id == pending.id)
        .unwrap();
    assert_eq!(pending_item.disposition, CleanupDisposition::PendingCommits);
    assert_eq!(
        pending_item.provenance.as_ref().unwrap().representation,
        CleanupRepresentation::Pending
    );
    assert_eq!(
        pending_item
            .provenance
            .as_ref()
            .unwrap()
            .pending_commit_count,
        1
    );

    let unproven_item = plan
        .worktrees
        .iter()
        .find(|item| item.session_id == unproven.id)
        .unwrap();
    assert_eq!(
        unproven_item.disposition,
        CleanupDisposition::UnprovenProvenance
    );
    assert_eq!(
        unproven_item.provenance.as_ref().unwrap().representation,
        CleanupRepresentation::Unproven
    );
    assert!(unproven_item.reason.contains("missing queue"));
}

/// Deleting a landed session's acceptance bookkeeping used to leave it
/// `unproven_provenance` even though its content sits on the default branch.
/// The shared landing check (#408) reads the commits, not the bookkeeping, so
/// the worktree is eligible and names where the work is. A squash merge whose
/// branch was deleted on merge leaves content as the only evidence.
#[test]
fn missing_acceptance_bookkeeping_does_not_strand_landed_work() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = open_fixture_broker(tmp.path()).unwrap();
    let session = broker
        .start_worktree("landed then forgotten", None)
        .unwrap();
    let path = std::path::PathBuf::from(&session.worktree_path);
    std::fs::write(path.join("landed.txt"), "landed\n").unwrap();
    sh(&path, &["add", "landed.txt"]);
    sh(&path, &["commit", "-qm", "landed work"]);
    aethyme_testkit::ensure_origin(broker.main_root());
    broker.submit(session.id).unwrap();
    aethyme_testkit::land_as_pull_request(
        broker.main_root(),
        &session.branch,
        aethyme_testkit::MergeStyle::Squash,
    );
    aethyme_testkit::delete_remote_branch(broker.main_root(), &session.branch);
    broker.close(session.id).unwrap();
    drop(broker);

    let database = tmp.path().join(aethyme_broker::BROKER_DB_RELPATH);
    let connection = rusqlite::Connection::open(database).unwrap();
    for statement in [
        "UPDATE sessions SET accepted_queue_entry_id = NULL WHERE id = ?1",
        "DELETE FROM gc_checkpoint_pin_releases WHERE session_id = ?1",
        "DELETE FROM session_representations WHERE session_id = ?1",
    ] {
        connection.execute(statement, [session.id]).unwrap();
    }
    drop(connection);

    let broker = open_fixture_broker(tmp.path()).unwrap();
    let plan = broker.cleanup_plan().unwrap();
    let item = plan
        .worktrees
        .iter()
        .find(|item| item.session_id == session.id)
        .unwrap();
    assert_eq!(item.disposition, CleanupDisposition::Eligible, "{item:#?}");
    assert!(
        item.reason.contains("represented on delivery target"),
        "{}",
        item.reason
    );
}

/// In a shallow clone, a session older than the shallow boundary cannot be
/// related to main: `git merge-base` exits 1 with no message, so the reason
/// used to end in an empty `failed:`. It stays `inspection_failed`, but
/// now names the shallow clone and how to deepen it (#525).
#[test]
fn cleanup_names_a_shallow_clone_when_inspection_fails() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = open_fixture_broker(tmp.path()).unwrap();
    let session = broker
        .start_worktree("older than the boundary", None)
        .unwrap();
    let path = std::path::PathBuf::from(&session.worktree_path);
    std::fs::write(path.join("old.txt"), "old\n").unwrap();
    sh(&path, &["add", "old.txt"]);
    sh(&path, &["commit", "-qm", "old work"]);
    let session_head = rev(&path, "HEAD");
    broker.close(session.id).unwrap();
    drop(broker);

    sh(
        tmp.path(),
        &["commit", "-q", "--allow-empty", "-m", "main moves on"],
    );
    let main_head = rev(tmp.path(), "HEAD");
    // Cut history at both tips, as a shallow fetch leaves it: neither
    // commit has a parent locally, so they share no ancestor.
    std::fs::write(
        tmp.path().join(".git/shallow"),
        format!("{main_head}\n{session_head}\n"),
    )
    .unwrap();

    let broker = open_fixture_broker(tmp.path()).unwrap();
    let plan = broker.cleanup_plan().unwrap();
    let item = plan
        .worktrees
        .iter()
        .find(|item| item.session_id == session.id)
        .unwrap();
    assert_eq!(
        item.disposition,
        CleanupDisposition::InspectionFailed,
        "{item:#?}"
    );
    assert!(item.reason.contains("shallow clone"), "{}", item.reason);
    assert!(item.reason.contains("--shallow-since"), "{}", item.reason);
}

#[test]
fn doctor_reports_healthy_then_finds_missing_worktree() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = open_fixture_broker(tmp.path()).unwrap();

    // Missing host ownership history is unknown, even for an otherwise
    // empty repository. It must not be silently interpreted as no orphans.
    let uninitialized = broker.doctor().unwrap();
    assert!(!uninitialized.healthy(), "{uninitialized:#?}");
    assert!(
        uninitialized
            .deferred_checks
            .iter()
            .any(|check| check == "orphan_scan"),
        "{uninitialized:#?}"
    );
    // A real managed start creates the fixture's isolated ownership history.
    broker
        .start_worktree("doctor ownership fixture", None)
        .unwrap();
    let report = broker.doctor().unwrap();
    assert_eq!(report.integrity, "ok");
    assert_eq!(report.version.status, VersionDriftStatus::NotAethymeSource);
    assert!(report.healthy(), "{report:#?}");

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
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    set_integration_at_head(tmp.path());
    let mut broker = open_fixture_broker(tmp.path()).unwrap();

    let first = broker
        .adopt_with(
            tmp.path(),
            Some("first task"),
            aethyme_broker::AdoptMode::New,
            None,
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
    // `finish` is the default close; `finish close` only keeps the checkout,
    // and says so.
    let finish = format!("aethyme broker finish --session {}", first.id);
    for needle in [
        "--reuse",
        finish.as_str(),
        "close --session",
        "keep the checkout",
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
            Some("Claude Opus 5 <noreply@anthropic.com>"),
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

    // close retains the checkout and branch while allowing policy-approved cache reclaim.
    broker.close(first.id).unwrap();
    let closed = broker.store().session(first.id).unwrap();
    assert_eq!(closed.status, SessionStatus::Closed);
    assert_eq!(
        closed.cleanup_state,
        aethyme_broker::SessionCleanupState::Closed
    );
    assert!(closed.closed_at.is_some());
    assert!(closed.cleanup_completed_at.is_none());
    assert!(tmp.path().join("README.md").exists());

    // After close, --reuse creates a fresh session because no live identity remains.
    let second = broker
        .adopt_with(
            tmp.path(),
            Some("third task"),
            aethyme_broker::AdoptMode::Reuse,
            None,
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
            None,
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
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = open_fixture_broker(tmp.path()).unwrap();
    let session = broker.start_worktree("first task", None).unwrap();
    let worktree = Path::new(&session.worktree_path);
    let session_head = rev(worktree, "HEAD");

    sh(tmp.path(), &["checkout", "-qb", "integration-work"]);
    std::fs::write(tmp.path().join("shared.txt"), "integration\n").unwrap();
    sh(tmp.path(), &["add", "shared.txt"]);
    sh(tmp.path(), &["commit", "-qm", "integration advances"]);
    let integration_head = rev(tmp.path(), "HEAD");
    sh(tmp.path(), &["branch", "aethyme/integration", "HEAD"]);
    sh(tmp.path(), &["checkout", "-q", "main"]);
    std::fs::write(worktree.join("shared.txt"), "dirty session edit\n").unwrap();

    let report = broker
        .adopt_with(worktree, Some("follow-up"), AdoptMode::Reuse, None)
        .unwrap();
    assert_eq!(report.session.id, session.id);
    let drift = report.integration_drift.expect("reuse drift");
    assert_eq!(drift.relation, AdoptIntegrationRelation::Behind);
    assert_eq!(drift.session_head, session_head);
    assert_eq!(drift.integration_head, integration_head);
    assert_eq!(drift.ahead_commits, 0);
    assert_eq!(drift.behind_commits, 1);
    assert_eq!(drift.overlapping_changed_paths, vec!["shared.txt"]);
    assert!(drift.warning.as_deref().unwrap().contains("behind"));
    assert_eq!(drift.safe_next_action, "aethyme broker status");
}

#[test]
fn adopt_reuse_reports_diverged_commit_counts_and_overlap() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = open_fixture_broker(tmp.path()).unwrap();
    let session = broker.start_worktree("first task", None).unwrap();
    let worktree = Path::new(&session.worktree_path);

    sh(tmp.path(), &["checkout", "-qb", "integration-work"]);
    std::fs::write(tmp.path().join("README.md"), "integration\n").unwrap();
    sh(tmp.path(), &["commit", "-qam", "integration advances"]);
    let integration_head = rev(tmp.path(), "HEAD");
    sh(tmp.path(), &["branch", "aethyme/integration", "HEAD"]);
    sh(tmp.path(), &["checkout", "-q", "main"]);
    std::fs::write(worktree.join("README.md"), "session\n").unwrap();
    sh(worktree, &["add", "README.md"]);
    sh(worktree, &["commit", "-qm", "session advances"]);
    let session_head = rev(worktree, "HEAD");

    let report = broker
        .adopt_with(worktree, Some("follow-up"), AdoptMode::Reuse, None)
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
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    set_integration_at_head(tmp.path());
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
    let mut broker = open_fixture_broker(tmp.path()).unwrap();
    let session = broker.adopt(&worktree, Some("first task")).unwrap();
    std::fs::write(worktree.join("ahead.txt"), "session work\n").unwrap();
    sh(&worktree, &["add", "ahead.txt"]);
    sh(&worktree, &["commit", "-qm", "session advances"]);

    let report = broker
        .adopt_with(&worktree, Some("follow-up"), AdoptMode::Reuse, None)
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
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = open_fixture_broker(tmp.path()).unwrap();
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
                planned_paths: Vec::new(),
            },
            None,
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
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    set_integration_at_head(tmp.path());
    let mut broker = open_fixture_broker(tmp.path()).unwrap();
    let session = broker.adopt(tmp.path(), Some("first task")).unwrap();
    let head = rev(tmp.path(), "HEAD");

    let report = broker
        .adopt_with_options(
            tmp.path(),
            Some("current follow-up"),
            AdoptOptions {
                mode: AdoptMode::Reuse,
                sync_integration: true,
                planned_paths: Vec::new(),
            },
            None,
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
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = open_fixture_broker(tmp.path()).unwrap();
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
                planned_paths: Vec::new(),
            },
            None,
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
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = open_fixture_broker(tmp.path()).unwrap();
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
                planned_paths: Vec::new(),
            },
            None,
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
fn finish_retention_policy_keeps_safe_worktrees_when_disabled_or_invalid() {
    for (policy, warning) in [
        (
            "[retention]\nauto_cleanup_worktrees_on_finish = false\n",
            "automatic finish cleanup is disabled",
        ),
        (
            "[retention]\nschema_version = 99\n",
            "automatic finish cleanup was skipped",
        ),
    ] {
        let tmp = common::FixtureRepository::new();
        let external_root = tempfile::tempdir().unwrap();
        let worktree_root = external_root.path().join("worktrees");
        init_repo(tmp.path());
        std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
        std::fs::write(tmp.path().join(".aethyme/broker.toml"), policy).unwrap();

        let mut broker = open_fixture_broker(tmp.path())
            .unwrap()
            .with_worktree_root(&worktree_root);
        let session = broker.start_worktree("keep landed worktree", None).unwrap();
        let wt = std::path::PathBuf::from(&session.worktree_path);
        std::fs::write(wt.join("landed.txt"), "delivered\n").unwrap();
        sh(&wt, &["add", "landed.txt"]);
        sh(&wt, &["commit", "-qm", "landed work"]);
        aethyme_testkit::ensure_origin(broker.main_root());
        broker.submit(session.id).unwrap();
        aethyme_testkit::land_as_pull_request(
            broker.main_root(),
            &session.branch,
            aethyme_testkit::MergeStyle::MergeCommit,
        );

        let finished = broker.finish(session.id).unwrap();
        assert_eq!(finished.status, FinishStatus::Closed);
        assert!(finished.cleanup_safe, "{finished:#?}");
        assert!(finished.cleanup.kept, "{finished:#?}");
        assert!(!finished.cleanup.attempted, "{finished:#?}");
        assert!(!finished.cleanup.completed, "{finished:#?}");
        assert!(wt.exists(), "policy must retain {wt:?}");
        assert!(ref_exists(
            tmp.path(),
            &format!("refs/heads/{}", session.branch)
        ));
        assert!(
            finished
                .warnings
                .iter()
                .any(|message| message.contains(warning)),
            "{finished:#?}"
        );
        assert!(
            finished
                .next_commands
                .iter()
                .any(|command| command.contains("finish cleanup")),
            "{finished:#?}"
        );
    }
}

/// The bug: a squash merge puts the session's work on the default branch under
/// a brand new SHA with no ancestry link, so `finish` counted the session's
/// commits as unsubmitted forever and the session could never close (#152).
#[test]
fn a_squash_merged_session_can_be_recorded_and_finished() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    sh(tmp.path(), &["remote", "add", "origin", "."]);
    sh(
        tmp.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    sh(
        tmp.path(),
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );

    let external_root = tempfile::tempdir().unwrap();
    let worktree_root = external_root.path().join("worktrees");
    let mut broker = open_fixture_broker(tmp.path())
        .unwrap()
        .with_worktree_root(&worktree_root);
    let session = broker.start_worktree("squash task", None).unwrap();
    let wt = std::path::PathBuf::from(&session.worktree_path);

    // Two commits in the session, as a real branch would have.
    std::fs::write(wt.join("feature.txt"), "first\n").unwrap();
    sh(&wt, &["add", "-A"]);
    sh(&wt, &["commit", "-qm", "wip"]);
    std::fs::write(wt.join("feature.txt"), "final\n").unwrap();
    sh(&wt, &["add", "-A"]);
    sh(&wt, &["commit", "-qm", "polish"]);

    // The provider squashes: same content, new SHA, no ancestry to the branch.
    std::fs::write(tmp.path().join("feature.txt"), "final\n").unwrap();
    sh(tmp.path(), &["add", "-A"]);
    sh(tmp.path(), &["commit", "-qm", "feat: add feature (#42)"]);
    let squash = rev(tmp.path(), "HEAD");
    assert!(
        !std::process::Command::new("git")
            .current_dir(tmp.path())
            .args(["merge-base", "--is-ancestor", &rev(&wt, "HEAD"), &squash])
            .status()
            .unwrap()
            .success(),
        "the squash must not be a descendant of the session, or the test proves nothing"
    );

    let blocked = broker.finish(session.id).unwrap();
    assert_eq!(blocked.status, FinishStatus::Blocked);
    assert_eq!(blocked.unsubmitted_commits, 2);

    let scan = broker.scan_session_representation(session.id).unwrap();
    let landing = scan.search.landing().expect("the squash carries the work");
    assert_eq!(landing.commit, squash);
    assert_eq!(scan.changed_paths, vec!["feature.txt".to_string()]);

    // A stale confirmation is refused; the printed one is accepted.
    broker
        .record_session_representation(session.id, "not-the-digest")
        .unwrap_err();
    let recorded = broker
        .record_session_representation(session.id, &scan.digest)
        .unwrap();
    assert_eq!(
        recorded
            .existing
            .as_ref()
            .and_then(|record| record.representing_commit.clone()),
        Some(squash.clone())
    );

    let finished = broker.finish(session.id).unwrap();
    assert_eq!(finished.status, FinishStatus::Cleaned);
    assert_eq!(finished.unsubmitted_commits, 0);
    assert_eq!(
        finished
            .representation
            .as_ref()
            .and_then(|record| record.representing_commit.as_deref()),
        Some(squash.as_str())
    );
    assert!(finished.cleanup.completed, "{finished:#?}");
    assert!(finished.cleanup.reclaimed_bytes > 0, "{finished:#?}");
    assert!(finished.cleanup.worktree_removed, "{finished:#?}");
    assert!(finished.cleanup.branch_removed, "{finished:#?}");
    assert!(!wt.exists(), "landed session worktree remains at {wt:?}");
    let branch_ref = format!("refs/heads/{}", session.branch);
    assert!(
        !std::process::Command::new("git")
            .current_dir(tmp.path())
            .args(["show-ref", "--verify", "--quiet", &branch_ref])
            .status()
            .unwrap()
            .success(),
        "landed session branch remains at {branch_ref}"
    );
}

/// A session whose work is genuinely still local must not be recordable, and
/// the refusal has to name what is missing rather than just saying no.
#[test]
fn unlanded_work_is_refused_and_the_session_stays_blocked() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    sh(
        tmp.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    sh(
        tmp.path(),
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );

    let wt = tmp.path().join("local-wt");
    sh(
        tmp.path(),
        &[
            "worktree",
            "add",
            "-b",
            "agent/local",
            wt.to_str().unwrap(),
            "main",
        ],
    );
    let mut broker = open_fixture_broker(tmp.path()).unwrap();
    let session = broker.adopt(&wt, Some("local task")).unwrap();
    std::fs::write(wt.join("local.txt"), "never merged\n").unwrap();
    sh(&wt, &["add", "-A"]);
    sh(&wt, &["commit", "-qm", "local only"]);

    let scan = broker.scan_session_representation(session.id).unwrap();
    assert!(scan.search.landing().is_none());
    let err = broker
        .record_session_representation(session.id, &scan.digest)
        .unwrap_err();
    assert!(
        err.to_string().contains("not represented"),
        "refusal should say the work is not on the branch, got: {err}"
    );
    assert_eq!(
        broker.finish(session.id).unwrap().status,
        FinishStatus::Blocked
    );
}

#[test]
fn finish_blocks_dirty_then_unsubmitted_commits() {
    let tmp = common::FixtureRepository::new();
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
    let mut broker = open_fixture_broker(tmp.path()).unwrap();
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
        SessionStatus::Closed
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
    // Submitting again cannot land work in a repository that lands by pull
    // request; proving it landed (#152), or abandoning it on the record, are
    // the cures offered. Without a remote there is no pull request to open.
    assert_eq!(
        unsubmitted.next_commands,
        vec![
            format!(
                "aethyme broker advanced representation scan --session {}",
                session.id
            ),
            format!(
                "aethyme broker finish --session {} --abandon --reason \"<why>\"",
                session.id
            ),
        ]
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
fn finish_counts_only_commits_missing_from_the_remote_default_branch() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let base = rev(tmp.path(), "HEAD");
    sh(tmp.path(), &["branch", "aethyme/integration", "HEAD"]);
    let wt = tmp.path().join("upstream-drift-wt");
    sh(
        tmp.path(),
        &[
            "worktree",
            "add",
            "-b",
            "agent/upstream-drift",
            wt.to_str().unwrap(),
            "main",
        ],
    );
    let mut broker = open_fixture_broker(tmp.path()).unwrap();
    let session = broker.adopt(&wt, Some("upstream drift task")).unwrap();

    // The session picks up a commit that reaches the remote default branch
    // while integration stays at the older session base.
    std::fs::write(wt.join("upstream.txt"), "already on origin/main\n").unwrap();
    sh(&wt, &["add", "-A"]);
    sh(&wt, &["commit", "-qm", "upstream change"]);
    let upstream_commit = rev(&wt, "HEAD");
    sh(
        tmp.path(),
        &[
            "update-ref",
            "refs/remotes/origin/main",
            upstream_commit.as_str(),
        ],
    );
    sh(
        tmp.path(),
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    assert_eq!(rev(tmp.path(), "refs/heads/aethyme/integration"), base);

    std::fs::write(wt.join("local.txt"), "still only in the session\n").unwrap();
    sh(&wt, &["add", "-A"]);
    sh(&wt, &["commit", "-qm", "local session change"]);
    let session_head = rev(&wt, "HEAD");

    let blocked = broker.finish(session.id).unwrap();
    assert_eq!(blocked.status, FinishStatus::Blocked);
    assert_eq!(blocked.unsubmitted_commits, 1);
    assert!(blocked.pending_work.present);
    assert!(!blocked.delivery.published);

    // Once the full session head is on the default branch, it can finish even
    // though integration still has not advanced from the original base.
    sh(
        tmp.path(),
        &[
            "update-ref",
            "refs/remotes/origin/main",
            session_head.as_str(),
        ],
    );
    let finished = broker.finish(session.id).unwrap();
    assert_eq!(finished.status, FinishStatus::Closed);
    assert_eq!(finished.unsubmitted_commits, 0);
    assert!(!finished.pending_work.present);
    assert!(finished.delivery.published);
    assert!(!finished.delivery.submitted);
}

#[test]
fn finish_closes_a_landed_session_and_suggests_cleanup_when_the_default_branch_contains_it() {
    let tmp = common::FixtureRepository::new();
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
    let mut broker = open_fixture_broker(tmp.path()).unwrap();
    let session = broker.adopt(&wt, Some("promoted finish task")).unwrap();

    std::fs::write(wt.join("done.txt"), "done\n").unwrap();
    sh(&wt, &["add", "-A"]);
    sh(&wt, &["commit", "-qm", "done"]);
    aethyme_testkit::ensure_origin(broker.main_root());
    broker.submit(session.id).unwrap();
    aethyme_testkit::land_as_pull_request(
        broker.main_root(),
        &session.branch,
        aethyme_testkit::MergeStyle::MergeCommit,
    );
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
            wait_duration_ms: Some(1),
            first_output_ms: Some(2),
            output_bytes: Some(3),
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
                55,
                &aethyme_broker::GateCacheProvenance {
                    execution_profile_digest: Some("sha256:profile".into()),
                    profile_scope: "fixture profile scope".into(),
                },
            )),
        )
        .unwrap();

    let closed = broker.finish(session.id).unwrap();
    assert_eq!(closed.status, FinishStatus::Closed);
    assert!(closed.closed);
    assert!(closed.cleanup_safe);
    assert_eq!(
        closed.next_commands,
        vec![format!("aethyme broker finish cleanup {}", session.id)]
    );
    assert!(closed.delivery.submitted);
    assert!(!closed.delivery.promoted);
    assert!(closed.delivery.published);
    assert!(!closed.pending_work.present);
    assert_eq!(closed.pending_work.dirty_path_count, 0);
    assert_eq!(closed.pending_work.unsubmitted_commits, 0);
    assert_eq!(
        closed.recommended_next_action,
        Some(format!("aethyme broker finish cleanup {}", session.id))
    );
    // A verified close releases the session's leases in the transaction
    // that closes it, before any physical cleanup (#358): the store holds
    // none of them any more, so the handoff must not read "active".
    assert!(
        broker
            .store()
            .active_leases()
            .unwrap()
            .iter()
            .all(|lease| lease.session_id != session.id)
    );
    assert!(
        closed
            .leases_held
            .iter()
            .any(|lease| { lease.path == "src/" && lease.state == FinishLeaseState::Released })
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
        SessionStatus::Closed
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
    let closed_index = events
        .iter()
        .position(|event| event.kind == "session.closed")
        .unwrap();
    let finished_index = events
        .iter()
        .position(|event| event.kind == events::SESSION_FINISHED)
        .unwrap();
    assert_eq!(finished_index, closed_index + 1);
    assert_eq!(events[closed_index].ts, events[finished_index].ts);
    let payload = finished[0].payload_json.as_deref().unwrap();
    let handoff: serde_json::Value = serde_json::from_str(payload).unwrap();
    assert_eq!(handoff["session_id"], session.id);
    assert_eq!(handoff["delivery"]["promoted"], false);
    assert_eq!(handoff["delivery"]["published"], true);
    assert_eq!(handoff["last_gate"]["cache_source"], "cache_hit");
    assert!(handoff["leases_held"].as_array().unwrap().len() >= 3);
    assert!(handoff.get("worktree_path").is_none());
    assert!(!payload.contains(tmp.path().to_str().unwrap()));
    assert!(!payload.contains("must/not/enter"));

    sh(tmp.path(), &["merge", "--ff-only", "origin/main"]);
    let already = broker.finish(session.id).unwrap();
    assert_eq!(already.status, FinishStatus::AlreadyClosed);
    assert!(!already.cleanup_safe);
    assert!(already.next_commands.is_empty());
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
fn finish_reports_pull_request_landed_work_as_published() {
    let tmp = common::FixtureRepository::new();
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
    let mut broker = open_fixture_broker(tmp.path()).unwrap();
    let session = broker.adopt(&wt, Some("published finish task")).unwrap();
    std::fs::write(wt.join("published.txt"), "published\n").unwrap();
    sh(&wt, &["add", "-A"]);
    sh(&wt, &["commit", "-qm", "published"]);
    aethyme_testkit::ensure_origin(broker.main_root());
    broker.submit(session.id).unwrap();
    aethyme_testkit::land_as_pull_request(
        broker.main_root(),
        &session.branch,
        aethyme_testkit::MergeStyle::MergeCommit,
    );

    let closed = broker.finish(session.id).unwrap();
    assert_eq!(closed.status, FinishStatus::Closed);
    assert!(closed.delivery.submitted);
    // Nothing is promoted locally: the pull request is the publication.
    assert!(!closed.delivery.promoted);
    assert!(closed.delivery.published);
    assert!(closed.last_gate.is_none());
    assert!(closed.cleanup_safe);
    assert_eq!(
        closed.recommended_next_action,
        Some(format!("aethyme broker finish cleanup {}", session.id))
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
    let tmp = common::FixtureRepository::new();
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
    let mut broker = open_fixture_broker(tmp.path()).unwrap();
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

/// Issue #144: adopting a worktree that already has commits records the current
/// HEAD as the baseline, so those commits are not session-owned. That was
/// silent, and only visible as `no_changes` plus an ownership field in the
/// submission plan JSON.
#[test]
fn adopting_a_worktree_with_pre_existing_commits_says_they_are_not_owned() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    init_repo(&repo);

    // A worktree carrying work committed before any session existed.
    let recovered = tmp.path().join("recovered");
    sh(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "recovered",
            recovered.to_str().unwrap(),
        ],
    );
    std::fs::write(recovered.join("rescued.txt"), "work from a backup branch\n").unwrap();
    sh(&recovered, &["add", "-A"]);
    sh(&recovered, &["commit", "-qm", "feat: recovered work"]);

    let mut broker = open_fixture_broker(&repo).unwrap();
    let report = broker
        .adopt_with(
            &recovered,
            Some("adopt recovered work"),
            AdoptMode::New,
            None,
        )
        .unwrap();

    let drift = report
        .integration_drift
        .expect("a fresh adopt must report drift, not only a reuse");
    assert_eq!(drift.ahead_commits, 1);
    let warning = drift
        .warning
        .expect("pre-existing commits must not be adopted silently");
    assert!(
        warning.contains("none are session-owned") && warning.contains("will not replay them"),
        "the warning must say the commits are excluded: {warning}"
    );
    assert!(
        warning.contains("re-adopt from a base that precedes them"),
        "the warning must name a remedy: {warning}"
    );
}

/// Issue #294: closing a session and adopting its worktree again used to
/// record the current HEAD as the new baseline, so the closed session's
/// unsubmitted commits silently stopped being session-owned and submit
/// superseded the entry without running a gate. The new session must keep the
/// previous ownership boundary, the way reuse of a live session already does.
#[test]
fn re_adopting_a_closed_session_keeps_its_unsubmitted_commits_owned() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    init_repo(&repo);
    let work = tmp.path().join("work");
    sh(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "work",
            work.to_str().unwrap(),
        ],
    );

    let mut broker = open_fixture_broker(&repo).unwrap();
    let first = broker
        .adopt_with(&work, Some("first task"), AdoptMode::New, None)
        .unwrap();
    let baseline = first.session.diff_base.clone().unwrap();

    // Nothing pending: a re-adoption has nothing to carry.
    broker.close(first.session.id).unwrap();
    let idle = broker
        .adopt_with(&work, Some("idle re-adopt"), AdoptMode::New, None)
        .unwrap();
    assert!(
        idle.carried_ownership.is_none(),
        "{:?}",
        idle.carried_ownership
    );

    std::fs::write(work.join("feature.txt"), "session work\n").unwrap();
    sh(&work, &["add", "-A"]);
    sh(&work, &["commit", "-qm", "feat: session work"]);
    broker.close(idle.session.id).unwrap();

    let again = broker
        .adopt_with(&work, Some("after close"), AdoptMode::New, None)
        .unwrap();
    let carried = again
        .carried_ownership
        .clone()
        .expect("the closed session's pending commit must stay owned");
    assert_eq!(carried.from_session, idle.session.id);
    assert_eq!(carried.baseline, baseline);
    assert_eq!(carried.pending_owned_commits, 1);
    assert_eq!(again.session.diff_base.as_deref(), Some(baseline.as_str()));
    let warning = again
        .integration_drift
        .and_then(|drift| drift.warning)
        .unwrap_or_default();
    assert!(
        warning.contains("1 pending session-owned commit(s) are safe to submit"),
        "submit must replay the carried commit: {warning}"
    );

    // Replacing a stale session carries the boundary the same way.
    let replaced = broker
        .adopt_with(&work, Some("replace"), AdoptMode::ReplaceStale, None)
        .unwrap();
    assert_eq!(
        replaced.carried_ownership.map(|carried| carried.baseline),
        Some(baseline)
    );
}

/// Issue #182. `broker status` is the mandated first step of every session, so
/// its cost is a tax on every agent -- measured at 2m54s with 19 live
/// sessions, of which only 6.2s was user time. The system time is the
/// implicit-lease refresh: two git subprocesses per live session, each walking
/// a worktree, so the cost scales with accumulated worktree state rather than
/// with anything the caller asked for.
///
/// The observable contract is that `status_brief` does not recompute implicit
/// leases. Asserting on the lease rows rather than on elapsed time keeps this
/// from being a flaky benchmark: the refresh is what costs, so not having run
/// it is the property worth pinning.
#[test]
fn status_brief_skips_the_implicit_lease_refresh() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = open_fixture_broker(tmp.path()).unwrap();
    let session = broker.adopt(tmp.path(), Some("brief task")).unwrap();
    std::fs::write(tmp.path().join("README.md"), "edited, uncommitted\n").unwrap();
    assert!(broker.store().active_leases().unwrap().is_empty());

    let brief = broker.status_brief(now_ms()).unwrap();
    assert!(
        !brief.leases_refreshed,
        "the cheap path must report that its lease counts are not current"
    );
    assert!(
        broker.store().active_leases().unwrap().is_empty(),
        "status_brief recomputed implicit leases, which is the expensive work \
         it exists to skip"
    );
    assert_eq!(brief.summary.live_sessions, 1);

    // The full view is what refreshes, so the fixture is genuinely one that
    // *would* have produced a lease. Without this the assertion above would
    // also pass against a repository that simply had nothing to claim.
    broker.status(now_ms()).unwrap();
    let refreshed = broker.store().active_leases().unwrap();
    assert!(
        refreshed.iter().any(|lease| lease.path == "README.md"),
        "expected the full view to derive an implicit lease: {refreshed:?}"
    );
    assert_eq!(refreshed[0].session_id, session.id);
}

/// What separates this from the existing `status_snapshot`: liveness is still
/// derived *and persisted*, so a cheap orientation call does not create a blind
/// spot in the event timeline. Only the per-session diff is skipped.
#[test]
fn status_brief_still_records_liveness_transitions() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = open_fixture_broker(tmp.path()).unwrap();
    let session = broker.adopt(tmp.path(), Some("liveness task")).unwrap();
    assert_eq!(
        broker.store().session(session.id).unwrap().status,
        SessionStatus::Active
    );

    // Stale, but inside the abandonment window: this test is about whether a
    // cheap orientation call persists liveness, not about the terminal
    // transition that follows it (see `tests/session_abandonment.rs`).
    let later = now_ms() + 3 * 60 * 60 * 1_000;
    let brief = broker.status_brief(later).unwrap();
    assert_eq!(brief.summary.stale_sessions, 1);
    assert_eq!(
        broker.store().session(session.id).unwrap().status,
        SessionStatus::Stale,
        "the transition must be persisted, unlike status_snapshot"
    );
}

#[test]
fn summary_reuses_recent_dead_pid_liveness_with_stored_status_semantics() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = open_fixture_broker(tmp.path()).unwrap();
    let live = broker.adopt(tmp.path(), Some("live lease owner")).unwrap();
    let exited = broker.start_agent("recent-dead", "true", None).unwrap();
    assert!(exited.pid.is_some());
    wait_for_agent_pid_exit(&broker, exited.id);

    broker
        .store()
        .claim_lease(live.id, "src/shared.rs", None)
        .unwrap();
    broker
        .store()
        .claim_lease(exited.id, "src/shared.rs", None)
        .unwrap();
    let stored = broker.store().session(exited.id).unwrap();
    assert_eq!(stored.status, SessionStatus::Active);
    assert!(now_ms().saturating_sub(stored.last_activity_at) < 60_000);

    let snapshot = Broker::open_snapshot(tmp.path()).unwrap();
    let read_only = snapshot.status_brief_snapshot(now_ms()).unwrap();
    assert_eq!(read_only.summary.overlap_count, 1);
    assert_eq!(
        broker.store().session(exited.id).unwrap().status,
        SessionStatus::Active,
        "read-only summary must leave the stored status untouched"
    );

    let brief = broker.status_brief(now_ms()).unwrap();
    assert_eq!(brief.summary.overlap_count, 0);
    assert_eq!(
        broker.store().session(exited.id).unwrap().status,
        SessionStatus::Exited,
        "writable summary persists the dead PID transition before pairing"
    );
    assert!(
        broker
            .store()
            .session_leases(exited.id)
            .unwrap()
            .iter()
            .any(|lease| lease.path == "src/shared.rs")
    );
    for phase in [
        "agents",
        "overlaps",
        "status_advice",
        "blockers",
        "ownership",
        "submits",
        "total",
    ] {
        assert!(
            brief.phase_timings_ms.contains_key(phase),
            "missing {phase}"
        );
    }
    let longest_phase = [
        "agents",
        "overlaps",
        "status_advice",
        "blockers",
        "ownership",
        "submits",
    ]
    .into_iter()
    .map(|phase| brief.phase_timings_ms[phase])
    .max()
    .unwrap();
    assert!(brief.phase_timings_ms["total"] >= longest_phase);
}

#[test]
fn status_brief_loads_promotion_config_once_from_committed_default() {
    const CHILD: &str = "AETHYME_STATUS_BRIEF_CONFIG_TRACE_CHILD";
    const NAME: &str = "status_brief_loads_promotion_config_once_from_committed_default";
    if std::env::var_os(CHILD).is_some() {
        let tmp = common::FixtureRepository::new();
        init_repo(tmp.path());
        let config_path = tmp.path().join(".aethyme/config.toml");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(
            &config_path,
            "[promote]\nmode = \"verify-only\"\nbranch = \"review/integration\"\n",
        )
        .unwrap();
        sh(tmp.path(), &["add", "-f", ".aethyme/config.toml"]);
        sh(tmp.path(), &["commit", "-qm", "committed promote policy"]);
        sh(
            tmp.path(),
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        );
        sh(tmp.path(), &["config", "remote.origin.url", "."]);
        sh(
            tmp.path(),
            &[
                "config",
                "remote.origin.fetch",
                "+refs/heads/*:refs/remotes/origin/*",
            ],
        );
        sh(tmp.path(), &["config", "branch.main.remote", "origin"]);
        sh(
            tmp.path(),
            &["config", "branch.main.merge", "refs/heads/main"],
        );

        // The working copy conflicts in both fields. Summary must use the
        // committed default config for mode and branch, from one Git read.
        std::fs::write(
            &config_path,
            "[promote]\nmode = \"auto\"\nbranch = \"working/integration\"\n",
        )
        .unwrap();
        let mut broker = open_fixture_broker(tmp.path()).unwrap();
        broker
            .adopt(tmp.path(), Some("summary config task"))
            .unwrap();

        let trace = std::path::PathBuf::from(
            std::env::var_os("GIT_TRACE2_EVENT").expect("parent supplies private trace"),
        );
        std::fs::write(&trace, "").unwrap();
        let brief = broker.status_brief(now_ms()).unwrap();
        assert!(!brief.summary.may_move_integration);
        assert!(
            brief
                .summary
                .message
                .contains("review/integration relation to not inspected not checked"),
            "summary must name the committed default branch: {}",
            brief.summary.message
        );
        assert!(
            brief
                .summary
                .message
                .contains("verify-only: submit does not move integration"),
            "summary must use the committed default mode: {}",
            brief.summary.message
        );

        let events = std::fs::read_to_string(&trace).unwrap();
        let starts = events
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|event| event["event"] == "start")
            .collect::<Vec<_>>();
        let tree_reads = starts
            .iter()
            .filter(|event| {
                event["argv"].as_array().is_some_and(|argv| {
                    argv.get(1).and_then(serde_json::Value::as_str) == Some("ls-tree")
                        && argv
                            .iter()
                            .any(|arg| arg.as_str().is_some_and(|arg| arg.ends_with("config.toml")))
                })
            })
            .count();
        let blob_reads = starts
            .iter()
            .filter(|event| {
                event["argv"].as_array().is_some_and(|argv| {
                    argv.get(1).and_then(serde_json::Value::as_str) == Some("cat-file")
                        && argv.get(2).and_then(serde_json::Value::as_str) == Some("blob")
                })
            })
            .count();
        assert_eq!(
            (tree_reads, blob_reads),
            (1, 1),
            "one summary must perform one strict committed-config tree/blob read, trace={events}"
        );
        return;
    }

    let owner = tempfile::tempdir().unwrap();
    let trace = owner
        .path()
        .canonicalize()
        .unwrap()
        .join("status-config-trace.jsonl");
    let mut command = Command::new(std::env::current_exe().unwrap());
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let output = command
        .args(["--exact", NAME, "--nocapture"])
        .env(CHILD, "1")
        .env("GIT_TRACE2_EVENT", &trace)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "status summary config child failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"),
        "child must execute exactly the selected regression: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn summary_keeps_stale_lease_rows_without_pairing_them() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = open_fixture_broker(tmp.path()).unwrap();
    let mut sessions = Vec::new();
    for name in ["fresh-a", "fresh-b", "quiet"] {
        let worktree = tmp.path().join(".aethyme/worktrees").join(name);
        std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
        let branch = format!("agent/summary-{name}");
        sh(
            tmp.path(),
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                &branch,
                worktree.to_str().unwrap(),
                "main",
            ],
        );
        let session = broker.adopt(&worktree, Some(name)).unwrap();
        broker
            .store()
            .claim_lease(session.id, "src/shared.rs", None)
            .unwrap();
        sessions.push((name, session));
    }

    let quiet_id = sessions
        .iter()
        .find(|(name, _)| *name == "quiet")
        .unwrap()
        .1
        .id;
    let stale_at = now_ms() - 3 * 60 * 60 * 1_000;
    broker
        .store()
        .touch_session_activity(quiet_id, stale_at)
        .unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3 * 60 * 60);
    for file in ["index", "HEAD"] {
        std::fs::File::open(tmp.path().join(".git/worktrees/quiet").join(file))
            .unwrap()
            .set_modified(old)
            .unwrap();
    }

    let brief = broker.status_brief(now_ms()).unwrap();
    assert_eq!(
        brief.summary.overlap_count, 1,
        "only the two fresh sessions pair"
    );
    assert_eq!(
        broker.store().session(quiet_id).unwrap().status,
        SessionStatus::Stale
    );
    assert!(
        broker
            .store()
            .active_leases()
            .unwrap()
            .iter()
            .any(|lease| lease.session_id == quiet_id && lease.path == "src/shared.rs")
    );
}

/// A finished session's declared targets must stop pairing with live ones.
///
/// The liveness filter is SQL, so the in-memory detector's tests cannot reach
/// it: they are handed rows directly and never see a session change status. An
/// earlier deny-list (`NOT IN ('closed', 'abandoned')`) admitted `cleaned` and
/// `exited` — and named a status that does not exist — so closed sessions kept
/// colliding forever, which would have made the signal progressively noisier
/// until it was ignored.
#[test]
fn a_finished_session_stops_colliding_on_its_declared_targets() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = open_fixture_broker(tmp.path()).unwrap();

    // Two real worktrees: one session per checkout, as the broker requires.
    let first_tree = tmp.path().join(".aethyme/worktrees/rewriter");
    let second_tree = tmp.path().join(".aethyme/worktrees/extender");
    for (path, branch) in [
        (&first_tree, "agent/rewriter"),
        (&second_tree, "agent/extender"),
    ] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        sh(
            tmp.path(),
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                branch,
                path.to_str().unwrap(),
                "main",
            ],
        );
    }
    let first = broker.adopt(&first_tree, Some("rewrite it")).unwrap();
    let second = broker.adopt(&second_tree, Some("extend it")).unwrap();

    for (session, operation) in [
        (first.id, aethyme_broker::ScopeOperation::Replace),
        (second.id, aethyme_broker::ScopeOperation::Extend),
    ] {
        broker
            .capture_session_scopes(
                session,
                &[(
                    aethyme_broker::ScopeKind::Symbol,
                    "PaymentService".to_string(),
                    operation,
                )],
                None,
            )
            .unwrap();
    }

    let live = broker.scope_overlaps_snapshot().unwrap();
    assert_eq!(live.len(), 1, "two live sessions must collide: {live:?}");
    assert_eq!(
        live[0].severity,
        aethyme_broker::ScopeConflictSeverity::High
    );

    broker.close(second.id).unwrap();

    let after = broker.scope_overlaps_snapshot().unwrap();
    assert!(
        after.is_empty(),
        "a finished session must not keep colliding: {after:?}"
    );
}

#[test]
fn routine_status_reports_unknown_checks_and_does_not_refresh_dirty_leases() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = open_fixture_broker(tmp.path()).unwrap();
    broker.adopt(tmp.path(), Some("routine status")).unwrap();
    std::fs::write(tmp.path().join("README.md"), "uncommitted\n").unwrap();
    let status = broker.status_current(now_ms()).unwrap();
    assert!(!status.leases_refreshed);
    assert!(status.leases_refreshed_at_ms.is_none());
    assert!(
        status
            .deferred_checks
            .iter()
            .any(|check| check == "dirty_worktrees")
    );
    assert!(!status.cleanup_retention.eligibility_checked);
    assert!(broker.store().active_leases().unwrap().is_empty());
    assert!(status.phase_timings_ms.contains_key("retention"));
    assert!(status.phase_timings_ms.contains_key("sessions"));
    let refreshed = broker.status(now_ms()).unwrap();
    assert!(refreshed.leases_refreshed);
    assert!(refreshed.leases_refreshed_at_ms.is_some());
    assert!(refreshed.deferred_checks.is_empty());
    assert!(refreshed.cleanup_retention.eligibility_checked);
    assert!(
        broker
            .store()
            .active_leases()
            .unwrap()
            .iter()
            .any(|l| l.path == "README.md")
    );
    let recorded = broker.status_current(now_ms()).unwrap();
    assert_eq!(
        recorded.leases_refreshed_at_ms,
        refreshed.leases_refreshed_at_ms
    );
}

#[test]
fn routine_status_never_authorizes_cleanup_of_a_dirty_closed_checkout() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = open_fixture_broker(tmp.path()).unwrap();
    let session = broker.start_worktree("closed retained work", None).unwrap();
    let checkout = std::path::PathBuf::from(&session.worktree_path);
    std::fs::write(checkout.join("keep.txt"), "untracked user work\n").unwrap();
    broker
        .store()
        .set_session_status(session.id, SessionStatus::Closed, None)
        .unwrap();
    let current = broker.status_current(now_ms()).unwrap();
    assert_eq!(current.cleanup_retention.broker_owned_worktree_count, 1);
    assert!(!current.cleanup_retention.eligibility_checked);
    assert_eq!(current.cleanup_retention.eligible_worktree_count, 0);
    assert!(current.cleanup_retention.unmeasured_worktree_count > 0);
    assert!(checkout.join("keep.txt").exists());
    // A routine inventory cannot bypass the independent audit on the GC lane.
    let plan = broker.cleanup_plan().unwrap();
    assert!(!plan.digest.is_empty());
    assert!(
        !plan
            .worktrees
            .iter()
            .find(|item| item.session_id == session.id)
            .unwrap()
            .eligible()
    );
    assert!(checkout.join("keep.txt").exists());
}

#[test]
fn summary_snapshot_does_not_persist_liveness_transitions() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = open_fixture_broker(tmp.path()).unwrap();
    let session = broker.adopt(tmp.path(), Some("snapshot status")).unwrap();
    let snapshot = Broker::open_snapshot(tmp.path()).unwrap();
    let report = snapshot
        .status_brief_snapshot(now_ms() + 3 * 60 * 60 * 1000)
        .unwrap();
    assert_eq!(report.summary.stale_sessions, 1);
    assert_eq!(
        broker.store().session(session.id).unwrap().status,
        SessionStatus::Active
    );
    assert!(!report.leases_refreshed);
}

#[test]
fn routine_status_counts_rendered_advisories_but_summary_does_not() {
    let tmp = common::FixtureRepository::new();
    init_repo(tmp.path());
    let mut broker = open_fixture_broker(tmp.path()).unwrap();
    let session = broker
        .adopt(tmp.path(), Some("advisory reporting"))
        .unwrap();
    broker
        .store()
        .record_advisory(&aethyme_broker::NewAdvisory {
            identity: "status-current-test".into(),
            audience: aethyme_broker::AdvisoryAudience::Session,
            producer: aethyme_broker::AdvisoryProducer::Coordination,
            session_id: Some(session.id),
            severity: aethyme_broker::AdvisorySeverity::Warning,
            queue_entry_id: None,
            integration_sha: None,
            paths: Vec::new(),
            evidence: Vec::new(),
        })
        .unwrap();
    broker.status_brief(now_ms()).unwrap();
    assert_eq!(
        broker
            .store()
            .advisory_delivery_summary()
            .unwrap()
            .total_shows,
        0
    );
    let current = broker.status_current(now_ms()).unwrap();
    assert_eq!(current.outstanding_advisories.len(), 1);
    assert_eq!(current.advisory_delivery.total_shows, 1);
    broker.status_brief(now_ms()).unwrap();
    assert_eq!(
        broker
            .store()
            .advisory_delivery_summary()
            .unwrap()
            .total_shows,
        1
    );
}
