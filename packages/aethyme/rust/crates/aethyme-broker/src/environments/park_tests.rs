//! Park and wake (#1212): capacity moves, reservations stay, and neither
//! direction leaves an environment half-up.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::compile::{CompileContext, CompiledEnvironment, EnvironmentOwner, compile};
use super::drivers::ComponentIdentity;
use super::identity::Observation;
use super::lifecycle::{Lifecycle, LifecycleError, UpOptions};
use super::spec::{EnvironmentsFile, HostEnvironmentsFile};
use super::store::{EnvironmentPhase, EnvironmentStore};
use super::{Loaded, parse_loaded};
use crate::resources::{HostLeaseState, HostResourceCoordinator};

struct Host {
    _dir: tempfile::TempDir,
    store: PathBuf,
    db: PathBuf,
    root: PathBuf,
}

struct ProcessGroupCleanup {
    pgid: i32,
    armed: bool,
}

impl ProcessGroupCleanup {
    fn new(pgid: i32) -> Self {
        Self { pgid, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ProcessGroupCleanup {
    fn drop(&mut self) {
        if self.armed {
            // SAFETY: the fixture records this process group from its own
            // component's background child, not from an unrelated process.
            unsafe {
                libc::killpg(self.pgid, libc::SIGKILL);
            }
        }
    }
}

impl Host {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        Self {
            store: dir.path().join("environments.db"),
            db: dir.path().join("host.db"),
            root: dir.path().join("environments"),
            _dir: dir,
        }
    }

    fn lifecycle(&self) -> Lifecycle {
        Lifecycle {
            store: EnvironmentStore::open(&self.store).unwrap(),
            coordinator: HostResourceCoordinator::open(&self.db).unwrap(),
            root: self.root.clone(),
            holder_pid: None,
            hook_timeout: Duration::from_secs(20),
            actor: "test".into(),
        }
    }

    /// Active capacity leases across all environments: the pool's real use.
    fn capacity_leases(&self) -> usize {
        HostResourceCoordinator::open(&self.db)
            .unwrap()
            .list(false)
            .unwrap()
            .iter()
            .filter(|l| {
                l.state == HostLeaseState::Active
                    && l.allocations.iter().any(|a| a.kind == "capacity")
            })
            .count()
    }

    fn leases_of(&self, env_id: &str) -> usize {
        HostResourceCoordinator::open(&self.db)
            .unwrap()
            .list(false)
            .unwrap()
            .iter()
            .filter(|l| l.run_id == env_id && l.state == HostLeaseState::Active)
            .count()
    }
}

fn snapshot(text: &str, worktree: &Path) -> CompiledEnvironment {
    let loaded: Loaded<EnvironmentsFile> = parse_loaded(PathBuf::from("t"), "t", text);
    let file = loaded.value.unwrap_or_else(|d| panic!("{}", d.render()));
    let found = super::validate(&file, &HostEnvironmentsFile::default(), "t", "h");
    assert!(found.is_empty(), "{found:#?}");
    let fingerprint = crate::console::worktree_fingerprint(worktree);
    compile(
        &file,
        &HostEnvironmentsFile::default(),
        &file.profiles[0].name,
        "t",
        &CompileContext {
            repository: "repo-park".into(),
            owner: EnvironmentOwner::Worktree {
                fingerprint: fingerprint.clone(),
            },
            worktree_fingerprint: fingerprint,
            source_commit: None,
            source_dirty: false,
            source_digest: String::new(),
        },
    )
    .unwrap()
}

fn up_options() -> UpOptions {
    UpOptions {
        wait: Duration::from_millis(500),
        reconfigure: false,
    }
}

// Tests have separate host databases, so the coordinator cannot keep
// parallel tests off each other's ports; each test file uses its own range.
const ONE_UNIT: &str = r#"
schema = 1
[ports]
start = 43300
end = 43399
[pools.envs]
limit = 2
[[profiles]]
name = "p"
units = 1
[[profiles.components]]
name = "api"
runtime = "process"
listen = true
start = "exec sleep 300"
health = { command = "true", timeout_seconds = 5, interval_ms = 50 }
"#;

fn process_identity(lifecycle: &Lifecycle, env_id: &str) -> Option<ComponentIdentity> {
    lifecycle
        .store
        .components(env_id)
        .unwrap()
        .into_iter()
        .find(|c| c.component == "api")
        .and_then(|c| c.identity)
        .and_then(|v| serde_json::from_value(v).ok())
}

/// #1212 acceptance: limit 2, three 1-unit environments. Two run, the
/// third waits for capacity; parking one lets it run; waking the parked one
/// without capacity leaves it parked with nothing started.
#[test]
fn parking_returns_units_so_a_third_environment_fits() {
    let host = Host::new();
    let (a_dir, b_dir, c_dir) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let (a, b, c) = (
        snapshot(ONE_UNIT, a_dir.path()),
        snapshot(ONE_UNIT, b_dir.path()),
        snapshot(ONE_UNIT, c_dir.path()),
    );
    let mut lifecycle = host.lifecycle();
    let a_up = lifecycle
        .up(&a, a_dir.path(), None, None, &up_options())
        .unwrap();
    lifecycle
        .up(&b, b_dir.path(), None, None, &up_options())
        .unwrap();
    assert_eq!(host.capacity_leases(), 2);
    match lifecycle.up(&c, c_dir.path(), None, None, &up_options()) {
        Err(error @ LifecycleError::Resources(_)) => assert_eq!(error.code(), "capacity_timeout"),
        other => panic!("the pool is full: {other:?}"),
    }
    assert_eq!(host.capacity_leases(), 2);

    let a_identity = process_identity(&lifecycle, &a.env_id).unwrap();
    let parked = lifecycle.park(&a.env_id, "test").unwrap();
    assert_eq!(parked.phase, EnvironmentPhase::Parked);
    assert!(parked.parked_at.is_some());
    if let ComponentIdentity::Process(process) = &a_identity {
        assert_eq!(process.observe(), Observation::Gone, "park proves the stop");
    }
    assert_eq!(host.capacity_leases(), 1, "parking returned the unit");
    assert_eq!(host.leases_of(&a.env_id), 1, "the reservation is kept");

    lifecycle
        .up(&c, c_dir.path(), None, None, &up_options())
        .unwrap();
    assert_eq!(host.capacity_leases(), 2);

    match lifecycle.wake(&a.env_id, Duration::from_millis(500)) {
        Err(error) => assert_eq!(error.code(), "capacity_timeout"),
        Ok(record) => panic!("woke without capacity: {:?}", record.phase),
    }
    let still = lifecycle.store.get(&a.env_id).unwrap().unwrap();
    assert_eq!(
        still.phase,
        EnvironmentPhase::Parked,
        "never active without capacity"
    );
    assert_eq!(still.last_error.unwrap()["code"], "capacity_timeout");
    assert_eq!(host.capacity_leases(), 2);

    lifecycle.park(&c.env_id, "test").unwrap();
    let woken = lifecycle.wake(&a.env_id, Duration::from_secs(2)).unwrap();
    assert_eq!(woken.phase, EnvironmentPhase::Active);
    assert_eq!(
        woken.namespace, a_up.namespace,
        "the namespace survives park and wake"
    );
    assert_eq!(
        woken.reservation_lease_id, a_up.reservation_lease_id,
        "same reservation, so the same ports"
    );
    assert_ne!(
        process_identity(&lifecycle, &a.env_id),
        Some(a_identity),
        "a new instance"
    );
    for id in [&a.env_id, &b.env_id, &c.env_id] {
        lifecycle.down(id, None).unwrap();
    }
    assert_eq!(host.capacity_leases(), 0);
}

#[test]
fn a_failed_wake_rolls_back_to_parked_with_nothing_running() {
    let host = Host::new();
    let flags = tempfile::tempdir().unwrap();
    let healthy = flags.path().join("healthy");
    std::fs::write(&healthy, "").unwrap();
    let text = format!(
        r#"
schema = 1
[pools.envs]
limit = 1
[[profiles]]
name = "p"
units = 1
[[profiles.components]]
name = "api"
runtime = "process"
start = "exec sleep 300"
health = {{ command = "true", timeout_seconds = 5, interval_ms = 50 }}
[[profiles.components]]
name = "worker"
runtime = "process"
depends_on = ["api"]
start = "exec sleep 300"
health = {{ command = 'test -f "{flag}"', timeout_seconds = 1, interval_ms = 50 }}
"#,
        flag = healthy.display()
    );
    let worktree = tempfile::tempdir().unwrap();
    let snapshot = snapshot(&text, worktree.path());
    let mut lifecycle = host.lifecycle();
    lifecycle
        .up(&snapshot, worktree.path(), None, None, &up_options())
        .unwrap();
    lifecycle.park(&snapshot.env_id, "test").unwrap();
    std::fs::remove_file(&healthy).unwrap();

    match lifecycle.wake(&snapshot.env_id, Duration::from_secs(2)) {
        Err(LifecycleError::Driver(error)) => assert_eq!(error.code(), "unhealthy"),
        other => panic!("{other:?}"),
    }
    let record = lifecycle.store.get(&snapshot.env_id).unwrap().unwrap();
    assert_eq!(record.phase, EnvironmentPhase::Parked);
    assert_eq!(record.last_error.unwrap()["component"], "worker");
    assert_eq!(
        host.capacity_leases(),
        0,
        "capacity released by the rollback"
    );
    if let Some(ComponentIdentity::Process(api)) = process_identity(&lifecycle, &snapshot.env_id) {
        assert_eq!(
            api.observe(),
            Observation::Gone,
            "the component that started was stopped"
        );
    }

    std::fs::write(&healthy, "").unwrap();
    let woken = lifecycle
        .wake(&snapshot.env_id, Duration::from_secs(2))
        .unwrap();
    assert_eq!(
        woken.phase,
        EnvironmentPhase::Active,
        "a later wake succeeds"
    );
    lifecycle.down(&snapshot.env_id, None).unwrap();
}

#[test]
fn a_failed_wake_stop_quarantines_identity_and_capacity_for_down_retry() {
    let host = Host::new();
    let flags = tempfile::tempdir().unwrap();
    let orphan_on_wake = flags.path().join("orphan-on-wake");
    let text = format!(
        r#"
schema = 1
[pools.envs]
limit = 1
[[profiles]]
name = "p"
units = 1
[[profiles.components]]
name = "api"
runtime = "process"
env = {{ MARKERS = "{m}" }}
start = 'if test -f "$MARKERS/orphan-on-wake"; then sleep 300 & echo "$$" > "$MARKERS/leader.pid"; echo "$!" > "$MARKERS/child.pid"; sleep 0.2; else exec sleep 300; fi'
health = {{ command = 'test ! -f "$MARKERS/orphan-on-wake" || (test -s "$MARKERS/leader.pid" && test -s "$MARKERS/child.pid")', timeout_seconds = 2, interval_ms = 50 }}
[[profiles.components]]
name = "worker"
runtime = "process"
depends_on = ["api"]
env = {{ MARKERS = "{m}" }}
start = 'if test -f "$MARKERS/orphan-on-wake"; then leader=$(cat "$MARKERS/leader.pid"); while kill -0 "$leader" 2>/dev/null; do sleep 0.01; done; exit 9; else exec sleep 300; fi'
health = {{ command = 'test ! -f "$MARKERS/orphan-on-wake" || false', timeout_seconds = 2, interval_ms = 50 }}
"#,
        m = flags.path().display()
    );
    let worktree = tempfile::tempdir().unwrap();
    let snapshot = snapshot(&text, worktree.path());
    let mut lifecycle = host.lifecycle();
    let active = lifecycle
        .up(&snapshot, worktree.path(), None, None, &up_options())
        .unwrap();
    lifecycle.park(&active.env_id, "test").unwrap();
    std::fs::write(&orphan_on_wake, "").unwrap();

    match lifecycle.wake(&active.env_id, Duration::from_secs(2)) {
        Err(LifecycleError::Driver(error)) => assert_eq!(error.code(), "start_exited"),
        other => panic!("the worker start should fail after the api leader exits: {other:?}"),
    }
    let child_pid: i32 = std::fs::read_to_string(flags.path().join("child.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // SAFETY: the PID came from the background child started by this fixture.
    let process_group = unsafe { libc::getpgid(child_pid) };
    assert!(process_group > 0, "fixture child has no process group");
    let mut cleanup = ProcessGroupCleanup::new(process_group);

    let quarantined = lifecycle.store.get(&active.env_id).unwrap().unwrap();
    assert_eq!(quarantined.phase, EnvironmentPhase::Quarantined);
    assert!(quarantined.capacity_lease_id.is_some());
    assert_eq!(host.capacity_leases(), 1, "rollback keeps capacity leased");
    assert_eq!(
        quarantined.last_error.as_ref().unwrap()["component"],
        "worker"
    );
    assert_eq!(
        quarantined.last_error.as_ref().unwrap()["rollback_errors"][0]["component"],
        "api"
    );
    assert_eq!(
        quarantined.last_error.as_ref().unwrap()["rollback_errors"][0]["code"],
        "unproven"
    );

    let api = lifecycle
        .store
        .components(&active.env_id)
        .unwrap()
        .into_iter()
        .find(|component| component.component == "api")
        .unwrap();
    assert_eq!(
        api.phase,
        crate::managed_services::ManagedServicePhase::Running
    );
    let saved_identity = api.identity.clone().expect("running identity retained");
    let identity: ComponentIdentity = serde_json::from_value(saved_identity.clone()).unwrap();
    let ComponentIdentity::Process(process) = identity else {
        panic!("api should have a process identity");
    };
    assert_eq!(process.pgid, process_group);
    assert_eq!(process.observe(), Observation::Unproven);

    assert!(lifecycle.down(&active.env_id, None).is_err());
    let failed_down = lifecycle.store.get(&active.env_id).unwrap().unwrap();
    assert_eq!(failed_down.phase, EnvironmentPhase::DestroyFailed);
    assert_eq!(host.capacity_leases(), 1);
    let api_after_down = lifecycle
        .store
        .components(&active.env_id)
        .unwrap()
        .into_iter()
        .find(|component| component.component == "api")
        .unwrap();
    assert_eq!(
        api_after_down.phase,
        crate::managed_services::ManagedServicePhase::Running
    );
    assert_eq!(api_after_down.identity, Some(saved_identity));

    // SAFETY: the group is the one verified from the retained process identity.
    unsafe {
        libc::killpg(process_group, libc::SIGKILL);
    }
    for _ in 0..80 {
        if !super::identity::group_exists(process_group) {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(!super::identity::group_exists(process_group));
    cleanup.disarm();
    let destroyed = lifecycle.down(&active.env_id, None).unwrap();
    assert_eq!(destroyed.phase, EnvironmentPhase::Destroyed);
    assert_eq!(host.capacity_leases(), 0);
}

#[test]
fn a_port_taken_while_parked_fails_the_wake_by_name() {
    let host = Host::new();
    let worktree = tempfile::tempdir().unwrap();
    let snapshot = snapshot(ONE_UNIT, worktree.path());
    let mut lifecycle = host.lifecycle();
    lifecycle
        .up(&snapshot, worktree.path(), None, None, &up_options())
        .unwrap();
    lifecycle.park(&snapshot.env_id, "test").unwrap();
    let grant = super::lifecycle::read_grant(
        &host.root,
        &snapshot.env_id,
        super::lifecycle::RESERVATION_GRANT,
    )
    .unwrap();
    let port: u16 = grant
        .lease
        .allocations
        .iter()
        .find(|a| a.key == "port_api")
        .unwrap()
        .value
        .parse()
        .unwrap();
    let squatter = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
    match lifecycle.wake(&snapshot.env_id, Duration::from_secs(2)) {
        Err(error) => assert!(
            error
                .to_string()
                .contains(&format!("port_in_use: port {port}")),
            "{error}"
        ),
        Ok(_) => panic!("woke onto a port another process holds"),
    }
    assert_eq!(
        lifecycle
            .store
            .get(&snapshot.env_id)
            .unwrap()
            .unwrap()
            .phase,
        EnvironmentPhase::Parked
    );
    assert_eq!(host.capacity_leases(), 0);
    drop(squatter);
    lifecycle.down(&snapshot.env_id, None).unwrap();
}

#[test]
fn up_on_a_parked_environment_wakes_it_and_park_is_idempotent() {
    let host = Host::new();
    let worktree = tempfile::tempdir().unwrap();
    let snapshot = snapshot(ONE_UNIT, worktree.path());
    let mut lifecycle = host.lifecycle();
    lifecycle
        .up(&snapshot, worktree.path(), None, None, &up_options())
        .unwrap();
    lifecycle.park(&snapshot.env_id, "test").unwrap();
    let again = lifecycle.park(&snapshot.env_id, "test").unwrap();
    assert_eq!(again.phase, EnvironmentPhase::Parked);
    let woken = lifecycle
        .up(&snapshot, worktree.path(), None, None, &up_options())
        .unwrap();
    assert_eq!(woken.phase, EnvironmentPhase::Active);
    match lifecycle.wake(&snapshot.env_id, Duration::from_secs(1)) {
        Ok(record) => assert_eq!(
            record.phase,
            EnvironmentPhase::Active,
            "waking an active one is a no-op"
        ),
        Err(error) => panic!("{error}"),
    }
    lifecycle.down(&snapshot.env_id, None).unwrap();
}
