//! Capacity preemption (#1215): all-or-nothing `park_lru`, the exclusions,
//! and `preempt = "none"` unchanged.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::activity::BusyMarker;
use super::compile::{CompileContext, CompiledEnvironment, EnvironmentOwner, compile};
use super::lifecycle::{Lifecycle, UpOptions};
use super::spec::{EnvironmentsFile, HostEnvironmentsFile};
use super::store::{EnvironmentPhase, EnvironmentStore};
use super::{Loaded, parse_loaded};
use crate::resources::HostResourceCoordinator;

struct Host {
    dir: tempfile::TempDir,
}

impl Host {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }
    fn root(&self) -> PathBuf {
        self.dir.path().join("environments")
    }
    fn lifecycle(&self) -> Lifecycle {
        Lifecycle {
            store: EnvironmentStore::open(&self.dir.path().join("environments.db")).unwrap(),
            coordinator: HostResourceCoordinator::open(&self.dir.path().join("host.db")).unwrap(),
            root: self.root(),
            holder_pid: None,
            hook_timeout: Duration::from_secs(20),
            actor: "test".into(),
        }
    }
}

/// One file, many profiles sharing one pool: `small` (1 unit), `big`
/// (3 units), `vip` (1 unit, priority 5).
fn file(preempt: &str, min_active: u64) -> String {
    format!(
        r#"
schema = 1
[pools.envs]
limit = 4
preempt = "{preempt}"
min_active_seconds = {min_active}
[[profiles]]
name = "small"
units = 1
[[profiles.components]]
name = "api"
runtime = "process"
start = "exec sleep 300"
health = {{ command = "true", timeout_seconds = 5, interval_ms = 50 }}
[[profiles]]
name = "big"
units = 3
[[profiles.components]]
name = "api"
runtime = "process"
start = "exec sleep 300"
health = {{ command = "true", timeout_seconds = 5, interval_ms = 50 }}
[[profiles]]
name = "vip"
units = 1
priority = 5
[[profiles.components]]
name = "api"
runtime = "process"
start = "exec sleep 300"
health = {{ command = "true", timeout_seconds = 5, interval_ms = 50 }}
"#
    )
}

fn snapshot(text: &str, profile: &str, worktree: &Path) -> CompiledEnvironment {
    let loaded: Loaded<EnvironmentsFile> = parse_loaded(PathBuf::from("t"), "t", text);
    let file = loaded.value.unwrap_or_else(|d| panic!("{}", d.render()));
    let found = super::validate(&file, &HostEnvironmentsFile::default(), "t", "h");
    assert!(found.is_empty(), "{found:#?}");
    let fingerprint = crate::console::worktree_fingerprint(worktree);
    compile(
        &file,
        &HostEnvironmentsFile::default(),
        profile,
        "t",
        &CompileContext {
            repository: "repo-preempt".into(),
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

struct Env {
    id: String,
    _worktree: tempfile::TempDir,
}

fn up(
    lifecycle: &mut Lifecycle,
    text: &str,
    profile: &str,
) -> Result<Env, super::lifecycle::LifecycleError> {
    let worktree = tempfile::tempdir().unwrap();
    let snapshot = snapshot(text, profile, worktree.path());
    lifecycle
        .up(
            &snapshot,
            worktree.path(),
            None,
            None,
            &UpOptions {
                wait: Duration::from_millis(300),
                reconfigure: false,
            },
        )
        .map(|record| Env {
            id: record.env_id,
            _worktree: worktree,
        })
}

fn phase(lifecycle: &Lifecycle, id: &str) -> EnvironmentPhase {
    lifecycle.store.get(id).unwrap().unwrap().phase
}

/// #1215 acceptance: a full pool of idle 1-unit environments, a 3-unit
/// request parks the three least-recently-active and succeeds.
#[test]
fn park_lru_parks_the_least_recently_active_until_the_request_fits() {
    let host = Host::new();
    let text = file("park_lru", 0);
    let mut lifecycle = host.lifecycle();
    let fleet: Vec<Env> = (0..4)
        .map(|_| up(&mut lifecycle, &text, "small").unwrap())
        .collect();
    // The last one is the most recently active and must survive.
    std::thread::sleep(Duration::from_millis(20));
    super::activity::touch(&host.root(), &fleet[3].id).unwrap();

    let big = up(&mut lifecycle, &text, "big").unwrap();
    assert_eq!(phase(&lifecycle, &big.id), EnvironmentPhase::Active);
    for env in &fleet[..3] {
        assert_eq!(
            phase(&lifecycle, &env.id),
            EnvironmentPhase::Parked,
            "{}",
            env.id
        );
        let notes = lifecycle.store.transitions(&env.id, 50).unwrap();
        let note = notes
            .iter()
            // The park itself is recorded as `preempted` too; the note is the
            // same-phase row that carries the beneficiary.
            .find(|t| t.reason == "preempted" && t.from == Some(t.to))
            .expect("victim note");
        assert_eq!(
            note.detail.as_ref().unwrap()["beneficiary"],
            big.id.as_str()
        );
        let park = notes
            .iter()
            .find(|t| t.to == EnvironmentPhase::Parked)
            .expect("park transition");
        assert_eq!(park.actor, big.id, "the beneficiary is the actor");
    }
    assert_eq!(
        phase(&lifecycle, &fleet[3].id),
        EnvironmentPhase::Active,
        "most recent survives"
    );
    let beneficiary = lifecycle.store.transitions(&big.id, 50).unwrap();
    assert!(beneficiary.iter().any(|t| t.reason == "preempted others"));
    for env in fleet.iter().chain([&big]) {
        lifecycle.down(&env.id, None).unwrap();
    }
}

/// Pinned, busy and higher-priority environments are never victims. Each
/// one alone would cover the 1-unit shortfall, so dropping any single
/// exclusion parks it and fails this test.
#[test]
fn each_exclusion_alone_keeps_its_environment_running() {
    let host = Host::new();
    let text = file("park_lru", 0);
    let mut lifecycle = host.lifecycle();
    let pinned = up(&mut lifecycle, &text, "small").unwrap();
    lifecycle
        .pin(&pinned.id, Duration::from_secs(3600), "demo")
        .unwrap();
    let busy = up(&mut lifecycle, &text, "small").unwrap();
    let mut command = std::process::Command::new("/bin/sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let _marker = BusyMarker::register(&host.root(), &busy.id, command.id() as i32).unwrap();
    let vip = up(&mut lifecycle, &text, "vip").unwrap();
    let pinned_too = up(&mut lifecycle, &text, "small").unwrap();
    lifecycle
        .pin(&pinned_too.id, Duration::from_secs(3600), "demo")
        .unwrap();

    match up(&mut lifecycle, &text, "small") {
        Err(error) => assert_eq!(error.code(), "capacity_timeout"),
        Ok(_) => panic!("every environment in the pool is excluded"),
    }
    for env in [&pinned, &busy, &vip, &pinned_too] {
        assert_eq!(
            phase(&lifecycle, &env.id),
            EnvironmentPhase::Active,
            "nothing parked: {}",
            env.id
        );
    }
    command.kill().unwrap();
    command.wait().unwrap();
    for env in [&pinned, &busy, &vip, &pinned_too] {
        lifecycle.down(&env.id, None).unwrap();
    }
}

/// When the eligible victims cannot free the whole shortfall, none is parked.
#[test]
fn a_partial_preemption_never_happens() {
    let host = Host::new();
    let text = file("park_lru", 0);
    let mut lifecycle = host.lifecycle();
    let pinned: Vec<Env> = (0..3)
        .map(|_| up(&mut lifecycle, &text, "small").unwrap())
        .collect();
    for env in &pinned {
        lifecycle
            .pin(&env.id, Duration::from_secs(3600), "demo")
            .unwrap();
    }
    let idle = up(&mut lifecycle, &text, "small").unwrap();
    // Only `idle` is eligible: one unit, short of the three needed.
    match up(&mut lifecycle, &text, "big") {
        Err(error) => assert_eq!(error.code(), "capacity_timeout"),
        Ok(_) => panic!("a 3-unit request cannot fit"),
    }
    assert_eq!(phase(&lifecycle, &idle.id), EnvironmentPhase::Active);
    for env in pinned.iter().chain([&idle]) {
        lifecycle.down(&env.id, None).unwrap();
    }
}

/// `min_active_seconds` counts from the latest activation: an environment
/// woken a moment ago is protected even though it first started long ago.
/// (Two agents alternately waking must not park each other on every wake.)
#[test]
fn a_freshly_woken_environment_is_protected_by_min_active_seconds() {
    let host = Host::new();
    let text = file("park_lru", 1);
    let mut lifecycle = host.lifecycle();
    let fleet: Vec<Env> = (0..4)
        .map(|_| up(&mut lifecycle, &text, "small").unwrap())
        .collect();
    for env in &fleet[1..] {
        lifecycle
            .pin(&env.id, Duration::from_secs(3600), "demo")
            .unwrap();
    }
    std::thread::sleep(Duration::from_millis(1200));
    lifecycle.park(&fleet[0].id, "test").unwrap();
    lifecycle
        .wake(&fleet[0].id, Duration::from_secs(5))
        .unwrap();

    let worktree = tempfile::tempdir().unwrap();
    let requester = snapshot(&text, "small", worktree.path());
    let parked = lifecycle
        .make_room(&requester, super::activity::now_ms())
        .unwrap();
    assert!(parked.is_empty(), "re-woken under a second ago: {parked:?}");
    assert_eq!(phase(&lifecycle, &fleet[0].id), EnvironmentPhase::Active);
    for env in &fleet {
        lifecycle.down(&env.id, None).unwrap();
    }
}

/// A victim another operation holds is skipped for the next candidate; it
/// never fails the requester.
#[test]
fn a_locked_victim_is_skipped_not_fatal() {
    let host = Host::new();
    let text = file("park_lru", 0);
    let mut lifecycle = host.lifecycle();
    let fleet: Vec<Env> = (0..4)
        .map(|_| up(&mut lifecycle, &text, "small").unwrap())
        .collect();
    for env in &fleet[1..] {
        std::thread::sleep(Duration::from_millis(20));
        super::activity::touch(&host.root(), &env.id).unwrap();
    }
    // fleet[0] is least recently active, but busy with another operation.
    let held = crate::file_lock::try_acquire_environment_operation_lock(&host.root(), &fleet[0].id)
        .unwrap();
    let requester = up(&mut lifecycle, &text, "small").expect("the next victim makes room");
    drop(held);
    assert_eq!(phase(&lifecycle, &fleet[0].id), EnvironmentPhase::Active);
    assert_eq!(phase(&lifecycle, &fleet[1].id), EnvironmentPhase::Parked);
    for env in fleet.iter().chain([&requester]) {
        lifecycle.down(&env.id, None).unwrap();
    }
}

#[test]
fn min_active_seconds_protects_recently_started_environments() {
    let host = Host::new();
    let text = file("park_lru", 3600);
    let mut lifecycle = host.lifecycle();
    let fleet: Vec<Env> = (0..4)
        .map(|_| up(&mut lifecycle, &text, "small").unwrap())
        .collect();
    assert!(
        up(&mut lifecycle, &text, "small").is_err(),
        "all were active for under an hour"
    );
    assert!(
        fleet
            .iter()
            .all(|env| phase(&lifecycle, &env.id) == EnvironmentPhase::Active)
    );
    for env in &fleet {
        lifecycle.down(&env.id, None).unwrap();
    }
}

#[test]
fn preempt_none_keeps_todays_waiting_behaviour() {
    let host = Host::new();
    let text = file("none", 0);
    let mut lifecycle = host.lifecycle();
    let fleet: Vec<Env> = (0..4)
        .map(|_| up(&mut lifecycle, &text, "small").unwrap())
        .collect();
    match up(&mut lifecycle, &text, "small") {
        Err(error) => assert_eq!(error.code(), "capacity_timeout"),
        Ok(_) => panic!("the pool is full and preemption is off"),
    }
    assert!(
        fleet
            .iter()
            .all(|env| phase(&lifecycle, &env.id) == EnvironmentPhase::Active)
    );
    for env in &fleet {
        lifecycle.down(&env.id, None).unwrap();
    }
}
