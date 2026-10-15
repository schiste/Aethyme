use super::*;
use crate::collaboration_gc::{self as gc, GcHooks};
use crate::collaboration_state::{CollaborationRoot, ProjectKey};
use crate::local_target::tests::{Fixture, git};
use std::path::Path;

struct Setup {
    f: Fixture,
    owner: Option<ControllerOwner>,
    broker: Broker,
    wave: RecordId,
    trust: std::path::PathBuf,
}
impl Drop for Setup {
    fn drop(&mut self) {
        crate::broker::gate_trust::forget_record(&self.trust);
    }
}
impl Setup {
    fn take_owner(&mut self) -> ControllerOwner {
        self.owner.take().expect("setup owner is present")
    }
}
fn setup(config: Option<&str>) -> Setup {
    let mut f = Fixture::new();
    let original = f.a.base.commit.clone();
    git(
        f._repo.path(),
        &["checkout", "-q", "--detach", original.as_str()],
    );
    if let Some(config) = config {
        std::fs::create_dir_all(f._repo.path().join(".aethyme")).unwrap();
        std::fs::write(f._repo.path().join(".aethyme/gates.toml"), config).unwrap();
        git(f._repo.path(), &["add", ".aethyme/gates.toml"]);
        git(
            f._repo.path(),
            &["commit", "-qm", "trusted independent Playground gates"],
        );
    }
    let base = archive::pin_commit(f._repo.path(), "HEAD").unwrap();
    std::fs::write(f._repo.path().join("one.txt"), "A\n").unwrap();
    git(f._repo.path(), &["commit", "-qam", "A"]);
    let a = archive::pin_commit(f._repo.path(), "HEAD").unwrap();
    f.a = archive::retain_contribution(&mut f.store, f._repo.path(), &base, &a).unwrap();
    git(
        f._repo.path(),
        &["checkout", "-q", "--detach", base.as_str()],
    );
    std::fs::write(f._repo.path().join("two.txt"), "B\n").unwrap();
    git(f._repo.path(), &["add", "two.txt"]);
    git(f._repo.path(), &["commit", "-qm", "B"]);
    let b = archive::pin_commit(f._repo.path(), "HEAD").unwrap();
    f.b = archive::retain_contribution(&mut f.store, f._repo.path(), &base, &b).unwrap();
    let owner = target::acquire_controller(&mut f.store, "native-gates", "appointed").unwrap();
    let mut spec = f.spec();
    spec.selected = vec!["A".into(), "B".into()];
    let revision = target::propose_revision(&mut f.store, &owner, spec).unwrap();
    target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
    wave::available(&mut f.store, &owner, "A", 0).unwrap();
    wave::available(&mut f.store, &owner, "B", 1).unwrap();
    let run = wave::tick(
        &mut f.store,
        &owner,
        f._repo.path(),
        wave::WavePolicy {
            debounce_ms: 0,
            ..Default::default()
        },
        1,
    )
    .unwrap();
    let wave = match run {
        wave::Run::RequiresVerification(w) => w.id,
        other => panic!("{other:?}"),
    };
    let p = policy(f._repo.path(), &base).unwrap();
    let trust = crate::broker::gate_trust::record_trust(
        f._repo.path(),
        &[&p.trust.policy_sha256],
        "independent synthetic test operator",
    )
    .unwrap();
    let broker = Broker::open(f._repo.path()).unwrap();
    Setup {
        f,
        owner: Some(owner),
        broker,
        wave,
        trust,
    }
}

fn open_store(host: &Path) -> CollaborationStore {
    CollaborationStore::open(
        &CollaborationRoot::under_host_state(host),
        &ProjectKey::parse("local-target-test").unwrap(),
        &[],
    )
    .unwrap()
}

fn root_count(store: &CollaborationStore, job: &RecordId) -> i64 {
    store
        .read_connection()
        .query_row(
            "SELECT count(*) FROM local_native_gate_roots WHERE job=?1",
            [job.as_str()],
            |row| row.get(0),
        )
        .unwrap()
}

fn age_object(store: &CollaborationStore, digest: &ObjectDigest) {
    std::fs::File::open(archive::object_path(store, digest))
        .unwrap()
        .set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1)),
        )
        .unwrap();
}

fn age_archive_objects(store: &CollaborationStore) {
    for prefix in std::fs::read_dir(archive::objects_dir(store)).unwrap() {
        for object in std::fs::read_dir(prefix.unwrap().path()).unwrap() {
            std::fs::File::open(object.unwrap().path())
                .unwrap()
                .set_times(
                    std::fs::FileTimes::new()
                        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1)),
                )
                .unwrap();
        }
    }
}

const GATES: &str = "[[gate]]\nname='retained-features'\ncommand='test \"$(cat one.txt)\" = A && test \"$(cat two.txt)\" = B && git rev-parse HEAD'\ncache=false\ntimeout_seconds=10\n";
#[test]
fn checkout_placement_refuses_upward_project_discovery() {
    let outside = tempfile::tempdir().unwrap();
    let repository = tempfile::tempdir().unwrap();
    checked_checkout_placement(outside.path(), repository.path()).unwrap();
    let nested = outside.path().join("nested");
    std::fs::create_dir(&nested).unwrap();
    std::fs::write(outside.path().join("Cargo.toml"), "[workspace]\n").unwrap();
    assert!(checked_checkout_placement(&nested, repository.path()).is_err());
    std::fs::remove_file(outside.path().join("Cargo.toml")).unwrap();
    std::fs::create_dir(outside.path().join(".git")).unwrap();
    assert!(checked_checkout_placement(&nested, repository.path()).is_err());
    let under_repository = repository.path().join("temporary");
    std::fs::create_dir(&under_repository).unwrap();
    assert!(checked_checkout_placement(&under_repository, repository.path()).is_err());
}
#[test]
fn actual_composer_candidate_runs_native_gates_without_legacy_queue_or_ref_writes() {
    let mut s = setup(Some(GATES));
    let before = git(s.f._repo.path(), &["show-ref"]);
    let job = prepare(
        &mut s.f.store,
        s.owner.as_ref().expect("setup owner is present"),
        &mut s.broker,
        &s.wave,
    )
    .unwrap();
    assert_eq!(job.state, "prepared");
    let complete = run(
        &mut s.f.store,
        s.owner.as_ref().expect("setup owner is present"),
        &mut s.broker,
        &job.id,
    )
    .unwrap();
    assert_eq!(complete.state, "completed");
    let observation = complete.observation.as_ref().unwrap();
    assert_eq!(observation["status"], "passed");
    assert_eq!(observation["selected_gates"], 1);
    assert_eq!(observation["candidate"]["commit"], job.spec.commit);
    assert_eq!(observation["gates"][0]["tree_hash"], job.spec.tree);
    assert_eq!(observation["trusted_evidence"], false);
    assert_eq!(observation["provider_acceptance"], false);
    let log =
        std::fs::read_to_string(observation["gates"][0]["log_path"].as_str().unwrap()).unwrap();
    assert!(
        log.contains(&job.spec.commit),
        "metadata-sensitive gate saw a replacement commit"
    );
    assert_eq!(git(s.f._repo.path(), &["show-ref"]), before);
    let count: i64 = s
        .broker
        .store()
        .connection()
        .query_row("SELECT count(*) FROM merge_queue", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        count, 0,
        "composed origin must never be manually promotable through the legacy queue"
    );
    let repeated = run(
        &mut s.f.store,
        s.owner.as_ref().expect("setup owner is present"),
        &mut s.broker,
        &job.id,
    )
    .unwrap();
    assert_eq!(repeated.observation, complete.observation);
}
#[test]
fn a_dispatched_job_reopens_unknown_without_any_retry() {
    let mut s = setup(Some(GATES));
    let job = prepare(
        &mut s.f.store,
        s.owner.as_ref().expect("setup owner is present"),
        &mut s.broker,
        &s.wave,
    )
    .unwrap();
    s.f.store
        .connection()
        .execute(
            "UPDATE local_native_gate_jobs SET state='dispatching' WHERE id=?1",
            [job.id.as_str()],
        )
        .unwrap();
    assert!(matches!(
        run(
            &mut s.f.store,
            s.owner.as_ref().expect("setup owner is present"),
            &mut s.broker,
            &job.id
        ),
        Err(NativeGateError::Unknown)
    ));
    let runs: i64 = s
        .broker
        .store()
        .connection()
        .query_row("SELECT count(*) FROM gate_results", [], |r| r.get(0))
        .unwrap();
    assert_eq!(runs, 0);
    assert_eq!(load(&s.f.store, &job.id).unwrap().state, "dispatching");
}
#[test]
#[cfg(unix)]
fn physical_subject_includes_mode_symlink_target_and_untracked_source() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source");
    std::fs::write(&path, b"same bytes").unwrap();
    let before = physical_snapshot(root.path()).unwrap().id();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_ne!(physical_snapshot(root.path()).unwrap().id(), before);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(physical_snapshot(root.path()).unwrap().id(), before);
    std::os::unix::fs::symlink("source", root.path().join("link")).unwrap();
    let link = physical_snapshot(root.path()).unwrap().id();
    std::fs::remove_file(root.path().join("link")).unwrap();
    std::os::unix::fs::symlink("other", root.path().join("link")).unwrap();
    assert_ne!(physical_snapshot(root.path()).unwrap().id(), link);
    std::fs::write(root.path().join("extra"), "untracked").unwrap();
    assert_ne!(physical_snapshot(root.path()).unwrap().id(), before);
}
#[test]
fn no_gate_configuration_is_explicitly_not_verification() {
    let mut s = setup(None);
    let job = prepare(
        &mut s.f.store,
        s.owner.as_ref().expect("setup owner is present"),
        &mut s.broker,
        &s.wave,
    )
    .unwrap();
    let complete = run(
        &mut s.f.store,
        s.owner.as_ref().expect("setup owner is present"),
        &mut s.broker,
        &job.id,
    )
    .unwrap();
    assert_eq!(complete.observation.unwrap()["status"], "no_configuration");
}
#[test]
fn corrupt_projected_job_identity_and_source_binding_refuse_before_dispatch() {
    let mut s = setup(Some(GATES));
    let job = prepare(
        &mut s.f.store,
        s.owner.as_ref().expect("setup owner is present"),
        &mut s.broker,
        &s.wave,
    )
    .unwrap();
    let mut forged = job.spec.clone();
    forged.subject = forged.baseline.clone();
    assert!(matches!(
        validate_source(&s.f.store, s.f._repo.path(), &forged),
        Err(NativeGateError::Mismatch | NativeGateError::Binding(_))
    ));
    s.f.store
        .connection()
        .execute(
            "UPDATE local_native_gate_jobs SET epoch='forged' WHERE id=?1",
            [job.id.as_str()],
        )
        .unwrap();
    assert!(matches!(
        run(
            &mut s.f.store,
            s.owner.as_ref().expect("setup owner is present"),
            &mut s.broker,
            &job.id
        ),
        Err(NativeGateError::Mismatch)
    ));
}

#[test]
fn prepared_job_cannot_borrow_another_repository_trust_or_survive_revocation() {
    let mut s = setup(Some(GATES));
    let job = prepare(
        &mut s.f.store,
        s.owner.as_ref().expect("setup owner is present"),
        &mut s.broker,
        &s.wave,
    )
    .unwrap();
    let other = tempfile::tempdir().unwrap();
    let repo = other.path().join("clone");
    git(
        s.f._repo.path(),
        &[
            "clone",
            "--no-hardlinks",
            "--",
            s.f._repo.path().to_str().unwrap(),
            repo.to_str().unwrap(),
        ],
    );
    let p = policy(&repo, &commit(&job.spec.base_commit).unwrap()).unwrap();
    let trust = crate::broker::gate_trust::record_trust(
        &repo,
        &[&p.trust.policy_sha256],
        "independent synthetic test operator",
    )
    .unwrap();
    let mut foreign = Broker::open(&repo).unwrap();
    assert!(matches!(
        run(
            &mut s.f.store,
            s.owner.as_ref().expect("setup owner is present"),
            &mut foreign,
            &job.id
        ),
        Err(NativeGateError::Binding(
            "broker repository trust namespace"
        ))
    ));
    crate::broker::gate_trust::forget_record(&trust);
    crate::broker::gate_trust::forget_record(&s.trust);
    assert!(
        run(
            &mut s.f.store,
            s.owner.as_ref().expect("setup owner is present"),
            &mut s.broker,
            &job.id
        )
        .is_err(),
        "revoked original policy still executed"
    );
    assert_eq!(load(&s.f.store, &job.id).unwrap().state, "prepared");
}

#[test]
fn budget_capacity_and_requirement_holds_do_not_dispatch_prepared_jobs() {
    for phase in [
        target::Phase::WaitingForBudget,
        target::Phase::WaitingForCapacity,
        target::Phase::BlockedRequirement,
    ] {
        let mut s = setup(Some(GATES));
        let job = prepare(
            &mut s.f.store,
            s.owner.as_ref().expect("setup owner is present"),
            &mut s.broker,
            &s.wave,
        )
        .unwrap();
        let mut status = target::status(
            &s.f.store,
            s.owner.as_ref().expect("setup owner is present").target(),
        )
        .unwrap();
        if status.phase == target::Phase::WaitingForInput {
            status = target::resume(
                &mut s.f.store,
                s.owner.as_ref().expect("setup owner is present"),
                status.version,
            )
            .unwrap();
        }
        target::transition(
            &mut s.f.store,
            s.owner.as_ref().expect("setup owner is present"),
            status.version,
            phase,
        )
        .unwrap();
        assert!(matches!(
            run(
                &mut s.f.store,
                s.owner.as_ref().expect("setup owner is present"),
                &mut s.broker,
                &job.id
            ),
            Err(NativeGateError::Binding(_))
        ));
        assert_eq!(load(&s.f.store, &job.id).unwrap().state, "prepared");
        let count: i64 = s
            .broker
            .store()
            .connection()
            .query_row("SELECT count(*) FROM gate_results", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
}
#[test]
fn native_gate_roots_survive_revision_root_release() {
    let mut s = setup(Some(GATES));
    let job = prepare(
        &mut s.f.store,
        s.owner.as_ref().expect("setup owner is present"),
        &mut s.broker,
        &s.wave,
    )
    .unwrap();
    s.f.store
        .connection()
        .execute("DELETE FROM local_target_roots", [])
        .unwrap();
    let roots: i64 =
        s.f.store
            .read_connection()
            .query_row(
                "SELECT count(*) FROM local_native_gate_roots WHERE job=?1",
                [job.id.as_str()],
                |r| r.get(0),
            )
            .unwrap();
    assert!(roots >= 8);
    let out = tempfile::tempdir().unwrap();
    archive::reconstruct(
        &s.f.store,
        &snapshot(&job.spec.subject).unwrap(),
        &out.path().join("retained"),
    )
    .unwrap();
}

#[test]
fn fresh_dispatch_refuses_a_job_completed_after_the_callers_snapshot() {
    let mut s = setup(None);
    let stale_prepared = prepare(
        &mut s.f.store,
        s.owner.as_ref().expect("setup owner is present"),
        &mut s.broker,
        &s.wave,
    )
    .unwrap();
    assert_eq!(stale_prepared.state, "prepared");
    let done = run_fresh(
        &mut s.f.store,
        s.owner.as_ref().expect("setup owner is present"),
        &mut s.broker,
        &stale_prepared.id,
    )
    .unwrap();
    assert_eq!(done.state, "completed");
    assert_eq!(
        done.observation.as_ref().unwrap()["current_authority"],
        true
    );
    assert!(
        matches!(
            run_fresh(
                &mut s.f.store,
                s.owner.as_ref().expect("setup owner is present"),
                &mut s.broker,
                &stale_prepared.id
            ),
            Err(NativeGateError::Unknown)
        ),
        "fresh dispatch rechecks actual durable state, not a callers prepared snapshot"
    );
    let historical = run(
        &mut s.f.store,
        s.owner.as_ref().expect("setup owner is present"),
        &mut s.broker,
        &stale_prepared.id,
    )
    .unwrap();
    assert_eq!(
        historical.observation, done.observation,
        "explicit history access remains read only"
    );
}

#[test]
fn released_prepared_job_remains_readable_but_cannot_be_dispatched_or_replayed() {
    let mut s = setup(None);
    let owner = s.owner.as_ref().expect("setup owner is present");
    let job = prepare(&mut s.f.store, owner, &mut s.broker, &s.wave).unwrap();
    let roots_before = root_count(&s.f.store, &job.id);
    assert!(roots_before > 0);

    let released = release_roots(&mut s.f.store, owner, &job.id).unwrap();
    assert_eq!(released.status, RootReleaseStatus::Released);
    assert_eq!(released.job.state, "prepared");
    assert!(released.job.roots_released_ms.is_some());
    assert_eq!(root_count(&s.f.store, &job.id), roots_before);
    assert_eq!(load(&s.f.store, &job.id).unwrap().state, "prepared");

    assert!(matches!(
        run_fresh(&mut s.f.store, owner, &mut s.broker, &job.id),
        Err(NativeGateError::RootsReleased)
    ));
    assert!(matches!(
        prepare(&mut s.f.store, owner, &mut s.broker, &s.wave),
        Err(NativeGateError::RootsReleased)
    ));
    assert_eq!(
        release_roots(&mut s.f.store, owner, &job.id)
            .unwrap()
            .status,
        RootReleaseStatus::AlreadyReleased
    );

    let old_client_update = s.f.store.connection().execute(
        "UPDATE local_native_gate_jobs SET state='dispatching' WHERE id=?1",
        [job.id.as_str()],
    );
    assert!(
        old_client_update.is_err(),
        "migration trigger must fence an older open client"
    );
    assert_eq!(
        load(&s.f.store, &job.id).unwrap().roots_released_ms,
        released.job.roots_released_ms
    );
}

#[test]
fn dispatching_and_unknown_jobs_stay_rooted_without_a_process_tree_proof() {
    let mut s = setup(None);
    let owner = s.owner.as_ref().expect("setup owner is present");
    let job = prepare(&mut s.f.store, owner, &mut s.broker, &s.wave).unwrap();
    let roots_before = root_count(&s.f.store, &job.id);
    s.f.store
        .connection()
        .execute(
            "UPDATE local_native_gate_jobs SET state='dispatching' WHERE id=?1",
            [job.id.as_str()],
        )
        .unwrap();

    for state in ["dispatching", "unknown"] {
        if state == "unknown" {
            s.f.store
                .connection()
                .execute(
                    "UPDATE local_native_gate_jobs SET state='unknown' WHERE id=?1",
                    [job.id.as_str()],
                )
                .unwrap();
        }
        let result = release_roots(&mut s.f.store, owner, &job.id).unwrap();
        assert_eq!(result.status, RootReleaseStatus::UnknownHeld);
        assert_eq!(result.job.state, state);
        assert_eq!(result.job.roots_released_ms, None);
        assert_eq!(root_count(&s.f.store, &job.id), roots_before);
    }

    s.f.store
        .connection()
        .execute("DELETE FROM local_target_roots", [])
        .unwrap();
    age_archive_objects(&s.f.store);
    let plan = gc::plan(&mut s.f.store, &gc::GcOptions::default()).unwrap();
    assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    assert!(
        plan.reclaimable.is_empty(),
        "unknown native job lost source roots: {:?}",
        plan.reclaimable
    );
    assert!(
        plan.entities.is_empty(),
        "unknown native job lost retained records: {:?}",
        plan.entities
    );
    assert_eq!(load(&s.f.store, &job.id).unwrap().state, "unknown");
}

#[test]
fn stale_owner_cannot_release_but_current_owner_can_release_old_epoch_history() {
    let mut s = setup(None);
    let job = prepare(
        &mut s.f.store,
        s.owner.as_ref().expect("setup owner is present"),
        &mut s.broker,
        &s.wave,
    )
    .unwrap();
    let stale_epoch = s
        .owner
        .as_ref()
        .expect("setup owner is present")
        .authority_epoch()
        .clone();
    let frozen = crate::local_authority::freeze(
        &mut s.f.store,
        &stale_epoch,
        "test stale native-gate owner",
    )
    .unwrap();
    assert!(
        release_roots(
            &mut s.f.store,
            s.owner.as_ref().expect("stale setup owner is retained"),
            &job.id
        )
        .is_err()
    );

    drop(s.take_owner());
    crate::local_authority::resume(&mut s.f.store, &frozen.epoch).unwrap();
    let current = target::acquire_controller(&mut s.f.store, "native-gates", "appointed").unwrap();
    let released = release_roots(&mut s.f.store, &current, &job.id).unwrap();
    assert_eq!(released.status, RootReleaseStatus::Released);
    assert_eq!(released.job.state, "prepared");
    assert!(released.job.roots_released_ms.is_some());
}

#[test]
fn completed_job_release_survives_real_gc_and_reopened_status_reads() {
    let mut s = setup(Some(GATES));
    let owner = s.owner.as_ref().expect("setup owner is present");
    let job = prepare(&mut s.f.store, owner, &mut s.broker, &s.wave).unwrap();
    let completed = run(&mut s.f.store, owner, &mut s.broker, &job.id).unwrap();
    assert_eq!(completed.state, "completed");
    let observation = completed.observation.clone().unwrap();
    assert_eq!(observation["status"], "passed");
    assert_eq!(observation["selected_gates"], 1);

    let released = release_roots(&mut s.f.store, owner, &job.id).unwrap();
    assert_eq!(released.status, RootReleaseStatus::Released);
    assert_eq!(released.job.state, "completed");
    assert!(released.job.roots_released_ms.is_some());
    assert_eq!(
        load(&s.f.store, &job.id).unwrap().observation,
        Some(observation)
    );

    // The target still independently owns its source. A real GC pass may
    // remove an unrelated orphan, but must preserve the native candidate.
    let orphan = archive::put_object(&s.f.store, b"unrelated orphan").unwrap();
    age_object(&s.f.store, &orphan);
    let first_plan = gc::plan(&mut s.f.store, &gc::GcOptions::default()).unwrap();
    assert!(first_plan.blockers.is_empty(), "{:?}", first_plan.blockers);
    let first_gc = gc::apply(
        &mut s.f.store,
        &first_plan.digest,
        &gc::GcOptions::default(),
    )
    .unwrap();
    assert!(
        first_gc
            .reclaimed
            .iter()
            .any(|item| item.class == gc::RetentionClass::Orphan)
    );
    let source_root = tempfile::tempdir().unwrap();
    archive::reconstruct(
        &s.f.store,
        &snapshot(&job.spec.subject).unwrap(),
        &source_root.path().join("subject"),
    )
    .unwrap();

    // Once that separate owner releases its revision roots, the job's
    // released edges no longer retain their former source objects.
    s.f.store
        .connection()
        .execute("DELETE FROM local_target_roots", [])
        .unwrap();
    age_archive_objects(&s.f.store);
    let final_plan = gc::plan(&mut s.f.store, &gc::GcOptions::default()).unwrap();
    assert!(final_plan.blockers.is_empty(), "{:?}", final_plan.blockers);
    assert!(!final_plan.reclaimable.is_empty());
    let final_gc = gc::apply(
        &mut s.f.store,
        &final_plan.digest,
        &gc::GcOptions::default(),
    )
    .unwrap();
    assert!(!final_gc.entities.is_empty());

    let host = s.f._host.path().to_owned();
    drop(s.take_owner());
    let mut reopened = open_store(&host);
    let reopened_owner =
        target::acquire_controller(&mut reopened, "native-gates", "appointed").unwrap();
    let status = retention_status_for(&reopened, &reopened_owner, &job.id).unwrap();
    assert_eq!(status.state, "completed");
    assert!(status.roots_released_ms.is_some());
    let historical = load(&reopened, &job.id).unwrap();
    assert_eq!(historical.observation, Some(observation));
    assert!(historical.roots_released_ms.is_some());
}

#[test]
fn root_release_waits_for_an_in_progress_gc_generation() {
    let mut s = setup(None);
    let job = prepare(
        &mut s.f.store,
        s.owner.as_ref().expect("setup owner is present"),
        &mut s.broker,
        &s.wave,
    )
    .unwrap();
    let orphan = archive::put_object(&s.f.store, b"gc lock interleaving").unwrap();
    age_object(&s.f.store, &orphan);
    let plan = gc::plan(&mut s.f.store, &gc::GcOptions::default()).unwrap();
    assert_eq!(plan.reclaimable.len(), 1, "{:?}", plan.reclaimable);
    assert_eq!(plan.reclaimable[0].class, gc::RetentionClass::Orphan);

    let host = s.f._host.path().to_owned();
    let mut gc_store = open_store(&host);
    let digest = plan.digest.clone();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (continue_tx, continue_rx) = std::sync::mpsc::channel();
    let continue_rx = std::sync::Arc::new(std::sync::Mutex::new(continue_rx));
    let paused_receiver = continue_rx.clone();
    let hooks = GcHooks {
        after_moves: Some((
            1,
            Box::new(move || {
                entered_tx.send(()).unwrap();
                paused_receiver.lock().unwrap().recv().unwrap();
            }),
        )),
        ..GcHooks::default()
    };
    let gc_thread = std::thread::spawn(move || {
        gc::apply_with(&mut gc_store, &digest, &gc::GcOptions::default(), &hooks).unwrap()
    });
    entered_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("GC should pause while holding the exclusive archive lock");

    let owner = s.take_owner();
    let release_host = host.clone();
    let key = job.id.clone();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let release_thread = std::thread::spawn(move || {
        let mut store = open_store(&release_host);
        started_tx.send(()).unwrap();
        let released = release_roots(&mut store, &owner, &key).unwrap();
        done_tx.send(released.status).unwrap();
    });
    started_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("release thread should start");
    assert!(
        matches!(
            done_rx.recv_timeout(std::time::Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ),
        "release must wait while GC owns the archive exclusively"
    );

    continue_tx.send(()).unwrap();
    let gc_report = gc_thread.join().unwrap();
    assert_eq!(gc_report.reclaimed.len(), 1);
    assert_eq!(
        done_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap(),
        RootReleaseStatus::Released
    );
    release_thread.join().unwrap();
}
