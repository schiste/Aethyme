use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use aethyme_broker::{
    AdvisoryEvidence, AdvisoryResolutionState, AdvisorySeverity, Broker, BrokerOpError,
    EntryExposureResolutionKind, EntryExposureState, IntegrationDeliveryState, NewAdvisory,
    OperationIdentityProvenance, OperationStatus, ShipFreshnessResult,
};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn git_output(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
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
    String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string()
}

fn git(cwd: &Path, args: &[&str]) {
    git_output(cwd, args);
}

struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    remote: PathBuf,
    host_operations: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let remote = tmp.path().join("remote.git");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&remote).unwrap();
        git(&remote, &["init", "--bare", "-q", "-b", "main"]);
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join(".gitignore"), ".aethyme/\n").unwrap();
        std::fs::write(repo.join("tracked.txt"), "base\n").unwrap();
        git(&repo, &["add", ".gitignore", "tracked.txt"]);
        git(&repo, &["commit", "-qm", "init"]);
        git(
            &repo,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&repo, &["push", "-qu", "origin", "main"]);
        Self {
            host_operations: tmp.path().join("host-state/operations.db"),
            _tmp: tmp,
            repo,
            remote,
        }
    }

    fn broker(&self) -> Broker {
        Broker::open(&self.repo)
            .unwrap()
            .with_host_operation_database(&self.host_operations)
    }

    fn promoted_entry(&self) -> (i64, i64, String) {
        let mut broker = self.broker();
        let session = broker.start_worktree("ship-plan").unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        std::fs::write(worktree.join("feature.txt"), "verified\n").unwrap();
        git(&worktree, &["add", "feature.txt"]);
        git(&worktree, &["commit", "-qm", "feat: verified"]);
        let outcome = broker.submit(session.id).unwrap();
        assert!(outcome.promoted);
        let integration = git_output(&self.repo, &["rev-parse", "aethyme/integration"]);
        (outcome.entry.id, session.id, integration)
    }

    fn refs(&self) -> String {
        git_output(
            &self.repo,
            &[
                "for-each-ref",
                "--format=%(refname):%(objectname)",
                "refs/heads",
                "refs/remotes",
            ],
        )
    }

    fn remote_main(&self) -> String {
        git_output(&self.remote, &["rev-parse", "refs/heads/main"])
    }

    fn advance_remote(&self) -> String {
        let outsider = self._tmp.path().join("outsider");
        git(
            self._tmp.path(),
            &[
                "clone",
                "-q",
                self.remote.to_str().unwrap(),
                outsider.to_str().unwrap(),
            ],
        );
        std::fs::write(outsider.join("remote-only.txt"), "remote advance\n").unwrap();
        git(&outsider, &["add", "remote-only.txt"]);
        git(&outsider, &["commit", "-qm", "remote advance"]);
        git(&outsider, &["push", "-q", "origin", "main"]);
        self.remote_main()
    }

    #[cfg(unix)]
    fn install_hook(&self, name: &str, script: &str) -> PathBuf {
        let path = self.remote.join("hooks").join(name);
        std::fs::write(&path, script).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }
}

#[test]
fn ship_plan_reports_exact_tip_and_does_not_mutate_refs() {
    let fixture = Fixture::new();
    let (entry_id, session_id, integration) = fixture.promoted_entry();
    let refs_before = fixture.refs();
    let remote_before = git_output(&fixture.remote, &["rev-parse", "refs/heads/main"]);

    let mut broker = fixture.broker();
    let plan = broker.ship_plan(entry_id).unwrap();

    assert_eq!(plan.queue_entry.id, entry_id);
    assert_eq!(plan.originating_session.id, session_id);
    assert_eq!(plan.integration_ref, "aethyme/integration");
    assert_eq!(plan.integration_sha, integration);
    assert_eq!(plan.publication_sha, plan.integration_sha);
    assert_eq!(plan.included_entries.len(), 1);
    assert_eq!(plan.included_entries[0].queue_entry_id, entry_id);
    assert!(plan.excluded_entries.is_empty());
    assert_eq!(plan.local_default_branch_ref, "refs/heads/main");
    assert_eq!(plan.remote_default_branch_ref, "refs/heads/main");
    assert_eq!(plan.remote_default_branch_sha, remote_before);
    assert_eq!(
        plan.planned_remote_base_sha.as_deref(),
        Some(plan.remote_default_branch_sha.as_str())
    );
    assert_eq!(plan.freshness.result, ShipFreshnessResult::Ready);
    assert!(plan.freshness.fast_forward);
    assert!(plan.local_main_sync_safe);
    assert_eq!(plan.target.normalized_host, "local");
    assert_eq!(
        plan.target.fetch_url,
        fixture.remote.to_string_lossy().trim_end_matches(".git")
    );
    assert_eq!(plan.target.fetch_url, plan.target.push_url);
    assert_eq!(
        plan.target.coordination_key,
        format!(
            "local:{}",
            fixture.remote.to_string_lossy().trim_end_matches(".git")
        )
    );
    assert_eq!(plan.target.remote_name, "origin");
    assert!(plan.target.caller_assertion.is_none());
    assert_eq!(
        plan.proposed_push.refspec,
        format!("{}:refs/heads/main", plan.publication_sha)
    );
    assert_eq!(plan.proposed_push.source_sha, plan.publication_sha);

    assert_eq!(fixture.refs(), refs_before);
    assert_eq!(
        git_output(&fixture.remote, &["rev-parse", "refs/heads/main"]),
        remote_before
    );
    assert_eq!(
        broker
            .store()
            .entry_path_exposure(entry_id)
            .unwrap()
            .unwrap()
            .state,
        EntryExposureState::Outstanding,
        "read-only ship planning must not resolve publication exposure"
    );
}

#[test]
fn ship_plan_rejects_an_entry_that_is_not_promoted() {
    let fixture = Fixture::new();
    let mut broker = fixture.broker();
    let session = broker.start_worktree("unpromoted").unwrap();
    let entry = broker
        .store()
        .submit(
            session.id,
            &session.diff_base.clone().unwrap(),
            &session.diff_base.unwrap(),
        )
        .unwrap();
    let error = broker.ship_plan(entry.id).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("requires a promoted queue entry")
    );
}

#[test]
fn ship_execute_publishes_and_verifies_the_exact_confirmed_sha() {
    let fixture = Fixture::new();
    let (entry_id, _, integration) = fixture.promoted_entry();
    let main_before = git_output(&fixture.repo, &["rev-parse", "main"]);

    let mut broker = fixture.broker();
    let advisory = broker
        .persist_advisory(NewAdvisory {
            identity: format!("test-promotion-exposure:{entry_id}"),
            session_id: None,
            severity: AdvisorySeverity::Warning,
            queue_entry_id: Some(entry_id),
            integration_sha: Some(integration.clone()),
            paths: vec!["feature.txt".into()],
            evidence: vec![AdvisoryEvidence {
                kind: "lease_overlap".into(),
                summary: "test exposure".into(),
            }],
        })
        .unwrap();
    let report = broker.ship_execute(entry_id, &integration).unwrap();

    assert_eq!(report.published_sha, integration);
    assert_eq!(report.verified_remote_sha, integration);
    assert_eq!(fixture.remote_main(), integration);
    assert_eq!(
        git_output(&fixture.repo, &["rev-parse", "main"]),
        main_before
    );
    assert_eq!(report.fetch_operation.status, OperationStatus::Succeeded);
    assert_eq!(report.push_operation.status, OperationStatus::Succeeded);
    assert_eq!(report.verify_operation.status, OperationStatus::Succeeded);
    assert_eq!(report.resolved_exposures.len(), 1);
    assert_eq!(report.resolved_advisories.len(), 1);
    assert_eq!(report.resolved_exposures[0].queue_entry_id, entry_id);
    assert_eq!(
        report.resolved_exposures[0].state,
        EntryExposureState::Resolved
    );
    assert_eq!(
        report.resolved_exposures[0].resolution_kind,
        Some(EntryExposureResolutionKind::ShipVerified)
    );
    let resolved_advisory = broker.advisory(advisory.id).unwrap();
    assert_eq!(
        resolved_advisory.resolution_state,
        AdvisoryResolutionState::Resolved
    );
    assert_eq!(resolved_advisory.resolved_at.is_some(), true);
    assert!(
        resolved_advisory
            .resolution_evidence
            .as_deref()
            .is_some_and(|evidence| evidence.contains("ship verified remote"))
    );
    assert!(broker.advisories(false).unwrap().is_empty());
    assert!(report.fetch_operation.host_operation_id.is_some());
    assert!(report.push_operation.host_operation_id.is_some());
    assert!(report.verify_operation.host_operation_id.is_none());
    assert_eq!(
        report.push_operation.identity_provenance,
        OperationIdentityProvenance::VerifiedCanonical
    );
    assert_eq!(
        report.fetch_operation.repository,
        report.plan.target.coordination_key
    );
    assert_eq!(
        report.push_operation.repository,
        report.plan.target.coordination_key
    );
    assert_eq!(
        report.verify_operation.repository,
        report.plan.target.coordination_key
    );
    assert!(!report.push_operation.command_json.contains("--force"));
    assert!(!report.local_main_sync.requested);
    assert!(!report.local_main_sync.synchronized);
    assert_eq!(report.local_main_sync.before_sha, main_before);
    assert_eq!(report.local_main_sync.after_sha, main_before);
    let follow_up = format!(
        "aethyme broker ship execute --entry {entry_id} --confirm {integration} --sync-main"
    );
    assert_eq!(
        report.local_main_sync.follow_up_command.as_deref(),
        Some(follow_up.as_str())
    );
}

#[test]
fn ship_retains_advisory_while_its_overlapping_lease_is_live() {
    let fixture = Fixture::new();
    let (entry_id, session_id, integration) = fixture.promoted_entry();
    let mut broker = fixture.broker();
    broker
        .store()
        .claim_lease(session_id, "feature.txt", None)
        .unwrap();
    let advisory = broker
        .persist_advisory(NewAdvisory {
            identity: format!("live-promotion-exposure:{entry_id}"),
            session_id: Some(session_id),
            severity: AdvisorySeverity::Warning,
            queue_entry_id: Some(entry_id),
            integration_sha: Some(integration.clone()),
            paths: vec!["feature.txt".into()],
            evidence: vec![AdvisoryEvidence {
                kind: "lease_overlap".into(),
                summary: "live overlap".into(),
            }],
        })
        .unwrap();

    let report = broker.ship_execute(entry_id, &integration).unwrap();
    assert_eq!(report.resolved_exposures.len(), 1);
    assert!(report.resolved_advisories.is_empty());
    let retained = broker.advisory(advisory.id).unwrap();
    assert_eq!(
        retained.resolution_state,
        AdvisoryResolutionState::Outstanding
    );

    let blocked_plan = broker.exposure_reconciliation_plan().unwrap();
    assert!(blocked_plan.safe);
    assert!(blocked_plan.contained_exposures.is_empty());
    assert_eq!(blocked_plan.advisories.len(), 1);
    assert!(!blocked_plan.advisories[0].eligible);
    assert_eq!(blocked_plan.advisories[0].blocking_leases, ["feature.txt"]);
    let _ = broker.status(0).unwrap();
    assert_eq!(
        broker.advisory(advisory.id).unwrap().resolution_state,
        AdvisoryResolutionState::Outstanding,
        "normal status must not silently reconcile publication lifecycle state"
    );

    broker.acknowledge_advisory(advisory.id).unwrap();
    broker.close(session_id).unwrap();
    let operator = broker.start_worktree("reconcile exposures").unwrap();
    let plan = broker.exposure_reconciliation_plan().unwrap();
    assert!(plan.safe);
    assert_eq!(plan.advisories.len(), 1);
    assert!(plan.advisories[0].eligible);
    let report = broker
        .apply_exposure_reconciliation(operator.id, &plan.digest)
        .unwrap();
    assert!(report.resolved_exposures.is_empty());
    assert_eq!(report.resolved_advisories.len(), 1);
    assert_eq!(
        broker.advisory(advisory.id).unwrap().resolution_state,
        AdvisoryResolutionState::Resolved
    );
}

#[test]
fn exposure_plan_refuses_when_the_fresh_remote_commit_is_missing_locally() {
    let fixture = Fixture::new();
    fixture.promoted_entry();
    let remote_head = fixture.advance_remote();
    let mut broker = fixture.broker();

    let plan = broker.exposure_reconciliation_plan().unwrap();
    assert_eq!(plan.remote_default_branch_sha, remote_head);
    assert!(!plan.safe);
    assert!(
        plan.refusals
            .iter()
            .any(|reason| reason.contains("not available locally")),
        "{:?}",
        plan.refusals
    );
}

#[test]
fn exposure_plan_cli_is_stable_and_does_not_mutate_lifecycle_state() {
    let fixture = Fixture::new();
    let (entry_id, _, _) = fixture.promoted_entry();
    let output = Command::new(CLI)
        .args(["exposures", "plan", "--json"])
        .current_dir(&fixture.repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(plan["schema_version"], 1);
    assert_eq!(plan["safe"], true);
    assert_eq!(plan["contained_exposures"].as_array().unwrap().len(), 0);
    assert_eq!(plan["remaining_exposures"][0]["queue_entry_id"], entry_id);
    assert_eq!(plan["digest"].as_str().unwrap().len(), 64);
    assert_eq!(
        fixture
            .broker()
            .store()
            .outstanding_entry_path_exposures()
            .unwrap()
            .len(),
        1,
        "read-only planning must not resolve exposure state"
    );
}

#[test]
fn selected_prefix_publication_resolves_only_contained_promoted_entries() {
    let fixture = Fixture::new();
    let (first_entry, _, _) = fixture.promoted_entry();
    let mut broker = fixture.broker();
    let second_session = broker.start_worktree("second promoted entry").unwrap();
    let second_worktree = PathBuf::from(&second_session.worktree_path);
    std::fs::write(second_worktree.join("second.txt"), "second\n").unwrap();
    git(&second_worktree, &["add", "second.txt"]);
    git(&second_worktree, &["commit", "-qm", "feat: second"]);
    let second = broker.submit(second_session.id).unwrap();
    assert!(second.promoted);
    let selected_integration = git_output(&fixture.repo, &["rev-parse", "aethyme/integration"]);

    let third_session = broker.start_worktree("later promoted entry").unwrap();
    let third_worktree = PathBuf::from(&third_session.worktree_path);
    std::fs::write(third_worktree.join("third.txt"), "third\n").unwrap();
    git(&third_worktree, &["add", "third.txt"]);
    git(&third_worktree, &["commit", "-qm", "feat: third"]);
    let third = broker.submit(third_session.id).unwrap();
    assert!(third.promoted);

    let current_integration = git_output(&fixture.repo, &["rev-parse", "aethyme/integration"]);
    let plan = broker.ship_plan(second.entry.id).unwrap();
    assert_eq!(plan.integration_sha, current_integration);
    assert_eq!(plan.publication_sha, selected_integration);
    assert_eq!(
        plan.included_entries
            .iter()
            .map(|entry| entry.queue_entry_id)
            .collect::<Vec<_>>(),
        vec![first_entry, second.entry.id]
    );
    assert_eq!(
        plan.excluded_entries
            .iter()
            .map(|entry| entry.queue_entry_id)
            .collect::<Vec<_>>(),
        vec![third.entry.id]
    );
    assert_eq!(plan.proposed_push.source_sha, selected_integration);

    let report = broker
        .ship_execute(second.entry.id, &selected_integration)
        .unwrap();
    assert_eq!(report.published_sha, selected_integration);
    assert_eq!(fixture.remote_main(), report.published_sha);
    assert_eq!(
        git_output(&fixture.repo, &["rev-parse", "aethyme/integration"]),
        current_integration,
        "publishing an older prefix must not rewind integration"
    );

    assert_eq!(
        report
            .resolved_exposures
            .iter()
            .map(|exposure| exposure.queue_entry_id)
            .collect::<Vec<_>>(),
        vec![first_entry, second.entry.id]
    );
    let outstanding = broker.store().outstanding_entry_path_exposures().unwrap();
    assert_eq!(outstanding.len(), 1);
    assert_eq!(outstanding[0].queue_entry_id, third.entry.id);
    assert_eq!(
        outstanding[0].state,
        EntryExposureState::Outstanding,
        "a later entry outside the verified published prefix remains exposed"
    );
}

#[test]
fn ship_plan_refuses_a_selected_prefix_with_unrecorded_integration_commits() {
    let fixture = Fixture::new();
    fixture.promoted_entry();

    let integration = git_output(&fixture.repo, &["rev-parse", "aethyme/integration"]);
    let tree_ref = format!("{integration}^{{tree}}");
    let tree = git_output(&fixture.repo, &["rev-parse", &tree_ref]);
    let unrecorded = git_output(
        &fixture.repo,
        &[
            "commit-tree",
            &tree,
            "-p",
            &integration,
            "-m",
            "unrecorded integration commit",
        ],
    );
    git(
        &fixture.repo,
        &[
            "update-ref",
            "refs/heads/aethyme/integration",
            &unrecorded,
            &integration,
        ],
    );

    let mut broker = fixture.broker();
    let session = broker.start_worktree("after unrecorded commit").unwrap();
    let worktree = PathBuf::from(&session.worktree_path);
    std::fs::write(worktree.join("after-unrecorded.txt"), "change\n").unwrap();
    git(&worktree, &["add", "after-unrecorded.txt"]);
    git(&worktree, &["commit", "-qm", "feat: after unrecorded"]);
    let outcome = broker.submit(session.id).unwrap();
    assert!(outcome.promoted);

    let error = broker.ship_plan(outcome.entry.id).unwrap_err().to_string();
    assert!(
        error.contains("selected prefix contains unrecorded integration commits"),
        "{error}"
    );
    assert!(error.contains(&unrecorded), "{error}");
}

#[test]
fn ship_execute_sync_main_fast_forwards_a_clean_primary_checkout() {
    let fixture = Fixture::new();
    let (entry_id, _, integration) = fixture.promoted_entry();
    let main_before = git_output(&fixture.repo, &["rev-parse", "main"]);
    let mut broker = fixture.broker();

    let report = broker
        .ship_execute_with_sync(entry_id, &integration, true)
        .unwrap();

    assert_eq!(fixture.remote_main(), integration);
    assert_eq!(
        git_output(&fixture.repo, &["rev-parse", "main"]),
        integration
    );
    assert_eq!(
        git_output(&fixture.repo, &["rev-parse", "HEAD"]),
        integration
    );
    assert_eq!(
        std::fs::read_to_string(fixture.repo.join("feature.txt")).unwrap(),
        "verified\n"
    );
    assert!(report.local_main_sync.requested);
    assert!(report.local_main_sync.synchronized);
    assert_eq!(report.local_main_sync.before_sha, main_before);
    assert_eq!(report.local_main_sync.after_sha, integration);
    assert!(report.local_main_sync.follow_up_command.is_none());
    assert_eq!(
        report.sync_operation.as_ref().unwrap().status,
        OperationStatus::Succeeded
    );
}

#[test]
fn integration_status_routes_promoted_published_and_synchronized_states_through_ship() {
    let fixture = Fixture::new();
    let (entry_id, _, integration) = fixture.promoted_entry();
    let mut broker = fixture.broker();

    let promoted = broker.integration_status(0).unwrap();
    assert_eq!(
        promoted.next_action.state,
        IntegrationDeliveryState::Promoted
    );
    assert_eq!(
        promoted.next_action.commands,
        vec![format!("aethyme broker ship plan --entry {entry_id}")]
    );

    broker.ship_execute(entry_id, &integration).unwrap();
    let published = broker.integration_status(0).unwrap();
    assert_eq!(
        published.next_action.state,
        IntegrationDeliveryState::Published
    );
    assert_eq!(
        published.next_action.commands,
        vec![format!(
            "aethyme broker ship execute --entry {entry_id} --confirm {integration} --sync-main"
        )]
    );

    broker
        .ship_execute_with_sync(entry_id, &integration, true)
        .unwrap();
    let synchronized = broker.integration_status(0).unwrap();
    assert_eq!(
        synchronized.next_action.state,
        IntegrationDeliveryState::LocallySynchronized
    );
    assert!(synchronized.next_action.commands.is_empty());
}

#[test]
fn ship_execute_sync_main_preserves_disjoint_untracked_paths() {
    let fixture = Fixture::new();
    let (entry_id, _, integration) = fixture.promoted_entry();
    std::fs::write(fixture.repo.join("dirty.txt"), "wip\n").unwrap();
    std::fs::create_dir_all(fixture.repo.join(".codex")).unwrap();
    std::fs::write(fixture.repo.join(".codex/local.md"), "local\n").unwrap();
    let mut broker = fixture.broker();

    let plan = broker.ship_plan(entry_id).unwrap();
    assert!(plan.local_main_sync_safe);
    assert_eq!(
        plan.local_main_sync_assessment.untracked_paths,
        vec![".codex/local.md", "dirty.txt"]
    );
    assert!(
        plan.local_main_sync_assessment
            .conflicting_untracked_paths
            .is_empty()
    );

    broker
        .ship_execute_with_sync(entry_id, &integration, true)
        .unwrap();
    assert_eq!(fixture.remote_main(), integration);
    assert_eq!(
        git_output(&fixture.repo, &["rev-parse", "main"]),
        integration
    );
    assert_eq!(
        std::fs::read_to_string(fixture.repo.join("dirty.txt")).unwrap(),
        "wip\n"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.repo.join(".codex/local.md")).unwrap(),
        "local\n"
    );
}

#[test]
fn ship_execute_sync_main_refuses_tracked_changes_with_paths() {
    let fixture = Fixture::new();
    let (entry_id, _, integration) = fixture.promoted_entry();
    let remote_before = fixture.remote_main();
    std::fs::write(fixture.repo.join("tracked.txt"), "tracked wip\n").unwrap();
    let mut broker = fixture.broker();

    let error = broker
        .ship_execute_with_sync(entry_id, &integration, true)
        .unwrap_err();
    assert!(matches!(
        error,
        BrokerOpError::ShipLocalMainUnsafe { reason }
            if reason.contains("tracked changes") && reason.contains("tracked.txt")
    ));
    assert_eq!(fixture.remote_main(), remote_before);
    assert!(broker.store().coordinated_operations().unwrap().is_empty());
}

#[test]
fn ship_execute_sync_main_refuses_untracked_incoming_path_collisions() {
    let fixture = Fixture::new();
    let (entry_id, _, integration) = fixture.promoted_entry();
    let remote_before = fixture.remote_main();
    std::fs::write(fixture.repo.join("feature.txt"), "local collision\n").unwrap();
    let mut broker = fixture.broker();

    let plan = broker.ship_plan(entry_id).unwrap();
    assert!(!plan.local_main_sync_safe);
    assert_eq!(
        plan.local_main_sync_assessment.conflicting_untracked_paths,
        vec!["feature.txt"]
    );
    let error = broker
        .ship_execute_with_sync(entry_id, &integration, true)
        .unwrap_err();
    assert!(matches!(
        error,
        BrokerOpError::ShipLocalMainUnsafe { reason }
            if reason.contains("untracked paths") && reason.contains("feature.txt")
    ));
    assert_eq!(fixture.remote_main(), remote_before);
    assert!(broker.store().coordinated_operations().unwrap().is_empty());
}

#[test]
fn ship_execute_sync_main_refuses_a_diverged_primary_checkout_before_publish() {
    let fixture = Fixture::new();
    let (entry_id, _, integration) = fixture.promoted_entry();
    let remote_before = fixture.remote_main();
    std::fs::write(fixture.repo.join("main-only.txt"), "diverged\n").unwrap();
    git(&fixture.repo, &["add", "main-only.txt"]);
    git(&fixture.repo, &["commit", "-qm", "main diverges"]);
    let mut broker = fixture.broker();

    let error = broker
        .ship_execute_with_sync(entry_id, &integration, true)
        .unwrap_err();
    assert!(matches!(
        error,
        BrokerOpError::ShipLocalMainUnsafe { reason } if reason.contains("diverged")
    ));
    assert_eq!(fixture.remote_main(), remote_before);
}

#[cfg(unix)]
#[test]
fn ship_execute_sync_main_rechecks_for_local_movement_after_publish() {
    let fixture = Fixture::new();
    let (entry_id, _, integration) = fixture.promoted_entry();
    let main_before = git_output(&fixture.repo, &["rev-parse", "main"]);
    let tree = git_output(&fixture.repo, &["rev-parse", "main^{tree}"]);
    let moved = git_output(
        &fixture.repo,
        &["commit-tree", &tree, "-p", &main_before, "-m", "move main"],
    );
    fixture.install_hook(
        "post-receive",
        &format!(
            "#!/bin/sh\nunset GIT_DIR\ngit -C {} update-ref refs/heads/main {}\n",
            fixture.repo.display(),
            moved
        ),
    );
    let mut broker = fixture.broker();

    let error = broker
        .ship_execute_with_sync(entry_id, &integration, true)
        .unwrap_err();
    assert!(matches!(
        error,
        BrokerOpError::ShipLocalMainMovedAfterPublish { published_sha, reason }
            if published_sha == integration && reason.contains("moved since planning")
    ));
    assert_eq!(fixture.remote_main(), integration);
    assert_eq!(git_output(&fixture.repo, &["rev-parse", "main"]), moved);
}

#[test]
fn ship_execute_rejects_abbreviated_and_mismatched_confirmations_before_operations() {
    let fixture = Fixture::new();
    let (entry_id, _, integration) = fixture.promoted_entry();
    let mut broker = fixture.broker();

    assert!(matches!(
        broker.ship_execute(entry_id, &integration[..12]),
        Err(BrokerOpError::ShipConfirmationNotFullSha)
    ));
    assert!(matches!(
        broker.ship_execute(entry_id, &"0".repeat(40)),
        Err(BrokerOpError::ShipConfirmationMismatch { .. })
    ));
    assert!(broker.store().coordinated_operations().unwrap().is_empty());
}

#[test]
fn ship_execute_rejects_a_remote_that_moved_since_the_planned_base() {
    let fixture = Fixture::new();
    let (entry_id, _, integration) = fixture.promoted_entry();
    let advanced = fixture.advance_remote();
    let mut broker = fixture.broker();

    let error = broker.ship_execute(entry_id, &integration).unwrap_err();
    assert!(matches!(
        error,
        BrokerOpError::ShipRemoteMoved { actual, .. } if actual == advanced
    ));
    assert_eq!(fixture.remote_main(), advanced);
    let operations = broker.store().coordinated_operations().unwrap();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].status, OperationStatus::Succeeded);
    let exposure = broker
        .store()
        .entry_path_exposure(entry_id)
        .unwrap()
        .unwrap();
    assert_eq!(exposure.state, EntryExposureState::Outstanding);
}

#[test]
fn ship_execute_rejects_a_non_fast_forward_remote() {
    let fixture = Fixture::new();
    let (entry_id, _, integration) = fixture.promoted_entry();
    let advanced = fixture.advance_remote();
    git(&fixture.repo, &["fetch", "-q", "origin", "main"]);
    let mut broker = fixture.broker();

    let error = broker.ship_execute(entry_id, &integration).unwrap_err();
    assert!(matches!(
        error,
        BrokerOpError::ShipNonFastForward { remote_sha, .. } if remote_sha == advanced
    ));
    assert_eq!(fixture.remote_main(), advanced);
}

#[cfg(unix)]
#[test]
fn ship_push_failure_with_unchanged_remote_is_failed_and_retryable() {
    let fixture = Fixture::new();
    let (entry_id, _, integration) = fixture.promoted_entry();
    let hook = fixture.install_hook("pre-receive", "#!/bin/sh\nexit 1\n");
    let mut broker = fixture.broker();

    let error = broker.ship_execute(entry_id, &integration).unwrap_err();
    let operation_id = match error {
        BrokerOpError::ShipOperationFailed {
            phase: "push",
            operation_id,
            status: "failed",
        } => operation_id,
        other => panic!("unexpected push error: {other}"),
    };
    assert_eq!(
        broker
            .store()
            .coordinated_operation(operation_id)
            .unwrap()
            .unwrap()
            .status,
        OperationStatus::Failed
    );
    std::fs::remove_file(hook).unwrap();
    let report = broker.ship_execute(entry_id, &integration).unwrap();
    assert_eq!(report.verified_remote_sha, integration);
}

#[cfg(unix)]
#[test]
fn ship_verify_failure_is_journaled_as_failed() {
    let fixture = Fixture::new();
    let (entry_id, _, integration) = fixture.promoted_entry();
    fixture.install_hook(
        "post-receive",
        "#!/bin/sh\ngit update-ref -d refs/heads/main\n",
    );
    let mut broker = fixture.broker();

    let error = broker.ship_execute(entry_id, &integration).unwrap_err();
    assert!(matches!(
        error,
        BrokerOpError::ShipVerificationMismatch { actual, .. } if actual == "<missing>"
    ));
    let operations = broker.store().coordinated_operations().unwrap();
    assert_eq!(operations.len(), 3);
    assert_eq!(operations[0].status, OperationStatus::Succeeded);
    assert_eq!(operations[1].status, OperationStatus::Succeeded);
    assert_eq!(operations[2].status, OperationStatus::Failed);
    let exposure = broker
        .store()
        .entry_path_exposure(entry_id)
        .unwrap()
        .unwrap();
    assert_eq!(exposure.state, EntryExposureState::Outstanding);
}
