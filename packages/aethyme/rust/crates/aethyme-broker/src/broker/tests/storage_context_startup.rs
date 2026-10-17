use super::super::{Broker, BrokerOpenOptions, WORKTREE_ROOT_MARKER, WORKTREE_ROOT_SCHEMA_VERSION};
use std::path::{Path, PathBuf};
use std::process::Command;

const CHILD_MODE: &str = "AETHYME_TEST_STORAGE_CONTEXT_STARTUP_CHILD";
const NONE_MODE: &str = "AETHYME_TEST_STORAGE_CONTEXT_NONE_MODE";
const SLOT_IDENTITY_MODE: &str = "AETHYME_TEST_STORAGE_CONTEXT_SLOT_IDENTITY_MODE";
const RELATIVE_MODE: &str = "AETHYME_TEST_STORAGE_CONTEXT_RELATIVE_MODE";
const TEST_REPOSITORY: &str = "AETHYME_TEST_STORAGE_CONTEXT_REPOSITORY";

/// Broker open runs maintenance before returning. A child process gives this
/// test poisoned ambient paths without mutating process-wide environment in
/// the test runner, then proves that startup cleanup stays in the paths
/// captured by `BrokerOpenOptions`.
#[test]
fn startup_maintenance_uses_the_broker_captured_storage_context() {
    match std::env::var(CHILD_MODE).as_deref() {
        Ok("1") => {
            run_captured_startup_child();
            return;
        }
        Ok("worker") => {
            run_captured_context_worker();
            return;
        }
        _ => {}
    }

    let (repo, _) = super::landing_fixture();
    let capsule = tempfile::tempdir().expect("private fixture capsule");
    let private_state = capsule.path().join("private-host-state");
    let private_cache = capsule.path().join("private-host-cache");
    let private_worktrees = capsule.path().join("private-worktrees");
    let poisoned_state = capsule.path().join("poisoned-host-state");
    let poisoned_cache = capsule.path().join("poisoned-host-cache");
    let poisoned_worktrees = capsule.path().join("poisoned-worktrees");
    let poisoned_xdg_state = capsule.path().join("poisoned-xdg-state");
    let poisoned_xdg_cache = capsule.path().join("poisoned-xdg-cache");
    let poisoned_home = capsule.path().join("poisoned-home");
    let private_entry = preparation_entry(&private_state);
    let poisoned_entry = preparation_entry(&poisoned_state);
    for entry in [&private_entry, &poisoned_entry] {
        std::fs::create_dir_all(entry).expect("cache entry directory");
        std::fs::write(entry.join("payload"), b"cache data").expect("cache payload");
    }

    let output = Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "broker::tests::storage_context_startup::startup_maintenance_uses_the_broker_captured_storage_context",
            "--nocapture",
        ])
        .env(CHILD_MODE, "1")
        .env("AETHYME_TEST_STORAGE_CONTEXT_REPOSITORY", repo.path())
        .env("AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_STATE", &private_state)
        .env("AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_CACHE", &private_cache)
        .env("AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_WORKTREES", &private_worktrees)
        .env("AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_ENTRY", &private_entry)
        .env("AETHYME_TEST_STORAGE_CONTEXT_POISONED_STATE", &poisoned_state)
        .env("AETHYME_TEST_STORAGE_CONTEXT_POISONED_CACHE", &poisoned_cache)
        .env("AETHYME_TEST_STORAGE_CONTEXT_POISONED_WORKTREES", &poisoned_worktrees)
        .env("AETHYME_TEST_STORAGE_CONTEXT_POISONED_XDG_STATE", &poisoned_xdg_state)
        .env("AETHYME_TEST_STORAGE_CONTEXT_POISONED_XDG_CACHE", &poisoned_xdg_cache)
        .env("AETHYME_TEST_STORAGE_CONTEXT_POISONED_HOME", &poisoned_home)
        .env("AETHYME_HOST_STATE_DIR", &poisoned_state)
        .env("AETHYME_HOST_CACHE_DIR", &poisoned_cache)
        .env("AETHYME_WORKTREE_ROOT", &poisoned_worktrees)
        .env_remove("AETHYME_BROKER_DB")
        .env_remove("AETHYME_GATE_BROKER_DATABASES")
        .env_remove("AETHYME_BROKER_OPEN_HOST_OPERATION_DB")
        .env_remove("AETHYME_BROKER_OPEN_HOST_STATE_EXPLICIT")
        .env_remove("AETHYME_BROKER_OPEN_WORKTREE_ROOT")
        .env_remove("AETHYME_BROKER_OPEN_COLLABORATION_ROOT")
        .env_remove("AETHYME_BROKER_STORAGE_CONTEXT_V1")
        .env_remove("XDG_STATE_HOME")
        .env_remove("XDG_CACHE_HOME")
        .output()
        .expect("run the isolated startup child");

    assert!(
        output.status.success(),
        "child failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !private_entry.exists(),
        "startup maintenance should reclaim the dead entry in the captured cache"
    );
    assert!(
        poisoned_entry.exists(),
        "poisoned ambient host state must remain untouched"
    );
}

/// Relative BrokerOpenOptions paths are captured against the opening
/// process's current directory, so a detached worker with a different cwd
/// still uses the same storage locations.
#[test]
fn relative_storage_options_are_anchored_before_child_cwd_changes() {
    match std::env::var(RELATIVE_MODE).as_deref() {
        Ok("parent") => {
            run_relative_context_parent();
            return;
        }
        Ok("worker") => {
            run_relative_context_worker();
            return;
        }
        _ => {}
    }

    let (repo, _) = super::landing_fixture();
    let capsule = tempfile::tempdir().expect("private fixture capsule");
    let capture_cwd = capsule.path().join("capture-cwd");
    let worker_cwd = capsule.path().join("worker-cwd");
    std::fs::create_dir_all(&capture_cwd).expect("capture cwd");
    std::fs::create_dir_all(&worker_cwd).expect("worker cwd");

    let output = Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "broker::tests::storage_context_startup::relative_storage_options_are_anchored_before_child_cwd_changes",
            "--nocapture",
        ])
        .env(RELATIVE_MODE, "parent")
        .env(TEST_REPOSITORY, repo.path())
        .env("AETHYME_TEST_STORAGE_CONTEXT_CAPTURE_CWD", &capture_cwd)
        .env("AETHYME_TEST_STORAGE_CONTEXT_WORKER_CWD", &worker_cwd)
        .env_remove("AETHYME_BROKER_STORAGE_CONTEXT_V1")
        .env_remove("AETHYME_HOST_STATE_DIR")
        .env_remove("AETHYME_HOST_CACHE_DIR")
        .env_remove("AETHYME_WORKTREE_ROOT")
        .env_remove("AETHYME_BROKER_OPEN_HOST_OPERATION_DB")
        .env_remove("AETHYME_BROKER_OPEN_WORKTREE_ROOT")
        .env_remove("AETHYME_BROKER_OPEN_COLLABORATION_ROOT")
        .env_remove("AETHYME_BROKER_DB")
        .env_remove("AETHYME_GATE_BROKER_DATABASES")
        .env_remove("XDG_STATE_HOME")
        .env_remove("XDG_CACHE_HOME")
        .env_remove("HOME")
        .current_dir(&capture_cwd)
        .output()
        .expect("run relative context parent");
    assert!(
        output.status.success(),
        "parent failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        capture_cwd
            .join("relative-state/host-resources.db")
            .is_file()
    );
    assert!(!worker_cwd.join("relative-state").exists());
    assert!(!worker_cwd.join("relative-cache").exists());
    assert!(!worker_cwd.join("relative-worktrees").exists());
    assert!(!worker_cwd.join("relative-collaboration").exists());
}

fn run_relative_context_parent() {
    let repository = PathBuf::from(std::env::var_os(TEST_REPOSITORY).expect("repository path"));
    let capture_cwd = PathBuf::from(
        std::env::var_os("AETHYME_TEST_STORAGE_CONTEXT_CAPTURE_CWD").expect("capture cwd path"),
    );
    let worker_cwd = PathBuf::from(
        std::env::var_os("AETHYME_TEST_STORAGE_CONTEXT_WORKER_CWD").expect("worker cwd path"),
    );
    let broker = Broker::open_with_options(
        &repository,
        BrokerOpenOptions::default()
            .with_host_state_dir("relative-state")
            .with_host_cache_dir("relative-cache")
            .with_host_operation_database("relative-state/host-operations.db")
            .with_worktree_root("relative-worktrees")
            .with_collaboration_state("relative-collaboration"),
    )
    .expect("open broker from relative storage options");
    let expected_state = capture_cwd.join("relative-state");
    assert_eq!(
        broker.storage_context().host_state_dir.as_deref(),
        Some(expected_state.as_path())
    );
    assert_eq!(
        broker.storage_context().host_cache_dir.as_deref(),
        Some(capture_cwd.join("relative-cache").as_path())
    );
    assert_eq!(
        broker.storage_context().host_operation_db_path.as_deref(),
        Some(expected_state.join("host-operations.db").as_path())
    );

    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    command
        .args([
            "--exact",
            "broker::tests::storage_context_startup::relative_storage_options_are_anchored_before_child_cwd_changes",
            "--nocapture",
        ])
        .env(RELATIVE_MODE, "worker")
        .env(TEST_REPOSITORY, &repository)
        .env("AETHYME_TEST_STORAGE_CONTEXT_CAPTURE_CWD", &capture_cwd)
        .env("AETHYME_TEST_STORAGE_CONTEXT_WORKER_CWD", &worker_cwd)
        .current_dir(&worker_cwd);
    broker
        .configure_storage_context_for_child(&mut command)
        .expect("encode anchored child context");
    command
        .env("AETHYME_HOST_STATE_DIR", worker_cwd.join("poison-state"))
        .env("AETHYME_HOST_CACHE_DIR", worker_cwd.join("poison-cache"))
        .env("AETHYME_WORKTREE_ROOT", worker_cwd.join("poison-worktrees"))
        .env("XDG_STATE_HOME", worker_cwd.join("poison-xdg-state"))
        .env("XDG_CACHE_HOME", worker_cwd.join("poison-xdg-cache"))
        .env("HOME", worker_cwd.join("poison-home"));
    let output = command.output().expect("run worker with changed cwd");
    assert!(
        output.status.success(),
        "worker failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_relative_context_worker() {
    let repository = PathBuf::from(std::env::var_os(TEST_REPOSITORY).expect("repository path"));
    let capture_cwd = PathBuf::from(
        std::env::var_os("AETHYME_TEST_STORAGE_CONTEXT_CAPTURE_CWD").expect("capture cwd path"),
    );
    let broker = Broker::open_with_options(&repository, BrokerOpenOptions::default())
        .expect("open broker from absolute captured child paths");
    let context = broker.storage_context();
    assert_eq!(
        context.host_state_dir.as_deref(),
        Some(capture_cwd.join("relative-state").as_path())
    );
    assert_eq!(
        context.host_resource_db_path.as_deref(),
        Some(
            capture_cwd
                .join("relative-state/host-resources.db")
                .as_path()
        )
    );
    assert_eq!(
        context.host_cache_dir.as_deref(),
        Some(capture_cwd.join("relative-cache").as_path())
    );
    assert_eq!(
        context.host_operation_db_path.as_deref(),
        Some(
            capture_cwd
                .join("relative-state/host-operations.db")
                .as_path()
        )
    );
    assert_eq!(
        context.collaboration_state_dir.as_deref(),
        Some(capture_cwd.join("relative-collaboration").as_path())
    );
    assert_eq!(
        broker
            .worktree_root_plan()
            .expect("captured root plan")
            .preferred_root,
        Some(capture_cwd.join("relative-worktrees"))
    );
}

/// A valid private storage context is bound to the repository that captured
/// it. Free slot planning must refuse a second repository before producing any
/// worktree, temporary, or repository fallback candidate.
#[test]
fn slot_planner_rejects_a_captured_context_bound_to_another_repository() {
    match std::env::var(SLOT_IDENTITY_MODE).as_deref() {
        Ok("worker") => {
            run_slot_identity_worker();
            return;
        }
        _ => {}
    }

    let (repo_a, _) = super::landing_fixture();
    let (repo_b, _) = super::landing_fixture();
    let capsule = tempfile::tempdir().expect("private fixture capsule");
    let private_state = capsule.path().join("private-host-state");
    let private_cache = capsule.path().join("private-host-cache");
    let private_worktrees = capsule.path().join("private-worktrees");
    let poisoned_state = capsule.path().join("poisoned-host-state");
    let poisoned_cache = capsule.path().join("poisoned-host-cache");
    let poisoned_worktrees = capsule.path().join("poisoned-worktrees");
    let broker = Broker::open_with_options(
        repo_a.path(),
        BrokerOpenOptions::default()
            .with_host_state_dir(private_state.clone())
            .with_host_cache_dir(private_cache)
            .with_worktree_root(private_worktrees.clone()),
    )
    .expect("open repo A with an isolated captured context");

    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    command
        .args([
            "--exact",
            "broker::tests::storage_context_startup::slot_planner_rejects_a_captured_context_bound_to_another_repository",
            "--nocapture",
        ])
        .env(SLOT_IDENTITY_MODE, "worker")
        .env("AETHYME_TEST_STORAGE_CONTEXT_REPOSITORY", repo_b.path())
        .env("AETHYME_TEST_STORAGE_CONTEXT_REPOSITORY_A", repo_a.path())
        .env("AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_WORKTREES", &private_worktrees)
        .env("AETHYME_HOST_STATE_DIR", &poisoned_state)
        .env("AETHYME_HOST_CACHE_DIR", &poisoned_cache)
        .env("AETHYME_WORKTREE_ROOT", &poisoned_worktrees);
    broker
        .configure_storage_context_for_child(&mut command)
        .expect("serialize the valid repo A context");
    command
        .env("AETHYME_HOST_STATE_DIR", &poisoned_state)
        .env("AETHYME_HOST_CACHE_DIR", &poisoned_cache)
        .env("AETHYME_WORKTREE_ROOT", &poisoned_worktrees);
    let output = command.output().expect("run cross-repository slot worker");

    assert!(
        output.status.success(),
        "worker failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !repo_b
            .path()
            .join(".aethyme/run/captured-context-mismatch")
            .exists()
    );
    assert!(
        !private_worktrees
            .join(".aethyme/run/captured-context-mismatch")
            .exists()
    );
    assert!(
        !poisoned_worktrees
            .join(".aethyme/run/captured-context-mismatch")
            .exists()
    );
}

fn run_slot_identity_worker() {
    let repo_a = PathBuf::from(
        std::env::var_os("AETHYME_TEST_STORAGE_CONTEXT_REPOSITORY_A")
            .expect("repo A path from parent"),
    );
    let repo_b = PathBuf::from(std::env::var_os(TEST_REPOSITORY).expect("repo B path from parent"));
    let private_worktrees = PathBuf::from(
        std::env::var_os("AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_WORKTREES")
            .expect("private worktree root from parent"),
    );

    let repo_a_git = crate::GitRepo::discover(&repo_a).expect("discover repo A");
    let common_a = repo_a_git
        .git_common_dir()
        .expect("repo A common directory");
    let root_plan_a =
        crate::broker::lifecycle::plan_worktree_root_for(&repo_a, Some(&common_a), None);
    assert!(
        root_plan_a.storage_context_refusal.is_none(),
        "the valid context should match repo A: {:?}",
        root_plan_a.storage_context_refusal
    );

    let stale_plan_placement = crate::verification::plan_slot_placement_with_worktree_root_plan(
        &repo_b,
        "captured-context-mismatch",
        &root_plan_a,
    );
    assert!(
        stale_plan_placement
            .blocking_refusal
            .as_deref()
            .is_some_and(|reason| reason.contains("different repository")),
        "a root plan cannot be reused for repo B: {:?}",
        stale_plan_placement.blocking_refusal
    );
    assert!(!stale_plan_placement.directory.exists());

    let root_plan_b = crate::verification::default_worktree_root_plan(&repo_b);
    assert!(
        root_plan_b
            .storage_context_refusal
            .as_deref()
            .is_some_and(|reason| reason.contains("different repository")),
        "a captured repo A context cannot plan for repo B: {:?}",
        root_plan_b.storage_context_refusal
    );
    assert!(crate::broker::lifecycle::worktree_root_candidates_for(&root_plan_b).is_empty());
    let placement = crate::verification::plan_slot_placement(&repo_b, "captured-context-mismatch");
    assert!(
        placement
            .blocking_refusal
            .as_deref()
            .is_some_and(|reason| reason.contains("different repository")),
        "the actual slot planner must fail before candidate allocation: {:?}",
        placement.blocking_refusal
    );
    assert!(!placement.directory.exists());
    assert!(!repo_b.path().join(".aethyme/run").exists());
    assert!(!private_worktrees.join(".aethyme/run").exists());
}

/// A Broker opened with no resolvable host state/cache must keep that exact
/// absence in its detached worker, even if XDG and HOME change after open.
#[test]
fn child_context_preserves_captured_absence_after_xdg_changes() {
    match std::env::var(NONE_MODE).as_deref() {
        Ok("parent") => {
            run_absent_context_parent();
            return;
        }
        Ok("worker") => {
            run_absent_context_worker();
            return;
        }
        _ => {}
    }

    let (repo, _) = super::landing_fixture();
    let capsule = tempfile::tempdir().expect("private fixture capsule");
    let poisoned_state = capsule.path().join("poisoned-xdg-state");
    let poisoned_cache = capsule.path().join("poisoned-xdg-cache");
    let poisoned_home = capsule.path().join("poisoned-home");
    let output = Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "broker::tests::storage_context_startup::child_context_preserves_captured_absence_after_xdg_changes",
            "--nocapture",
        ])
        .env(NONE_MODE, "parent")
        .env(TEST_REPOSITORY, repo.path())
        .env("AETHYME_TEST_STORAGE_CONTEXT_POISONED_STATE", &poisoned_state)
        .env("AETHYME_TEST_STORAGE_CONTEXT_POISONED_CACHE", &poisoned_cache)
        .env("AETHYME_TEST_STORAGE_CONTEXT_POISONED_HOME", &poisoned_home)
        .env_remove("AETHYME_HOST_STATE_DIR")
        .env_remove("AETHYME_HOST_CACHE_DIR")
        .env_remove("AETHYME_BROKER_STORAGE_CONTEXT_V1")
        .env_remove("AETHYME_BROKER_OPEN_HOST_OPERATION_DB")
        .env_remove("AETHYME_BROKER_OPEN_HOST_STATE_EXPLICIT")
        .env_remove("AETHYME_BROKER_OPEN_WORKTREE_ROOT")
        .env_remove("AETHYME_BROKER_OPEN_COLLABORATION_ROOT")
        .env_remove("AETHYME_WORKTREE_ROOT")
        .env_remove("AETHYME_BROKER_DB")
        .env_remove("AETHYME_GATE_BROKER_DATABASES")
        .env_remove("XDG_STATE_HOME")
        .env_remove("XDG_CACHE_HOME")
        .env_remove("HOME")
        .output()
        .expect("run the isolated absent-context parent");

    assert!(
        output.status.success(),
        "child failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !poisoned_state.join("aethyme/host-resources.db").exists(),
        "captured absence must not resolve a changed XDG state directory"
    );
    assert!(
        !poisoned_state.join("host-resources.db").exists(),
        "captured absence must not create an explicitly named host DB"
    );
    assert!(
        !poisoned_cache.join("aethyme").exists(),
        "captured absence must not resolve a changed XDG cache directory"
    );
    assert!(
        !poisoned_home
            .join("Library/Application Support/Aethyme")
            .exists(),
        "captured absence must not resolve a changed HOME directory"
    );
}

/// Malformed private context is rejected before the broker opens or performs
/// maintenance, rather than falling through to ambient host paths.
#[test]
fn malformed_private_storage_context_fails_before_open_maintenance() {
    if std::env::var(CHILD_MODE).as_deref() == Ok("invalid-context") {
        let repository =
            PathBuf::from(std::env::var_os(TEST_REPOSITORY).expect("repository path from parent"));
        let root_plan = crate::verification::default_worktree_root_plan(&repository);
        assert!(root_plan.storage_context_refusal.is_some());
        assert!(crate::broker::lifecycle::worktree_root_candidates_for(&root_plan).is_empty());
        let placement = crate::verification::plan_slot_placement(&repository, "malformed-context");
        assert!(placement.blocking_refusal.is_some());
        assert!(!placement.directory.exists());
        let plan_error = crate::storage::storage_plan(&repository)
            .expect_err("storage plan must reject malformed context before scanning");
        assert!(
            plan_error
                .to_string()
                .contains("invalid captured broker storage context"),
            "unexpected storage-plan refusal: {plan_error}"
        );
        let confirmation = "a".repeat(64);
        let apply_error = crate::storage::storage_apply(&repository, &confirmation)
            .expect_err("storage apply must reject malformed context before writes");
        assert!(
            apply_error
                .to_string()
                .contains("invalid captured broker storage context"),
            "unexpected storage-apply refusal: {apply_error}"
        );
        let error = match Broker::open_with_options(&repository, BrokerOpenOptions::default()) {
            Err(error) => error,
            Ok(_) => panic!("malformed private context must be rejected"),
        };
        assert!(
            error
                .to_string()
                .contains("invalid captured broker storage context"),
            "unexpected error: {error}"
        );
        let expected = std::env::var("AETHYME_TEST_STORAGE_CONTEXT_EXPECTED_ERROR")
            .expect("expected context refusal");
        assert!(
            error.to_string().contains(&expected),
            "expected {expected:?} in error: {error}"
        );
        return;
    }

    let (repo, _) = super::landing_fixture();
    let capsule = tempfile::tempdir().expect("private fixture capsule");
    let poisoned_state = capsule.path().join("poisoned-state");
    let poisoned_cache = capsule.path().join("poisoned-cache");
    let entry = preparation_entry(&poisoned_state);
    std::fs::create_dir_all(&entry).expect("poisoned preparation entry");
    std::fs::write(entry.join("payload"), b"keep me").expect("poisoned payload");

    let broker = Broker::open_with_options(&repo_a, BrokerOpenOptions::default())
        .expect("open repo A under its captured storage context");
    let storage_plan_a =
        crate::storage::storage_plan_with_context(&repo_a, broker.storage_context())
            .expect("plan repo A with its captured context");
    assert_eq!(storage_plan_a.storage_root, private_worktrees);
    let unbound_context = crate::broker::BrokerStorageContext::default();
    let unbound_error = crate::storage::storage_plan_with_context(&repo_a, &unbound_context)
        .expect_err("storage planning requires a repository-bound context");
    assert!(
        unbound_error.to_string().contains("repository-bound"),
        "unexpected unbound context refusal: {unbound_error}"
    );
    let storage_plan_error =
        crate::storage::storage_plan_with_context(&repo_b, broker.storage_context())
            .expect_err("repo A context must not plan storage for repo B");
    assert!(
        storage_plan_error
            .to_string()
            .contains("different repository root"),
        "unexpected storage-plan refusal: {storage_plan_error}"
    );
    let confirmation = "a".repeat(64);
    let storage_apply_error = crate::storage::storage_apply_with_context(
        &repo_b,
        &confirmation,
        broker.storage_context(),
    )
    .expect_err("repo A context must not apply storage for repo B");
    assert!(
        storage_apply_error
            .to_string()
            .contains("different repository root"),
        "unexpected storage-apply refusal: {storage_apply_error}"
    );

    let repo_git = crate::GitRepo::discover(repo.path()).expect("discover fixture repository");
    let main_root = repo_git.main_root().expect("resolve main repository");
    let common_dir = repo_git.git_common_dir().expect("resolve common directory");
    let v1_payload = serde_json::json!({
        "version": 1,
        "repository_root": main_root,
        "git_common_dir": common_dir,
        "host_state_dir": null,
        "host_resource_db_path": null,
        "host_cache_dir": null,
        "host_operation_db_path": null,
        "worktree_root_override": null,
        "worktree_root_environment_override": null,
        "collaboration_state_dir": null,
        "host_state_dir_is_explicit": false
    })
    .to_string();
    let mut incomplete_v2: serde_json::Value = serde_json::from_str(
        &serde_json::json!({
            "version": 2,
            "repository_root": main_root,
            "git_common_dir": common_dir,
            "host_state_dir": null,
            "host_resource_db_path": null,
            "host_cache_dir": null,
            "host_operation_db_path": null,
            "worktree_root_override": null,
            "worktree_root_environment_override": null,
            "collaboration_state_dir": null,
            "host_state_dir_is_explicit": false
        })
        .to_string(),
    )
    .expect("parse V2 fixture");
    incomplete_v2
        .as_object_mut()
        .expect("V2 object")
        .remove("host_cache_dir");
    let cases = [
        ("not-json".to_string(), "child context is malformed"),
        (v1_payload, "unsupported child-context version 1"),
        (incomplete_v2.to_string(), "missing field"),
    ];

    for (payload, expected_error) in cases {
        let output = Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "broker::tests::storage_context_startup::malformed_private_storage_context_fails_before_open_maintenance",
                "--nocapture",
            ])
            .env(CHILD_MODE, "invalid-context")
            .env(TEST_REPOSITORY, repo.path())
            .env("AETHYME_TEST_STORAGE_CONTEXT_EXPECTED_ERROR", expected_error)
            .env("AETHYME_BROKER_STORAGE_CONTEXT_V1", payload)
            .env("AETHYME_HOST_STATE_DIR", &poisoned_state)
            .env("AETHYME_HOST_CACHE_DIR", &poisoned_cache)
            .env_remove("AETHYME_BROKER_DB")
            .env_remove("AETHYME_GATE_BROKER_DATABASES")
            .output()
            .expect("run invalid-context child");
        assert!(
            output.status.success(),
            "child failed for {expected_error:?}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    assert!(
        entry.join("payload").is_file(),
        "invalid contexts must not sweep cache"
    );
    assert!(
        !poisoned_state.join("host-resources.db").exists(),
        "invalid contexts must be rejected before opening host resources"
    );
}

fn run_absent_context_parent() {
    let repository = PathBuf::from(std::env::var_os(TEST_REPOSITORY).expect("repository path"));
    let poisoned_state = PathBuf::from(
        std::env::var_os("AETHYME_TEST_STORAGE_CONTEXT_POISONED_STATE")
            .expect("poisoned state path"),
    );
    let poisoned_cache = PathBuf::from(
        std::env::var_os("AETHYME_TEST_STORAGE_CONTEXT_POISONED_CACHE")
            .expect("poisoned cache path"),
    );
    let poisoned_home = PathBuf::from(
        std::env::var_os("AETHYME_TEST_STORAGE_CONTEXT_POISONED_HOME").expect("poisoned home path"),
    );

    let broker = Broker::open_with_options(&repository, BrokerOpenOptions::default())
        .expect("open broker without host storage");
    assert!(broker.storage_context().host_state_dir.is_none());
    assert!(broker.storage_context().host_cache_dir.is_none());

    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    command
        .args([
            "--exact",
            "broker::tests::storage_context_startup::child_context_preserves_captured_absence_after_xdg_changes",
            "--nocapture",
        ])
        .env(NONE_MODE, "worker")
        .env(TEST_REPOSITORY, &repository)
        .env("AETHYME_TEST_STORAGE_CONTEXT_POISONED_STATE", &poisoned_state)
        .env("AETHYME_TEST_STORAGE_CONTEXT_POISONED_CACHE", &poisoned_cache)
        .env("AETHYME_TEST_STORAGE_CONTEXT_POISONED_HOME", &poisoned_home)
        .env("XDG_STATE_HOME", &poisoned_state)
        .env("XDG_CACHE_HOME", &poisoned_cache)
        .env("HOME", &poisoned_home);
    broker
        .configure_storage_context_for_child(&mut command)
        .expect("serialize captured absent context");
    let output = command.output().expect("run worker with changed XDG/HOME");
    assert!(
        output.status.success(),
        "worker failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_absent_context_worker() {
    let repository = PathBuf::from(std::env::var_os(TEST_REPOSITORY).expect("repository path"));
    let poisoned_state = PathBuf::from(
        std::env::var_os("AETHYME_TEST_STORAGE_CONTEXT_POISONED_STATE")
            .expect("poisoned state path"),
    );
    let poisoned_cache = PathBuf::from(
        std::env::var_os("AETHYME_TEST_STORAGE_CONTEXT_POISONED_CACHE")
            .expect("poisoned cache path"),
    );
    let poisoned_home = PathBuf::from(
        std::env::var_os("AETHYME_TEST_STORAGE_CONTEXT_POISONED_HOME").expect("poisoned home path"),
    );
    let broker = Broker::open_with_options(&repository, BrokerOpenOptions::default())
        .expect("open broker from captured child context");
    assert!(broker.storage_context().host_state_dir.is_none());
    assert!(broker.storage_context().host_resource_db_path.is_none());
    assert!(broker.storage_context().host_operation_db_path.is_none());
    assert!(broker.storage_context().host_cache_dir.is_none());
    assert!(!poisoned_state.join("aethyme/host-resources.db").exists());
    assert!(!poisoned_cache.join("aethyme").exists());
    assert!(
        !poisoned_home
            .join("Library/Application Support/Aethyme")
            .exists()
    );
}

fn run_captured_startup_child() {
    let var_path = |name: &str| {
        PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("missing {name}")))
    };
    let repository = var_path("AETHYME_TEST_STORAGE_CONTEXT_REPOSITORY");
    let private_state = var_path("AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_STATE");
    let private_cache = var_path("AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_CACHE");
    let private_worktrees = var_path("AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_WORKTREES");
    let private_entry = var_path("AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_ENTRY");
    let poisoned_state = var_path("AETHYME_TEST_STORAGE_CONTEXT_POISONED_STATE");
    let poisoned_cache = var_path("AETHYME_TEST_STORAGE_CONTEXT_POISONED_CACHE");
    let poisoned_worktrees = var_path("AETHYME_TEST_STORAGE_CONTEXT_POISONED_WORKTREES");
    let poisoned_xdg_state = var_path("AETHYME_TEST_STORAGE_CONTEXT_POISONED_XDG_STATE");
    let poisoned_xdg_cache = var_path("AETHYME_TEST_STORAGE_CONTEXT_POISONED_XDG_CACHE");
    let poisoned_home = var_path("AETHYME_TEST_STORAGE_CONTEXT_POISONED_HOME");

    let repo_handle = crate::GitRepo::discover(&repository).expect("discover fixture repository");
    let git_common_dir = repo_handle
        .git_common_dir()
        .expect("fixture Git common directory");
    let root_marker = crate::broker::WorktreeRootMarker {
        schema_version: WORKTREE_ROOT_SCHEMA_VERSION,
        repository_key: crate::host_state::repository_key(&repository, Some(&git_common_dir)),
        repository_root: repository.clone(),
    };
    std::fs::create_dir_all(&private_worktrees).expect("private worktree root");
    std::fs::write(
        private_worktrees.join(WORKTREE_ROOT_MARKER),
        serde_json::to_vec(&root_marker).expect("serialize marker"),
    )
    .expect("write private worktree marker");

    let options = BrokerOpenOptions::default()
        .with_host_state_dir(private_state.clone())
        .with_host_cache_dir(private_cache.clone())
        .with_host_operation_database(private_state.join("host-operations.db"))
        .with_worktree_root(private_worktrees.clone());
    let broker = Broker::open_with_options(&repository, options).expect("open broker");
    let context = broker.storage_context();
    assert_eq!(
        context.host_state_dir.as_deref(),
        Some(private_state.as_path())
    );
    assert_eq!(
        context.host_cache_dir.as_deref(),
        Some(private_cache.as_path())
    );
    assert_eq!(
        broker
            .worktree_root_plan()
            .expect("captured worktree plan")
            .preferred_root,
        Some(private_worktrees)
    );
    assert!(private_state.join("host-resources.db").is_file());
    assert!(!private_entry.exists());
    assert!(
        preparation_entry(&poisoned_state).join("payload").is_file(),
        "startup maintenance must not sweep poisoned host state"
    );
    assert!(
        !poisoned_state.join("host-resources.db").exists(),
        "the captured sweep must not initialize the ambient resource database"
    );
    assert!(
        !poisoned_worktrees.exists(),
        "the poison worktree container was only an ambient decoy"
    );
    assert!(!poisoned_cache.exists());

    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    command
        .args([
            "--exact",
            "broker::tests::storage_context_startup::startup_maintenance_uses_the_broker_captured_storage_context",
            "--nocapture",
        ])
        .env(CHILD_MODE, "worker")
        .env(TEST_REPOSITORY, &repository)
        .env("AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_STATE", &private_state)
        .env("AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_CACHE", &private_cache)
        .env("AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_WORKTREES", &private_worktrees)
        .env("AETHYME_TEST_STORAGE_CONTEXT_POISONED_STATE", &poisoned_state)
        .env("AETHYME_TEST_STORAGE_CONTEXT_POISONED_CACHE", &poisoned_cache)
        .env("AETHYME_TEST_STORAGE_CONTEXT_POISONED_XDG_STATE", &poisoned_xdg_state)
        .env("AETHYME_TEST_STORAGE_CONTEXT_POISONED_XDG_CACHE", &poisoned_xdg_cache)
        .env("AETHYME_TEST_STORAGE_CONTEXT_POISONED_HOME", &poisoned_home)
        .env("XDG_STATE_HOME", &poisoned_xdg_state)
        .env("XDG_CACHE_HOME", &poisoned_xdg_cache)
        .env("HOME", &poisoned_home);
    broker
        .configure_storage_context_for_child(&mut command)
        .expect("serialize captured present context");
    // The broker's private context must stay bound even if a later caller
    // changes the public host paths. Other child commands still see HOME/XDG.
    command
        .env("AETHYME_HOST_STATE_DIR", &poisoned_state)
        .env("AETHYME_HOST_CACHE_DIR", &poisoned_cache);
    let output = command
        .output()
        .expect("run worker with changed host/XDG paths");
    assert!(
        output.status.success(),
        "worker failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_captured_context_worker() {
    let var_path = |name: &str| {
        PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("missing {name}")))
    };
    let repository = var_path(TEST_REPOSITORY);
    let private_state = var_path("AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_STATE");
    let private_cache = var_path("AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_CACHE");
    let private_worktrees = var_path("AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_WORKTREES");
    let poisoned_state = var_path("AETHYME_TEST_STORAGE_CONTEXT_POISONED_STATE");
    let poisoned_cache = var_path("AETHYME_TEST_STORAGE_CONTEXT_POISONED_CACHE");
    let poisoned_xdg_state = var_path("AETHYME_TEST_STORAGE_CONTEXT_POISONED_XDG_STATE");
    let poisoned_xdg_cache = var_path("AETHYME_TEST_STORAGE_CONTEXT_POISONED_XDG_CACHE");
    let poisoned_home = var_path("AETHYME_TEST_STORAGE_CONTEXT_POISONED_HOME");

    let broker = Broker::open_with_options(&repository, BrokerOpenOptions::default())
        .expect("open broker from captured child context");
    assert_eq!(
        broker.storage_context().host_state_dir.as_deref(),
        Some(private_state.as_path())
    );
    assert_eq!(
        broker.storage_context().host_cache_dir.as_deref(),
        Some(private_cache.as_path())
    );
    let expected_resource_db = private_state.join("host-resources.db");
    assert_eq!(
        broker.storage_context().host_resource_db_path.as_deref(),
        Some(expected_resource_db.as_path())
    );
    assert_eq!(
        broker.storage_context().host_operation_db_path.as_deref(),
        Some(private_state.join("host-operations.db").as_path())
    );
    assert_eq!(
        broker
            .worktree_root_plan()
            .expect("captured child worktree plan")
            .preferred_root,
        Some(private_worktrees)
    );
    assert!(!poisoned_state.join("host-resources.db").exists());
    assert!(!poisoned_cache.exists());
    assert!(
        !poisoned_xdg_state
            .join("aethyme/host-resources.db")
            .exists()
    );
    assert!(!poisoned_xdg_cache.join("aethyme").exists());
    assert!(
        !poisoned_home
            .join("Library/Application Support/Aethyme")
            .exists()
    );
}

fn preparation_entry(host_state: &Path) -> PathBuf {
    host_state
        .join("preparation-cache")
        .join("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .join("bbbbbbbbbbbb")
}
