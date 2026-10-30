//! Resource accounting and calibrate (#1218).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::compile::{CompileContext, CompiledEnvironment, EnvironmentOwner, compile};
use super::lifecycle::{Lifecycle, UpOptions};
use super::metrics::{
    MIN_SAMPLES, ProcessRow, Sample, calibrate, parse_mem_usage, parse_process_table, proposal_diff,
};
use super::spec::{EnvironmentsFile, HostEnvironmentsFile};
use super::store::EnvironmentStore;
use super::{Loaded, parse_loaded};
use crate::resources::HostResourceCoordinator;

const MIB: u64 = 1024 * 1024;

fn parse(text: &str) -> EnvironmentsFile {
    let loaded: Loaded<EnvironmentsFile> = parse_loaded(PathBuf::from("t"), "t", text);
    loaded.value.unwrap_or_else(|d| panic!("{}", d.render()))
}

fn lifecycle(dir: &Path) -> Lifecycle {
    Lifecycle {
        store: EnvironmentStore::open(&dir.join("environments.db")).unwrap(),
        coordinator: HostResourceCoordinator::open(&dir.join("host.db")).unwrap(),
        root: dir.join("environments"),
        holder_pid: None,
        hook_timeout: Duration::from_secs(20),
        actor: "test".into(),
    }
}

fn snapshot(text: &str, worktree: &Path) -> CompiledEnvironment {
    let file = parse(text);
    let fingerprint = crate::console::worktree_fingerprint(worktree);
    compile(
        &file,
        &HostEnvironmentsFile::default(),
        &file.profiles[0].name,
        "t",
        &CompileContext {
            repository: "repo-metrics".into(),
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

fn up(lifecycle: &mut Lifecycle, text: &str) -> (String, tempfile::TempDir) {
    let worktree = tempfile::tempdir().unwrap();
    let snapshot = snapshot(text, worktree.path());
    let record = lifecycle
        .up(
            &snapshot,
            worktree.path(),
            None,
            None,
            &UpOptions {
                wait: Duration::from_secs(1),
                reconfigure: false,
            },
        )
        .unwrap();
    (record.env_id, worktree)
}

#[test]
fn process_tables_and_engine_memory_parse() {
    let rows = parse_process_table("  101  100  2048  1.5\n  102  100  1024  0.0\nnot a row\n");
    assert_eq!(
        rows,
        [
            ProcessRow {
                pid: 101,
                pgid: 100,
                rss_bytes: 2048 * 1024,
                cpu_pct: 1.5
            },
            ProcessRow {
                pid: 102,
                pgid: 100,
                rss_bytes: 1024 * 1024,
                cpu_pct: 0.0
            },
        ]
    );
    assert_eq!(parse_mem_usage("512MiB / 7.6GiB"), Some(512 * MIB));
    assert_eq!(parse_mem_usage("1.5GiB / 8GiB"), Some(3 * 512 * MIB));
    assert_eq!(parse_mem_usage("12kB / 1GB"), Some(12 * 1024));
    assert_eq!(parse_mem_usage("garbage"), None);
}

const PROCESS: &str = r#"
schema = 1
[[profiles]]
name = "p"
[[profiles.components]]
name = "api"
runtime = "process"
start = "exec sleep 300"
health = { command = "true", timeout_seconds = 5, interval_ms = 50 }
"#;

const HOOKS: &str = r#"
schema = 1
[[profiles]]
name = "h"
[[profiles.components]]
name = "db"
runtime = "hooks"
provision = "true"
destroy = "true"
"#;

/// #1218 acceptance: every active environment shows memory with its method,
/// and "unknown" is null, never 0.
#[test]
fn active_environments_are_sampled_with_their_method() {
    let dir = tempfile::tempdir().unwrap();
    let mut lifecycle = lifecycle(dir.path());
    let (process, _a) = up(&mut lifecycle, PROCESS);
    let (hooks, _b) = up(&mut lifecycle, HOOKS);
    let sampled = lifecycle.sample_active(super::activity::now_ms()).unwrap();
    assert_eq!(sampled, 2);
    let measured = lifecycle.store.latest_sample(&process).unwrap().unwrap();
    assert_eq!(measured.method, "ps_rss");
    assert!(measured.bytes.is_some_and(|b| b > 0), "{measured:?}");
    let nothing = lifecycle.store.latest_sample(&hooks).unwrap().unwrap();
    assert_eq!(nothing.method, "none");
    assert_eq!(nothing.bytes, None, "unknown is null, never zero");
    lifecycle.down(&process, None).unwrap();
    lifecycle.down(&hooks, None).unwrap();
}

#[test]
fn history_is_bounded_per_profile() {
    let dir = tempfile::tempdir().unwrap();
    let mut lifecycle = lifecycle(dir.path());
    let (id, _w) = up(&mut lifecycle, HOOKS);
    let record = lifecycle.store.get(&id).unwrap().unwrap();
    for n in 0..30u64 {
        let sample = Sample {
            bytes: Some(n * MIB),
            cpu_pct: None,
            method: "test".into(),
            sampled_at: n as i64,
        };
        lifecycle.store.record_sample(&record, &sample, 10).unwrap();
    }
    let history = lifecycle.store.footprints(&record.repository).unwrap();
    assert_eq!(history["h"].len(), 10);
    assert_eq!(
        *history["h"].iter().min().unwrap(),
        20 * MIB,
        "the newest are kept"
    );
    lifecycle.down(&id, None).unwrap();
}

/// Sampling ten active environments is one `ps` call: cheap per tick.
#[test]
fn sampling_ten_environments_is_one_cheap_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let mut lifecycle = lifecycle(dir.path());
    let fleet: Vec<_> = (0..10).map(|_| up(&mut lifecycle, PROCESS)).collect();
    let started = Instant::now();
    assert_eq!(
        lifecycle.sample_active(super::activity::now_ms()).unwrap(),
        10
    );
    let elapsed = started.elapsed();
    eprintln!("sampling 10 environments took {elapsed:?}");
    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
    for (id, _) in &fleet {
        lifecycle.down(id, None).unwrap();
    }
}

const CALIBRATED: &str = r#"
schema = 1
[pools.envs]
limit = 2

[[profiles]]
name = "site"
[[profiles.components]]
name = "web"
runtime = "hooks"
provision = "true"
destroy = "true"

[[profiles]]
name = "small"
units = 1
[[profiles.components]]
name = "api"
runtime = "process"
start = "x"

[[profiles]]
name = "big"
units = 1
[[profiles.components]]
name = "api"
runtime = "process"
start = "x"

[[profiles]]
name = "new"
units = 1
[[profiles.components]]
name = "api"
runtime = "process"
start = "x"
"#;

fn history() -> BTreeMap<String, Vec<u64>> {
    let spread = |base: u64| (0..20).map(|n| (base + n) * MIB).collect::<Vec<_>>();
    BTreeMap::from([
        ("site".to_string(), spread(10)),
        ("small".to_string(), spread(100)),
        ("big".to_string(), spread(300)),
        ("new".to_string(), vec![50 * MIB; MIN_SAMPLES - 1]),
    ])
}

/// #1218 acceptance: proposed units keep a full pool's p95 footprint within
/// the RAM fraction, net of the engine VM; thin history is not guessed at.
#[test]
fn calibrate_fits_a_full_pool_in_the_budget() {
    let file = parse(CALIBRATED);
    let ram = 16 * 1024 * MIB;
    let vm = 2 * 1024 * MIB;
    let calibration = calibrate(&file, &history(), ram, Some(("docker".into(), vm)), 0.5).unwrap();
    let units = |name: &str| {
        calibration
            .profiles
            .iter()
            .find(|p| p.profile == name)
            .unwrap()
            .proposed_units
    };
    assert_eq!(units("site"), None, "zero-unit profiles take no capacity");
    assert_eq!(
        units("small"),
        Some(1),
        "the smallest pool consumer is one unit"
    );
    assert_eq!(units("big"), Some(3));
    assert_eq!(units("new"), None);
    assert_eq!(calibration.insufficient, ["new"]);
    assert_eq!(
        calibration.budget_bytes,
        ram / 2 - vm,
        "the engine VM is subtracted"
    );
    let limit = calibration.pools["envs"] as u64;
    assert!(limit * calibration.unit_bytes <= calibration.budget_bytes);
    assert!(
        (limit + 1) * calibration.unit_bytes > calibration.budget_bytes,
        "and the limit is not needlessly low"
    );

    let diff = proposal_diff(CALIBRATED, &calibration);
    assert!(diff.contains(&format!("+limit = {limit}")), "{diff}");
    assert!(diff.contains("+units = 3"), "{diff}");
    assert!(
        !diff.contains("+name"),
        "only units and limit lines change: {diff}"
    );
}

#[test]
fn calibrate_refuses_without_enough_history() {
    let file = parse(CALIBRATED);
    let thin = BTreeMap::from([("small".to_string(), vec![MIB; MIN_SAMPLES - 1])]);
    let error = calibrate(&file, &thin, 16 * 1024 * MIB, None, 0.5).unwrap_err();
    assert!(error.contains("insufficient history"), "{error}");
}
