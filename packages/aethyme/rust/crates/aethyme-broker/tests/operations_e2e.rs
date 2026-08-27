use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use aethyme_broker::{
    Broker, BrokerOpError, CoordinatedCommand, GitRepo, NewCoordinatedOperation, OperationEffect,
    OperationProvider, OperationReconciliationState, OperationStatus,
};

fn git(cwd: &Path, args: &[&str]) {
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
}

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
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn init_repo(root: &Path) {
    git(root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("tracked.txt"), "base\n").unwrap();
    git(root, &["add", "tracked.txt"]);
    git(root, &["commit", "-qm", "init"]);
}

fn add_worktree(root: &Path, name: &str) -> std::path::PathBuf {
    let worktree = root.join(".aethyme/worktrees").join(name);
    std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    git(
        root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &format!("agent/{name}"),
            worktree.to_str().unwrap(),
            "main",
        ],
    );
    worktree
}

fn request(session_id: i64, args: &[&str]) -> CoordinatedCommand {
    CoordinatedCommand {
        session_id,
        provider: OperationProvider::Git,
        repository: None,
        resolved_target: None,
        scope: None,
        declared_effect: None,
        destructive_confirmed: false,
        authorization_reason: Some("test workflow".into()),
        args: args.iter().map(|arg| (*arg).into()).collect(),
    }
}

struct PushFixture {
    remote: std::path::PathBuf,
    worktree: std::path::PathBuf,
    broker: Broker,
    session_id: i64,
}

fn push_fixture(root: &Path, name: &str) -> PushFixture {
    let repo = root.join("repo");
    let remote = root.join("remote.git");
    std::fs::create_dir(&repo).unwrap();
    init_repo(&repo);
    git(root, &["init", "--bare", "-q", remote.to_str().unwrap()]);
    git(
        &repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&repo, &["push", "-q", "origin", "main"]);
    let worktree = add_worktree(&repo, name);
    let mut broker = Broker::open(&repo)
        .unwrap()
        .with_host_operation_database(root.join("host-operations.db"));
    let session_id = broker.adopt(&worktree, None).unwrap().id;
    PushFixture {
        remote,
        worktree,
        broker,
        session_id,
    }
}

fn commit_push_fixture(fixture: &PushFixture, contents: &str) -> String {
    std::fs::write(fixture.worktree.join("tracked.txt"), contents).unwrap();
    git(&fixture.worktree, &["add", "tracked.txt"]);
    git(&fixture.worktree, &["commit", "-qm", contents.trim()]);
    git_output(&fixture.worktree, &["rev-parse", "HEAD"])
}

fn exact_push_request(session_id: i64, worktree: &Path, refspecs: &[&str]) -> CoordinatedCommand {
    let mut args = vec!["push", "origin"];
    args.extend(refspecs);
    let mut command = request(session_id, &args);
    command.resolved_target = Some(
        GitRepo::discover(worktree)
            .unwrap()
            .resolve_remote_target("origin", None)
            .unwrap(),
    );
    command
}

#[cfg(unix)]
fn write_executable(path: &Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::write(path, contents).unwrap();
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

fn github_request(session_id: i64, repository: &str, args: &[&str]) -> CoordinatedCommand {
    CoordinatedCommand {
        session_id,
        provider: OperationProvider::Github,
        repository: Some(repository.into()),
        resolved_target: None,
        scope: Some("github:test".into()),
        declared_effect: Some(OperationEffect::Read),
        destructive_confirmed: false,
        authorization_reason: None,
        args: args.iter().map(|arg| (*arg).into()).collect(),
    }
}

#[test]
fn successful_operation_is_durably_journaled_with_events() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let worktree = add_worktree(tmp.path(), "journal");
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.adopt(&worktree, None).unwrap();

    let report = broker
        .run_coordinated_operation(request(session.id, &["branch", "coordinated"]))
        .unwrap();
    assert!(report.ok());
    assert_eq!(report.operation.effect, OperationEffect::Write);
    assert_eq!(report.operation.status, OperationStatus::Succeeded);
    assert_eq!(
        report.operation.authorization_reason.as_deref(),
        Some("test workflow")
    );
    assert!(report.operation.command_json.contains("coordinated"));

    let events = broker.store().events_after(0, i64::MAX).unwrap();
    let kinds: Vec<&str> = events.iter().map(|event| event.kind.as_str()).collect();
    assert!(kinds.contains(&"operation.prepared"));
    assert!(kinds.contains(&"operation.running"));
    assert!(kinds.contains(&"operation.succeeded"));
}

#[test]
fn destructive_and_ambiguous_operations_fail_closed() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let worktree = add_worktree(tmp.path(), "closed");
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.adopt(&worktree, None).unwrap();

    let destructive = broker
        .run_coordinated_operation(request(session.id, &["branch", "-D", "main"]))
        .unwrap_err();
    assert!(destructive.to_string().contains("requires --destructive"));

    let ambiguous = broker
        .run_coordinated_operation(request(session.id, &["unknown-extension"]))
        .unwrap_err();
    assert!(ambiguous.to_string().contains("ambiguous"));

    let mut missing_reason = request(session.id, &["branch", "authorized-why"]);
    missing_reason.authorization_reason = None;
    let missing_reason = broker
        .run_coordinated_operation(missing_reason)
        .unwrap_err();
    assert!(missing_reason.to_string().contains("require --reason"));

    let mut invalid_assertion = request(session.id, &["fetch", "origin"]);
    invalid_assertion.repository = Some("not-a-slug".into());
    let invalid_assertion = broker
        .run_coordinated_operation(invalid_assertion)
        .unwrap_err();
    assert!(
        invalid_assertion
            .to_string()
            .contains("exact owner/name slug")
    );
    assert!(broker.store().coordinated_operations().unwrap().is_empty());
}

#[test]
fn remote_git_journals_resolved_identity_and_assertion_evidence() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    git(
        tmp.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/Owner/Repo.git",
        ],
    );
    let worktree = add_worktree(tmp.path(), "remote-identity");
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.adopt(&worktree, None).unwrap();
    let mut command = request(session.id, &["ls-remote", "--get-url", "origin"]);
    command.repository = Some("Owner/Repo".into());

    let report = broker.run_coordinated_operation(command).unwrap();
    assert!(report.ok());
    assert_eq!(report.operation.repository, "github.com/owner/repo");
    let target = report.resolved_target.as_ref().unwrap();
    assert_eq!(target.coordination_key, report.operation.repository);
    assert_eq!(target.caller_assertion.as_deref(), Some("Owner/Repo"));
    assert_eq!(target.remote_name, "origin");

    let details: serde_json::Value =
        serde_json::from_str(report.operation.details_json.as_deref().unwrap()).unwrap();
    assert_eq!(details["resolved_target"]["caller_assertion"], "Owner/Repo");
    assert_eq!(
        details["resolved_target"]["coordination_key"],
        "github.com/owner/repo"
    );
}

#[test]
fn remote_git_refuses_mismatched_assertions_and_multiple_push_urls() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    git(
        tmp.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/Owner/Repo.git",
        ],
    );
    let worktree = add_worktree(tmp.path(), "remote-refusal");
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.adopt(&worktree, None).unwrap();

    let mut mismatch = request(session.id, &["ls-remote", "--get-url", "origin"]);
    mismatch.repository = Some("Other/Repo".into());
    assert!(
        broker
            .run_coordinated_operation(mismatch)
            .unwrap_err()
            .to_string()
            .contains("does not match resolved repository")
    );
    assert!(broker.store().coordinated_operations().unwrap().is_empty());

    git(
        tmp.path(),
        &[
            "config",
            "--add",
            "remote.origin.pushurl",
            "git@github.com:Owner/Repo.git",
        ],
    );
    git(
        tmp.path(),
        &[
            "config",
            "--add",
            "remote.origin.pushurl",
            "ssh://git@github.com/Owner/Repo.git",
        ],
    );
    let mut ambiguous = request(session.id, &["ls-remote", "--get-url", "origin"]);
    ambiguous.repository = Some("Owner/Repo".into());
    assert!(
        broker
            .run_coordinated_operation(ambiguous)
            .unwrap_err()
            .to_string()
            .contains("has 2 push URLs")
    );
    assert!(broker.store().coordinated_operations().unwrap().is_empty());
}

#[test]
fn github_operations_journal_normalized_identity_and_display_spelling() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let worktree = add_worktree(tmp.path(), "github-identity");
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.adopt(&worktree, None).unwrap();

    let report = broker
        .run_coordinated_operation(github_request(
            session.id,
            "Schiste/Aethyme",
            &["--version"],
        ))
        .unwrap();
    assert!(report.ok());
    assert_eq!(report.operation.repository, "github.com/schiste/aethyme");
    let target = report.github_target.as_ref().unwrap();
    assert_eq!(target.coordination_key, report.operation.repository);
    assert_eq!(target.display_slug, "Schiste/Aethyme");

    let details: serde_json::Value =
        serde_json::from_str(report.operation.details_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        details["github_target"]["coordination_key"],
        "github.com/schiste/aethyme"
    );
    assert_eq!(details["github_target"]["display_slug"], "Schiste/Aethyme");
}

#[test]
fn github_api_mismatch_is_refused_before_execution_or_journaling() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let worktree = add_worktree(tmp.path(), "github-mismatch");
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.adopt(&worktree, None).unwrap();

    let error = broker
        .run_coordinated_operation(github_request(
            session.id,
            "Schiste/Aethyme",
            &["api", "repos/Other/Repo/issues"],
        ))
        .unwrap_err();
    assert!(error.to_string().contains("does not match broker --repo"));
    assert!(broker.store().coordinated_operations().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn exact_rejected_push_is_failed_when_every_destination_remains_at_its_base() {
    let tmp = tempfile::tempdir().unwrap();
    let mut fixture = push_fixture(tmp.path(), "push-failed");
    let base = git_output(&fixture.remote, &["rev-parse", "refs/heads/main"]);
    let proposed = commit_push_fixture(&fixture, "rejected\n");
    write_executable(
        &fixture.remote.join("hooks/pre-receive"),
        "#!/bin/sh\nprintf 'push succeeded according to stderr\\n' >&2\nexit 1\n",
    );

    let report = fixture
        .broker
        .run_coordinated_operation(exact_push_request(
            fixture.session_id,
            &fixture.worktree,
            &["HEAD:refs/heads/main"],
        ))
        .unwrap();
    assert!(!report.command_success);
    assert!(!report.ok());
    assert_eq!(report.operation.status, OperationStatus::Failed);
    assert!(report.stderr.contains("push succeeded according to stderr"));
    let details: serde_json::Value =
        serde_json::from_str(report.operation.details_json.as_deref().unwrap()).unwrap();
    let reconciliation = &details["push_reconciliation"];
    assert_eq!(reconciliation["planning"], "planned");
    assert_eq!(
        reconciliation["plan"]["destinations"][0]["pre_push_sha"],
        base
    );
    assert_eq!(
        reconciliation["plan"]["destinations"][0]["proposed_sha"],
        proposed
    );
    assert_eq!(reconciliation["evidence"]["classification"], "failed");
    assert_eq!(
        reconciliation["evidence"]["destinations"][0]["observed_sha"],
        base
    );
    let shown = fixture
        .broker
        .show_coordinated_operation(report.operation.id)
        .unwrap();
    assert_eq!(
        shown.reconciliation.state,
        OperationReconciliationState::NotRequired
    );
    assert!(!shown.reconciliation.write_blocked);
    assert!(!shown.reconciliation.automatic_retry_allowed);
    assert_eq!(
        shown.reconciliation.evidence.as_ref().unwrap()["evidence"]["classification"],
        "failed"
    );

    let second = fixture
        .broker
        .run_coordinated_operation(exact_push_request(
            fixture.session_id,
            &fixture.worktree,
            &["HEAD:refs/heads/main"],
        ))
        .unwrap();
    assert_eq!(second.operation.status, OperationStatus::Failed);
}

#[cfg(unix)]
#[test]
fn exact_push_is_succeeded_when_transport_exits_nonzero_after_every_update() {
    let tmp = tempfile::tempdir().unwrap();
    let mut fixture = push_fixture(tmp.path(), "push-succeeded");
    let proposed = commit_push_fixture(&fixture, "landed\n");
    let git_exec_path = git_output(&fixture.worktree, &["--exec-path"]);
    let receive_pack = tmp.path().join("receive-pack-then-fail");
    write_executable(
        &receive_pack,
        &format!(
            "#!/bin/sh\n\"{git_exec_path}/git-receive-pack\" \"$@\"\nstatus=$?\nif [ \"$status\" -ne 0 ]; then exit \"$status\"; fi\nexit 1\n"
        ),
    );
    git(
        &fixture.worktree,
        &[
            "config",
            "remote.origin.receivepack",
            receive_pack.to_str().unwrap(),
        ],
    );

    let report = fixture
        .broker
        .run_coordinated_operation(exact_push_request(
            fixture.session_id,
            &fixture.worktree,
            &["HEAD:refs/heads/main"],
        ))
        .unwrap();
    assert!(
        !report.command_success,
        "fixture must exercise non-zero push"
    );
    assert!(report.ok());
    assert_eq!(report.operation.status, OperationStatus::Succeeded);
    assert_eq!(
        git_output(&fixture.remote, &["rev-parse", "refs/heads/main"]),
        proposed
    );
    let details: serde_json::Value =
        serde_json::from_str(report.operation.details_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        details["push_reconciliation"]["evidence"]["classification"],
        "succeeded"
    );
    let shown = fixture
        .broker
        .show_coordinated_operation(report.operation.id)
        .unwrap();
    assert_eq!(
        shown.reconciliation.state,
        OperationReconciliationState::NotRequired
    );
    assert!(!shown.reconciliation.automatic_retry_allowed);
    assert_eq!(
        shown.reconciliation.evidence.as_ref().unwrap()["evidence"]["classification"],
        "succeeded"
    );
}

#[cfg(unix)]
#[test]
fn mixed_exact_destinations_remain_partial_and_write_blocking() {
    let tmp = tempfile::tempdir().unwrap();
    let mut fixture = push_fixture(tmp.path(), "push-partial");
    commit_push_fixture(&fixture, "partial\n");
    write_executable(
        &fixture.remote.join("hooks/update"),
        "#!/bin/sh\nif [ \"$1\" = 'refs/heads/rejected' ]; then exit 1; fi\nexit 0\n",
    );

    let report = fixture
        .broker
        .run_coordinated_operation(exact_push_request(
            fixture.session_id,
            &fixture.worktree,
            &["HEAD:refs/heads/accepted", "HEAD:refs/heads/rejected"],
        ))
        .unwrap();
    assert!(!report.command_success);
    assert_eq!(report.operation.status, OperationStatus::OutcomeUnknown);
    let details: serde_json::Value =
        serde_json::from_str(report.operation.details_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        details["push_reconciliation"]["evidence"]["classification"],
        "partial"
    );
    let shown = fixture
        .broker
        .show_coordinated_operation(report.operation.id)
        .unwrap();
    assert_eq!(
        shown.reconciliation.state,
        OperationReconciliationState::Required
    );
    assert!(shown.reconciliation.write_blocked);
    assert!(!shown.reconciliation.automatic_retry_allowed);
    let recovery = shown.reconciliation.recovery.as_ref().unwrap();
    assert!(recovery.succeeded_command.contains("--outcome succeeded"));
    assert!(recovery.failed_command.contains("--outcome failed"));
    assert!(
        fixture
            .broker
            .run_coordinated_operation(exact_push_request(
                fixture.session_id,
                &fixture.worktree,
                &["HEAD:refs/heads/after-partial"],
            ))
            .unwrap_err()
            .to_string()
            .contains("Blind retry is forbidden")
    );
    fixture
        .broker
        .reconcile_coordinated_operation(
            report.operation.id,
            false,
            "reviewed both destination refs",
        )
        .unwrap();
    let reconciled = fixture
        .broker
        .show_coordinated_operation(report.operation.id)
        .unwrap();
    assert_eq!(
        reconciled.reconciliation.state,
        OperationReconciliationState::ReconciledFailed
    );
    assert_eq!(
        reconciled.reconciliation.evidence.as_ref().unwrap()["evidence"]["classification"],
        "partial"
    );
    assert_eq!(
        reconciled.reconciliation.operator_reason.as_deref(),
        Some("reviewed both destination refs")
    );
}

#[cfg(unix)]
#[test]
fn missing_post_push_evidence_keeps_an_exact_push_unknown() {
    let tmp = tempfile::tempdir().unwrap();
    let mut fixture = push_fixture(tmp.path(), "push-missing");
    commit_push_fixture(&fixture, "missing evidence\n");
    let receive_pack = tmp.path().join("remove-remote-then-fail");
    write_executable(
        &receive_pack,
        "#!/bin/sh\nmv \"$1\" \"$1.unavailable\"\nprintf 'remote disappeared\\n' >&2\nexit 1\n",
    );
    git(
        &fixture.worktree,
        &[
            "config",
            "remote.origin.receivepack",
            receive_pack.to_str().unwrap(),
        ],
    );

    let report = fixture
        .broker
        .run_coordinated_operation(exact_push_request(
            fixture.session_id,
            &fixture.worktree,
            &["HEAD:refs/heads/main"],
        ))
        .unwrap();
    assert_eq!(report.operation.status, OperationStatus::OutcomeUnknown);
    let details: serde_json::Value =
        serde_json::from_str(report.operation.details_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        details["push_reconciliation"]["evidence"]["reason"],
        "post_push_remote_evidence_unavailable"
    );
    let shown = fixture
        .broker
        .show_coordinated_operation(report.operation.id)
        .unwrap();
    assert_eq!(
        shown.reconciliation.state,
        OperationReconciliationState::Required
    );
    assert!(!shown.reconciliation.automatic_retry_allowed);
    assert_eq!(
        shown.reconciliation.evidence.as_ref().unwrap()["evidence"]["reason"],
        "post_push_remote_evidence_unavailable"
    );
}

#[cfg(unix)]
#[test]
fn complex_unplannable_push_remains_conservatively_unknown() {
    let tmp = tempfile::tempdir().unwrap();
    let mut fixture = push_fixture(tmp.path(), "push-unplanned");
    commit_push_fixture(&fixture, "all branches\n");
    write_executable(
        &fixture.remote.join("hooks/pre-receive"),
        "#!/bin/sh\nexit 1\n",
    );
    let mut command = request(fixture.session_id, &["push", "--all", "origin"]);
    command.resolved_target = Some(
        GitRepo::discover(&fixture.worktree)
            .unwrap()
            .resolve_remote_target("origin", None)
            .unwrap(),
    );

    let report = fixture.broker.run_coordinated_operation(command).unwrap();
    assert_eq!(report.operation.status, OperationStatus::OutcomeUnknown);
    let details: serde_json::Value =
        serde_json::from_str(report.operation.details_json.as_deref().unwrap()).unwrap();
    assert_eq!(details["push_reconciliation"]["planning"], "unsupported");
    assert_eq!(
        details["push_reconciliation"]["evidence"]["classification"],
        "unknown"
    );
}

#[test]
fn crashed_write_blocks_until_operator_reconciliation() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let worktree = add_worktree(tmp.path(), "crash");
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.adopt(&worktree, None).unwrap();
    let repository = format!("local:{}", broker.main_root().display());

    let stale = broker
        .store()
        .create_coordinated_operation(&NewCoordinatedOperation {
            session_id: session.id,
            provider: OperationProvider::Git,
            repository,
            scope: "repository".into(),
            effect: OperationEffect::Write,
            authorization_reason: Some("simulated authorized crash".into()),
            command_json: r#"["git","branch","possibly-created"]"#.into(),
            pid: 999_999,
            host_operation_id: None,
            identity_provenance: aethyme_broker::OperationIdentityProvenance::LocalRepository,
        })
        .unwrap();
    broker
        .store()
        .transition_coordinated_operation(stale.id, OperationStatus::Running, None, None)
        .unwrap();

    let blocked = broker
        .run_coordinated_operation(request(session.id, &["branch", "after-crash"]))
        .unwrap_err();
    assert!(matches!(
        blocked,
        BrokerOpError::CoordinatedOperationBlocked { operation_id, .. }
            if operation_id == stale.id
    ));
    assert_eq!(
        broker
            .store()
            .coordinated_operation(stale.id)
            .unwrap()
            .unwrap()
            .status,
        OperationStatus::OutcomeUnknown
    );
    let shown = broker.show_coordinated_operation(stale.id).unwrap();
    assert_eq!(
        shown.reconciliation.state,
        OperationReconciliationState::Required
    );
    assert!(!shown.reconciliation.automatic_retry_allowed);

    broker
        .reconcile_coordinated_operation(stale.id, false, "remote inspection found no change")
        .unwrap();
    let reconciled = broker.show_coordinated_operation(stale.id).unwrap();
    assert_eq!(
        reconciled.reconciliation.state,
        OperationReconciliationState::ReconciledFailed
    );
    assert_eq!(
        reconciled.reconciliation.operator_reason.as_deref(),
        Some("remote inspection found no change")
    );
    assert!(!reconciled.reconciliation.automatic_retry_allowed);
    let retry = broker
        .run_coordinated_operation(request(session.id, &["branch", "after-crash"]))
        .unwrap();
    assert!(retry.ok());
}

#[test]
fn nonzero_write_is_unknown_because_partial_effects_are_possible() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let worktree = add_worktree(tmp.path(), "partial");
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session = broker.adopt(&worktree, None).unwrap();

    let report = broker
        .run_coordinated_operation(request(session.id, &["branch", "main"]))
        .unwrap();
    assert!(!report.command_success);
    assert_eq!(report.operation.status, OperationStatus::OutcomeUnknown);
    let recovery = report.unknown_outcome_recovery().unwrap().to_string();
    assert!(recovery.contains("Canonical repository local:"));
    assert!(recovery.contains("is now write-blocked"));
    assert!(recovery.contains(&format!("Operation ID: {}", report.operation.id)));
    assert!(recovery.contains("Inspect local Git refs and worktree state"));
    assert!(recovery.contains(&format!(
        "aethyme broker operations reconcile --operation {} --outcome succeeded --reason \"external inspection confirmed operation {} took effect\"",
        report.operation.id, report.operation.id
    )));
    assert!(recovery.contains(&format!(
        "aethyme broker operations reconcile --operation {} --outcome failed --reason \"external inspection confirmed operation {} did not take effect\"",
        report.operation.id, report.operation.id
    )));
    assert!(recovery.contains("Blind retry is forbidden"));

    let blocked = broker
        .run_coordinated_operation(request(session.id, &["branch", "after-nonzero"]))
        .unwrap_err();
    assert_eq!(blocked.to_string(), recovery);
    assert!(matches!(
        blocked,
        BrokerOpError::CoordinatedOperationBlocked { operation_id, .. }
            if operation_id == report.operation.id
    ));
}

#[test]
fn repository_write_lock_serializes_independent_process_clients() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    let worktree_a = add_worktree(tmp.path(), "lock-a");
    let worktree_b = add_worktree(tmp.path(), "lock-b");
    let mut broker = Broker::open(tmp.path()).unwrap();
    let session_a = broker.adopt(&worktree_a, None).unwrap();
    let session_b = broker.adopt(&worktree_b, None).unwrap();

    let root_a = tmp.path().to_path_buf();
    let root_b = root_a.clone();
    let run = |root: std::path::PathBuf, session_id| {
        std::thread::spawn(move || {
            let mut broker = Broker::open(&root).unwrap();
            broker
                .run_coordinated_operation(CoordinatedCommand {
                    session_id,
                    provider: OperationProvider::Git,
                    repository: None,
                    resolved_target: None,
                    scope: Some("test:sleep".into()),
                    declared_effect: Some(OperationEffect::Write),
                    destructive_confirmed: false,
                    authorization_reason: Some("concurrency regression".into()),
                    args: vec!["-c".into(), "alias.pause=!sleep 1".into(), "pause".into()],
                })
                .unwrap()
        })
    };

    let started = Instant::now();
    let first = run(root_a, session_a.id);
    std::thread::sleep(Duration::from_millis(100));
    let second = run(root_b, session_b.id);
    assert!(first.join().unwrap().ok());
    assert!(second.join().unwrap().ok());
    assert!(
        started.elapsed() >= Duration::from_millis(1_800),
        "repository-wide writes should serialize: {:?}",
        started.elapsed()
    );
}
