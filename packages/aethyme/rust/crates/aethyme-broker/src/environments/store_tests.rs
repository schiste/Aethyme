use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::compile::{CompileContext, CompiledEnvironment, EnvironmentOwner, compile};
use super::identity::{Observation, ProcessIdentity};
use super::logs::{log_path, open_for_append, remove_environment_directory, tail};
use super::spec::{EnvironmentsFile, HostEnvironmentsFile};
use super::store::{EnvironmentPhase, EnvironmentStore, StoreError, valid_env_id};
use super::supervisor::{
    self, ExitReason, StoreWatch, SupervisorConfig, SupervisorWork, TickOutcome,
};
use super::{Loaded, parse_loaded};
use crate::managed_services::ManagedServicePhase;

const FILE: &str = r#"
schema = 1
[pools.envs]
limit = 2

[[profiles]]
name = "backend"
units = 1

[[profiles.components]]
name = "db"
runtime = "hooks"
provision = "true"
destroy = "true"

[[profiles.components]]
name = "api"
runtime = "process"
depends_on = ["db"]
start = "sleep 1"
"#;

fn snapshot(owner: &str) -> CompiledEnvironment {
    let loaded: Loaded<EnvironmentsFile> = parse_loaded(PathBuf::from("t"), "t", FILE);
    let file = loaded.value.unwrap();
    compile(
        &file,
        &HostEnvironmentsFile::default(),
        "backend",
        "env.toml",
        &CompileContext {
            repository: "repo-0123456789abcdef".into(),
            owner: EnvironmentOwner::Worktree {
                fingerprint: owner.into(),
            },
            worktree_fingerprint: owner.into(),
            source_commit: None,
            source_dirty: true,
            source_digest: "d".repeat(64),
        },
    )
    .unwrap()
}

fn store() -> (tempfile::TempDir, EnvironmentStore) {
    let directory = tempfile::tempdir().unwrap();
    let store = EnvironmentStore::open(&directory.path().join("environments.db")).unwrap();
    (directory, store)
}

#[test]
fn registering_twice_returns_the_same_environment() {
    let (_dir, mut store) = store();
    let first = store.register(&snapshot("a"), None, Some(7)).unwrap();
    let again = store.register(&snapshot("a"), None, Some(7)).unwrap();
    assert_eq!(first, again);
    assert_eq!(first.phase, EnvironmentPhase::Planned);
    assert_eq!(first.phase_generation, 0);
    assert!(valid_env_id(&first.env_id), "{}", first.env_id);
    let components = store.components(&first.env_id).unwrap();
    let names: Vec<&str> = components.iter().map(|c| c.component.as_str()).collect();
    assert_eq!(
        names,
        ["db", "api"],
        "components are reported in start order"
    );
    assert!(
        components
            .iter()
            .all(|c| c.phase == ManagedServicePhase::Intent)
    );
    let other = store.register(&snapshot("b"), None, None).unwrap();
    assert_ne!(first.env_id, other.env_id);
}

#[test]
fn phase_changes_are_compare_and_swap_and_recorded() {
    let (_dir, mut store) = store();
    let record = store.register(&snapshot("a"), None, None).unwrap();
    let id = record.env_id.clone();
    let provisioning = store
        .transition(&id, 0, EnvironmentPhase::Provisioning, "up", "test", None)
        .unwrap();
    assert_eq!(provisioning.phase_generation, 1);
    match store.transition(&id, 0, EnvironmentPhase::Starting, "stale", "test", None) {
        Err(StoreError::Conflict {
            expected: 0,
            actual: 1,
            ..
        }) => {}
        other => panic!("a stale generation must conflict: {other:?}"),
    }
    match store.transition(&id, 1, EnvironmentPhase::Parked, "skip", "test", None) {
        Ok(_) => {} // provisioning -> parked is legal (provision without start)
        Err(error) => panic!("{error}"),
    }
    match store.transition(&id, 2, EnvironmentPhase::Active, "no", "test", None) {
        Err(StoreError::IllegalTransition {
            from: "parked",
            to: "active",
            ..
        }) => {}
        other => panic!("parked -> active must go through waking: {other:?}"),
    }
    let parked = store.get(&id).unwrap().unwrap();
    assert!(parked.parked_at.is_some());
    let waking = store
        .transition(&id, 2, EnvironmentPhase::Waking, "wake", "test", None)
        .unwrap();
    assert_eq!(
        waking.parked_at, None,
        "parked_at describes the current park only"
    );
    let history = store.transitions(&id, 100).unwrap();
    let path: Vec<(Option<&str>, &str)> = history
        .iter()
        .map(|t| (t.from.map(|p| p.as_str()), t.to.as_str()))
        .collect();
    assert_eq!(
        path,
        [
            (None, "planned"),
            (Some("planned"), "provisioning"),
            (Some("provisioning"), "parked"),
            (Some("parked"), "waking"),
        ]
    );
}

#[test]
fn a_destroyed_environment_is_replanned_under_the_same_id() {
    let (_dir, mut store) = store();
    let record = store.register(&snapshot("a"), None, None).unwrap();
    let id = record.env_id.clone();
    store
        .transition(
            &id,
            0,
            EnvironmentPhase::Destroyed,
            "never started",
            "test",
            None,
        )
        .unwrap();
    store.set_namespace(&id, "ns-1").unwrap();
    let again = store.register(&snapshot("a"), None, None).unwrap();
    assert_eq!(again.env_id, id);
    assert_eq!(again.phase, EnvironmentPhase::Planned);
    assert_eq!(again.phase_generation, 2);
    assert_eq!(
        again.namespace, None,
        "a re-created environment gets a fresh reservation"
    );
}

#[test]
fn only_planned_and_destroyed_release_the_worktree() {
    use EnvironmentPhase::*;
    for phase in [
        Provisioning,
        Starting,
        Active,
        Stopping,
        Parked,
        Waking,
        Destroying,
        Failed,
        DestroyFailed,
        Quarantined,
    ] {
        assert!(phase.retains_worktree(), "{phase:?}");
    }
    assert!(!Planned.retains_worktree());
    assert!(!Destroyed.retains_worktree());
    assert!(
        !Destroyed.may_follow(DestroyFailed),
        "a failed destroy is retried, never skipped"
    );
    assert!(!Destroyed.may_follow(Active), "destroy is always explicit");
}

#[test]
fn namespaces_are_unique_across_environments() {
    let (_dir, mut store) = store();
    let a = store.register(&snapshot("a"), None, None).unwrap();
    let b = store.register(&snapshot("b"), None, None).unwrap();
    store.set_namespace(&a.env_id, "env-x-1").unwrap();
    match store.set_namespace(&b.env_id, "env-x-1") {
        Err(StoreError::NamespaceTaken(ns)) => assert_eq!(ns, "env-x-1"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn component_phases_are_write_ahead_and_a_new_instance_counts_attempts() {
    let (_dir, mut store) = store();
    let id = store.register(&snapshot("a"), None, None).unwrap().env_id;
    use ManagedServicePhase::*;
    for phase in [AdmissionPending, Admitted, CreatePending] {
        store.set_component(&id, "api", phase, None, None).unwrap();
    }
    match store.set_component(&id, "api", Settled, None, None) {
        Err(StoreError::IllegalComponentTransition {
            from: "create_pending",
            ..
        }) => {}
        other => panic!("an unknown create is never assumed absent: {other:?}"),
    }
    let identity = serde_json::json!({"pid": 1, "pgid": 1, "start_time": 1});
    let created = store
        .set_component(&id, "api", Created, Some(&identity), None)
        .unwrap();
    assert_eq!(created.identity, Some(identity));
    for phase in [StartPending, Running, RemovalPending, Removed, Settled] {
        store.set_component(&id, "api", phase, None, None).unwrap();
    }
    let again = store.set_component(&id, "api", Intent, None, None).unwrap();
    assert_eq!(again.attempt, 1, "settled -> intent starts a new instance");
    assert_eq!(
        again.identity, None,
        "a new instance forgets the old identity"
    );
}

#[test]
fn a_newer_store_schema_is_refused_and_a_missing_store_is_not_created() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("environments.db");
    assert!(EnvironmentStore::open_read_only(&path).unwrap().is_none());
    assert!(!path.exists(), "reading never creates the database");
    drop(EnvironmentStore::open(&path).unwrap());
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute(
        "UPDATE meta SET value = '99' WHERE key = 'schema_version'",
        [],
    )
    .unwrap();
    drop(conn);
    match EnvironmentStore::open(&path) {
        Err(StoreError::SchemaTooNew {
            found: 99,
            supported: 1,
        }) => {}
        other => panic!("{:?}", other.err()),
    }
}

#[test]
fn identity_proves_only_the_process_it_captured() {
    let own = ProcessIdentity::capture(std::process::id() as i32).expect("own identity");
    assert_eq!(own.observe(), Observation::Alive);
    let recycled = ProcessIdentity {
        start_time: own.start_time.wrapping_add(1),
        ..own
    };
    assert_eq!(
        recycled.observe(),
        Observation::Unproven,
        "a live pid with another start time is never ours"
    );
    // Components run in their own process group; a child left in the test
    // runner's group would (correctly) stay Unproven after exiting, because
    // its group is still alive.
    let mut child = {
        use std::os::unix::process::CommandExt as _;
        std::process::Command::new("/bin/sleep")
            .arg("0.2")
            .process_group(0)
            .spawn()
            .unwrap()
    };
    let pid = child.id() as i32;
    let captured = ProcessIdentity::capture(pid);
    child.wait().unwrap();
    if let Some(captured) = captured {
        assert_eq!(captured.observe(), Observation::Gone);
    }
}

#[test]
fn logs_rotate_at_the_cap_and_tail_spans_the_rotation() {
    let directory = tempfile::tempdir().unwrap();
    let path = log_path(directory.path(), "env-0123456789ab", "api").unwrap();
    {
        use std::io::Write;
        let mut log = open_for_append(&path, 64).unwrap();
        for n in 0..20 {
            writeln!(log, "old {n}").unwrap();
        }
    }
    {
        use std::io::Write;
        let mut log = open_for_append(&path, 64).unwrap();
        writeln!(log, "new 0").unwrap();
    }
    let mut rotated = path.as_os_str().to_owned();
    rotated.push(".1");
    assert!(PathBuf::from(rotated).exists(), "over the cap rotates");
    assert_eq!(tail(&path, 2).unwrap(), ["old 19", "new 0"]);
    assert!(log_path(directory.path(), "../etc", "api").is_none());
    assert!(log_path(directory.path(), "env-0123456789ab", "../x").is_none());
    remove_environment_directory(directory.path(), "env-0123456789ab").unwrap();
    assert!(!path.exists());
}

struct Script(Vec<bool>);

impl SupervisorWork for Script {
    fn tick(&mut self) -> TickOutcome {
        let needs = if self.0.is_empty() {
            false
        } else {
            self.0.remove(0)
        };
        TickOutcome {
            needs_supervisor: needs,
        }
    }
}

fn quick(root: PathBuf, linger_ms: u64) -> SupervisorConfig {
    SupervisorConfig {
        root,
        linger: Duration::from_millis(linger_ms),
        tick: Duration::from_millis(10),
    }
}

#[test]
fn the_supervisor_lingers_out_and_withdraws_its_record() {
    let directory = tempfile::tempdir().unwrap();
    let config = quick(directory.path().to_path_buf(), 50);
    let reason = supervisor::run(&config, &mut Script(vec![true, true, true]), &|| false).unwrap();
    assert_eq!(reason, ExitReason::Linger);
    assert_eq!(supervisor::status(directory.path()).state, "absent");
}

#[test]
fn a_second_supervisor_exits_while_one_runs() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_path_buf();
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let first = {
        let root = root.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            let config = quick(root, 60_000);
            supervisor::run(&config, &mut Script(Vec::new()), &|| {
                stop.load(Ordering::SeqCst)
            })
            .unwrap()
        })
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !supervisor::status(&root).running() {
        assert!(
            std::time::Instant::now() < deadline,
            "first supervisor never published"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let status = supervisor::status(&root);
    assert_eq!(status.pid, Some(std::process::id() as i32));
    let second =
        supervisor::run(&quick(root.clone(), 10), &mut Script(Vec::new()), &|| false).unwrap();
    assert_eq!(second, ExitReason::AlreadyRunning);
    assert!(
        supervisor::status(&root).running(),
        "the loser never withdraws the winner's record"
    );
    stop.store(true, Ordering::SeqCst);
    assert_eq!(first.join().unwrap(), ExitReason::Stopped);
    assert_eq!(supervisor::status(&root).state, "absent");
}

#[test]
fn store_watch_keeps_the_supervisor_while_an_environment_needs_it() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("environments.db");
    let mut watch = StoreWatch {
        store_path: path.clone(),
    };
    assert!(
        !watch.tick().needs_supervisor,
        "no store, nothing to supervise"
    );
    let mut store = EnvironmentStore::open(&path).unwrap();
    let id = store.register(&snapshot("a"), None, None).unwrap().env_id;
    assert!(!watch.tick().needs_supervisor, "planned needs nothing yet");
    store
        .transition(&id, 0, EnvironmentPhase::Provisioning, "up", "test", None)
        .unwrap();
    assert!(watch.tick().needs_supervisor);
}
