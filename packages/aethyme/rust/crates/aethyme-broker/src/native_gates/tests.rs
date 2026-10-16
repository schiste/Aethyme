use super::*;
use crate::collaboration_gc::{self as gc, GcHooks};
use crate::collaboration_state::{CollaborationRoot, ProjectKey};
use crate::local_target::tests::{Fixture, git};
use rusqlite::OptionalExtension;
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

fn object_relpath(store: &CollaborationStore, digest: &ObjectDigest) -> String {
    archive::object_path(store, digest)
        .strip_prefix(store.project_dir())
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

fn assert_native_job_sources_retained(
    store: &CollaborationStore,
    plan: &gc::GcPlan,
    job: &Job,
    expected_snapshots: &[(String, SourceSnapshot)],
    expected_contributions: &[&archive::RetainedContribution],
    phase: &str,
) {
    let expected_snapshot_ids: std::collections::BTreeSet<_> = expected_snapshots
        .iter()
        .map(|(id, source)| {
            assert_eq!(
                source.id().as_str(),
                id.as_str(),
                "{phase}: fixture snapshot ID"
            );
            id.clone()
        })
        .collect();
    let expected_contribution_ids: std::collections::BTreeSet<_> = expected_contributions
        .iter()
        .map(|contribution| contribution.lineage_record_id.to_string())
        .collect();
    assert!(
        expected_snapshot_ids.contains(&job.spec.subject)
            && expected_snapshot_ids.contains(&job.spec.baseline),
        "{phase}: expected closure does not contain the frozen subject and baseline"
    );
    for contribution in expected_contributions {
        assert!(
            expected_snapshot_ids.contains(&contribution.base.snapshot_id.to_string())
                && expected_snapshot_ids.contains(&contribution.result.snapshot_id.to_string()),
            "{phase}: expected closure omits a selected contribution snapshot"
        );
    }

    let missing_snapshots: Vec<_> = expected_snapshot_ids
        .iter()
        .filter(|id| {
            plan.entities
                .contains(&gc::GcEntity::Snapshot((*id).clone()))
        })
        .collect();
    let missing_contributions: Vec<_> = expected_contribution_ids
        .iter()
        .filter(|id| {
            plan.entities
                .contains(&gc::GcEntity::Contribution((*id).clone()))
        })
        .collect();
    assert!(
        missing_snapshots.is_empty() && missing_contributions.is_empty(),
        "{phase}: native job source entities became reclaimable; missing snapshots={missing_snapshots:?}, missing contributions={missing_contributions:?}; full GC plan={plan:#?}"
    );

    let mut expected_objects = std::collections::BTreeMap::<ObjectDigest, String>::new();
    for (snapshot_id, source) in expected_snapshots {
        let manifest = ObjectDigest::of(&source.manifest_bytes());
        assert_eq!(
            manifest,
            ObjectDigest::of_snapshot(&source.id()),
            "{phase}: source manifest digest"
        );
        assert_eq!(
            archive::read_object(store, &manifest).unwrap(),
            source.manifest_bytes(),
            "{phase}: archived manifest differs from frozen fixture snapshot"
        );
        expected_objects
            .entry(manifest)
            .or_insert_with(|| format!("snapshot {snapshot_id} manifest"));
        for entry in source.entries() {
            let blob = ObjectDigest::from_bytes(*entry.content_sha256());
            archive::read_object(store, &blob).unwrap();
            expected_objects.entry(blob).or_insert_with(|| {
                format!(
                    "snapshot {snapshot_id} entry {} blob",
                    String::from_utf8_lossy(entry.path())
                )
            });
        }

        let (record_id, record_sha256): (String, String) = store
            .read_connection()
            .query_row(
                "SELECT record_id,record_sha256 FROM retained_snapshots WHERE snapshot_id=?1",
                [snapshot_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let record_digest = digest(&record_sha256).unwrap();
        let record_bytes = archive::read_object(store, &record_digest).unwrap();
        let record = Record::decode(&record_bytes, &[&archive::RETAINED_SNAPSHOT_SCHEMA]).unwrap();
        let encoded_snapshot =
            aethyme_contracts::experimental_v0::canonical_json::Value::String(snapshot_id.clone());
        assert_eq!(record.canonical_bytes(), record_bytes);
        assert_eq!(record.id().as_str(), record_id.as_str());
        assert_eq!(record.get("snapshot"), Some(&encoded_snapshot));
        expected_objects
            .entry(record_digest)
            .or_insert_with(|| format!("snapshot {snapshot_id} metadata record"));
    }

    for contribution in expected_contributions {
        let lineage = &contribution.lineage_record_id;
        let (record_sha256, base_snapshot, result_snapshot): (String, String, String) = store
            .read_connection()
            .query_row(
                "SELECT record_sha256,base_snapshot,result_snapshot FROM retained_contributions WHERE lineage_record_id=?1",
                [lineage.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            base_snapshot.as_str(),
            contribution.base.snapshot_id.as_str()
        );
        assert_eq!(
            result_snapshot.as_str(),
            contribution.result.snapshot_id.as_str()
        );
        let record_digest = digest(&record_sha256).unwrap();
        let record_bytes = archive::read_object(store, &record_digest).unwrap();
        let record =
            Record::decode(&record_bytes, &[&archive::CONTRIBUTION_LINEAGE_SCHEMA]).unwrap();
        let encoded_base = aethyme_contracts::experimental_v0::canonical_json::Value::String(
            contribution.base.snapshot_id.to_string(),
        );
        let encoded_result = aethyme_contracts::experimental_v0::canonical_json::Value::String(
            contribution.result.snapshot_id.to_string(),
        );
        assert_eq!(record.canonical_bytes(), record_bytes);
        assert_eq!(record.id().as_str(), lineage.as_str());
        assert_eq!(record.get("base"), Some(&encoded_base));
        assert_eq!(record.get("result"), Some(&encoded_result));
        expected_objects
            .entry(record_digest)
            .or_insert_with(|| format!("contribution {lineage} lineage record"));

        let brief: Option<(String, String)> = store
            .read_connection()
            .query_row(
                "SELECT brief_record_id,brief_sha256 FROM contribution_briefs WHERE lineage_record_id=?1",
                [lineage.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .unwrap();
        if let Some((brief_id, brief_sha256)) = brief {
            let brief_digest = digest(&brief_sha256).unwrap();
            let brief_bytes = archive::read_object(store, &brief_digest).unwrap();
            let brief_record = Record::decode(
                &brief_bytes,
                &[&aethyme_contracts::experimental_v0::brief::BRIEF_SCHEMA],
            )
            .unwrap();
            assert_eq!(brief_record.canonical_bytes(), brief_bytes);
            assert_eq!(brief_record.id().as_str(), brief_id.as_str());
            expected_objects
                .entry(brief_digest)
                .or_insert_with(|| format!("contribution {lineage} brief record"));
        }
    }

    let job_record = ObjectDigest::of(&record(&job.spec).unwrap().canonical_bytes());
    let recipe_record = digest(&job.spec.recipe).unwrap();
    archive::read_object(store, &job_record).unwrap();
    archive::read_object(store, &recipe_record).unwrap();
    expected_objects
        .entry(job_record)
        .or_insert_with(|| "native job record".into());
    expected_objects
        .entry(recipe_record)
        .or_insert_with(|| "composition recipe record".into());

    for (object, label) in expected_objects {
        let relpath = object_relpath(store, &object);
        assert!(
            !plan.reclaimable.iter().any(|item| item.relpath == relpath),
            "{phase}: retained native job closure object {label} ({}) became reclaimable at {relpath}; full GC plan={plan:#?}",
            object.hex()
        );
    }
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
    if run_in_clean_git_child(
        "native_gates::tests::actual_composer_candidate_runs_native_gates_without_legacy_queue_or_ref_writes",
    ) {
        return;
    }
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
    if run_in_clean_git_child(
        "native_gates::tests::no_gate_configuration_is_explicitly_not_verification",
    ) {
        return;
    }
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
    if run_in_clean_git_child(
        "native_gates::tests::fresh_dispatch_refuses_a_job_completed_after_the_callers_snapshot",
    ) {
        return;
    }
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
    // Reopen the durable store as a schema-22 client would see it, then keep
    // that connection open while a new client adopts migrations 23 through 26.
    // The old state update uses only columns that existed before migration 23.
    crate::collaboration_state::drop_identity_revision_schema_for_legacy_fixture(
        s.f.store.connection(),
    )
    .unwrap();
    crate::collaboration_sensitive::drop_sensitive_response_tables_for_legacy_fixture(
        s.f.store.connection(),
    )
    .unwrap();
    s.f.store
        .connection()
        .execute_batch(
            "DROP TABLE IF EXISTS local_capture_notice_outcomes; DROP TABLE IF EXISTS local_capture_notices; DROP TRIGGER local_native_gate_no_dispatch_after_root_release;
             ALTER TABLE local_native_gate_jobs DROP COLUMN roots_released_ms;
             UPDATE meta SET value='22' WHERE key='schema_version';
             UPDATE meta SET value='22' WHERE key='min_compatible_schema';",
        )
        .unwrap();
    let database = s.f.store.project_dir().join("state.db");
    let old_client = rusqlite::Connection::open(database).unwrap();
    let host = s.f._host.path().to_owned();
    let mut migrated = open_store(&host);
    assert_eq!(
        migrated.schema_version(),
        crate::collaboration_state::COLLABORATION_STATE_SCHEMA_VERSION
    );
    let migrated_floor: i64 = migrated
        .read_connection()
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM meta WHERE key='min_compatible_schema'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        migrated_floor,
        crate::collaboration_state::COLLABORATION_STATE_SCHEMA_VERSION
    );
    let roots_before = root_count(&s.f.store, &job.id);
    assert!(roots_before > 0);

    let released = release_roots(&mut migrated, owner, &job.id).unwrap();
    assert_eq!(released.status, RootReleaseStatus::Released);
    assert_eq!(released.job.state, "prepared");
    assert!(released.job.roots_released_ms.is_some());
    assert_eq!(root_count(&migrated, &job.id), roots_before);
    assert_eq!(load(&migrated, &job.id).unwrap().state, "prepared");

    assert!(matches!(
        run_fresh(&mut migrated, owner, &mut s.broker, &job.id),
        Err(NativeGateError::RootsReleased)
    ));
    assert!(matches!(
        prepare(&mut migrated, owner, &mut s.broker, &s.wave),
        Err(NativeGateError::RootsReleased)
    ));
    assert_eq!(
        release_roots(&mut migrated, owner, &job.id).unwrap().status,
        RootReleaseStatus::AlreadyReleased
    );

    let old_client_update = old_client.execute(
        "UPDATE local_native_gate_jobs SET state='dispatching' WHERE id=?1",
        [job.id.as_str()],
    );
    assert!(
        old_client_update.is_err(),
        "migration 23 trigger must fence a connection opened on schema 22"
    );
    assert_eq!(
        load(&migrated, &job.id).unwrap().roots_released_ms,
        released.job.roots_released_ms
    );
}

#[test]
fn dispatching_and_unknown_jobs_stay_rooted_without_a_process_tree_proof() {
    let mut s = setup(None);
    let owner = s.owner.as_ref().expect("setup owner is present");
    let job = prepare(&mut s.f.store, owner, &mut s.broker, &s.wave).unwrap();
    let roots_before = root_count(&s.f.store, &job.id);
    let repo = s.f._repo.path();
    let candidate_source =
        archive::snapshot_of_commit(repo, &commit(&job.spec.commit).unwrap()).unwrap();
    let baseline_source = archive::snapshot_of_commit(repo, &s.f.a.base.commit).unwrap();
    let a_base_source = archive::snapshot_of_commit(repo, &s.f.a.base.commit).unwrap();
    let a_result_source = archive::snapshot_of_commit(repo, &s.f.a.result.commit).unwrap();
    let b_base_source = archive::snapshot_of_commit(repo, &s.f.b.base.commit).unwrap();
    let b_result_source = archive::snapshot_of_commit(repo, &s.f.b.result.commit).unwrap();
    let expected_snapshots = [
        (job.spec.subject.clone(), candidate_source),
        (job.spec.baseline.clone(), baseline_source),
        (s.f.a.base.snapshot_id.to_string(), a_base_source),
        (s.f.a.result.snapshot_id.to_string(), a_result_source),
        (s.f.b.base.snapshot_id.to_string(), b_base_source),
        (s.f.b.result.snapshot_id.to_string(), b_result_source),
    ];
    let expected_contributions = [&s.f.a, &s.f.b];
    let mut expected_inputs = [
        s.f.a.result.commit.as_str().to_owned(),
        s.f.b.result.commit.as_str().to_owned(),
    ];
    expected_inputs.sort();
    let mut actual_inputs = job.spec.inputs.clone();
    actual_inputs.sort();
    assert_eq!(
        actual_inputs, expected_inputs,
        "fixture lineages must match the frozen native job inputs"
    );

    s.f.store
        .connection()
        .execute(
            "UPDATE local_native_gate_jobs SET state='dispatching' WHERE id=?1",
            [job.id.as_str()],
        )
        .unwrap();

    // Remove target ownership and age the source. The expected closure is
    // derived from Git snapshots and the known fixture contribution records;
    // unrelated orphaned fixture history can still be collectible.
    s.f.store
        .connection()
        .execute("DELETE FROM local_target_roots", [])
        .unwrap();
    age_archive_objects(&s.f.store);

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
        let plan = gc::plan(&mut s.f.store, &gc::GcOptions::default()).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_native_job_sources_retained(
            &s.f.store,
            &plan,
            &job,
            &expected_snapshots,
            &expected_contributions,
            state,
        );

        let result = release_roots(&mut s.f.store, owner, &job.id).unwrap();
        assert_eq!(result.status, RootReleaseStatus::UnknownHeld);
        assert_eq!(result.job.state, state);
        assert_eq!(result.job.roots_released_ms, None);
        assert_eq!(root_count(&s.f.store, &job.id), roots_before);
    }

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

    crate::local_authority::resume(&mut s.f.store, &frozen.epoch).unwrap();
    assert!(
        release_roots(
            &mut s.f.store,
            s.owner.as_ref().expect("stale setup owner is retained"),
            &job.id
        )
        .is_err(),
        "the old owner remains stale after authority resumes"
    );
    drop(s.take_owner());
    let current = target::acquire_controller(&mut s.f.store, "native-gates", "appointed").unwrap();
    let released = release_roots(&mut s.f.store, &current, &job.id).unwrap();
    assert_eq!(released.status, RootReleaseStatus::Released);
    assert_eq!(released.job.state, "prepared");
    assert!(released.job.roots_released_ms.is_some());
}

#[test]
fn completed_job_release_survives_real_gc_and_reopened_status_reads() {
    if run_in_clean_git_child(
        "native_gates::tests::completed_job_release_survives_real_gc_and_reopened_status_reads",
    ) {
        return;
    }
    let mut s = setup(Some(GATES));
    let owner = s.owner.as_ref().expect("setup owner is present");
    let job = prepare(&mut s.f.store, owner, &mut s.broker, &s.wave).unwrap();
    let completed = run(&mut s.f.store, owner, &mut s.broker, &job.id).unwrap();
    assert_eq!(completed.state, "completed");
    let observation = completed.observation.clone().unwrap();
    assert_eq!(observation["status"], "passed");
    assert_eq!(observation["selected_gates"], 1);

    // With the target revision roots removed, the completed job is the only
    // owner of this exact subject-manifest object until its explicit release.
    s.f.store
        .connection()
        .execute("DELETE FROM local_target_roots", [])
        .unwrap();
    age_archive_objects(&s.f.store);
    let subject_object = ObjectDigest::of_snapshot(&snapshot(&job.spec.subject).unwrap());
    let subject_relpath = object_relpath(&s.f.store, &subject_object);
    let orphan = archive::put_object(&s.f.store, b"unrelated orphan").unwrap();
    age_object(&s.f.store, &orphan);
    let before_release_plan = gc::plan(&mut s.f.store, &gc::GcOptions::default()).unwrap();
    assert!(
        before_release_plan.blockers.is_empty(),
        "{:?}",
        before_release_plan.blockers
    );
    assert!(
        !before_release_plan
            .reclaimable
            .iter()
            .any(|item| item.relpath == subject_relpath),
        "the exact job-owned subject manifest must remain retained before release"
    );
    let before_release_gc = gc::apply(
        &mut s.f.store,
        &before_release_plan.digest,
        &gc::GcOptions::default(),
    )
    .unwrap();
    assert!(
        before_release_gc
            .reclaimed
            .iter()
            .any(|item| item.class == gc::RetentionClass::Orphan)
    );
    assert!(
        !before_release_gc
            .reclaimed
            .iter()
            .any(|item| item.relpath == subject_relpath)
    );
    assert!(archive::object_path(&s.f.store, &subject_object).exists());

    let released = release_roots(&mut s.f.store, owner, &job.id).unwrap();
    assert_eq!(released.status, RootReleaseStatus::Released);
    assert_eq!(released.job.state, "completed");
    assert!(released.job.roots_released_ms.is_some());
    assert_eq!(
        load(&s.f.store, &job.id).unwrap().observation,
        Some(observation.clone())
    );

    let final_plan = gc::plan(&mut s.f.store, &gc::GcOptions::default()).unwrap();
    assert!(final_plan.blockers.is_empty(), "{:?}", final_plan.blockers);
    assert!(
        final_plan
            .reclaimable
            .iter()
            .any(|item| item.relpath == subject_relpath),
        "the same job-owned subject manifest must become reclaimable after release"
    );
    let final_gc = gc::apply(
        &mut s.f.store,
        &final_plan.digest,
        &gc::GcOptions::default(),
    )
    .unwrap();
    assert!(final_gc.entities.iter().any(|entity| match entity {
        gc::GcEntity::Snapshot(id) => id == &job.spec.subject,
        gc::GcEntity::Contribution(_) => false,
    }));
    assert!(
        final_gc
            .reclaimed
            .iter()
            .any(|item| item.relpath == subject_relpath)
    );
    assert!(!archive::object_path(&s.f.store, &subject_object).exists());

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
    let (probe_tx, probe_rx) = std::sync::mpsc::channel();
    let release_thread = std::thread::spawn(move || {
        let mut store = open_store(&release_host);
        let _probe = gc::observe_archive_use(probe_tx.clone());
        let result = release_roots(&mut store, &owner, &key).unwrap().status;
        probe_tx
            .send(gc::ArchiveUseProbeEvent::Finished)
            .expect("GC completion probe receiver remains open");
        result
    });
    // The release thread's own archive acquisition must observe the kernel's
    // exclusive GC lock before blocking. It reports Acquired only after the
    // shared lock becomes available, then Finished after release_roots returns.
    let first_event = probe_rx.recv_timeout(std::time::Duration::from_secs(5));

    continue_tx.send(()).unwrap();
    let gc_report = gc_thread.join().unwrap();
    assert_eq!(gc_report.reclaimed.len(), 1);
    assert_eq!(release_thread.join().unwrap(), RootReleaseStatus::Released);
    let mut events: Vec<_> = first_event.into_iter().collect();
    events.extend(probe_rx.try_iter());
    assert_eq!(
        events,
        [
            gc::ArchiveUseProbeEvent::Blocked,
            gc::ArchiveUseProbeEvent::Acquired,
            gc::ArchiveUseProbeEvent::Finished,
        ],
        "release_roots must wait for GC at its own archive lock"
    );
}
