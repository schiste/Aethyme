//! Placement on an adopted storage root: resolution, readers, migration.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::*;
use crate::storage_ledger::check;

/// A host state directory and an adopted storage root beside it.
struct Fixture {
    _dir: tempfile::TempDir,
    host_state: PathBuf,
    storage: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        set_local_for_test(Some(true));
        let dir = tempfile::tempdir().unwrap();
        let host_state = dir.path().join("state");
        let storage = dir.path().join("storage");
        fs::create_dir_all(&host_state).unwrap();
        fs::create_dir_all(&storage).unwrap();
        adopt(&host_state, &storage);
        Self {
            _dir: dir,
            host_state,
            storage,
        }
    }

    fn configure(&self, text: &str) {
        fs::write(self.host_state.join(BUDGET_FILE), text).unwrap();
    }

    fn host_archive(&self, id: &str) -> PathBuf {
        self.host_state.join(RECOVERY_ARCHIVE_DIRECTORY).join(id)
    }

    fn stored_archive(&self, id: &str) -> PathBuf {
        self.storage.join(RECOVERY_ARCHIVE_DIRECTORY).join(id)
    }

    fn migrate(&self) -> PlacementReport {
        self.migrate_until(Instant::now() + Duration::from_secs(60), &mut || true)
    }

    fn migrate_until(&self, deadline: Instant, alive: &mut dyn FnMut() -> bool) -> PlacementReport {
        let mut report = PlacementReport::default();
        migrate_archives(
            &self.host_state,
            &self.storage,
            deadline,
            Duration::ZERO,
            alive,
            &mut report,
        );
        report
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        set_local_for_test(None);
        CRASH_FOR_TEST.with(|cell| cell.set(None));
        FORCE_COPY_FOR_TEST.with(|cell| cell.set(false));
    }
}

/// What `aethyme storage adopt` leaves: the marker, and the registration.
fn adopt(host_state: &Path, root: &Path) {
    fs::write(root.join(STORAGE_ROOT_MARKER), "adopted\n").unwrap();
    check::register_root(host_state, root, RootKind::Storage, None);
}

/// A complete archive naming `repository`, with a nested file and a link.
fn archive(path: &Path, repository: &Path) {
    fs::create_dir_all(path.join("untracked/deep")).unwrap();
    fs::write(
        path.join("manifest.json"),
        serde_json::json!({"repository": repository, "created_at_ms": 1_800_000_000_000_u64})
            .to_string(),
    )
    .unwrap();
    fs::write(path.join("commits.bundle"), vec![3_u8; 200_000]).unwrap();
    fs::write(path.join("untracked/deep/notes.txt"), "work in progress\n").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("deep/notes.txt", path.join("untracked/link")).unwrap();
}

fn entries(path: &Path) -> BTreeMap<PathBuf, Entry> {
    tree_entries(path, Instant::now() + Duration::from_secs(60))
        .unwrap()
        .unwrap()
}

#[test]
fn placement_is_the_host_unless_configured_and_qualified() {
    let fixture = Fixture::new();
    let host = resolve(&fixture.host_state, Data::RecoveryArchives);
    assert_eq!(host.on_storage(), None);
    assert_eq!(host.fallback, None, "the default is no fallback");

    fixture.configure("[placement]\nrecovery_archives = \"storage\"\n");
    let archives = resolve(&fixture.host_state, Data::RecoveryArchives);
    assert_eq!(archives.on_storage(), Some(fixture.storage.as_path()));
    let gates = resolve(&fixture.host_state, Data::GateCaches);
    assert_eq!(
        gates.on_storage(),
        None,
        "each kind is configured on its own"
    );
    assert_eq!(gates.fallback, None);

    // `[budget]` still reads beside `[placement]`.
    fixture.configure("[budget]\nfree_floor_bytes = 5\n\n[placement]\ngate_caches = \"storage\"\n");
    let (budgets, error) = crate::storage_ledger::Budgets::load(&fixture.host_state);
    assert_eq!(error, None);
    assert_eq!(budgets.free_floor_bytes, 5);
    assert!(
        resolve(&fixture.host_state, Data::GateCaches)
            .on_storage()
            .is_some()
    );
}

#[test]
fn only_an_adopted_local_real_directory_of_this_user_qualifies() {
    let fixture = Fixture::new();
    assert_eq!(qualify(&fixture.storage), Ok(()));

    // Missing (an unmounted volume), or not a directory.
    assert_eq!(
        qualify(&fixture.storage.join("absent")),
        Err(Fallback::Unavailable)
    );
    let plain = fixture.storage.join("plain-file");
    fs::write(&plain, "x").unwrap();
    assert_eq!(qualify(&plain), Err(Fallback::Unavailable));

    // No marker, or a marker that is not a plain file.
    let unadopted = fixture.storage.join("unadopted");
    fs::create_dir(&unadopted).unwrap();
    assert_eq!(qualify(&unadopted), Err(Fallback::NotAdopted));
    fs::create_dir(unadopted.join(STORAGE_ROOT_MARKER)).unwrap();
    assert_eq!(qualify(&unadopted), Err(Fallback::NotAdopted));

    // A link to an adopted root is not one.
    #[cfg(unix)]
    {
        let link = fixture.storage.with_file_name("storage-link");
        std::os::unix::fs::symlink(&fixture.storage, &link).unwrap();
        assert_eq!(qualify(&link), Err(Fallback::Symlink));
    }

    // A network file system.
    set_local_for_test(Some(false));
    assert_eq!(qualify(&fixture.storage), Err(Fallback::NotLocal));
}

#[test]
fn a_storage_placement_falls_back_to_the_host_and_says_why() {
    let fixture = Fixture::new();
    let fallback = |fixture: &Fixture| resolve(&fixture.host_state, Data::RecoveryArchives);

    // A relative root, an invalid table.
    fixture.configure("[placement]\nrecovery_archives = \"storage\"\nstorage_root = \"rel\"\n");
    assert_eq!(fallback(&fixture).fallback, Some(Fallback::ConfigInvalid));
    fixture.configure("[placement]\nrecovery_archives = \"elsewhere\"\n");
    assert_eq!(fallback(&fixture).fallback, Some(Fallback::ConfigInvalid));

    // The root disappears (its volume unmounted).
    fixture.configure("[placement]\nrecovery_archives = \"storage\"\n");
    let away = fixture.storage.with_file_name("storage-away");
    fs::rename(&fixture.storage, &away).unwrap();
    let resolved = fallback(&fixture);
    assert_eq!(resolved.on_storage(), None);
    assert_eq!(resolved.fallback, Some(Fallback::Unavailable));
    fs::rename(&away, &fixture.storage).unwrap();

    // Two adopted roots and none named: ambiguous; naming one settles it.
    let second = fixture.storage.with_file_name("second");
    fs::create_dir(&second).unwrap();
    adopt(&fixture.host_state, &second);
    assert_eq!(
        fallback(&fixture).fallback,
        Some(Fallback::AmbiguousStorageRoot)
    );
    fixture.configure(&format!(
        "[placement]\nrecovery_archives = \"storage\"\nstorage_root = {:?}\n",
        second.to_string_lossy()
    ));
    assert_eq!(fallback(&fixture).on_storage(), Some(second.as_path()));

    // No root adopted at all.
    let bare = tempfile::tempdir().unwrap();
    fs::write(
        bare.path().join(BUDGET_FILE),
        "[placement]\nrecovery_archives = \"storage\"\n",
    )
    .unwrap();
    assert_eq!(
        resolve(bare.path(), Data::RecoveryArchives).fallback,
        Some(Fallback::NoStorageRoot)
    );
}

/// The storage worker inventories placed archives and gate caches as it
/// does in host state, and `storage keep-archive` finds an archive there.
#[test]
fn placed_data_is_classified_and_found_like_host_data() {
    use crate::storage_ledger::{Class, classify};
    assert_eq!(
        classify(RootKind::Storage, RECOVERY_ARCHIVE_DIRECTORY),
        Class::RecoveryArchives
    );
    assert_eq!(
        classify(RootKind::Storage, GATE_CACHE_DIRECTORY),
        Class::GateCache
    );
    assert_eq!(
        classify(RootKind::HostState, ARCHIVE_STORAGE_ROOTS_FILE),
        Class::Coordination
    );
    assert_eq!(
        classify(RootKind::HostState, MIGRATION_STATE_FILE),
        Class::Coordination
    );

    let fixture = Fixture::new();
    let repository = fixture.storage.with_file_name("gone-repository");
    archive(&fixture.host_archive("key/1-host"), &repository);
    archive(&fixture.stored_archive("key/2-stored"), &repository);
    let found = |id: &str| crate::storage_ledger::cli::archive_path(&fixture.host_state, id);
    assert_eq!(
        found("key/1-host").unwrap(),
        fixture.host_archive("key/1-host")
    );
    assert_eq!(
        found("key/2-stored").unwrap(),
        fixture.stored_archive("key/2-stored")
    );
    assert!(found("key/3-nowhere").is_err());
    crate::storage_ledger::cli::keep_archive(&fixture.host_state, "key/2-stored", true).unwrap();
    assert!(crate::storage_ledger::keep_marker(&fixture.stored_archive("key/2-stored")).is_file());
}

#[test]
fn migration_moves_complete_archives_with_their_markers_and_nothing_else() {
    let fixture = Fixture::new();
    let repository = fixture.storage.with_file_name("gone-repository");
    archive(&fixture.host_archive("key/1-a"), &repository);
    fs::write(
        crate::storage_ledger::keep_marker(&fixture.host_archive("key/1-a")),
        "kept\n",
    )
    .unwrap();
    let before = entries(&fixture.host_archive("key/1-a"));
    // Being written, and incomplete: never moved.
    archive(&fixture.host_archive("key/.partial-9-x"), &repository);
    archive(&fixture.host_archive("key/9-y.partial"), &repository);
    fs::create_dir_all(fixture.host_archive("key/3-no-manifest/data")).unwrap();

    let report = fixture.migrate();

    assert_eq!(report.archives_migrated, 1, "{report:?}");
    assert_eq!(report.migration_stop.as_deref(), Some("done"));
    assert_eq!(entries(&fixture.stored_archive("key/1-a")), before);
    assert!(crate::storage_ledger::keep_marker(&fixture.stored_archive("key/1-a")).is_file());
    assert!(!fixture.host_archive("key/1-a").exists());
    assert!(!crate::storage_ledger::keep_marker(&fixture.host_archive("key/1-a")).exists());
    for left in ["key/.partial-9-x", "key/9-y.partial", "key/3-no-manifest"] {
        assert!(fixture.host_archive(left).is_dir(), "{left} stays");
        assert!(
            !fixture.stored_archive(left).exists(),
            "{left} is not moved"
        );
    }
    assert_eq!(report.archives_left, 1, "the one without a manifest");
    // Readers learn of the root before anything lands there.
    assert_eq!(
        recorded_archive_storage_roots(&fixture.host_state),
        std::slice::from_ref(&fixture.storage)
    );
    assert_eq!(
        MigrationState::load(&fixture.host_state).archives_migrated_total,
        1
    );
}

#[test]
fn a_copy_across_volumes_is_verified_and_resumes_after_a_crash() {
    let fixture = Fixture::new();
    FORCE_COPY_FOR_TEST.with(|cell| cell.set(true));
    let repository = fixture.storage.with_file_name("gone-repository");
    archive(&fixture.host_archive("key/1-a"), &repository);
    let before = entries(&fixture.host_archive("key/1-a"));

    // Copied, not yet in place: the staging copy is thrown away next time.
    CRASH_FOR_TEST.with(|cell| cell.set(Some("after_copy")));
    let report = fixture.migrate();
    assert_eq!(report.archives_migrated, 0, "{report:?}");
    assert_eq!(entries(&fixture.host_archive("key/1-a")), before);
    assert!(!fixture.stored_archive("key/1-a").exists());
    let staging = fixture
        .stored_archive("key")
        .join(format!("{MIGRATING_PREFIX}1-a.partial"));
    assert!(staging.is_dir(), "a crash leaves the staging copy");

    // In place, source not yet removed: both copies exist.
    CRASH_FOR_TEST.with(|cell| cell.set(Some("after_place")));
    let report = fixture.migrate();
    assert_eq!(report.archives_migrated, 0, "{report:?}");
    assert!(!staging.exists(), "the earlier staging copy is replaced");
    assert_eq!(entries(&fixture.stored_archive("key/1-a")), before);
    assert_eq!(entries(&fixture.host_archive("key/1-a")), before);

    // The next pass proves them identical and removes the source.
    CRASH_FOR_TEST.with(|cell| cell.set(None));
    let report = fixture.migrate();
    assert_eq!(report.archives_migrated, 1, "{report:?}");
    assert!(!fixture.host_archive("key/1-a").exists());
    assert_eq!(entries(&fixture.stored_archive("key/1-a")), before);
}

#[test]
fn a_different_archive_already_at_the_destination_keeps_the_source() {
    let fixture = Fixture::new();
    let repository = fixture.storage.with_file_name("gone-repository");
    archive(&fixture.host_archive("key/1-a"), &repository);
    archive(&fixture.stored_archive("key/1-a"), &repository);
    fs::write(fixture.stored_archive("key/1-a/commits.bundle"), "other").unwrap();

    let report = fixture.migrate();

    assert_eq!(report.archives_failed, 1, "{report:?}");
    assert!(fixture.host_archive("key/1-a/manifest.json").is_file());
}

/// A restore, retirement or archive bound holds its repository's lifecycle
/// lock: an archive of that repository is not moved under it.
#[test]
fn an_archive_whose_repository_is_mid_recovery_is_not_moved() {
    let fixture = Fixture::new();
    let repository = fixture.storage.with_file_name("repository");
    fs::create_dir_all(repository.join(".git")).unwrap();
    archive(&fixture.host_archive("key/1-a"), &repository);
    let held = crate::file_lock::try_acquire_repository_lifecycle_lock_for_storage(&repository)
        .unwrap()
        .expect("the lock is free");

    let report = fixture.migrate();

    assert_eq!(report.archives_migrated, 0, "{report:?}");
    assert_eq!(report.archives_skipped, 1, "{report:?}");
    assert!(fixture.host_archive("key/1-a/manifest.json").is_file());
    drop(held);
    assert_eq!(fixture.migrate().archives_migrated, 1);
}

/// A pass that stops (deadline, lost lock) resumes after the last archive
/// it reached.
#[test]
fn an_interrupted_migration_resumes_where_it_stopped() {
    let fixture = Fixture::new();
    let repository = fixture.storage.with_file_name("gone-repository");
    for id in ["key/1-a", "key/2-b", "key/3-c"] {
        archive(&fixture.host_archive(id), &repository);
    }

    let report = fixture.migrate_until(Instant::now(), &mut || true);
    assert_eq!(report.archives_migrated, 0);
    assert_eq!(report.migration_stop.as_deref(), Some("deadline"));

    // The worker loses its lock after the first archive.
    let mut checks = 0;
    let report = fixture.migrate_until(Instant::now() + Duration::from_secs(60), &mut || {
        checks += 1;
        checks <= 1
    });
    assert_eq!(report.archives_migrated, 1, "{report:?}");
    assert_eq!(report.migration_stop.as_deref(), Some("lock_lost"));
    assert_eq!(
        MigrationState::load(&fixture.host_state).cursor.as_deref(),
        Some("key/1-a")
    );
    assert_eq!(report.archives_left, 2);

    let report = fixture.migrate();
    assert_eq!(report.archives_migrated, 2, "{report:?}");
    assert_eq!(report.archives_left, 0);
    assert_eq!(MigrationState::load(&fixture.host_state).cursor, None);
    for id in ["key/1-a", "key/2-b", "key/3-c"] {
        assert!(fixture.stored_archive(id).join("manifest.json").is_file());
    }
}

/// With the root gone, the pass places nothing there and moves nothing.
#[test]
fn a_missing_root_moves_nothing_and_reports_the_fallback() {
    let fixture = Fixture::new();
    fixture.configure("[placement]\nrecovery_archives = \"storage\"\ngate_caches = \"storage\"\n");
    let repository = fixture.storage.with_file_name("gone-repository");
    archive(&fixture.host_archive("key/1-a"), &repository);
    let away = fixture.storage.with_file_name("storage-away");
    fs::rename(&fixture.storage, &away).unwrap();
    let mut ledger = Ledger::default();

    let report = run(
        &fixture.host_state,
        &mut ledger,
        &fixture.host_state.join("host-resources.db"),
        Duration::ZERO,
        &mut || true,
    )
    .expect("placement is configured");

    assert_eq!(report.recovery_archives, "host");
    assert_eq!(
        report.recovery_archives_fallback.as_deref(),
        Some("unavailable")
    );
    assert_eq!(report.gate_caches_fallback.as_deref(), Some("unavailable"));
    assert_eq!(report.archives_migrated, 0);
    assert!(fixture.host_archive("key/1-a/manifest.json").is_file());
    assert!(!away.join(RECOVERY_ARCHIVE_DIRECTORY).exists());

    // Nothing configured: the pass does nothing here at all.
    fs::remove_file(fixture.host_state.join(BUDGET_FILE)).unwrap();
    assert_eq!(
        run(
            &fixture.host_state,
            &mut ledger,
            &fixture.host_state.join("host-resources.db"),
            Duration::ZERO,
            &mut || true,
        ),
        None
    );
}

/// Host gate caches are evicted, not moved, once gate caches go to the
/// storage root; a leased one stays.
#[test]
fn superseded_host_gate_caches_are_evicted_unless_leased() {
    let fixture = Fixture::new();
    let cache = fixture.storage.with_file_name("cache");
    for key in ["rust-workspace-v1", "lint-v1"] {
        let path = cache.join("gates/example-0123").join(key);
        fs::create_dir_all(path.join("target")).unwrap();
        fs::write(path.join("target/blob"), vec![1_u8; 4096]).unwrap();
    }
    let registry = fixture.host_state.join("host-resources.db");
    let mut coordinator = crate::HostResourceCoordinator::open(&registry).unwrap();
    let _held = coordinator
        .acquire(&crate::HostResourceRequest {
            schema_version: crate::HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
            request_id: "gate-run".into(),
            repository: "example-0123".into(),
            worktree_fingerprint: "fingerprint".into(),
            run_id: "run".into(),
            ttl_seconds: 600,
            holder_pid: Some(std::process::id()),
            resources: vec![crate::HostResourceRequirement {
                key: "cache".into(),
                resource: crate::HostResourceKind::ExclusiveKey {
                    name: crate::gates::managed_cache_lease_name("example-0123", "lint-v1"),
                },
            }],
        })
        .unwrap();
    let mut ledger = Ledger {
        roots: vec![crate::storage_ledger::Root::new(
            &cache,
            RootKind::HostCache,
        )],
        ..Ledger::default()
    };
    crate::storage_ledger::discover(&mut ledger);
    for item in &mut ledger.items {
        crate::storage_ledger::measure_item(
            item,
            Instant::now() + Duration::from_secs(60),
            &mut || {},
        );
    }
    fixture.configure("[placement]\ngate_caches = \"storage\"\n");

    let report = run(
        &fixture.host_state,
        &mut ledger,
        &registry,
        Duration::ZERO,
        &mut || true,
    )
    .unwrap();

    assert_eq!(report.gate_caches, "storage");
    assert_eq!(report.host_gate_caches_evicted, 1, "{report:?}");
    assert!(!cache.join("gates/example-0123/rust-workspace-v1").exists());
    assert!(
        cache.join("gates/example-0123/lint-v1").is_dir(),
        "a gate still running on it keeps it"
    );
}

/// Gates resolve their managed cache directory from the placement, and a
/// missing root sends them back to the host cache.
#[test]
fn gate_caches_follow_the_placement_and_fall_back_to_the_host_cache() {
    let fixture = Fixture::new();
    let cache = fixture.storage.with_file_name("cache");
    let locations = crate::gates::GateStorageLocations::Captured {
        host_resource_db_path: Some(fixture.host_state.join("host-resources.db")),
        host_cache_dir: Some(cache.clone()),
    };
    assert_eq!(locations.gate_cache_dir(), Some(cache.clone()));
    fixture.configure("[placement]\ngate_caches = \"storage\"\n");
    assert_eq!(locations.gate_cache_dir(), Some(fixture.storage.clone()));
    let away = fixture.storage.with_file_name("storage-away");
    fs::rename(&fixture.storage, &away).unwrap();
    assert_eq!(locations.gate_cache_dir(), Some(cache));
    // No host cache captured: unavailable, as before.
    let none = crate::gates::GateStorageLocations::Captured {
        host_resource_db_path: Some(fixture.host_state.join("host-resources.db")),
        host_cache_dir: None,
    };
    assert_eq!(none.gate_cache_dir(), None);
}
