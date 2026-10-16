use super::super::{Broker, BrokerOpenOptions, WORKTREE_ROOT_MARKER, WORKTREE_ROOT_SCHEMA_VERSION};
use std::path::{Path, PathBuf};
use std::process::Command;

const CHILD_MODE: &str = "AETHYME_TEST_STORAGE_CONTEXT_STARTUP_CHILD";
const NONE_MODE: &str = "AETHYME_TEST_STORAGE_CONTEXT_NONE_MODE";
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
    if std::env::var(CHILD_MODE).as_deref() == Ok("malformed") {
        let repository =
            PathBuf::from(std::env::var_os(TEST_REPOSITORY).expect("repository path from parent"));
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
        return;
    }

    let (repo, _) = super::landing_fixture();
    let capsule = tempfile::tempdir().expect("private fixture capsule");
    let poisoned_state = capsule.path().join("poisoned-state");
    let poisoned_cache = capsule.path().join("poisoned-cache");
    let entry = preparation_entry(&poisoned_state);
    std::fs::create_dir_all(&entry).expect("poisoned preparation entry");
    std::fs::write(entry.join("payload"), b"keep me").expect("poisoned payload");

    let output = Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "broker::tests::storage_context_startup::malformed_private_storage_context_fails_before_open_maintenance",
            "--nocapture",
        ])
        .env(CHILD_MODE, "malformed")
        .env(TEST_REPOSITORY, repo.path())
        .env("AETHYME_BROKER_STORAGE_CONTEXT_V1", "not-json")
        .env("AETHYME_HOST_STATE_DIR", &poisoned_state)
        .env("AETHYME_HOST_CACHE_DIR", &poisoned_cache)
        .env_remove("AETHYME_BROKER_DB")
        .env_remove("AETHYME_GATE_BROKER_DATABASES")
        .output()
        .expect("run the malformed-context child");

    assert!(
        output.status.success(),
        "child failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        entry.join("payload").is_file(),
        "malformed context must not sweep cache"
    );
    assert!(
        !poisoned_state.join("host-resources.db").exists(),
        "malformed context must be rejected before opening host resources"
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
