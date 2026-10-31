use super::super::{Broker, BrokerOpenOptions, WORKTREE_ROOT_MARKER, WORKTREE_ROOT_SCHEMA_VERSION};
use std::path::{Path, PathBuf};
use std::process::Command;

const CHILD_MODE: &str = "AETHYME_TEST_STORAGE_CONTEXT_STARTUP_CHILD";
const NONE_MODE: &str = "AETHYME_TEST_STORAGE_CONTEXT_NONE_MODE";
const SLOT_IDENTITY_MODE: &str = "AETHYME_TEST_STORAGE_CONTEXT_SLOT_IDENTITY_MODE";
const RELATIVE_MODE: &str = "AETHYME_TEST_STORAGE_CONTEXT_RELATIVE_MODE";
const TEST_REPOSITORY: &str = "AETHYME_TEST_STORAGE_CONTEXT_REPOSITORY";
const RESTRICTIVE_UMASK_CHILD: &str = "AETHYME_TEST_PREPARATION_CACHE_UMASK_CHILD";
const PRIVATE_LEASE_ID: &str = "AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_LEASE_ID";
const PRIVATE_LEASE_GENERATION: &str = "AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_LEASE_GENERATION";
const POISONED_LEASE_ID: &str = "AETHYME_TEST_STORAGE_CONTEXT_POISONED_LEASE_ID";
const POISONED_LEASE_GENERATION: &str = "AETHYME_TEST_STORAGE_CONTEXT_POISONED_LEASE_GENERATION";
const PRIVATE_POLICY_DIGEST: &str = "AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_POLICY_DIGEST";
const POISONED_POLICY_DIGEST: &str = "AETHYME_TEST_STORAGE_CONTEXT_POISONED_POLICY_DIGEST";

/// Broker open runs maintenance before returning. A child process gives this
/// test poisoned ambient paths without mutating process-wide environment in
/// the test runner, then proves that startup cleanup stays in the paths
/// captured by `BrokerOpenOptions`. Inline startup work has a short budget;
/// when it defers, its persisted request must survive for an explicit bounded
/// continuation using the same captured context.
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
    let capsule_root = capsule.path().canonicalize().unwrap();
    let private_state = capsule_root.as_path().join("private-host-state");
    let private_cache = capsule_root.as_path().join("private-host-cache");
    let private_preparation_root = capsule_root.as_path().join("private-preparation-cache");
    std::fs::create_dir(&private_preparation_root).expect("private preparation-cache root");
    let private_worktrees = capsule_root.as_path().join("private-worktrees");
    let poisoned_state = capsule_root.as_path().join("poisoned-host-state");
    let poisoned_cache = capsule_root.as_path().join("poisoned-host-cache");
    let poisoned_worktrees = capsule_root.as_path().join("poisoned-worktrees");
    let poisoned_xdg_state = capsule_root.as_path().join("poisoned-xdg-state");
    let poisoned_xdg_cache = capsule_root.as_path().join("poisoned-xdg-cache");
    let poisoned_home = capsule_root.as_path().join("poisoned-home");

    // First writable open initializes the opt-in root while it is empty. The
    // test payload is placed only inside the resulting nonce-scoped namespace.
    let mut initialized = Broker::open_with_options(
        repo.path(),
        BrokerOpenOptions::default()
            .with_host_state_dir(private_state.clone())
            .with_host_cache_dir(private_cache.clone())
            .with_host_operation_database(private_state.join("host-operations.db"))
            .with_worktree_root(private_worktrees.clone())
            .with_preparation_cache_root(private_preparation_root.clone()),
    )
    .expect("initialize the separate preparation-cache root");
    let private_effective_root = initialized
        .storage_context()
        .preparation_cache_root()
        .expect("initialized preparation-cache path")
        .expect("separate preparation-cache root");
    let mut configured_child = Command::new(std::env::current_exe().expect("test executable"));
    initialized
        .configure_storage_context_for_child(&mut configured_child)
        .expect("configure an actual override child");
    let override_payload = configured_child
        .get_envs()
        .find_map(|(name, value)| {
            (name == "AETHYME_BROKER_STORAGE_CONTEXT_V1")
                .then(|| value.and_then(|value| value.to_str()))
                .flatten()
        })
        .expect("the configured child receives its captured context");
    let override_json: serde_json::Value = serde_json::from_str(override_payload).unwrap();
    assert_eq!(override_json["version"], 4);
    assert!(override_json["preparation_cache_coordinator_db_path"].is_string());

    // Cache initialization is independent of worktree placement. Startup
    // maintenance itself requires a root bound through the real lifecycle.
    initialized
        .start_worktree("initialize captured startup maintenance", None)
        .expect("bind the fixture root before testing startup maintenance");
    let private_entry = preparation_entry_under(&private_effective_root);
    let poisoned_entry = preparation_entry(&poisoned_state);
    for entry in [&private_entry, &poisoned_entry] {
        std::fs::create_dir_all(entry).expect("cache entry directory");
        std::fs::write(entry.join("payload"), b"cache data").expect("cache payload");
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    publish_captured_cache_rotation(
        &private_effective_root,
        &private_state.join("host-resources.db"),
    );

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
        .env(
            "AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_PREPARATION_ROOT",
            &private_preparation_root,
        )
        .env(
            "AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_EFFECTIVE_ROOT",
            &private_effective_root,
        )
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
        .env(
            "AETHYME_PREPARATION_CACHE_ROOT",
            poisoned_state.join("preparation-cache"),
        )
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

    let protected = private_effective_root.join("marker-change-sentinel");
    std::fs::write(&protected, b"keep after marker change").unwrap();
    let marker_path = private_preparation_root.join(".aethyme-preparation-cache-root-v2");
    let marker_before = std::fs::read(&marker_path).unwrap();
    let mut marker: serde_json::Value = serde_json::from_slice(&marker_before).unwrap();
    marker["coordinator_db_path"] = serde_json::Value::from(
        poisoned_state
            .join("host-resources.db")
            .to_string_lossy()
            .into_owned(),
    );
    std::fs::write(&marker_path, serde_json::to_vec(&marker).unwrap()).unwrap();
    let mut changed_child = Command::new("not-launched-after-invalid-binding");
    assert!(
        initialized
            .configure_storage_context_for_child(&mut changed_child)
            .is_err()
    );
    assert_eq!(
        std::fs::read(&protected).unwrap(),
        b"keep after marker change"
    );
}

#[test]
fn default_child_context_uses_the_legacy_v2_payload() {
    let (repo, _) = super::landing_fixture();
    let host_state = tempfile::tempdir().expect("private host-state fixture");
    let broker = Broker::open_with_options(
        repo.path(),
        BrokerOpenOptions::default().with_host_state_dir(host_state.path()),
    )
    .expect("open isolated broker");
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    broker
        .configure_storage_context_for_child(&mut command)
        .expect("configure the real child command");

    let payload = command
        .get_envs()
        .find_map(|(name, value)| {
            (name == "AETHYME_BROKER_STORAGE_CONTEXT_V1")
                .then(|| value.and_then(|value| value.to_str()))
                .flatten()
        })
        .expect("the real child environment carries its captured context");
    let parsed: serde_json::Value = serde_json::from_str(payload).unwrap();
    assert_eq!(parsed["version"], 2);
    assert!(parsed.get("preparation_cache_root").is_none());
    assert!(
        parsed
            .get("preparation_cache_coordinator_db_path")
            .is_none()
    );
}

#[cfg(unix)]
#[test]
fn separate_cache_root_modes_survive_a_restrictive_process_umask() {
    if std::env::var(RESTRICTIVE_UMASK_CHILD).as_deref() == Ok("1") {
        initialize_cache_root_in_restrictive_umask_child();
        return;
    }

    let capsule = tempfile::tempdir().expect("fixture capsule");
    let capsule_root = capsule.path().canonicalize().unwrap();
    let root = capsule_root.as_path().join("preparation-cache");
    let host_state = capsule_root.as_path().join("host-state");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&host_state).unwrap();
    let resource_db = host_state.join("host-resources.db");
    std::fs::File::create(&resource_db).unwrap();
    let resource_db = resource_db.canonicalize().unwrap();

    let output = Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "broker::tests::storage_context_startup::separate_cache_root_modes_survive_a_restrictive_process_umask",
            "--nocapture",
        ])
        .env(RESTRICTIVE_UMASK_CHILD, "1")
        .env("AETHYME_TEST_PREPARATION_CACHE_ROOT", &root)
        .env("AETHYME_TEST_PREPARATION_CACHE_RESOURCE_DB", &resource_db)
        .output()
        .expect("run restrictive-umask child");
    assert!(
        output.status.success(),
        "child failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    use std::os::unix::fs::MetadataExt;
    let marker_path = root.join(".aethyme-preparation-cache-root-v2");
    let marker_mode = std::fs::symlink_metadata(&marker_path).unwrap().mode() & 0o777;
    let lock_mode = std::fs::symlink_metadata(root.join(".aethyme-preparation-cache-root.lock"))
        .unwrap()
        .mode()
        & 0o777;
    let marker: serde_json::Value =
        serde_json::from_slice(&std::fs::read(marker_path).unwrap()).unwrap();
    let nonce = marker["nonce"].as_str().unwrap();
    let namespace_mode =
        std::fs::symlink_metadata(root.join(format!(".aethyme-preparation-cache-{nonce}")))
            .unwrap()
            .mode()
            & 0o777;
    assert_eq!(lock_mode, 0o600);
    assert_eq!(marker_mode, 0o600);
    assert_eq!(namespace_mode, 0o700);
}

#[cfg(unix)]
fn initialize_cache_root_in_restrictive_umask_child() {
    let path = PathBuf::from(
        std::env::var_os("AETHYME_TEST_PREPARATION_CACHE_ROOT").expect("fixture cache root"),
    );
    let resource_db = PathBuf::from(
        std::env::var_os("AETHYME_TEST_PREPARATION_CACHE_RESOURCE_DB")
            .expect("fixture coordinator database"),
    );
    let capture = crate::preparation::rotation_debt::capture_preparation_cache_root(&path)
        .expect("capture existing empty root before umask change");
    // SAFETY: this mutation is confined to the child test process, which exits
    // immediately after initialization. The test runner process is untouched.
    unsafe {
        libc::umask(0o777);
    }
    crate::preparation::rotation_debt::initialize_preparation_cache_root(
        &path,
        &capture.identity,
        &capture.directory,
        &resource_db,
        || Ok(false),
    )
    .expect("initialization normalizes modes on newly created owned entries");
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
    let capture_cwd = capture_cwd.canonicalize().expect("canonical capture cwd");
    let worker_cwd = worker_cwd.canonicalize().expect("canonical worker cwd");

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
    let repository = PathBuf::from(std::env::var_os(TEST_REPOSITORY).expect("repository path"))
        .canonicalize()
        .expect("canonicalize relative-context repository");
    let capture_cwd = PathBuf::from(
        std::env::var_os("AETHYME_TEST_STORAGE_CONTEXT_CAPTURE_CWD").expect("capture cwd path"),
    );
    let worker_cwd = PathBuf::from(
        std::env::var_os("AETHYME_TEST_STORAGE_CONTEXT_WORKER_CWD").expect("worker cwd path"),
    );
    let repository_git = crate::GitRepo::discover(&repository).expect("discover relative fixture");
    let repository_fingerprint = crate::gates::git_origin_fingerprint(&repository_git);
    let relative_resource_db = capture_cwd.join("relative-state/host-resources.db");
    let _relative_lease = seed_quarantined_resource_lease(
        &relative_resource_db,
        &repository_fingerprint,
        "relative-context",
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
    assert_eq!(
        broker.storage_context().host_resource_db_path.as_deref(),
        Some(relative_resource_db.as_path())
    );
    assert!(relative_resource_db.is_file());

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
    let repository = PathBuf::from(std::env::var_os(TEST_REPOSITORY).expect("repository path"))
        .canonicalize()
        .expect("canonicalize relative-context repository");
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
    if let Ok("worker") = std::env::var(SLOT_IDENTITY_MODE).as_deref() {
        run_slot_identity_worker();
        return;
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
    assert!(!repo_b.join(".aethyme/run").exists());
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
        poisoned_state.join("host-resources.db").is_file(),
        "the explicit poison database is a seeded sentinel and must remain available for checks"
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
    let repository = PathBuf::from(std::env::var_os(TEST_REPOSITORY).expect("repository path"))
        .canonicalize()
        .expect("canonicalize absent-context repository");
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
    let poisoned_xdg_state = poisoned_state.join("xdg-state");
    let repository_git =
        crate::GitRepo::discover(&repository).expect("discover absent-context fixture");
    let repository_fingerprint = crate::gates::git_origin_fingerprint(&repository_git);
    let (poisoned_lease_id, poisoned_lease_generation) = seed_quarantined_resource_lease(
        &poisoned_state.join("host-resources.db"),
        &repository_fingerprint,
        "absent-context",
    );
    let poisoned_policy_digest = "b".repeat(64);
    crate::broker::gate_trust::record_trust_for_host_state(
        &repository,
        &poisoned_state,
        &[poisoned_policy_digest.as_str()],
        "fixture",
    )
    .expect("seed trust only in the poison host state");

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
        .env(POISONED_LEASE_ID, &poisoned_lease_id)
        .env(
            POISONED_LEASE_GENERATION,
            poisoned_lease_generation.to_string(),
        )
        .env(POISONED_POLICY_DIGEST, poisoned_policy_digest)
        .env("XDG_STATE_HOME", &poisoned_xdg_state)
        .env("XDG_CACHE_HOME", &poisoned_cache)
        .env("HOME", &poisoned_home);
    broker
        .configure_storage_context_for_child(&mut command)
        .expect("serialize captured absent context");
    command
        .env("AETHYME_HOST_STATE_DIR", &poisoned_state)
        .env("AETHYME_HOST_CACHE_DIR", &poisoned_cache)
        .env_remove(crate::broker::gate_trust::TEST_ESCAPE_ENV);
    let output = command.output().expect("run worker with changed XDG/HOME");
    assert!(
        output.status.success(),
        "worker failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_absent_context_worker() {
    let repository = PathBuf::from(std::env::var_os(TEST_REPOSITORY).expect("repository path"))
        .canonicalize()
        .expect("canonicalize absent-context repository");
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
    let mut broker = Broker::open_with_options(&repository, BrokerOpenOptions::default())
        .expect("open broker from captured child context");
    assert!(broker.storage_context().host_state_dir.is_none());
    assert!(broker.storage_context().host_resource_db_path.is_none());
    assert!(broker.storage_context().host_operation_db_path.is_none());
    assert!(broker.storage_context().host_cache_dir.is_none());
    let poisoned_lease_id =
        std::env::var(POISONED_LEASE_ID).expect("poisoned lease id from parent");
    let poisoned_lease_generation =
        std::env::var(POISONED_LEASE_GENERATION).expect("poisoned lease generation from parent");
    let report = broker.blockers();
    assert!(
        report
            .unavailable
            .iter()
            .any(|source| source.source == "host-resources.db"),
        "missing captured DB must be reported unavailable, got {report:#?}"
    );
    assert!(
        !report
            .blockers
            .iter()
            .any(|blocker| blocker.id == format!("resource:{poisoned_lease_id}")),
        "a lease in ambient state must not be reported: {report:#?}"
    );
    assert!(
        matches!(
            broker.unblock(&crate::UnblockRequest {
                id: format!("resource:{poisoned_lease_id}"),
                confirm: Some(poisoned_lease_generation.clone()),
                ..crate::UnblockRequest::default()
            }),
            Err(crate::BrokerOpError::InvalidStorageContext { .. })
        ),
        "unblock must refuse when the captured resource DB is absent"
    );
    let poisoned_policy_digest =
        std::env::var(POISONED_POLICY_DIGEST).expect("poisoned policy digest from parent");
    let trust_error = broker
        .require_trusted_policy(&context_probe_policy(&poisoned_policy_digest), None)
        .expect_err("ambient approval must not substitute for missing captured host state");
    assert!(
        matches!(
            trust_error,
            crate::BrokerOpError::InvalidStorageContext { .. }
        ),
        "unexpected trust result: {trust_error}"
    );
    assert_eq!(
        broker
            .recovery_archive_root()
            .expect("repo-local archive fallback"),
        repository.join(".aethyme/recovery-archives")
    );
    assert_eq!(
        resource_lease_state(
            &poisoned_state.join("host-resources.db"),
            &poisoned_lease_id
        ),
        crate::HostLeaseState::Quarantined,
        "failed absent-context unblock must leave the poison lease alone"
    );
    let poisoned_xdg_state = poisoned_state.join("xdg-state");
    assert!(
        !poisoned_xdg_state
            .join("aethyme/host-resources.db")
            .exists()
    );
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
    let repository = var_path("AETHYME_TEST_STORAGE_CONTEXT_REPOSITORY")
        .canonicalize()
        .expect("canonicalize captured-context repository");
    let private_state = var_path("AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_STATE");
    let private_cache = var_path("AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_CACHE");
    let private_preparation_root =
        var_path("AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_PREPARATION_ROOT");
    let private_effective_root = var_path("AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_EFFECTIVE_ROOT");
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
    let root_marker: crate::broker::WorktreeRootMarker = serde_json::from_slice(
        &std::fs::read(private_worktrees.join(WORKTREE_ROOT_MARKER))
            .expect("managed start published the private worktree marker"),
    )
    .expect("decode private worktree marker");
    assert_eq!(root_marker.schema_version, WORKTREE_ROOT_SCHEMA_VERSION);
    assert_eq!(root_marker.repository_root, repository);
    assert_eq!(
        root_marker.repository_key,
        crate::host_state::repository_key(&repository, Some(&git_common_dir))
    );

    let repository_fingerprint = crate::gates::git_origin_fingerprint(&repo_handle);
    let (private_lease_id, private_lease_generation) = seed_quarantined_resource_lease(
        &private_state.join("host-resources.db"),
        &repository_fingerprint,
        "captured-context",
    );
    let (poisoned_lease_id, poisoned_lease_generation) = seed_quarantined_resource_lease(
        &poisoned_state.join("host-resources.db"),
        &repository_fingerprint,
        "poisoned-context",
    );
    let private_policy_digest = "a".repeat(64);
    let poisoned_policy_digest = "b".repeat(64);
    crate::broker::gate_trust::record_trust_for_host_state(
        &repository,
        &private_state,
        &[private_policy_digest.as_str()],
        "fixture",
    )
    .expect("seed trust in the captured host state");
    crate::broker::gate_trust::record_trust_for_host_state(
        &repository,
        &poisoned_state,
        &[poisoned_policy_digest.as_str()],
        "fixture",
    )
    .expect("seed trust in the poison host state");

    let options = BrokerOpenOptions::default()
        .with_host_state_dir(private_state.clone())
        .with_host_cache_dir(private_cache.clone())
        .with_host_operation_database(private_state.join("host-operations.db"))
        .with_worktree_root(private_worktrees.clone())
        .with_preparation_cache_root(private_preparation_root.clone());
    let mut broker = Broker::open_with_options(&repository, options).expect("open broker");
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
        context
            .preparation_cache_root()
            .expect("captured preparation root"),
        Some(private_effective_root.clone())
    );
    assert_eq!(
        broker
            .worktree_root_plan()
            .expect("captured worktree plan")
            .preferred_root,
        Some(private_worktrees.clone())
    );
    assert!(private_state.join("host-resources.db").is_file());
    finish_captured_startup_recovery(&mut broker, &private_entry);
    assert!(!private_entry.exists());
    assert!(
        preparation_entry(&poisoned_state).join("payload").is_file(),
        "startup maintenance must not sweep poisoned host state"
    );
    std::fs::create_dir_all(&private_entry).expect("reseed captured preparation cache");
    std::fs::write(private_entry.join("payload"), b"worker cache data")
        .expect("seed worker cache entry");
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    publish_captured_cache_rotation(
        &private_effective_root,
        &private_state.join("host-resources.db"),
    );
    assert!(
        poisoned_state.join("host-resources.db").is_file(),
        "the poison resource database is a seeded sentinel"
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
        .env(
            "AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_PREPARATION_ROOT",
            &private_preparation_root,
        )
        .env(
            "AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_EFFECTIVE_ROOT",
            &private_effective_root,
        )
        .env("AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_ENTRY", &private_entry)
        .env("AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_WORKTREES", &private_worktrees)
        .env("AETHYME_TEST_STORAGE_CONTEXT_POISONED_STATE", &poisoned_state)
        .env("AETHYME_TEST_STORAGE_CONTEXT_POISONED_CACHE", &poisoned_cache)
        .env("AETHYME_TEST_STORAGE_CONTEXT_POISONED_XDG_STATE", &poisoned_xdg_state)
        .env("AETHYME_TEST_STORAGE_CONTEXT_POISONED_XDG_CACHE", &poisoned_xdg_cache)
        .env("AETHYME_TEST_STORAGE_CONTEXT_POISONED_HOME", &poisoned_home)
        .env(PRIVATE_LEASE_ID, &private_lease_id)
        .env(
            PRIVATE_LEASE_GENERATION,
            private_lease_generation.to_string(),
        )
        .env(POISONED_LEASE_ID, &poisoned_lease_id)
        .env(
            POISONED_LEASE_GENERATION,
            poisoned_lease_generation.to_string(),
        )
        .env(PRIVATE_POLICY_DIGEST, &private_policy_digest)
        .env(POISONED_POLICY_DIGEST, &poisoned_policy_digest)
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
        .env("AETHYME_HOST_CACHE_DIR", &poisoned_cache)
        .env(
            "AETHYME_PREPARATION_CACHE_ROOT",
            poisoned_state.join("preparation-cache"),
        )
        .env_remove(crate::broker::gate_trust::TEST_ESCAPE_ENV);
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
    let private_effective_root = var_path("AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_EFFECTIVE_ROOT");
    let private_entry = var_path("AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_ENTRY");
    let private_worktrees = var_path("AETHYME_TEST_STORAGE_CONTEXT_PRIVATE_WORKTREES");
    let poisoned_state = var_path("AETHYME_TEST_STORAGE_CONTEXT_POISONED_STATE");
    let poisoned_cache = var_path("AETHYME_TEST_STORAGE_CONTEXT_POISONED_CACHE");
    let poisoned_xdg_state = var_path("AETHYME_TEST_STORAGE_CONTEXT_POISONED_XDG_STATE");
    let poisoned_xdg_cache = var_path("AETHYME_TEST_STORAGE_CONTEXT_POISONED_XDG_CACHE");
    let poisoned_home = var_path("AETHYME_TEST_STORAGE_CONTEXT_POISONED_HOME");

    let mut broker = Broker::open_with_options(&repository, BrokerOpenOptions::default())
        .expect("open broker from captured child context");
    assert_eq!(
        broker.storage_context().host_state_dir.as_deref(),
        Some(private_state.as_path())
    );
    assert_eq!(
        broker.storage_context().host_cache_dir.as_deref(),
        Some(private_cache.as_path())
    );
    assert_eq!(
        broker
            .storage_context()
            .preparation_cache_root()
            .expect("worker captured preparation root"),
        Some(private_effective_root.clone())
    );
    finish_captured_startup_recovery(&mut broker, &private_entry);
    assert!(
        !private_entry.exists(),
        "worker must sweep only its captured cache root"
    );
    assert!(
        preparation_entry(&poisoned_state).join("payload").is_file(),
        "worker must not sweep the poisoned public cache-root override"
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
    let private_lease_id = std::env::var(PRIVATE_LEASE_ID).expect("private lease id from parent");
    let private_lease_generation =
        std::env::var(PRIVATE_LEASE_GENERATION).expect("private lease generation from parent");
    let poisoned_lease_id =
        std::env::var(POISONED_LEASE_ID).expect("poisoned lease id from parent");
    let private_state_db = private_state.join("host-resources.db");
    let report = broker.blockers();
    assert!(
        report
            .blockers
            .iter()
            .any(|blocker| blocker.id == format!("resource:{private_lease_id}")),
        "the blocker reader must use the captured DB: {report:#?}"
    );
    assert!(
        !report
            .blockers
            .iter()
            .any(|blocker| blocker.id == format!("resource:{poisoned_lease_id}")),
        "the blocker reader must ignore the poison DB: {report:#?}"
    );
    let private_policy_digest =
        std::env::var(PRIVATE_POLICY_DIGEST).expect("private policy digest from parent");
    let poisoned_policy_digest =
        std::env::var(POISONED_POLICY_DIGEST).expect("poisoned policy digest from parent");
    broker
        .require_trusted_policy(&context_probe_policy(&private_policy_digest), None)
        .expect("trust recorded in captured host state is accepted");
    let poison_trust_error = broker
        .require_trusted_policy(&context_probe_policy(&poisoned_policy_digest), None)
        .expect_err("trust in the poison host state must not be accepted");
    assert!(
        matches!(
            poison_trust_error,
            crate::BrokerOpError::GatePolicyUntrusted { .. }
        ),
        "unexpected trust result: {poison_trust_error}"
    );
    let repository_key = broker
        .worktree_root_plan()
        .expect("captured repository identity")
        .repository_key;
    assert_eq!(
        broker
            .recovery_archive_root()
            .expect("captured archive root"),
        private_state.join("recovery-archives").join(repository_key)
    );
    assert!(matches!(
        broker.unblock(&crate::UnblockRequest {
            id: format!("resource:{private_lease_id}"),
            confirm: Some(private_lease_generation.clone()),
            ..crate::UnblockRequest::default()
        }),
        Ok(crate::UnblockOutcome::Cleared(_))
    ));
    assert_eq!(
        resource_lease_state(&private_state_db, &private_lease_id),
        crate::HostLeaseState::Released
    );
    assert_eq!(
        resource_lease_state(
            &poisoned_state.join("host-resources.db"),
            &poisoned_lease_id
        ),
        crate::HostLeaseState::Quarantined
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn finish_captured_startup_recovery(broker: &mut Broker, entry: &Path) {
    use crate::preparation::rotation_journal::JournalProbe;

    let context = broker.storage_context();
    let resource_db = context
        .host_resource_db_path
        .as_deref()
        .expect("fixture captures its host resource database")
        .to_path_buf();
    let cache_root = context
        .preparation_cache_root()
        .expect("captured preparation root")
        .expect("fixture captures its preparation root");
    let root_identity = crate::preparation::rotation_debt::DirectoryIdentity::capture(&cache_root)
        .expect("capture fixture preparation root identity")
        .encode()
        .expect("encode fixture preparation root identity");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let debt = crate::preparation::rotation_journal::probe(
            &resource_db,
            &cache_root,
            Some(&root_identity),
        )
        .expect("inspect captured rotation journal");
        // Recovery removes old content before settling its durable row. The
        // path alone must not authorize publishing the next synthetic debt.
        if debt == Some(JournalProbe::NoDebt) && !entry.exists() {
            break;
        }
        assert!(
            matches!(debt, Some(JournalProbe::Pending | JournalProbe::NoDebt)),
            "fixture debt became unknown or its published journal disappeared: {debt:?}"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "captured recovery did not settle before reseeding: {debt:?}"
        );
        if entry.exists() {
            assert!(
                broker.artifact_sweep_recovery_requested().unwrap(),
                "bounded startup must retain the persisted recovery request"
            );
        }
        // Continue through the real captured-context path and finite policy
        // budget; a detached startup worker may still own the recovery lease.
        let report = broker
            .gc_sweep_artifacts()
            .expect("resume captured cache recovery");
        assert!(report.budget_ms > 0);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn finish_captured_startup_recovery(broker: &mut Broker, entry: &Path) {
    if entry.exists() {
        assert!(
            broker.artifact_sweep_recovery_requested().unwrap(),
            "bounded startup must retain the persisted recovery request"
        );
        // Use the real continuation and its existing finite policy budget.
        // Do not increase the production inline cap or fall back to ambient
        // host paths merely to satisfy a timing-sensitive fixture.
        let report = broker
            .gc_sweep_artifacts()
            .expect("resume captured cache recovery");
        assert!(report.budget_ms > 0);
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn publish_captured_cache_rotation(cache_root: &Path, resource_db: &Path) {
    let repository = "a".repeat(64);
    let store = cache_root.join(&repository);
    let root_identity =
        crate::preparation::rotation_debt::DirectoryIdentity::capture(cache_root).unwrap();
    let debt = crate::preparation::rotation_debt::RotationDebt::new(
        &repository,
        &crate::preparation::rotation_debt::new_transaction_id(),
        root_identity.clone(),
        crate::preparation::rotation_debt::DirectoryIdentity::capture(&store).unwrap(),
    )
    .unwrap();
    crate::preparation::rotation_journal::publish(
        resource_db,
        cache_root,
        &root_identity.encode().unwrap(),
        &repository,
        &crate::preparation::rotation_debt::encode(&debt).unwrap(),
    )
    .unwrap();
}

fn preparation_entry(host_state: &Path) -> PathBuf {
    preparation_entry_under(&host_state.join("preparation-cache"))
}

fn preparation_entry_under(cache_root: &Path) -> PathBuf {
    cache_root
        .join("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .join("bbbbbbbbbbbb")
}

fn context_probe_policy(digest: &str) -> crate::broker::gate_trust::GatePolicy {
    crate::broker::gate_trust::GatePolicy {
        policy_sha256: digest.to_string(),
        commands: vec![crate::broker::gate_trust::PolicyCommand {
            source: "gate",
            name: "captured-context-probe".into(),
            command: "true".into(),
        }],
    }
}

fn seed_quarantined_resource_lease(
    database: &Path,
    repository: &str,
    suffix: &str,
) -> (String, u64) {
    let mut coordinator =
        crate::HostResourceCoordinator::open(database).expect("open sentinel resource database");
    let grant = coordinator
        .acquire(&crate::HostResourceRequest {
            schema_version: crate::HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
            request_id: format!("storage-context-{suffix}"),
            repository: repository.into(),
            worktree_fingerprint: format!("storage-context-worktree-{suffix}"),
            run_id: format!("storage-context-run-{suffix}"),
            ttl_seconds: 300,
            holder_pid: None,
            resources: vec![crate::HostResourceRequirement {
                key: format!("storage-context-{suffix}"),
                resource: crate::HostResourceKind::ExclusiveKey {
                    name: format!("storage-context-{suffix}"),
                },
            }],
        })
        .expect("acquire sentinel resource lease");
    let lease = coordinator
        .quarantine(
            &grant.lease.lease_id,
            grant.lease.generation,
            &grant.ownership_token,
        )
        .expect("quarantine sentinel resource lease");
    (lease.lease_id, lease.generation)
}

fn resource_lease_state(database: &Path, lease_id: &str) -> crate::HostLeaseState {
    crate::HostResourceCoordinator::open(database)
        .expect("open sentinel resource database")
        .list(true)
        .expect("list sentinel resource database")
        .into_iter()
        .find(|lease| lease.lease_id == lease_id)
        .expect("sentinel lease is present")
        .state
}
