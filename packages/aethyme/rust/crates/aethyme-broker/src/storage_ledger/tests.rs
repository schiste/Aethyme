//! The ledger, eviction and the start-of-command check on temporary roots.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

use super::check::{self, PassInputs, Reclaimable, Summary, WorkerLock};
use super::evict::{self, Candidate, Reason};
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
        if fs::symlink_metadata(&next).unwrap().is_dir() {
            pending.extend(
                fs::read_dir(&next)
                    .unwrap()
                    .flatten()
                    .map(|entry| entry.path()),
            );
        }
        all.push(next);
    }
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

fn root(path: &Path, kind: RootKind) -> Root {
    Root::new(path, kind)
}

fn empty_registry(host_state: &Path) -> PathBuf {
    let path = host_state.join("host-resources.db");
    crate::HostResourceCoordinator::open(&path).unwrap();
    path
}

fn lease(registry: &Path, request_id: &str, key: &str) -> crate::HostResourceGrant {
    crate::HostResourceCoordinator::open(registry)
        .unwrap()
        .acquire(&crate::HostResourceRequest {
            schema_version: crate::HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
            request_id: request_id.into(),
            repository: "example".into(),
            worktree_fingerprint: "fingerprint".into(),
            run_id: "run".into(),
            ttl_seconds: 600,
            holder_pid: Some(std::process::id()),
            resources: vec![crate::HostResourceRequirement {
                key: "cache".into(),
                resource: crate::HostResourceKind::ExclusiveKey {
                    name: crate::gates::managed_cache_lease_name("example-0123", key),
                },
            }],
        })
        .unwrap()
}

// Must-fix 1: exact names only, per root kind; hostile names stay.
#[test]
fn only_exact_aethyme_names_are_classified_in_their_own_root() {
    use Class::*;
    use RootKind::*;
    for (kind, name, class) in [
        (HostState, "recovery-archives", RecoveryArchives),
        (HostState, "rollout-v0.8.53-20261102", RolloutArtifacts),
        (
            HostState,
            "rollout-prep-v0.8.29-2c78fd1b-20261022-v2",
            RolloutArtifacts,
        ),
        (
            HostState,
            "release-assets-v0.8.28-ece40948-4924f365",
            ReleaseArtifacts,
        ),
        (HostCache, "gates", GateCache),
        (HostCache, "symbol-index", SymbolIndex),
        (HostCache, "node-compile-cache", ToolCaches),
        (
            WorktreeRoot,
            "aethyme-broker-quick-test-49725-17924098-4f64122b85fd1c82",
            QuickTestPools,
        ),
        (Storage, "release-0863-cargo-test.log", ReleaseArtifacts),
        (Storage, "build-cache", ToolCaches),
        // Hostile or merely similar names.
        (Storage, "target", Unclassified),
        (Storage, "cache", Unclassified),
        (HostState, "release-notes", Unclassified),
        (HostState, "rollout-vacation-photos", Unclassified),
        (HostState, "build-cache", Unclassified),
        (
            HostState,
            "aethyme-broker-quick-test-49725-17924098-4f64122b85fd1c82",
            Unclassified,
        ),
        (WorktreeRoot, "foo-quick-test-bar", Worktrees),
        (
            WorktreeRoot,
            "aethyme-broker-quick-test-1-2-notahexsuffix00",
            Worktrees,
        ),
        (Storage, "foo-quick-test-bar", Unclassified),
    ] {
        assert_eq!(classify(kind, name), class, "{kind:?} {name}");
    }
}

#[test]
fn a_directory_is_a_storage_root_only_with_the_marker_and_never_a_repository() {
    let dir = tempfile::tempdir().unwrap();
    let host_state = dir.path().join("state");
    let named_aethyme = dir.path().join("Aethyme");
    fs::create_dir_all(&host_state).unwrap();
    fs::create_dir_all(named_aethyme.join("target")).unwrap();
    // Registered without the marker: not a root, whatever its name.
    check::register_root(&host_state, &named_aethyme, RootKind::Storage, None);
    let roots = check::roots(&host_state, None);
    assert!(
        !roots.iter().any(|root| root.path == named_aethyme),
        "{roots:?}"
    );
    // Adopted: the marker makes it one.
    check::adopt(&host_state, &named_aethyme).unwrap();
    let roots = check::roots(&host_state, None);
    let canonical = fs::canonicalize(&named_aethyme).unwrap();
    assert!(
        roots
            .iter()
            .any(|root| root.path == canonical && root.kind == RootKind::Storage)
    );
    // A repository, its `.aethyme/` or anything inside it is refused.
    let repository = dir.path().join("repository");
    fs::create_dir_all(repository.join(".git")).unwrap();
    fs::create_dir_all(repository.join(".aethyme/cache")).unwrap();
    for path in [
        repository.clone(),
        repository.join(".aethyme"),
        repository.join(".aethyme/cache"),
    ] {
        assert!(
            check::adopt(&host_state, &path).is_err(),
            "{}",
            path.display()
        );
    }
}

#[test]
fn hostile_names_in_a_worktree_root_survive_a_pass() {
    let dir = tempfile::tempdir().unwrap();
    let host_state = dir.path().join("state");
    let worktrees = dir.path().join("worktrees");
    fs::create_dir_all(&host_state).unwrap();
    let pool = "aethyme-broker-quick-test-49725-17924098-4f64122b85fd1c82";
    for name in ["foo-quick-test-bar", pool] {
        file(&worktrees.join(name).join("checkout/file"), 4096);
        age(&worktrees.join(name), 30);
    }
    let ledger = pass(
        &host_state,
        vec![
            root(&host_state, RootKind::HostState),
            root(&worktrees, RootKind::WorktreeRoot),
        ],
        Budgets::default(),
    );
    assert!(worktrees.join("foo-quick-test-bar/checkout/file").exists());
    assert!(
        !worktrees.join(pool).exists(),
        "an expired quick-test pool goes"
    );
    assert_eq!(
        ledger.last_pass.unwrap().evicted,
        vec![format!("quick_test_pools:{pool}")]
    );
}

#[test]
fn unclassified_entries_are_listed_and_never_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let host_state = dir.path();
    file(&host_state.join("mystery/data.bin"), 64 * 1024);
    file(&host_state.join("rollout-v0.1.0-old/files.bin"), 64 * 1024);
    age(&host_state.join("mystery"), 400);
    age(&host_state.join("rollout-v0.1.0-old"), 30);
    pass(
        host_state,
        vec![root(host_state, RootKind::HostState)],
        Budgets::default(),
    );
    assert!(host_state.join("mystery/data.bin").exists());
    assert!(!host_state.join("rollout-v0.1.0-old").exists());
    let summary = Summary::load(host_state);
    assert_eq!(summary.unclassified_entries, 1);
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
    let _held = lease(&registry, "gate-run", "rust-workspace-v1");
    let budgets = Budgets {
        gate_cache_bytes_per_repository: 1,
        ..Budgets::default()
    };
    pass(
        &host_state,
        vec![
            root(&host_state, RootKind::HostState),
            root(&cache, RootKind::HostCache),
        ],
        budgets,
    );
    assert_eq!(
        super::read_names(&cache.join("gates/example-0123")),
        vec![
            "lint-v1".to_string(),
            "rust-workspace-v1".to_string(),
            "rust-workspace-v3".to_string()
        ]
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
    let candidate = Candidate {
        class: Class::GateCache,
        root_kind: RootKind::HostCache,
        root: cache.clone(),
        relative: "gates/example-0123/rust-workspace-v1".into(),
        bytes: 1,
        newest_ms: 0,
        reason: Reason::OverBudget,
        expires_ms: None,
        allow_unlanded: false,
    };
    let leases = evict::leased_gate_caches(&registry).unwrap();
    assert_eq!(
        evict::refuse(&candidate, Some(&leases), super::now_ms()),
        None
    );
    let grant = lease(&registry, "late-gate", "rust-workspace-v1");
    let removed = evict::remove_protected(&candidate, &registry, &mut || {}, None);
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
    crate::HostResourceCoordinator::open(&registry)
        .unwrap()
        .release(
            &grant.lease.lease_id,
            grant.lease.generation,
            &grant.ownership_token,
        )
        .unwrap();
    assert!(evict::remove_protected(&candidate, &registry, &mut || {}, None).unwrap());
    assert!(!cache.join("gates/example-0123/rust-workspace-v1").exists());
    assert!(evict::leased_gate_caches(&registry).unwrap().is_empty());
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

fn archive(host_state: &Path, id: &str, repository: &Path, head: &str, created_days_ago: u64) {
    let path = host_state.join("recovery-archives/example-0123").join(id);
    let created = super::now_ms() - created_days_ago * DAY_MS;
    let manifest = serde_json::json!({
        "created_at_ms": created,
        "minimum_archive_retention_ms": 15 * DAY_MS,
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
    // Files inside may be old on day one: never a reason to remove it.
    age(&path, 400);
}

// Must-fix 2: never by age, never unlanded, landed only after its promised
// retention.
#[test]
fn archives_go_only_when_landed_and_past_their_promised_retention() {
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
    archive(&host_state, "old-landed", &repository, &landed, 40);
    archive(&host_state, "older-unlanded", &repository, &unlanded, 90);
    archive(&host_state, "young-landed", &repository, &landed, 3);
    empty_registry(&host_state);
    let budgets = Budgets {
        recovery_archives_bytes: 1,
        ..Budgets::default()
    };
    let ledger = pass(
        &host_state,
        vec![root(&host_state, RootKind::HostState)],
        budgets,
    );
    let report = ledger.last_pass.unwrap();
    assert_eq!(
        report.evicted,
        vec!["recovery_archives:recovery-archives/example-0123/old-landed".to_string()],
        "{report:?}"
    );
    assert_eq!(
        report.unlanded_archives,
        vec!["recovery-archives/example-0123/older-unlanded".to_string()]
    );
    assert!(
        host_state
            .join("recovery-archives/example-0123/older-unlanded")
            .exists()
    );
    assert!(
        host_state
            .join("recovery-archives/example-0123/young-landed")
            .exists()
    );
    // The pass reports its landing decisions and keeps, for the next pass,
    // only what it proved not landed.
    assert!(report.landing.contains_key("probes"), "{report:?}");
    let cache: serde_json::Value =
        serde_json::from_slice(&fs::read(host_state.join(super::landing::CACHE_FILE)).unwrap())
            .unwrap();
    let facts = cache["repositories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["repository"] == serde_json::json!(repository))
        .unwrap_or_else(|| panic!("{cache}"));
    assert_eq!(facts["tip"], serde_json::json!(landed), "{cache}");
    assert_eq!(
        facts["not_contained"],
        serde_json::json!([unlanded]),
        "{cache}"
    );
    assert!(facts.get("contained").is_none(), "{cache}");
    assert_eq!(
        classify(RootKind::HostState, super::landing::CACHE_FILE),
        Class::Coordination
    );
    // Within budget, nothing goes at all, however old.
    archive(&host_state, "ancient-landed", &repository, &landed, 400);
    let ledger = pass(
        &host_state,
        vec![root(&host_state, RootKind::HostState)],
        Budgets::default(),
    );
    assert!(ledger.last_pass.unwrap().evicted.is_empty());
}

// Must-fix 3: no walk of report-only classes; walks resume within deadlines
// and refresh the heartbeat.
#[test]
fn report_only_classes_are_counted_never_walked() {
    let dir = tempfile::tempdir().unwrap();
    let host_state = dir.path();
    for index in 0..5 {
        file(
            &host_state.join(format!("worktrees/checkout-{index}/deep/file")),
            1024,
        );
    }
    let mut item = super::Item {
        root: host_state.to_path_buf(),
        root_kind: RootKind::HostState,
        name: "worktrees".into(),
        class: Class::Worktrees,
        device: 0,
        bytes: 0,
        measured: false,
        entries: 0,
        oldest_ms: 0,
        newest_ms: 0,
        measured_at_ms: 0,
        children: Vec::new(),
        walk: None,
    };
    let walks = super::WALKS.with(std::cell::Cell::get);
    assert!(super::measure_item(
        &mut item,
        Instant::now() + Duration::from_secs(5),
        &mut || {}
    ));
    assert_eq!(
        super::WALKS.with(std::cell::Cell::get),
        walks,
        "never walked"
    );
    assert_eq!(item.entries, 5);
    assert!(!item.measured, "bytes are unmeasured, not zero");
}

#[test]
fn a_walk_stops_at_its_deadline_and_resumes_where_it_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path();
    for index in 0..40 {
        file(
            &cache.join(format!("symbol-index/index-{index}/data")),
            4096,
        );
    }
    let mut item = super::Item {
        root: cache.to_path_buf(),
        root_kind: RootKind::HostCache,
        name: "symbol-index".into(),
        class: Class::SymbolIndex,
        device: 0,
        bytes: 0,
        measured: false,
        entries: 0,
        oldest_ms: 0,
        newest_ms: 0,
        measured_at_ms: 0,
        children: Vec::new(),
        walk: None,
    };
    // An expired deadline: one step, then a persisted cursor.
    assert!(!super::measure_item(&mut item, Instant::now(), &mut || {}));
    assert!(item.walk.is_some());
    let round_trip: super::Item =
        serde_json::from_str(&serde_json::to_string(&item).unwrap()).unwrap();
    let mut item = round_trip;
    let mut rounds = 1;
    while !super::measure_item(&mut item, Instant::now(), &mut || {}) {
        rounds += 1;
        assert!(rounds < 10_000, "the walk makes progress");
    }
    assert!(item.measured);
    assert_eq!(item.children.len(), 40);
    assert!(item.bytes >= 40 * 4096);
}

#[test]
fn a_walk_refreshes_the_heartbeat_as_it_goes() {
    let dir = tempfile::tempdir().unwrap();
    for index in 0..20 {
        file(&dir.path().join(format!("f{index}")), 16);
    }
    let device = super::device_of(dir.path()).unwrap();
    let mut pending = vec![dir.path().to_path_buf()];
    let mut usage = super::Usage::default();
    let mut beats = 0;
    // With a zero interval every entry refreshes it.
    assert!(super::walk_until(
        &mut pending,
        &mut usage,
        device,
        Instant::now() + Duration::from_secs(30),
        Duration::ZERO,
        &mut || beats += 1,
    ));
    assert_eq!(beats, 21, "the directory and its twenty files");
    assert_eq!(usage.entries, 21);
    // Deleting refreshes it too.
    let mut deletion_beats = 0;
    assert!(evict::delete_tree(dir.path(), &mut || deletion_beats += 1, None).unwrap());
    assert!(!dir.path().exists());
    assert!(
        deletion_beats <= 21,
        "at most one refresh per removed entry"
    );
}

// Must-fix 4: only writers refuse, only below the minimum, after a bounded
// reclaim; between the minimum and the floor they proceed.
#[test]
fn only_writing_commands_refuse_and_only_below_the_hard_minimum() {
    let words = |text: &str| text.split(' ').map(str::to_string).collect::<Vec<_>>();
    assert!(check::writes_to(&words("submit prepare status")).is_empty());
    assert!(check::writes_to(&words("status --json")).is_empty());
    assert!(check::writes_to(&words("submit --session 3")).contains(&RootKind::HostCache));
    assert!(check::writes_to(&words("start --task x")).contains(&RootKind::WorktreeRoot));

    let dir = tempfile::tempdir().unwrap();
    let host_state = dir.path();
    let regenerable = host_state.join("symbol-index/old-index");
    file(&regenerable.join("index.bin"), 1024);
    age(&regenerable, 30);
    let other = host_state.join("symbol-index/older-index");
    file(&other.join("index.bin"), 1024);
    age(&other, 40);
    Summary {
        updated_at_ms: super::now_ms(),
        roots: vec![root(host_state, RootKind::HostState)],
        floor_bytes: 1000,
        hard_minimum_bytes: 100,
        reclaimable: [&other, &regenerable]
            .iter()
            .map(|path| Reclaimable {
                class: Class::SymbolIndex,
                root_kind: RootKind::HostState,
                root: host_state.to_path_buf(),
                relative: path
                    .strip_prefix(host_state)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                bytes: 1024,
                newest_ms: super::now_ms() - 30 * DAY_MS,
            })
            .collect(),
        ..Summary::default()
    }
    .save(host_state)
    .unwrap();
    let start = check::writes_to(&words("start"));
    let walks = super::WALKS.with(std::cell::Cell::get);

    // Between the minimum and the floor: no refusal (advice warns).
    let decision = check::decide(host_state, &start, None, &mut |_| Some(500), &mut || {});
    assert_eq!(decision.refuse, None);
    assert!(
        regenerable.exists() && other.exists(),
        "nothing reclaimed above the minimum"
    );

    // Below the minimum: the oldest regenerable entry is enough; one goes.
    let mut free = |_: &Path| Some(if other.exists() { 50 } else { 5_000 });
    let decision = check::decide(host_state, &start, None, &mut free, &mut || {});
    assert_eq!(decision.refuse, None);
    assert_eq!(decision.reclaimed, 1024);
    assert!(
        !other.exists() && regenerable.exists(),
        "bounded: only what was needed"
    );

    // A reader never refuses, even below the minimum.
    let reader = check::writes_to(&words("submit prepare status"));
    assert_eq!(
        check::decide(host_state, &reader, None, &mut |_| Some(1), &mut || {}).refuse,
        None
    );

    // A writer still short after reclaim refuses, naming the key and override.
    let decision = check::decide(host_state, &start, None, &mut |_| Some(1), &mut || {});
    let refusal = decision.refuse.expect("refused");
    assert!(
        refusal.contains("hard_minimum_bytes") && refusal.contains("AETHYME_STORAGE_LEDGER"),
        "{refusal}"
    );
    assert_eq!(
        super::WALKS.with(std::cell::Cell::get),
        walks,
        "the check never walks"
    );
}

#[test]
fn worker_starts_are_rate_limited_and_skipped_while_one_runs() {
    let dir = tempfile::tempdir().unwrap();
    let host_state = dir.path();
    Summary {
        updated_at_ms: super::now_ms(),
        roots: vec![root(host_state, RootKind::HostState)],
        floor_bytes: 1000,
        hard_minimum_bytes: 10,
        ..Summary::default()
    }
    .save(host_state)
    .unwrap();
    let mut kicks = 0;
    for _ in 0..3 {
        check::decide(host_state, &[], None, &mut |_| Some(500), &mut || {
            kicks += 1
        });
    }
    assert_eq!(kicks, 1, "at most one start a minute, even below the floor");
    let mut summary = Summary::load(host_state);
    summary.last_kick_ms = 0;
    summary.save(host_state).unwrap();
    let _lock = WorkerLock::acquire(host_state).unwrap();
    check::decide(host_state, &[], None, &mut |_| Some(500), &mut || {
        kicks += 1
    });
    assert_eq!(kicks, 1, "never while a worker holds a fresh lock");
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
    write(gone.id(), "long ago", super::now_ms());
    let lock = WorkerLock::acquire(host_state).expect("a dead owner's lock is taken over");
    assert!(lock.held());
    drop(lock);
    let me = std::process::id();
    let started = crate::mcp::hub::process_start(me).unwrap();
    write(me, &started, super::now_ms());
    assert!(
        WorkerLock::acquire(host_state).is_err(),
        "a live, beating owner keeps it"
    );
    let stale =
        super::now_ms() - u64::try_from(check::STALE_HEARTBEAT.as_millis()).unwrap() - 1_000;
    write(me, &started, stale);
    let mut lock = WorkerLock::acquire(host_state).expect("a stalled lock is taken over");
    assert!(lock.beat());
    write(me, &started, super::now_ms());
    assert!(!lock.beat(), "the old holder notices it lost the lock");
    assert!(lock.lost());
}

// Should-fix 5, 6, 9.
#[test]
fn links_recent_use_and_unknown_recency_keep_an_entry() {
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path();
    let index = cache.join("symbol-index/old");
    file(&index.join("data"), 1024);
    age(&index, 30);
    let candidate = |relative: &str, newest_ms: u64| Candidate {
        class: Class::SymbolIndex,
        root_kind: RootKind::HostCache,
        root: cache.to_path_buf(),
        relative: relative.into(),
        bytes: 1024,
        newest_ms,
        reason: Reason::OverBudget,
        expires_ms: None,
        allow_unlanded: false,
    };
    let now = super::now_ms();
    let measured = now - 30 * DAY_MS + 1000;
    assert_eq!(
        evict::refuse(&candidate("symbol-index/old", measured), None, now),
        None
    );
    // A link anywhere on the path.
    std::os::unix::fs::symlink(&index, cache.join("symbol-index/linked")).unwrap();
    assert!(evict::refuse(&candidate("symbol-index/linked", measured), None, now).is_some());
    std::os::unix::fs::symlink(cache.join("symbol-index"), cache.join("alias")).unwrap();
    assert!(evict::refuse(&candidate("alias/old", measured), None, now).is_some());
    // Used since it was measured.
    fs::write(index.join("fresh"), "x").unwrap();
    assert!(evict::refuse(&candidate("symbol-index/old", measured), None, now).is_some());
    // Unknown recency is kept, not treated as oldest.
    let item = super::Item {
        root: cache.to_path_buf(),
        root_kind: RootKind::HostCache,
        name: "symbol-index".into(),
        class: Class::SymbolIndex,
        device: 0,
        bytes: 10 * 1024,
        measured: true,
        entries: 1,
        oldest_ms: 0,
        newest_ms: 0,
        measured_at_ms: now,
        children: vec![super::Child {
            name: "unknown".into(),
            bytes: 10 * 1024,
            newest_ms: 0,
            ctime_ms: 0,
            created_ms: None,
            min_retention_ms: None,
            session_id: None,
            kept: false,
        }],
        walk: None,
    };
    let ledger = super::Ledger {
        items: vec![item],
        ..super::Ledger::default()
    };
    let budgets = Budgets {
        symbol_index_bytes: 1,
        ..Budgets::default()
    };
    assert!(evict::plan(&ledger, &budgets, now, &Default::default()).is_empty());
}

// Second review 1: the lock token is re-read before each removal.
#[test]
fn a_worker_that_lost_its_lock_removes_nothing_more() {
    let dir = tempfile::tempdir().unwrap();
    let host_state = dir.path();
    file(&host_state.join("rollout-v0.1.0-old/files.bin"), 4096);
    age(&host_state.join("rollout-v0.1.0-old"), 30);
    let mut lock = WorkerLock::acquire(host_state).unwrap();
    // Taken over by another worker, with no heartbeat in between.
    fs::write(
        host_state.join(check::LOCK_FILE),
        serde_json::to_vec(&check::LockRecord {
            pid: std::process::id(),
            started: String::new(),
            heartbeat_ms: super::now_ms(),
            token: "another-worker".into(),
        })
        .unwrap(),
    )
    .unwrap();
    assert!(!lock.lost(), "the cached flag has not noticed");
    let registry = host_state.join("host-resources.db");
    let mut free = |_: &Path| Some((u64::MAX / 2, u64::MAX / 2));
    let ledger = check::run_pass(
        PassInputs {
            host_state,
            roots: vec![root(host_state, RootKind::HostState)],
            budgets: Budgets::default(),
            free: &mut free,
            measure_budget: Duration::from_secs(60),
            pace: Duration::ZERO,
            registry,
        },
        Some(&mut lock),
    )
    .unwrap();
    assert!(host_state.join("rollout-v0.1.0-old/files.bin").exists());
    let report = ledger.last_pass.unwrap();
    assert!(report.evicted.is_empty());
    assert!(
        report
            .skipped
            .iter()
            .any(|line| line.contains("taken over")),
        "{report:?}"
    );
}

// Second review 2: only this repository's worktree root counts for `start`.
#[test]
fn another_repositorys_full_volume_does_not_refuse_this_start() {
    let dir = tempfile::tempdir().unwrap();
    let host_state = dir.path().join("state");
    let ours = dir.path().join("our-worktrees");
    let theirs = dir.path().join("their-worktrees");
    let our_repository = dir.path().join("our-repository");
    for path in [&host_state, &ours, &theirs, &our_repository] {
        fs::create_dir_all(path).unwrap();
    }
    let worktree_root = |path: &Path, repository: &Path| Root {
        path: path.to_path_buf(),
        kind: RootKind::WorktreeRoot,
        repositories: vec![repository.to_path_buf()],
    };
    Summary {
        updated_at_ms: super::now_ms(),
        roots: vec![
            root(&host_state, RootKind::HostState),
            worktree_root(&ours, &our_repository),
            worktree_root(&theirs, &dir.path().join("their-repository")),
        ],
        floor_bytes: 1000,
        hard_minimum_bytes: 100,
        ..Summary::default()
    }
    .save(&host_state)
    .unwrap();
    let start = check::writes_to(&["start".to_string()]);
    let cwd = our_repository.join("src");
    let theirs_full = |path: &Path| Some(if path == theirs { 1 } else { 5_000 });
    let decision = check::decide(
        &host_state,
        &start,
        Some(&cwd),
        &mut { theirs_full },
        &mut || {},
    );
    assert_eq!(
        decision.refuse, None,
        "their volume is not ours to refuse for"
    );
    let ours_full = |path: &Path| Some(if path == ours { 1 } else { 5_000 });
    let decision = check::decide(
        &host_state,
        &start,
        Some(&cwd),
        &mut { ours_full },
        &mut || {},
    );
    assert!(decision.refuse.is_some(), "our own worktree volume is");
    // From inside a session worktree of ours, too.
    let session = ours.join("repository-key/session");
    let decision = check::decide(
        &host_state,
        &start,
        Some(&session),
        &mut { ours_full },
        &mut || {},
    );
    assert!(decision.refuse.is_some());
}

// Second review 3: the reclaim deadline holds inside a deletion.
#[test]
fn an_interrupted_removal_leaves_its_remainder_for_the_sweep() {
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path();
    for index in 0..200 {
        file(&cache.join(format!("symbol-index/big/part-{index}")), 16);
    }
    let candidate = Candidate {
        class: Class::SymbolIndex,
        root_kind: RootKind::HostCache,
        root: cache.to_path_buf(),
        relative: "symbol-index/big".into(),
        bytes: 1,
        newest_ms: 0,
        reason: Reason::OverBudget,
        expires_ms: None,
        allow_unlanded: false,
    };
    let finished = evict::remove(&candidate, &mut || {}, Some(Instant::now())).unwrap();
    assert!(!finished, "the deadline stopped the deletion");
    assert!(
        !cache.join("symbol-index/big").exists(),
        "gone from its live location"
    );
    let aside: Vec<String> = super::read_names(cache)
        .into_iter()
        .filter(|name| name.starts_with(".aethyme-storage-evicting-"))
        .collect();
    assert_eq!(aside.len(), 1, "the remainder is recorded by its name");
    evict::sweep_aside(cache, &mut || {});
    assert!(
        super::read_names(cache)
            .iter()
            .all(|name| !name.starts_with(".aethyme-storage-evicting-"))
    );
}

fn manifest_archive(
    host_state: &Path,
    id: &str,
    created_days_ago: Option<u64>,
    file_days_ago: u64,
) -> PathBuf {
    let path = host_state.join("recovery-archives/example-0123").join(id);
    let mut manifest = serde_json::json!({
        "session_id": 42,
        "head": "0".repeat(40),
        "staged_patch": {"bytes": 1},
        "unstaged_patch": {"bytes": 0},
        "untracked_files": [],
        "ignored_files": [],
    });
    if let Some(days) = created_days_ago {
        manifest["created_at_ms"] = serde_json::json!(super::now_ms() - days * DAY_MS);
    }
    file(&path.join("bundle.bin"), 8 * 1024);
    fs::write(path.join("manifest.json"), manifest.to_string()).unwrap();
    age(&path, file_days_ago);
    path
}

fn archive_child(
    created_days_ago: Option<u64>,
    min_retention_days: Option<u64>,
    kept: bool,
) -> super::Child {
    super::Child {
        name: "example-0123/archive".into(),
        bytes: 1,
        newest_ms: 0,
        ctime_ms: 0,
        created_ms: created_days_ago.map(|days| super::now_ms() - days * DAY_MS),
        min_retention_ms: min_retention_days.map(|days| days * DAY_MS),
        session_id: Some(1),
        kept,
    }
}

#[test]
fn archive_expiry_is_from_the_manifest_with_retention_keep_and_off_switch() {
    let budgets = Budgets::default();
    let created = |child: &super::Child| child.created_ms.unwrap();
    let child = archive_child(Some(40), None, false);
    assert_eq!(
        super::archive_expiry(&child, &budgets),
        Some(created(&child) + 90 * DAY_MS)
    );
    let child = archive_child(Some(40), Some(120), false);
    assert_eq!(
        super::archive_expiry(&child, &budgets),
        Some(created(&child) + 120 * DAY_MS)
    );
    assert_eq!(
        super::archive_expiry(&archive_child(Some(400), None, true), &budgets),
        None
    );
    assert_eq!(
        super::archive_expiry(&archive_child(None, None, false), &budgets),
        None
    );
    let off = Budgets {
        unlanded_archive_days: 0,
        ..Budgets::default()
    };
    assert_eq!(
        super::archive_expiry(&archive_child(Some(400), None, false), &off),
        None
    );
}

#[test]
fn a_notice_is_served_after_a_week_and_two_passes_a_day_apart() {
    let now = super::now_ms();
    let child = archive_child(Some(400), None, false);
    let budgets = Budgets::default();
    let mut notice = super::Notice::new(now - 8 * DAY_MS);
    assert!(
        !super::archive_deletable(&child, &budgets, Some(&notice), now),
        "one pass only"
    );
    notice.publish(now - 8 * DAY_MS + 3_600_000);
    assert_eq!(
        notice.passes, 1,
        "a second publication within a day does not count"
    );
    notice.publish(now - 6 * DAY_MS);
    assert_eq!(notice.passes, 2);
    assert!(super::archive_deletable(
        &child,
        &budgets,
        Some(&notice),
        now
    ));
    let recent = super::Notice {
        noticed_at_ms: now - 6 * DAY_MS,
        passes: 5,
        last_published_ms: now,
    };
    assert!(
        !super::archive_deletable(&child, &budgets, Some(&recent), now),
        "under a week"
    );
    assert!(
        !super::archive_deletable(&child, &budgets, None, now),
        "never announced"
    );
}

#[test]
fn implausible_creation_times_are_unknown() {
    let dir = tempfile::tempdir().unwrap();
    for (name, created) in [
        ("too-old", serde_json::json!(1_000)),
        ("future", serde_json::json!(super::now_ms() + 30 * DAY_MS)),
        ("text", serde_json::json!("yesterday")),
    ] {
        let path = dir.path().join(name);
        fs::create_dir_all(&path).unwrap();
        fs::write(
            path.join("manifest.json"),
            serde_json::json!({"created_at_ms": created}).to_string(),
        )
        .unwrap();
        assert_eq!(super::archive_meta(&path).created_ms, None, "{name}");
    }
    let path = dir.path().join("plausible");
    fs::create_dir_all(&path).unwrap();
    let created = super::now_ms() - DAY_MS;
    fs::write(
        path.join("manifest.json"),
        serde_json::json!({"created_at_ms": created}).to_string(),
    )
    .unwrap();
    assert_eq!(super::archive_meta(&path).created_ms, Some(created));
}

/// Move every notice and the last pass back in time, as if `days` passed.
fn time_passes(host_state: &Path, notice_days: u64, last_pass_hours: u64) {
    let mut ledger = super::Ledger::load(host_state);
    for notice in ledger.archive_notices.values_mut() {
        notice.noticed_at_ms -= notice_days * DAY_MS;
        notice.last_published_ms -= 25 * 3_600_000;
    }
    ledger.updated_at_ms -= last_pass_hours * 3_600_000;
    ledger.save(host_state).unwrap();
}

#[test]
fn expiry_needs_two_notices_a_day_apart_and_a_week_and_skips_kept_and_undated() {
    let dir = tempfile::tempdir().unwrap();
    let host_state = dir.path();
    empty_registry(host_state);
    // Created 40 days ago with files touched today: age is the manifest's.
    let old = manifest_archive(host_state, "old-unlanded", Some(100), 0);
    // Created 5 days ago with year-old files: not expiring.
    let young = manifest_archive(host_state, "young-unlanded", Some(5), 365);
    let kept = manifest_archive(host_state, "kept-unlanded", Some(100), 0);
    let undated = manifest_archive(host_state, "undated-unlanded", None, 365);
    super::cli::keep_archive(host_state, "example-0123/kept-unlanded", true).unwrap();
    let roots = || vec![root(host_state, RootKind::HostState)];

    // First pass: announced, nothing removed.
    let ledger = pass(host_state, roots(), Budgets::default());
    assert!(ledger.last_pass.unwrap().evicted.is_empty());
    let summary = Summary::load(host_state);
    assert_eq!(summary.archives_expiring_total, 1, "{summary:?}");
    assert_eq!(summary.archives_expiring[0].id, "example-0123/old-unlanded");
    assert_eq!(summary.archives_expiring[0].session_id, Some(42));
    let advice = super::cli::status_advice(host_state);
    let expiring = advice
        .iter()
        .find(|row| row.id == "storage.archives-expiring")
        .expect("advice");
    assert_eq!(
        expiring.commands[0],
        "aethyme storage keep-archive example-0123/old-unlanded"
    );

    // A week later but only one publication so far: not yet; this pass
    // publishes the second notice.
    time_passes(host_state, 8, 25);
    let ledger = pass(host_state, roots(), Budgets::default());
    assert!(
        ledger.last_pass.unwrap().evicted.is_empty(),
        "one notice is not enough"
    );
    assert!(old.exists());
    assert_eq!(
        super::Ledger::load(host_state).archive_notices["example-0123/old-unlanded"].passes,
        2
    );

    // Two notices a day apart, a week since the first: an archive holding
    // unlanded work is still kept at its age, and named as such.
    time_passes(host_state, 0, 25);
    let ledger = pass(host_state, roots(), Budgets::default());
    let report = ledger.last_pass.unwrap();
    assert!(report.evicted.is_empty(), "{report:?}");
    assert!(old.exists(), "age alone never destroys unlanded work");
    assert_eq!(
        report.unlanded_archives,
        vec!["recovery-archives/example-0123/old-unlanded".to_string()],
        "{report:?}"
    );
    let advice = super::cli::status_advice(host_state);
    let kept_row = advice
        .iter()
        .find(|row| row.id == "storage.unlanded-archives-at-age")
        .expect("advice names the kept archive");
    assert!(
        kept_row
            .evidence
            .iter()
            .any(|line| line.contains("session 42")),
        "{kept_row:?}"
    );

    // Its owner dismisses it: it goes.
    super::cli::dismiss_archive(host_state, "example-0123/old-unlanded").unwrap();
    time_passes(host_state, 0, 25);
    let ledger = pass(host_state, roots(), Budgets::default());
    let report = ledger.last_pass.unwrap();
    assert_eq!(
        report.evicted,
        vec!["recovery_archives:recovery-archives/example-0123/old-unlanded".to_string()],
        "{report:?}"
    );
    assert!(young.exists() && kept.exists() && undated.exists());
    assert_eq!(ledger.archive_telemetry.expired, 1);
    assert_eq!(ledger.archive_telemetry.kept, 1);
    assert!(super::keep_marker(&kept).exists());
    assert_eq!(
        super::cli::archives_value(host_state)["archives"]
            .as_array()
            .unwrap()
            .len(),
        3
    );

    // Unkept, it needs a notice of its own first.
    super::cli::keep_archive(host_state, "example-0123/kept-unlanded", false).unwrap();
    pass(host_state, roots(), Budgets::default());
    assert!(kept.exists(), "a fresh notice, not an immediate removal");
    assert!(
        undated.exists(),
        "an archive without a creation time is never removed by age"
    );
}

fn eligible_after_two_notices(host_state: &Path, count: usize) {
    empty_registry(host_state);
    for index in 0..count {
        manifest_archive(
            host_state,
            &format!("archive-{index:03}"),
            Some(100 + index as u64),
            0,
        );
    }
    let roots = vec![root(host_state, RootKind::HostState)];
    pass(host_state, roots.clone(), Budgets::default());
    time_passes(host_state, 8, 25);
    pass(host_state, roots, Budgets::default());
    // Served notices; the last pass was a day ago.
    time_passes(host_state, 0, 25);
}

#[test]
fn a_long_gap_or_a_clock_change_removes_nothing_and_restarts_notices() {
    for (case, shift) in [
        ("machine off for over a week", 8 * 24),
        ("clock went back", 0),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let host_state = dir.path();
        eligible_after_two_notices(host_state, 2);
        let mut ledger = super::Ledger::load(host_state);
        if shift > 0 {
            ledger.updated_at_ms -= shift * 3_600_000;
        } else {
            ledger.updated_at_ms = super::now_ms() + 2 * DAY_MS;
        }
        ledger.save(host_state).unwrap();
        let ledger = pass(
            host_state,
            vec![root(host_state, RootKind::HostState)],
            Budgets::default(),
        );
        let report = ledger.last_pass.unwrap();
        assert!(report.evicted.is_empty(), "{case}: {report:?}");
        assert!(
            report
                .skipped
                .iter()
                .any(|line| line.contains("age expiry paused")),
            "{case}"
        );
        let notices = super::Ledger::load(host_state).archive_notices;
        assert_eq!(notices.len(), 2, "{case}");
        assert!(notices.values().all(|notice| notice.passes == 1
            && super::now_ms() - notice.noticed_at_ms < DAY_MS), "{case}: notices restart");
    }
}

#[test]
fn age_expiry_is_capped_per_pass_oldest_first() {
    let dir = tempfile::tempdir().unwrap();
    let host_state = dir.path();
    eligible_after_two_notices(host_state, 55);
    // The setting lets archives holding unlanded work expire by age too.
    let ledger = pass(
        host_state,
        vec![root(host_state, RootKind::HostState)],
        Budgets {
            expire_unlanded_archives: true,
            ..Budgets::default()
        },
    );
    let report = ledger.last_pass.unwrap();
    assert_eq!(
        report.evicted.len(),
        super::AGE_EXPIRY_PER_PASS,
        "{report:?}"
    );
    // The oldest (highest index: created longest ago) went first.
    assert!(
        report
            .evicted
            .iter()
            .any(|label| label.ends_with("archive-054"))
    );
    assert!(
        !report
            .evicted
            .iter()
            .any(|label| label.ends_with("archive-000"))
    );
    assert_eq!(
        report
            .skipped
            .iter()
            .filter(|line| line.contains("age-expiry cap"))
            .count(),
        5
    );
}

#[test]
fn keep_archive_refuses_anything_but_an_archive_and_never_follows_a_link() {
    let dir = tempfile::tempdir().unwrap();
    let host_state = dir.path();
    let real = manifest_archive(host_state, "real", Some(1), 0);
    for id in [
        "../outside",
        "example-0123",
        "example-0123/missing",
        "a/b/c",
        "example-0123/real.aethyme-keep",
    ] {
        assert!(
            super::cli::keep_archive(host_state, id, true).is_err(),
            "{id}"
        );
    }
    // A link where the marker goes: refused, and its target never written.
    let target = dir.path().join("elsewhere");
    std::os::unix::fs::symlink(&target, super::keep_marker(&real)).unwrap();
    assert!(super::cli::keep_archive(host_state, "example-0123/real", true).is_err());
    assert!(!target.exists());
    fs::remove_file(super::keep_marker(&real)).unwrap();
    assert!(
        super::cli::keep_archive(host_state, "recovery-archives/example-0123/real", true).is_ok()
    );
    // Already kept: fine, unchanged.
    assert!(super::cli::keep_archive(host_state, "example-0123/real", true).is_ok());
}

#[test]
fn only_the_update_staging_copy_or_a_temporary_copy_counts_as_ephemeral() {
    let temp = std::env::temp_dir();
    let staged = temp
        .join(".tmpAbC123")
        .join(crate::update::PAYLOAD_DIRECTORY)
        .join("aethyme");
    let elsewhere_payload = Path::new("/opt/projects/payload/aethyme");
    let installed = Path::new("/opt/aethyme-installed/bin/aethyme");
    assert!(check::update_payload(&staged));
    assert!(check::ephemeral_executable(&staged));
    assert!(
        !check::update_payload(elsewhere_payload),
        "a directory merely named payload"
    );
    assert!(!check::ephemeral_executable(elsewhere_payload));
    assert!(!check::ephemeral_executable(installed));
    let words = |text: &str| text.split(' ').map(str::to_string).collect::<Vec<_>>();
    assert!(!check::start_check_allowed(
        &words("quick-test"),
        Some(installed)
    ));
    assert!(!check::start_check_allowed(
        &words("status --json"),
        Some(&staged)
    ));
    assert!(!check::start_check_allowed(&words("status"), None));
    assert!(check::start_check_allowed(
        &words("status --json"),
        Some(installed)
    ));
}

#[test]
fn the_worker_starts_only_from_an_install_of_this_build() {
    let installed = Path::new("/opt/aethyme-installed/bin/aethyme");
    let never = |_: &Path| false;
    // A real install runs it.
    assert_eq!(
        check::worker_executable(installed, &[], &never),
        Some(installed.to_path_buf())
    );
    // A temporary copy: only a known location of this exact build.
    let staged = std::env::temp_dir()
        .join(".tmpXyZ")
        .join("payload")
        .join("aethyme");
    let dir = tempfile::tempdir().unwrap();
    let other = dir.path().join("bin/aethyme");
    file(&other, 1);
    // `other` sits under the temporary directory too, so make a location
    // that is not: none qualifies without the build match.
    assert_eq!(
        check::worker_executable(&staged, std::slice::from_ref(&other), &never),
        None
    );
    assert_eq!(
        check::worker_executable(&staged, &[], &|_| true),
        None,
        "no candidate"
    );
    let locations = check::install_locations(Some(Path::new("/home/someone")));
    assert_eq!(
        locations[0],
        PathBuf::from("/home/someone/.local/bin/aethyme")
    );
}

#[test]
fn storage_status_shows_paused_age_expiry_and_restarted_notices() {
    let dir = tempfile::tempdir().unwrap();
    let host_state = dir.path();
    eligible_after_two_notices(host_state, 2);
    let status = super::cli::status_value(host_state);
    assert_eq!(status["archive_expiry"]["paused_total"], 0);
    let mut ledger = super::Ledger::load(host_state);
    ledger.updated_at_ms -= 8 * DAY_MS;
    ledger.save(host_state).unwrap();
    pass(
        host_state,
        vec![root(host_state, RootKind::HostState)],
        Budgets::default(),
    );
    let status = super::cli::status_value(host_state);
    let expiry = &status["archive_expiry"];
    assert_eq!(expiry["paused_total"], 1, "{expiry}");
    assert_eq!(expiry["notices_reset_total"], 2, "{expiry}");
    assert_eq!(expiry["last_pass_paused"], "gap_over_7_days");
    assert_eq!(expiry["last_pass_notices_reset"], 2);
    // The next normal pass is not paused; the cumulative count stays.
    pass(
        host_state,
        vec![root(host_state, RootKind::HostState)],
        Budgets::default(),
    );
    let status = super::cli::status_value(host_state);
    assert_eq!(status["archive_expiry"]["paused_total"], 1);
    assert!(status["archive_expiry"]["last_pass_paused"].is_null());
}

/// Thousands of archives kept for unlanded work made `storage.over-budget`
/// most of the full status document; it now names a few and counts the rest.
#[test]
fn unlanded_archive_evidence_names_a_few_and_counts_the_rest() {
    let few: Vec<String> = (0..3).map(|index| format!("pool/{index}")).collect();
    assert_eq!(super::cli::unlanded_archive_evidence(&few).len(), 3);

    let many: Vec<String> = (0..4_000).map(|index| format!("pool/{index:04}")).collect();
    let evidence = super::cli::unlanded_archive_evidence(&many);
    assert_eq!(evidence.len(), 6);
    assert_eq!(
        evidence[0],
        "recovery archive pool/0000 holds unlanded work and is kept"
    );
    assert!(evidence[5].starts_with("and 3995 more recovery archive(s)"));
    assert!(evidence[5].contains("(4000 in all)"));
    assert!(evidence[5].contains("aethyme storage status --json"));
    assert!(serde_json::to_vec(&evidence).unwrap().len() < 1024);
}

/// Session scratch usage is its own reported class, never unclassified, and
/// advises only above its thresholds.
#[test]
fn session_scratch_usage_is_reported_and_advised_when_large() {
    let owner = tempfile::tempdir().unwrap();
    let host_state = owner.path();
    assert_eq!(
        classify(RootKind::HostState, super::SESSION_SCRATCH_DIRECTORY),
        Class::Coordination
    );
    let small = super::SessionScratchUsage {
        directories: 2,
        bytes: 1024,
        largest_bytes: 1000,
        largest_session_id: Some(7),
        observed_at_ms: 1,
        ..super::SessionScratchUsage::default()
    };
    super::record_session_scratch_usage(host_state, "/repo/a", small.clone()).unwrap();
    assert_eq!(
        super::session_scratch_usage(host_state).get("/repo/a"),
        Some(&small)
    );
    let status = super::cli::status_value(host_state);
    assert_eq!(status["classes"]["session_scratch"]["directories"], 2);
    let advice = super::cli::status_advice(host_state);
    assert!(
        advice
            .iter()
            .all(|row| row.id != "storage.session-scratch-large"),
        "small usage does not advise"
    );
    let large = super::SessionScratchUsage {
        bytes: super::SESSION_SCRATCH_ADVICE_BYTES + 1,
        largest_bytes: super::SESSION_SCRATCH_ADVICE_ONE_BYTES + 1,
        largest_session_id: Some(9),
        ..small
    };
    super::record_session_scratch_usage(host_state, "/repo/b", large).unwrap();
    let advice = super::cli::status_advice(host_state);
    let row = advice
        .iter()
        .find(|row| row.id == "storage.session-scratch-large")
        .expect("advice");
    assert_eq!(row.evidence.len(), 1, "{row:?}");
    assert!(row.evidence[0].contains("session 9"), "{row:?}");
    assert!(
        advice
            .iter()
            .all(|row| row.id != "storage.command-temp-backlog"),
        "no finished runs, no backlog advice"
    );
    let backlog = super::SessionScratchUsage {
        finished_runs: super::COMMAND_TEMP_BACKLOG_ADVICE_RUNS,
        finished_run_bytes: 2048,
        ..super::SessionScratchUsage::default()
    };
    super::record_session_scratch_usage(host_state, "/repo/c", backlog).unwrap();
    let advice = super::cli::status_advice(host_state);
    let row = advice
        .iter()
        .find(|row| row.id == "storage.command-temp-backlog")
        .expect("backlog advice");
    assert_eq!(row.evidence.len(), 1, "{row:?}");
    assert!(row.evidence[0].contains("64 finished"), "{row:?}");
}

/// A gate cache prune stages its units under the host cache root. The
/// staged entry is listed with the size its stager recorded, shown in
/// `broker status` once it has waited, makes the next command start a
/// worker, and goes with the worker's sweep (its size record too).
#[test]
fn a_staged_prune_is_advised_starts_the_worker_and_is_swept() {
    let dir = tempfile::tempdir().unwrap();
    let host_state = dir.path().join("state");
    let cache = dir.path().join("cache");
    fs::create_dir_all(&host_state).unwrap();
    fs::create_dir_all(cache.join("gates")).unwrap();
    let name = ".aethyme-storage-evicting-14616-1795248476768-pruned";
    let staged = cache.join(name);
    fs::create_dir_all(staged.join("000001-unit")).unwrap();
    fs::write(staged.join("000001-unit/lib"), b"artefact").unwrap();
    fs::write(cache.join(format!("{name}.bytes")), "5368709120").unwrap();

    let found = check::staged_deletions(&cache);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].name, name);
    assert_eq!(found[0].bytes, Some(5_368_709_120));
    assert!(
        super::cli::staged_deletion_advice(found.clone()).is_none(),
        "a fresh staging is not advice yet"
    );
    let waited: Vec<_> = found
        .into_iter()
        .map(|mut staged| {
            staged.age_ms = 10 * 60_000;
            staged
        })
        .collect();
    let advice = super::cli::staged_deletion_advice(waited).unwrap();
    assert_eq!(advice.id, "storage.staged-deletion-pending");
    assert!(
        advice.summary.contains("awaiting deletion"),
        "{}",
        advice.summary
    );

    // A staging that waited past the threshold starts a worker even while
    // the ledger is fresh.
    let old = SystemTime::now() - Duration::from_secs(10 * 60);
    fs::File::open(&staged).unwrap().set_modified(old).unwrap();
    Summary {
        updated_at_ms: super::now_ms(),
        roots: vec![
            Root::new(&host_state, RootKind::HostState),
            Root::new(&cache, RootKind::HostCache),
        ],
        ..Summary::default()
    }
    .save(&host_state)
    .unwrap();
    let mut kicks = 0;
    let decision = check::decide(&host_state, &[], None, &mut |_| Some(u64::MAX), &mut || {
        kicks += 1;
    });
    assert!(decision.kick, "{decision:?}");
    assert_eq!(kicks, 1);

    evict::sweep_aside(&cache, &mut || {});
    assert!(check::staged_deletions(&cache).is_empty());
    assert!(!cache.join(format!("{name}.bytes")).exists());
    assert!(cache.join("gates").is_dir(), "only staged entries go");
}

/// A repository with `main` at commit A (also `origin/main`) and a branch
/// `work` one commit ahead (B): `(A, B)`.
fn landing_repository(repository: &Path) -> (String, String) {
    fs::create_dir_all(repository).unwrap();
    git(repository, &["init", "-q", "-b", "main"]);
    fs::write(repository.join("a"), "a").unwrap();
    git(repository, &["add", "a"]);
    git(repository, &["commit", "-q", "-m", "a"]);
    let a = git(repository, &["rev-parse", "HEAD"]);
    git(repository, &["update-ref", "refs/remotes/origin/main", &a]);
    git(repository, &["checkout", "-q", "-b", "work"]);
    fs::write(repository.join("b"), "b").unwrap();
    git(repository, &["add", "b"]);
    git(repository, &["commit", "-q", "-m", "b"]);
    let b = git(repository, &["rev-parse", "HEAD"]);
    (a, b)
}

fn refusal(result: Result<(), super::landing::Refusal>) -> super::landing::Refusal {
    result.expect_err("expected a refusal")
}

/// The worker re-proved every archive on every pass, one `merge-base` per
/// recorded commit (head and branch tip are usually the same commit). A
/// commit is now probed once per tip; a forward move of the tip carries the
/// answer with one `rev-list`, and any other move drops what was known.
#[test]
fn a_landing_answer_is_probed_once_per_tip_and_carried_along_a_forward_move() {
    let dir = tempfile::tempdir().unwrap();
    let repository = dir.path().join("repository");
    let (a, b) = landing_repository(&repository);
    let mut prover = super::landing::LandingProver::default();

    prover
        .prove(&repository, &[a.clone(), a.clone(), a.clone()])
        .unwrap();
    assert_eq!(prover.stats().probes, 1);
    assert_eq!(prover.stats().duplicate_commits, 2);
    assert_eq!(prover.stats().probe_contained, 1);
    assert_eq!(prover.stats().live_tip_checks, 1);

    let why = refusal(prover.prove(&repository, std::slice::from_ref(&b)));
    assert!(
        matches!(&why, super::landing::Refusal::NotLanded(text) if text.contains("is not in")),
        "{why:?}"
    );
    assert_eq!(prover.stats().probes, 2);
    assert_eq!(prover.stats().probe_not_contained, 1);

    // Asked again with the tip unchanged: no probe, no tip read (only the
    // live check before an `Ok`).
    assert!(prover.prove(&repository, std::slice::from_ref(&b)).is_err());
    assert!(prover.prove(&repository, std::slice::from_ref(&a)).is_ok());
    assert_eq!(prover.stats().probes, 2);
    assert_eq!(prover.stats().tip_reads, 1);
    assert_eq!(prover.stats().tip_memo_hits, 3);
    assert_eq!(prover.stats().cached_not_contained, 1);
    assert_eq!(prover.stats().cached_contained, 1);

    // The default branch moves forward onto B: B has landed, learned from
    // the range between the tips, with no new probe.
    git(&repository, &["update-ref", "refs/remotes/origin/main", &b]);
    prover.expire_tips();
    prover.prove(&repository, std::slice::from_ref(&b)).unwrap();
    assert_eq!(prover.stats().tip_advanced, 1);
    assert_eq!(prover.stats().tip_reset, 0);
    assert_eq!(prover.stats().probes, 2);
    assert_eq!(prover.stats().cached_contained, 2);

    // It moves back to A: nothing known survives, and B is probed again.
    git(&repository, &["update-ref", "refs/remotes/origin/main", &a]);
    prover.expire_tips();
    assert!(prover.prove(&repository, std::slice::from_ref(&b)).is_err());
    assert_eq!(prover.stats().tip_reset, 1);
    assert_eq!(prover.stats().probes, 3);
    assert_eq!(prover.stats().probe_not_contained, 2);
}

/// A remembered tip never lends "contained" to a removal: rewound inside
/// the memo's lifetime, the next answer is checked against the live tip.
#[test]
fn a_rewound_default_branch_refuses_within_the_tip_memo() {
    let dir = tempfile::tempdir().unwrap();
    let repository = dir.path().join("repository");
    let (a, b) = landing_repository(&repository);
    git(&repository, &["update-ref", "refs/remotes/origin/main", &b]);
    let mut prover = super::landing::LandingProver::default();
    prover.prove(&repository, std::slice::from_ref(&b)).unwrap();

    // Rewound with the memo still fresh (no `expire_tips`).
    git(&repository, &["update-ref", "refs/remotes/origin/main", &a]);
    let why = refusal(prover.prove(&repository, std::slice::from_ref(&b)));
    assert!(
        matches!(why, super::landing::Refusal::NotLanded(_)),
        "{why:?}"
    );
    assert_eq!(prover.stats().live_tip_moved, 1);
    assert_eq!(prover.stats().tip_reset, 1);
}

/// Only "not landed" crosses processes, and an archive with one known
/// unlanded commit costs no probe, whichever commit comes first.
#[test]
fn only_not_landed_answers_are_kept_across_processes() {
    let dir = tempfile::tempdir().unwrap();
    let host_state = dir.path().join("state");
    fs::create_dir_all(&host_state).unwrap();
    let repository = dir.path().join("repository");
    let (a, b) = landing_repository(&repository);
    let mut first = super::landing::LandingProver::default();
    // Head contained, branch tip not.
    assert!(first.prove(&repository, &[a.clone(), b.clone()]).is_err());
    assert_eq!(first.stats().probes, 2);
    first.save(&host_state).unwrap();

    let mut next = super::landing::LandingProver::default();
    next.load(&host_state);
    assert!(next.prove(&repository, &[a.clone(), b.clone()]).is_err());
    assert_eq!(next.stats().probes, 0, "B is known not landed");
    assert_eq!(next.stats().cached_not_contained, 1);
    next.prove(&repository, std::slice::from_ref(&a)).unwrap();
    assert_eq!(next.stats().probes, 1, "A is proven again, never read back");

    // A commit nobody asks about any more drops out of the file.
    let mut quiet = super::landing::LandingProver::default();
    quiet.load(&host_state);
    quiet.prove(&repository, std::slice::from_ref(&a)).unwrap();
    quiet.save(&host_state).unwrap();
    let mut after = super::landing::LandingProver::default();
    after.load(&host_state);
    assert!(after.prove(&repository, std::slice::from_ref(&b)).is_err());
    assert_eq!(after.stats().probes, 1, "B was dropped, so it is probed");

    // A damaged file is ignored, never trusted.
    fs::write(host_state.join(super::landing::CACHE_FILE), b"{not json").unwrap();
    let mut damaged = super::landing::LandingProver::default();
    damaged.load(&host_state);
    assert!(
        damaged
            .prove(&repository, std::slice::from_ref(&b))
            .is_err()
    );
    assert_eq!(damaged.stats().probes, 1);
}

/// A probe that outlives its budget proves nothing, is not remembered, and
/// is unproven rather than unlanded; once the process budget is spent, no
/// Git read runs at all, tip reads included.
#[test]
fn landing_probes_are_bounded_and_an_unfinished_one_keeps_the_archive() {
    let dir = tempfile::tempdir().unwrap();
    let repository = dir.path().join("repository");
    let (a, _) = landing_repository(&repository);
    let mut prover =
        super::landing::LandingProver::with_budgets(Duration::ZERO, Duration::from_secs(600));
    for attempt in 1..=2 {
        let why = refusal(prover.prove(&repository, std::slice::from_ref(&a)));
        assert!(
            matches!(&why, super::landing::Refusal::Unproven(text) if text.contains("time budget")),
            "{why:?}"
        );
        assert_eq!(prover.stats().probe_timeouts, attempt);
        assert_eq!(prover.stats().cached_contained, 0);
        assert_eq!(prover.stats().cached_not_contained, 0);
    }

    let mut spent =
        super::landing::LandingProver::with_budgets(Duration::from_secs(20), Duration::ZERO);
    let why = refusal(spent.prove(&repository, std::slice::from_ref(&a)));
    assert!(
        matches!(&why, super::landing::Refusal::Unproven(text) if text.contains("proof budget is spent")),
        "{why:?}"
    );
    assert_eq!(spent.stats().probes, 0);
    assert_eq!(spent.stats().budget_spent, 1);
}

/// Replace refs and grafts can make a commit look like an ancestor of the
/// default branch; the proof reads the real commits only.
#[test]
fn a_replace_ref_faking_ancestry_does_not_prove_landing() {
    let dir = tempfile::tempdir().unwrap();
    let repository = dir.path().join("repository");
    let (a, b) = landing_repository(&repository);
    // With the replacement, A's parent is B: B looks contained in main.
    git(&repository, &["replace", "--graft", &a, &b]);
    // Plain Git is fooled (the helper asserts success).
    git(&repository, &["merge-base", "--is-ancestor", &b, &a]);
    let mut prover = super::landing::LandingProver::default();
    let why = refusal(prover.prove(&repository, std::slice::from_ref(&b)));
    assert!(
        matches!(why, super::landing::Refusal::NotLanded(_)),
        "{why:?}"
    );
}

/// An archive whose head and branch tip are one commit costs one probe,
/// not two, and still lands.
#[test]
fn an_archive_repeating_one_commit_is_probed_once() {
    let dir = tempfile::tempdir().unwrap();
    let host_state = dir.path().join("state");
    let repository = dir.path().join("repository");
    let (a, _) = landing_repository(&repository);
    archive(&host_state, "landed", &repository, &a, 40);
    let mut prover = super::landing::LandingProver::default();
    evict::archive_landed_with(
        &host_state.join("recovery-archives/example-0123/landed"),
        &mut prover,
    )
    .unwrap();
    assert_eq!(prover.stats().probes, 1);
    assert_eq!(prover.stats().duplicate_commits, 1);
}
