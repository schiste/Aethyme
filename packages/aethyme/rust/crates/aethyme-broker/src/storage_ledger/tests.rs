//! The ledger, eviction and the start-of-command check on temporary roots.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use super::check::{self, PassInputs, Reclaimable, Summary, WorkerLock};
use super::{Budgets, Class, Root, RootKind, classify};

const DAY_MS: u64 = 24 * 60 * 60 * 1000;

/// A file of `bytes` bytes (allocated, not sparse).
fn file(path: &Path, bytes: usize) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![7u8; bytes]).unwrap();
}

/// Set the modification time of `path` and everything inside it.
fn age(path: &Path, days: u64) {
    let when = SystemTime::now() - Duration::from_millis(days * DAY_MS);
    let mut pending = vec![path.to_path_buf()];
    let mut all = Vec::new();
    while let Some(next) = pending.pop() {
        if next.is_dir() {
            pending.extend(
                fs::read_dir(&next)
                    .unwrap()
                    .flatten()
                    .map(|entry| entry.path()),
            );
        }
        all.push(next);
    }
    // Children first, so setting a directory's time is the last change.
    for entry in all.iter().rev() {
        fs::File::open(entry)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(when))
            .unwrap();
    }
}

fn pass(host_state: &Path, roots: Vec<Root>, budgets: Budgets) -> super::Ledger {
    let registry = host_state.join("host-resources.db");
    let mut free = |_: &Path| Some((u64::MAX / 2, u64::MAX / 2));
    check::run_pass(
        PassInputs {
            host_state,
            roots,
            budgets,
            free: &mut free,
            measure_budget: Duration::from_secs(60),
            pace: Duration::ZERO,
            registry,
        },
        None,
    )
    .unwrap()
}

fn state_root(path: &Path) -> Root {
    Root {
        path: path.to_path_buf(),
        kind: RootKind::HostState,
    }
}

fn empty_registry(host_state: &Path) -> PathBuf {
    let path = host_state.join("host-resources.db");
    crate::HostResourceCoordinator::open(&path).unwrap();
    path
}

#[test]
fn names_are_classified_and_unknown_ones_are_listed_never_deleted() {
    assert_eq!(
        classify(RootKind::HostState, "recovery-archives"),
        Class::RecoveryArchives
    );
    assert_eq!(
        classify(RootKind::HostState, "rollout-v0.9.1-20260101"),
        Class::RolloutArtifacts
    );
    assert_eq!(
        classify(RootKind::Storage, "release-0863-cargo-test.log"),
        Class::ReleaseArtifacts
    );
    assert_eq!(
        classify(RootKind::Storage, "aethyme-broker-quick-test-12"),
        Class::QuickTestPools
    );
    assert_eq!(classify(RootKind::HostCache, "gates"), Class::GateCache);
    assert_eq!(
        classify(RootKind::HostCache, "symbol-index"),
        Class::SymbolIndex
    );
    assert_eq!(
        classify(RootKind::HostState, "host-resources.db"),
        Class::Coordination
    );
    assert_eq!(
        classify(RootKind::HostState, "someone-elses-dir"),
        Class::Unclassified
    );

    let dir = tempfile::tempdir().unwrap();
    let host_state = dir.path();
    file(&host_state.join("mystery/data.bin"), 64 * 1024);
    file(&host_state.join("rollout-v0.1-old/files.bin"), 64 * 1024);
    age(&host_state.join("mystery"), 400);
    age(&host_state.join("rollout-v0.1-old"), 30);
    let ledger = pass(host_state, vec![state_root(host_state)], Budgets::default());
    assert!(
        host_state.join("mystery/data.bin").exists(),
        "unclassified is never deleted"
    );
    assert!(
        !host_state.join("rollout-v0.1-old").exists(),
        "an expired rollout is removed"
    );
    let summary = Summary::load(host_state);
    assert_eq!(summary.unclassified_entries, 1);
    assert!(summary.unclassified_bytes >= 64 * 1024, "{summary:?}");
    let report = ledger.last_pass.unwrap();
    assert_eq!(
        report.evicted,
        vec!["rollout_artifacts:rollout-v0.1-old".to_string()]
    );
    assert_eq!(ledger.telemetry[&Class::RolloutArtifacts].evictions, 1);
}

fn gate_entry(cache: &Path, key: &str, days: u64) {
    let path = cache.join("gates/example-0123/").join(key);
    file(&path.join("target/blob"), 32 * 1024);
    age(&path, days);
}

#[test]
fn an_over_budget_gate_cache_keeps_one_entry_per_kind_and_every_leased_one() {
    let dir = tempfile::tempdir().unwrap();
    let host_state = dir.path().join("state");
    let cache = dir.path().join("cache");
    fs::create_dir_all(&host_state).unwrap();
    let registry = empty_registry(&host_state);
    gate_entry(&cache, "rust-workspace-v1", 9);
    gate_entry(&cache, "rust-workspace-v2", 5);
    gate_entry(&cache, "rust-workspace-v3", 1);
    gate_entry(&cache, "rust-workspace-v4", 7);
    gate_entry(&cache, "lint-v1", 20);
    // A gate running right now holds v1.
    let mut coordinator = crate::HostResourceCoordinator::open(&registry).unwrap();
    coordinator
        .acquire(&crate::HostResourceRequest {
            schema_version: crate::HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
            request_id: "gate-run".into(),
            repository: "example".into(),
            worktree_fingerprint: "fingerprint".into(),
            run_id: "run".into(),
            ttl_seconds: 600,
            holder_pid: Some(std::process::id()),
            resources: vec![crate::HostResourceRequirement {
                key: "cache".into(),
                resource: crate::HostResourceKind::ExclusiveKey {
                    name: crate::gates::managed_cache_lease_name(
                        "example-0123",
                        "rust-workspace-v1",
                    ),
                },
            }],
        })
        .unwrap();
    let budgets = Budgets {
        gate_cache_bytes_per_repository: 1,
        ..Budgets::default()
    };
    let roots = vec![
        state_root(&host_state),
        Root {
            path: cache.clone(),
            kind: RootKind::HostCache,
        },
    ];
    pass(&host_state, roots, budgets);
    let left: Vec<String> = super::read_names(&cache.join("gates/example-0123"));
    assert_eq!(
        left,
        vec![
            "lint-v1".to_string(),
            "rust-workspace-v1".to_string(),
            "rust-workspace-v3".to_string()
        ],
        "the most recent entry per kind stays, the leased one stays, the rest go"
    );
}

fn git(repository: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(repository)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn archive(host_state: &Path, id: &str, repository: &Path, head: &str, days: u64) {
    let path = host_state.join("recovery-archives/example-0123").join(id);
    let manifest = serde_json::json!({
        "repository": repository,
        "head": head,
        "branch_tip": head,
        "unlanded_commits": [],
        "staged_patch": {"bytes": 0},
        "unstaged_patch": {"bytes": 0},
        "untracked_files": [],
        "ignored_files": [],
    });
    file(&path.join("bundle.bin"), 64 * 1024);
    fs::write(path.join("manifest.json"), manifest.to_string()).unwrap();
    age(&path, days);
}

#[test]
fn archives_go_oldest_first_only_when_landed_and_unlanded_ones_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    let host_state = dir.path().join("state");
    let repository = dir.path().join("repository");
    fs::create_dir_all(&host_state).unwrap();
    fs::create_dir_all(&repository).unwrap();
    git(&repository, &["init", "-q", "-b", "main"]);
    fs::write(repository.join("a"), "a").unwrap();
    git(&repository, &["add", "a"]);
    git(&repository, &["commit", "-q", "-m", "a"]);
    let landed = git(&repository, &["rev-parse", "HEAD"]);
    git(
        &repository,
        &["update-ref", "refs/remotes/origin/main", &landed],
    );
    git(&repository, &["checkout", "-q", "-b", "work"]);
    fs::write(repository.join("b"), "b").unwrap();
    git(&repository, &["add", "b"]);
    git(&repository, &["commit", "-q", "-m", "b"]);
    let unlanded = git(&repository, &["rev-parse", "HEAD"]);
    archive(&host_state, "old-landed", &repository, &landed, 9);
    archive(&host_state, "middle-unlanded", &repository, &unlanded, 6);
    archive(&host_state, "new-landed", &repository, &landed, 2);
    empty_registry(&host_state);
    let budgets = Budgets {
        recovery_archives_bytes: 1,
        ..Budgets::default()
    };
    let ledger = pass(&host_state, vec![state_root(&host_state)], budgets);
    let report = ledger.last_pass.unwrap();
    assert_eq!(
        report.evicted,
        vec![
            "recovery_archives:recovery-archives/example-0123/old-landed".to_string(),
            "recovery_archives:recovery-archives/example-0123/new-landed".to_string(),
        ],
        "{report:?}"
    );
    assert_eq!(
        report.unlanded_archives,
        vec!["recovery-archives/example-0123/middle-unlanded".to_string()]
    );
    assert!(
        host_state
            .join("recovery-archives/example-0123/middle-unlanded")
            .exists()
    );
    assert_eq!(
        Summary::load(&host_state).unlanded_archives,
        vec!["recovery-archives/example-0123/middle-unlanded".to_string()]
    );
}

#[test]
fn the_command_check_never_walks_and_refuses_only_when_reclaim_is_not_enough() {
    let dir = tempfile::tempdir().unwrap();
    let host_state = dir.path();
    let regenerable = host_state.join("symbol-index/old-index");
    file(&regenerable.join("index.bin"), 1024);
    let summary = Summary {
        updated_at_ms: super::now_ms(),
        roots: vec![state_root(host_state)],
        floor_bytes: 100,
        reclaimable: vec![Reclaimable {
            class: Class::SymbolIndex,
            root_kind: RootKind::HostState,
            root: host_state.to_path_buf(),
            relative: "symbol-index/old-index".into(),
            bytes: 1024,
            newest_ms: 0,
        }],
        ..Summary::default()
    };
    summary.save(host_state).unwrap();
    let walks_before = super::WALKS.with(std::cell::Cell::get);

    // Above the floor: nothing to do, no worker, no walk.
    let mut kicks = 0;
    let decision = check::decide(host_state, true, &mut |_| Some(1_000), &mut || kicks += 1);
    assert_eq!(decision.refuse, None);
    assert!(!decision.kick);

    // Below the floor, and removing the regenerable index is enough.
    let mut free = |_: &Path| Some(if regenerable.exists() { 50 } else { 500 });
    let decision = check::decide(host_state, true, &mut free, &mut || kicks += 1);
    assert!(decision.kick, "a floor breach starts the worker");
    assert_eq!(decision.reclaimed, 1024);
    assert_eq!(decision.refuse, None, "reclaim was enough");
    assert!(!regenerable.exists());

    // Still below the floor with nothing left to reclaim: a command that
    // needs space refuses, one that does not only starts the worker.
    let decision = check::decide(host_state, true, &mut |_| Some(50), &mut || kicks += 1);
    assert!(
        decision
            .refuse
            .as_deref()
            .is_some_and(|why| why.contains("below the")),
        "{decision:?}"
    );
    let decision = check::decide(host_state, false, &mut |_| Some(50), &mut || kicks += 1);
    assert_eq!(decision.refuse, None);
    assert_eq!(kicks, 3);
    assert_eq!(
        super::WALKS.with(std::cell::Cell::get),
        walks_before,
        "the check never walks"
    );
}

#[test]
fn a_dead_or_stalled_worker_lock_is_taken_over_and_a_live_one_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let host_state = dir.path();
    let lock_path = host_state.join(check::LOCK_FILE);
    let mut gone = Command::new("true").spawn().unwrap();
    gone.wait().unwrap();
    let write = |pid: u32, started: &str, heartbeat_ms: u64| {
        fs::write(
            &lock_path,
            serde_json::to_vec(&check::LockRecord {
                pid,
                started: started.into(),
                heartbeat_ms,
                token: "other".into(),
            })
            .unwrap(),
        )
        .unwrap();
    };
    // Dead owner.
    write(gone.id(), "long ago", super::now_ms());
    let lock = WorkerLock::acquire(host_state).expect("a dead owner's lock is taken over");
    assert!(lock.held());
    drop(lock);
    assert!(!lock_path.exists(), "released on drop");

    // A live owner (this process) with a fresh heartbeat keeps it.
    let me = std::process::id();
    let started = crate::mcp::hub::process_start(me).unwrap();
    write(me, &started, super::now_ms());
    assert!(WorkerLock::acquire(host_state).is_err());

    // The same live owner, stalled past the heartbeat limit, loses it.
    let stale =
        super::now_ms() - u64::try_from(check::STALE_HEARTBEAT.as_millis()).unwrap() - 1_000;
    write(me, &started, stale);
    let mut lock = WorkerLock::acquire(host_state).expect("a stalled lock is taken over");
    assert!(lock.beat());
    // Taken over in turn: the old holder notices before its next removal.
    write(me, &started, super::now_ms());
    assert!(!lock.beat());
}

#[test]
fn storage_roots_are_only_aethyme_directories() {
    assert_eq!(
        check::storage_root_for(Path::new("/volume/Application Support/Aethyme/worktrees")),
        Some(PathBuf::from("/volume/Application Support/Aethyme"))
    );
    assert_eq!(
        check::storage_root_for(Path::new("/home/someone/worktrees")),
        None
    );
}

#[test]
fn a_gate_that_takes_the_cache_after_selection_keeps_it() {
    let dir = tempfile::tempdir().unwrap();
    let host_state = dir.path().join("state");
    let cache = dir.path().join("cache");
    fs::create_dir_all(&host_state).unwrap();
    let registry = empty_registry(&host_state);
    gate_entry(&cache, "rust-workspace-v1", 9);
    // Selected for removal while nothing held it...
    let candidate = super::evict::Candidate {
        class: Class::GateCache,
        root_kind: RootKind::HostCache,
        root: cache.clone(),
        relative: "gates/example-0123/rust-workspace-v1".into(),
        bytes: 1,
        newest_ms: 0,
        reason: super::evict::Reason::OverBudget,
    };
    let leases = super::evict::leased_gate_caches(&registry).unwrap();
    assert_eq!(super::evict::refuse(&candidate, Some(&leases)), None);
    // ...then a gate starts and takes its lease before the removal.
    let mut coordinator = crate::HostResourceCoordinator::open(&registry).unwrap();
    let grant = coordinator
        .acquire(&crate::HostResourceRequest {
            schema_version: crate::HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
            request_id: "late-gate".into(),
            repository: "example".into(),
            worktree_fingerprint: "fingerprint".into(),
            run_id: "run".into(),
            ttl_seconds: 600,
            holder_pid: Some(std::process::id()),
            resources: vec![crate::HostResourceRequirement {
                key: "cache".into(),
                resource: crate::HostResourceKind::ExclusiveKey {
                    name: crate::gates::managed_cache_lease_name(
                        "example-0123",
                        "rust-workspace-v1",
                    ),
                },
            }],
        })
        .unwrap();
    let removed = super::evict::remove_protected(&candidate, &registry);
    assert!(
        removed
            .as_ref()
            .is_err_and(|why| why.contains("a gate holds it")),
        "{removed:?}"
    );
    assert!(
        cache
            .join("gates/example-0123/rust-workspace-v1/target/blob")
            .exists()
    );
    // Once the gate is done, the removal holds the lease itself and frees it.
    coordinator
        .release(
            &grant.lease.lease_id,
            grant.lease.generation,
            &grant.ownership_token,
        )
        .unwrap();
    super::evict::remove_protected(&candidate, &registry).unwrap();
    assert!(!cache.join("gates/example-0123/rust-workspace-v1").exists());
    let held = super::evict::leased_gate_caches(&registry).unwrap();
    assert!(held.is_empty(), "the removal released its lease: {held:?}");
}
