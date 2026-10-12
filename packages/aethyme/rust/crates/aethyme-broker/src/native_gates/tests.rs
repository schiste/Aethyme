use super::*;
use crate::local_target::tests::{Fixture, git};

struct Setup {
    f: Fixture,
    owner: ControllerOwner,
    broker: Broker,
    wave: RecordId,
    trust: std::path::PathBuf,
}
impl Drop for Setup {
    fn drop(&mut self) {
        crate::broker::gate_trust::forget_record(&self.trust);
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
        owner,
        broker,
        wave,
        trust,
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
    let job = prepare(&mut s.f.store, &s.owner, &mut s.broker, &s.wave).unwrap();
    assert_eq!(job.state, "prepared");
    let complete = run(&mut s.f.store, &s.owner, &mut s.broker, &job.id).unwrap();
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
    let repeated = run(&mut s.f.store, &s.owner, &mut s.broker, &job.id).unwrap();
    assert_eq!(repeated.observation, complete.observation);
}
#[test]
fn a_dispatched_job_reopens_unknown_without_any_retry() {
    let mut s = setup(Some(GATES));
    let job = prepare(&mut s.f.store, &s.owner, &mut s.broker, &s.wave).unwrap();
    s.f.store
        .connection()
        .execute(
            "UPDATE local_native_gate_jobs SET state='dispatching' WHERE id=?1",
            [job.id.as_str()],
        )
        .unwrap();
    assert!(matches!(
        run(&mut s.f.store, &s.owner, &mut s.broker, &job.id),
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
    let job = prepare(&mut s.f.store, &s.owner, &mut s.broker, &s.wave).unwrap();
    let complete = run(&mut s.f.store, &s.owner, &mut s.broker, &job.id).unwrap();
    assert_eq!(complete.observation.unwrap()["status"], "no_configuration");
}
#[test]
fn corrupt_projected_job_identity_and_source_binding_refuse_before_dispatch() {
    let mut s = setup(Some(GATES));
    let job = prepare(&mut s.f.store, &s.owner, &mut s.broker, &s.wave).unwrap();
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
        run(&mut s.f.store, &s.owner, &mut s.broker, &job.id),
        Err(NativeGateError::Mismatch)
    ));
}

#[test]
fn prepared_job_cannot_borrow_another_repository_trust_or_survive_revocation() {
    let mut s = setup(Some(GATES));
    let job = prepare(&mut s.f.store, &s.owner, &mut s.broker, &s.wave).unwrap();
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
        run(&mut s.f.store, &s.owner, &mut foreign, &job.id),
        Err(NativeGateError::Binding(
            "broker repository trust namespace"
        ))
    ));
    crate::broker::gate_trust::forget_record(&trust);
    crate::broker::gate_trust::forget_record(&s.trust);
    assert!(
        run(&mut s.f.store, &s.owner, &mut s.broker, &job.id).is_err(),
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
        let job = prepare(&mut s.f.store, &s.owner, &mut s.broker, &s.wave).unwrap();
        let mut status = target::status(&s.f.store, s.owner.target()).unwrap();
        if status.phase == target::Phase::WaitingForInput {
            status = target::resume(&mut s.f.store, &s.owner, status.version).unwrap();
        }
        target::transition(&mut s.f.store, &s.owner, status.version, phase).unwrap();
        assert!(matches!(
            run(&mut s.f.store, &s.owner, &mut s.broker, &job.id),
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
    let job = prepare(&mut s.f.store, &s.owner, &mut s.broker, &s.wave).unwrap();
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
