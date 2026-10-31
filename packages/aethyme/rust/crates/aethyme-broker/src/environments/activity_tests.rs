//! Activity and idle parking (#1213): every signal biased toward staying
//! awake, and nothing parked under a running command.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::activity::{BusyMarker, Idleness, has_live_busy_marker, last_touch_ms, now_ms, touch};
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
            repository: "repo-activity".into(),
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

fn profile(probe: Option<&str>) -> String {
    let probe = probe
        .map(|p| format!("activity_probe = '{p}'\n"))
        .unwrap_or_default();
    format!(
        r#"
schema = 1
default_idle_seconds = 60
[[profiles]]
name = "p"
[[profiles.components]]
name = "api"
runtime = "process"
start = "exec sleep 300"
health = {{ command = "true", timeout_seconds = 5, interval_ms = 50 }}
{probe}"#
    )
}

fn up(host: &Host, text: &str) -> (Lifecycle, String, tempfile::TempDir) {
    let worktree = tempfile::tempdir().unwrap();
    let snapshot = snapshot(text, worktree.path());
    let mut lifecycle = host.lifecycle();
    let record = lifecycle
        .up(
            &snapshot,
            worktree.path(),
            None,
            None,
            &UpOptions {
                wait: Duration::from_secs(1),
                reconfigure: false,
                recreate: Vec::new(),
            },
        )
        .unwrap();
    (lifecycle, record.env_id, worktree)
}

const LATER: i64 = 61_000;

#[test]
fn an_environment_is_recent_until_idle_seconds_pass() {
    let host = Host::new();
    let (lifecycle, id, _w) = up(&host, &profile(None));
    let record = lifecycle.store.get(&id).unwrap().unwrap();
    assert!(matches!(
        lifecycle.idleness(&record, now_ms(), true),
        Idleness::Recent { .. }
    ));
    assert!(matches!(
        lifecycle.idleness(&record, now_ms() + LATER, true),
        Idleness::Idle { .. }
    ));
    let mut lifecycle = lifecycle;
    lifecycle.down(&id, None).unwrap();
}

#[test]
fn touch_is_a_file_time_and_moves_activity_forward() {
    let host = Host::new();
    let (lifecycle, id, _w) = up(&host, &profile(None));
    assert_eq!(last_touch_ms(&host.root(), &id), None);
    touch(&host.root(), &id).unwrap();
    let touched = last_touch_ms(&host.root(), &id).unwrap();
    assert!((now_ms() - touched).abs() < 5_000);
    let record = lifecycle.store.get(&id).unwrap().unwrap();
    assert!(matches!(
        lifecycle.idleness(&record, touched + 30_000, true),
        Idleness::Recent { .. }
    ));
    assert!(
        touch(&host.root(), "../escape").is_err(),
        "ids never become paths unchecked"
    );
    let mut lifecycle = lifecycle;
    lifecycle.down(&id, None).unwrap();
}

/// #1213 acceptance: a command running against the environment keeps it
/// awake however old its last activity, and a dead command's marker does not.
#[test]
fn a_live_busy_marker_keeps_an_environment_awake() {
    let host = Host::new();
    let (lifecycle, id, _w) = up(&host, &profile(None));
    let mut command = std::process::Command::new("/bin/sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let marker = BusyMarker::register(&host.root(), &id, command.id() as i32).unwrap();
    let record = lifecycle.store.get(&id).unwrap().unwrap();
    assert_eq!(
        lifecycle.idleness(&record, now_ms() + 10 * LATER, true),
        Idleness::Busy
    );

    command.kill().unwrap();
    command.wait().unwrap();
    std::mem::forget(marker); // as if the exec wrapper itself was killed
    assert!(
        !has_live_busy_marker(&host.root(), &id),
        "a dead command's marker is cleared"
    );
    assert!(matches!(
        lifecycle.idleness(&record, now_ms() + LATER, true),
        Idleness::Idle { .. }
    ));
    let mut lifecycle = lifecycle;
    lifecycle.down(&id, None).unwrap();
}

#[test]
fn probes_keep_it_awake_and_a_hung_probe_counts_as_active() {
    let host = Host::new();
    for (probe, active) in [("true", true), ("false", false), ("sleep 30", true)] {
        let (mut lifecycle, id, _w) = up(&host, &profile(Some(probe)));
        let record = lifecycle.store.get(&id).unwrap().unwrap();
        let state = lifecycle.idleness(&record, now_ms() + LATER, true);
        assert_eq!(
            state == Idleness::Probed,
            active,
            "probe {probe:?}: {state:?}"
        );
        lifecycle.down(&id, None).unwrap();
    }
}

#[test]
fn a_pin_suppresses_idle_parking_until_it_ends() {
    let host = Host::new();
    let (mut lifecycle, id, _w) = up(&host, &profile(None));
    let pinned = lifecycle
        .pin(&id, Duration::from_secs(3600), "demo")
        .unwrap();
    assert!(matches!(
        lifecycle.idleness(&pinned, now_ms() + LATER, true),
        Idleness::Pinned { .. }
    ));
    assert!(lifecycle.park_idle(now_ms() + LATER).unwrap().is_empty());
    let history = lifecycle.store.transitions(&id, 50).unwrap();
    assert!(
        history.iter().any(|t| t.reason == "pinned"),
        "pins are recorded"
    );
    let unpinned = lifecycle.unpin(&id).unwrap();
    assert!(matches!(
        lifecycle.idleness(&unpinned, now_ms() + LATER, true),
        Idleness::Idle { .. }
    ));
    lifecycle.down(&id, None).unwrap();
}

#[test]
fn the_supervisor_tick_parks_only_idle_environments() {
    let host = Host::new();
    let (mut lifecycle, idle, _a) = up(&host, &profile(None));
    let (_, busy, _b) = up(&host, &profile(Some("true")));
    let parked = lifecycle.park_idle(now_ms() + LATER).unwrap();
    assert_eq!(parked, vec![idle.clone()]);
    assert_eq!(
        lifecycle.store.get(&idle).unwrap().unwrap().phase,
        EnvironmentPhase::Parked
    );
    assert_eq!(
        lifecycle.store.get(&busy).unwrap().unwrap().phase,
        EnvironmentPhase::Active
    );
    let history = lifecycle.store.transitions(&idle, 50).unwrap();
    assert!(history.iter().any(|t| t.reason == "parked after idle"));
    lifecycle.down(&idle, None).unwrap();
    lifecycle.down(&busy, None).unwrap();
}
