use super::*;
use crate::collaboration_state::{CollaborationRoot, ProjectKey};
use crate::local_budget::{self as budget, ProjectPolicy, Request, State, Units, WorkKind};
use crate::local_target::{self as target, tests::Fixture};
use crate::local_wave::{self as wave, Admission, WavePhase, WavePolicy};

#[test]
fn a_frozen_witness_blocks_even_matching_database_and_handle_epochs() {
    let (f, owner, _w, _a) = setup();
    let mut w = status(&f.store).unwrap();
    w.status = Status::Frozen;
    write(&directory(&f.store).unwrap(), &w).unwrap();
    assert!(matches!(
        guard(&f.store, owner.authority_epoch()),
        Err(AuthorityError::Frozen)
    ));
}

#[test]
fn completed_freeze_rebinds_the_frozen_epoch_and_allows_sensitive_quarantine() {
    let (mut f, owner, _w, _a) = setup();
    let frozen = freeze(&mut f.store, owner.authority_epoch(), "sensitive response").unwrap();
    assert_eq!(
        bound(f.store.read_connection()).unwrap(),
        Some(frozen.epoch.clone())
    );
    assert!(frozen_guard(&f.store, &frozen.epoch).is_ok());

    let incident = format!("sha256:{}", "a".repeat(64));
    let status = crate::collaboration_sensitive::quarantine(
        &mut f.store,
        &incident,
        "local-operator",
        frozen.epoch.as_str(),
        crate::collaboration_sensitive::ReasonCode::SuspectedTestSecretExposure,
    )
    .unwrap();
    assert_eq!(status.state, "quarantined");
    assert_eq!(status.incident_id, incident);
    let archive_probe = crate::collaboration_gc::try_admin_exclusive(&f.store)
        .unwrap()
        .expect("archive barrier should be released after quarantine");
    drop(archive_probe);
    let response_probe = crate::collaboration_sensitive::try_response_exclusive(&f.store)
        .unwrap()
        .expect("response barrier should be released after quarantine");
    drop(response_probe);
    let retry = crate::collaboration_sensitive::quarantine(
        &mut f.store,
        &incident,
        "local-operator",
        frozen.epoch.as_str(),
        crate::collaboration_sensitive::ReasonCode::SuspectedTestSecretExposure,
    )
    .unwrap();
    assert_eq!(retry.request_digest, status.request_digest);
    let id = RecordId::parse(&incident).unwrap();
    assert!(matches!(
        target::load_revision(&f.store, &id),
        Err(target::TargetError::Sensitive(
            crate::collaboration_sensitive::SensitiveError::Quarantined
        ))
    ));
    assert!(matches!(
        target::dispositions(&f.store, &id),
        Err(target::TargetError::Sensitive(
            crate::collaboration_sensitive::SensitiveError::Quarantined
        ))
    ));
    assert!(matches!(
        wave::load(&f.store, &id),
        Err(wave::WaveError::Sensitive(
            crate::collaboration_sensitive::SensitiveError::Quarantined
        ))
    ));
    assert!(matches!(
        crate::local_acceptance::load(&f.store, &id),
        Err(crate::local_acceptance::AcceptanceError::Sensitive(
            crate::collaboration_sensitive::SensitiveError::Quarantined
        ))
    ));
    assert!(matches!(
        crate::local_source_transport::load(&f.store, &id),
        Err(crate::local_source_transport::TransportError::Sensitive(
            crate::collaboration_sensitive::SensitiveError::Quarantined
        ))
    ));
    assert!(matches!(
        crate::local_acceptance::last_receipt(&f.store, &id),
        Err(crate::local_acceptance::AcceptanceError::Sensitive(
            crate::collaboration_sensitive::SensitiveError::Quarantined
        ))
    ));
    assert!(matches!(
        crate::local_source_transport::last_receipt(&f.store, &id),
        Err(crate::local_source_transport::TransportError::Sensitive(
            crate::collaboration_sensitive::SensitiveError::Quarantined
        ))
    ));
    assert!(matches!(
        crate::local_budget::load(&f.store, &id),
        Err(crate::local_budget::BudgetError::Sensitive(
            crate::collaboration_sensitive::SensitiveError::Quarantined
        ))
    ));
    assert!(matches!(
        crate::native_gates::load(&f.store, &id),
        Err(crate::native_gates::NativeGateError::Sensitive(
            crate::collaboration_sensitive::SensitiveError::Quarantined
        ))
    ));
    assert!(matches!(
        crate::local_controller::load(&f.store, &id),
        Err(crate::local_controller::ControllerError::Sensitive(
            crate::collaboration_sensitive::SensitiveError::Quarantined
        ))
    ));
}

#[test]
fn legacy_version_one_active_witness_remains_readable() {
    let (f, owner, _w, _a) = setup();
    let mut witness = status(&f.store).unwrap();
    witness.version = 1;
    witness.origin = None;
    write(&directory(&f.store).unwrap(), &witness).unwrap();

    assert_eq!(status(&f.store).unwrap().version, 1);
    assert!(guard(&f.store, owner.authority_epoch()).is_ok());
}

#[test]
fn interrupted_freeze_requires_explicit_completion_before_sensitive_response() {
    let (mut f, owner, _w, _a) = setup();
    let prior = owner.authority_epoch().clone();
    let epoch = fresh_epoch().unwrap();
    let witness_path = directory(&f.store).unwrap();
    write(
        &witness_path,
        &Witness {
            version: 1,
            origin: None,
            epoch: epoch.clone(),
            status: Status::Frozen,
            checkpoint: None,
            reason: Some("interrupted freeze".into()),
        },
    )
    .unwrap();

    assert_eq!(bound(f.store.read_connection()).unwrap(), Some(prior));
    assert!(matches!(
        frozen_guard(&f.store, &epoch),
        Err(AuthorityError::Stale)
    ));
    let completed = complete_interrupted_freeze(&mut f.store, &epoch).unwrap();
    assert_eq!(completed.epoch, epoch);
    assert!(completed.checkpoint.is_some());
    assert_eq!(
        bound(f.store.read_connection()).unwrap(),
        Some(epoch.clone())
    );
    assert!(frozen_guard(&f.store, &epoch).is_ok());
    assert!(complete_interrupted_freeze(&mut f.store, &epoch).is_err());
}

#[test]
fn empty_initial_installation_recovers_witness_written_before_first_binding() {
    let fixture = Fixture::new();
    let mut store = fixture.store;
    let epoch = fresh_epoch().unwrap();
    let witness_path = directory(&store).unwrap();
    prepare(&witness_path).unwrap();
    write(
        &witness_path,
        &Witness {
            version: 2,
            origin: Some(WitnessOrigin::InitialInstallation),
            epoch: epoch.clone(),
            status: Status::Frozen,
            checkpoint: None,
            reason: Some(INITIAL_INSTALLATION_INTERRUPTED.into()),
        },
    )
    .unwrap();

    assert_eq!(bound(store.read_connection()).unwrap(), None);
    let completed = complete_interrupted_freeze(&mut store, &epoch).unwrap();
    assert_eq!(completed.epoch, epoch);
    assert!(completed.checkpoint.is_some());
    assert_eq!(bound(store.read_connection()).unwrap(), Some(epoch.clone()));
    assert!(frozen_guard(&store, &epoch).is_ok());
}

#[test]
fn historical_local_records_survive_exact_initial_installation_recovery() {
    let (mut f, _owner, _w, _a) = setup();
    f.store
        .connection()
        .execute("DELETE FROM local_authority_state", [])
        .unwrap();
    let epoch = fresh_epoch().unwrap();
    let witness_path = directory(&f.store).unwrap();
    write(
        &witness_path,
        &Witness {
            version: 2,
            origin: Some(WitnessOrigin::InitialInstallation),
            epoch: epoch.clone(),
            status: Status::Frozen,
            checkpoint: None,
            reason: Some(INITIAL_INSTALLATION_INTERRUPTED.into()),
        },
    )
    .unwrap();

    let completed = complete_interrupted_freeze(&mut f.store, &epoch).unwrap();
    assert_eq!(completed.epoch, epoch);
    assert!(completed.checkpoint.is_some());
    assert_eq!(
        bound(f.store.read_connection()).unwrap(),
        Some(epoch.clone())
    );
    assert!(frozen_guard(&f.store, &epoch).is_ok());
    assert_eq!(status(&f.store).unwrap().status, Status::Frozen);
    assert_eq!(
        target::status(&f.store, "target").unwrap().phase,
        target::Phase::Stopped
    );
}

#[test]
fn populated_store_with_generic_missing_binding_stays_stale() {
    let (mut f, _owner, _w, _a) = setup();
    f.store
        .connection()
        .execute("DELETE FROM local_authority_state", [])
        .unwrap();
    let epoch = fresh_epoch().unwrap();
    let witness_path = directory(&f.store).unwrap();
    write(
        &witness_path,
        &Witness {
            version: 2,
            origin: Some(WitnessOrigin::OperatorFreeze),
            epoch: epoch.clone(),
            status: Status::Frozen,
            checkpoint: None,
            reason: Some("operator freeze".into()),
        },
    )
    .unwrap();

    assert!(matches!(
        complete_interrupted_freeze(&mut f.store, &epoch),
        Err(AuthorityError::Stale)
    ));
    assert_eq!(bound(f.store.read_connection()).unwrap(), None);
    assert!(status(&f.store).unwrap().checkpoint.is_none());
}

#[test]
fn public_freeze_cannot_claim_initial_installation_origin_with_reserved_reason() {
    let (mut f, owner, _w, _a) = setup();
    let prior = owner.authority_epoch().clone();
    let mut witness = freeze(&mut f.store, &prior, INITIAL_INSTALLATION_INTERRUPTED).unwrap();
    assert_eq!(witness.origin, Some(WitnessOrigin::OperatorFreeze));
    witness.checkpoint = None;
    write(&directory(&f.store).unwrap(), &witness).unwrap();
    f.store
        .connection()
        .execute("DELETE FROM local_authority_state", [])
        .unwrap();

    assert!(matches!(
        complete_interrupted_freeze(&mut f.store, &witness.epoch),
        Err(AuthorityError::Stale)
    ));
    assert_eq!(bound(f.store.read_connection()).unwrap(), None);
}

#[test]
fn restored_matching_handle_and_database_cannot_bypass_the_current_witness() {
    let (mut f, owner, _w, _a) = setup();
    let frozen = freeze(&mut f.store, owner.authority_epoch(), "revoke").unwrap();
    resume(&mut f.store, &frozen.epoch).unwrap();
    f.store
        .connection()
        .execute(
            "UPDATE local_authority_state SET epoch=?1",
            [owner.authority_epoch().as_str()],
        )
        .unwrap();
    assert!(matches!(
        guard(&f.store, owner.authority_epoch()),
        Err(AuthorityError::Stale)
    ));
}

#[test]
fn escaped_reasons_and_details_remain_readable_at_the_source_byte_limit() {
    let (mut f, owner, _w, a) = setup();
    let detail = "\0".repeat(4096);
    let frozen = freeze(&mut f.store, owner.authority_epoch(), &detail).unwrap();
    assert_eq!(status(&f.store).unwrap(), frozen);
    let observation = record_late_outcome(
        &f.store,
        &a,
        &RecordId::parse(&reference('e')).unwrap(),
        &detail,
    )
    .unwrap();
    assert_eq!(historical_outcomes(&f.store).unwrap(), vec![observation]);
    resume(&mut f.store, &frozen.epoch).unwrap();
}

#[test]
fn identical_history_retry_flushes_both_directories_after_interrupted_flush() {
    let (mut f, owner, _w, a) = setup();
    freeze(&mut f.store, owner.authority_epoch(), "stop").unwrap();
    let receipt = RecordId::parse(&reference('e')).unwrap();
    let first = record_late_outcome_with(&f.store, &a, &receipt, "late", |_| {
        Err(std::io::Error::other("injected directory flush failure").into())
    });
    assert!(first.is_err());
    assert_eq!(historical_outcomes(&f.store).unwrap().len(), 1);
    let mut flushed = Vec::new();
    record_late_outcome_with(&f.store, &a, &receipt, "late", |path| {
        flushed.push(path.to_owned());
        Ok(sync_directory(path)?)
    })
    .unwrap();
    let path = directory(&f.store).unwrap();
    assert_eq!(flushed, vec![path.join("history"), path]);
}

#[test]
fn an_active_witness_refuses_a_database_bound_to_another_epoch() {
    let (mut f, owner, _w, _a) = setup();
    let wrong = fresh_epoch().unwrap();
    f.store
        .connection()
        .execute(
            "UPDATE local_authority_state SET epoch=?1",
            [wrong.as_str()],
        )
        .unwrap();
    assert!(wave::available(&mut f.store, &owner, "A", 1).is_err());
    assert!(crate::collaboration_gc::plan(&mut f.store, &Default::default()).is_err());
    drop(owner);
    assert!(target::acquire_controller(&mut f.store, "target", "appointed").is_err());
}

fn reference(n: char) -> String {
    format!("sha256:{}", n.to_string().repeat(64))
}
fn units(n: i64) -> Units {
    Units {
        time_ms: n,
        ..Default::default()
    }
}
fn setup() -> (Fixture, target::ControllerOwner, RecordId, budget::Attempt) {
    let mut f = Fixture::new();
    let owner = target::acquire_controller(&mut f.store, "target", "appointed").unwrap();
    let spec = f.spec();
    let rev = target::propose_revision(&mut f.store, &owner, spec).unwrap();
    target::select_revision(&mut f.store, &owner, &rev.id).unwrap();
    wave::available(&mut f.store, &owner, "A", 0).unwrap();
    let Admission::Frozen(w) = wave::admit(
        &mut f.store,
        &owner,
        WavePolicy {
            debounce_ms: 0,
            ..Default::default()
        },
        0,
    )
    .unwrap() else {
        panic!("frozen")
    };
    budget::configure_project(
        &mut f.store,
        &owner,
        ProjectPolicy {
            authorization: reference('7'),
            price_profile: reference('8'),
            cap: units(10),
            recovery_headroom: units(1),
            mandatory_headroom: units(3),
            max_root_reservations: 32,
        },
    )
    .unwrap();
    budget::authorize_objective(&mut f.store, &owner, units(10)).unwrap();
    let r = budget::reserve(
        &mut f.store,
        &owner,
        &w.id,
        Request {
            nonce: reference('a'),
            recipe: reference('9'),
            kind: WorkKind::Composition,
            units: units(3),
            max_attempts: 2,
            deadline_ms: 1000,
        },
        0,
    )
    .unwrap();
    let a = budget::begin_attempt(&mut f.store, &owner, &r.id, 0).unwrap();
    (f, owner, w.id, a)
}
fn reopen(host: &Path) -> CollaborationStore {
    CollaborationStore::open(
        &CollaborationRoot::under_host_state(host),
        &ProjectKey::parse("local-target-test").unwrap(),
        &[],
    )
    .unwrap()
}

#[test]
fn freeze_rotates_external_authority_and_preserves_unknown_liveness_and_cost() {
    let (mut f, owner, w, a) = setup();
    // Simulate an outstanding composition intent independent of the model budget.
    f.store
        .connection()
        .execute(
            "UPDATE local_waves SET phase='composing' WHERE wave=?1",
            [w.as_str()],
        )
        .unwrap();
    let old = owner.authority_epoch().clone();
    let frozen = freeze(&mut f.store, &old, "operator stop").unwrap();
    assert_ne!(frozen.epoch, old);
    assert_eq!(frozen.status, Status::Frozen);
    assert_eq!(
        wave::load(&f.store, &w).unwrap().phase,
        WavePhase::OutcomeUnknown
    );
    assert_eq!(
        target::status(&f.store, "target").unwrap().phase,
        target::Phase::StopRequested
    );
    let r = budget::load(&f.store, a.reservation()).unwrap();
    assert_eq!(r.state, State::ConsumptionUnknown);
    assert_eq!(r.allocated, units(3));
    assert!(wave::available(&mut f.store, &owner, "A", 1).is_err());
    assert!(budget::begin_attempt(&mut f.store, &owner, a.reservation(), 1).is_err());
    assert!(
        budget::settle(
            &mut f.store,
            &owner,
            &a,
            units(1),
            &RecordId::parse(&reference('e')).unwrap()
        )
        .is_err()
    );
    let spec = f.spec();
    assert!(target::propose_revision(&mut f.store, &owner, spec).is_err());
    assert!(crate::collaboration_gc::plan(&mut f.store, &Default::default()).is_err());
    assert!(freeze(&mut f.store, &old, "duplicate").is_err());
    assert_eq!(status(&f.store).unwrap(), frozen);
}
#[test]
fn exact_resume_does_not_revive_an_old_owner_or_attempt_even_with_aba_state() {
    let (mut f, owner, w, a) = setup();
    let old = owner.authority_epoch().clone();
    let frozen = freeze(&mut f.store, &old, "revoke handles").unwrap();
    let resumed = resume(&mut f.store, &frozen.epoch).unwrap();
    assert_ne!(resumed.epoch, old);
    assert_ne!(resumed.epoch, frozen.epoch);
    assert!(wave::available(&mut f.store, &owner, "A", 1).is_err());
    assert!(target::acquire_controller(&mut f.store, "target", "appointed").is_err());
    drop(owner);
    let owner = target::acquire_controller(&mut f.store, "target", "appointed").unwrap();
    // Deliberately collide the mutable state/version with the original attempt.
    // The epoch must refuse even if a restored row makes other guards match.
    f.store.connection().execute("UPDATE local_budget_reservations SET phase='completion_pending',version=1 WHERE reservation=?1",[a.reservation().as_str()]).unwrap();
    assert!(matches!(
        budget::settle(
            &mut f.store,
            &owner,
            &a,
            units(1),
            &RecordId::parse(&reference('e')).unwrap()
        ),
        Err(budget::BudgetError::Stale)
    ));
    assert_eq!(wave::load(&f.store, &w).unwrap().phase, WavePhase::Stopped);
}
#[test]
fn announced_restore_behind_the_checkpoint_stays_frozen_with_old_handles_alive() {
    let (mut f, owner, _w, a) = setup();
    let backup = f._host.path().join("before.db");
    f.store
        .connection()
        .execute("VACUUM INTO ?1", [backup.to_str().unwrap()])
        .unwrap();
    // Data added after the backup cannot vanish from the reconciliation checkpoint.
    let receipt = RecordId::parse(&reference('e')).unwrap();
    budget::settle(&mut f.store, &owner, &a, units(1), &receipt).unwrap();
    let frozen = freeze(&mut f.store, owner.authority_epoch(), "announced restore").unwrap();
    let host = f._host.path().to_owned();
    let database = f.store.project_dir().join("state.db");
    drop(f.store);
    // Close SQLite before restoring the exact isolated fixture database.
    std::fs::copy(&backup, &database).unwrap();
    let mut restored = reopen(&host);
    assert!(matches!(
        resume(&mut restored, &frozen.epoch),
        Err(AuthorityError::Stale)
    ));
    assert!(wave::available(&mut restored, &owner, "A", 1).is_err());
    assert!(crate::collaboration_gc::plan(&mut restored, &Default::default()).is_err());
    assert_eq!(status(&restored).unwrap(), frozen);
}
#[test]
fn checkpoint_is_stable_across_reopen_and_detects_changed_budget_policy() {
    let (mut f, owner, _w, _a) = setup();
    let frozen = freeze(&mut f.store, owner.authority_epoch(), "freeze").unwrap();
    let mut reopened = reopen(f._host.path());
    assert_eq!(
        checkpoint(reopened.read_connection()).unwrap(),
        frozen.checkpoint.clone().unwrap()
    );
    reopened
        .connection()
        .execute("DELETE FROM local_budget_objectives", [])
        .unwrap();
    assert!(matches!(
        resume(&mut reopened, &frozen.epoch),
        Err(AuthorityError::Stale)
    ));
}
#[test]
fn missing_or_corrupt_witness_cannot_be_reinitialized_from_restored_db() {
    let (mut f, owner, _w, _a) = setup();
    drop(owner);
    let path = directory(&f.store).unwrap().join("witness.json");
    std::fs::remove_file(&path).unwrap();
    assert!(target::acquire_controller(&mut f.store, "target", "appointed").is_err());
    assert!(crate::collaboration_gc::plan(&mut f.store, &Default::default()).is_err());
    std::fs::write(path, b"{}").unwrap();
    assert!(target::acquire_controller(&mut f.store, "target", "appointed").is_err());
}
#[test]
fn interrupted_freeze_requires_exact_token_and_explicit_completion() {
    let (mut f, owner, _w, a) = setup();
    let path = directory(&f.store).unwrap();
    let frozen = Witness {
        version: 2,
        origin: Some(WitnessOrigin::OperatorFreeze),
        epoch: fresh_epoch().unwrap(),
        status: Status::Frozen,
        checkpoint: None,
        reason: Some("injected crash before checkpoint".into()),
    };
    write(&path, &frozen).unwrap();
    assert!(resume(&mut f.store, &frozen.epoch).is_err());
    assert!(complete_interrupted_freeze(&mut f.store, owner.authority_epoch()).is_err());
    let recovered = complete_interrupted_freeze(&mut f.store, &frozen.epoch).unwrap();
    assert!(recovered.checkpoint.is_some());
    assert_eq!(
        budget::load(&f.store, a.reservation()).unwrap().state,
        State::ConsumptionUnknown
    );
    assert!(complete_interrupted_freeze(&mut f.store, &recovered.epoch).is_err());
    resume(&mut f.store, &recovered.epoch).unwrap();
    assert!(wave::available(&mut f.store, &owner, "A", 1).is_err());
}
#[test]
fn a_mutation_guard_excludes_freeze_until_it_is_dropped() {
    let (mut f, owner, _w, _a) = setup();
    let held = target::authorize(&f.store, &owner).unwrap();
    let path = directory(&f.store).unwrap();
    assert!(
        ExclusiveFileLock::try_acquire(open_lock_file(&path.join("ingress.lock")).unwrap())
            .unwrap()
            .is_none()
    );
    drop(held);
    freeze(
        &mut f.store,
        owner.authority_epoch(),
        "after admitted mutation",
    )
    .unwrap();
}
#[test]
fn initial_witness_is_outside_the_restorable_project_directory() {
    let (mut f, owner, _w, _a) = setup();
    let path = directory(&f.store).unwrap();
    assert!(!path.starts_with(f.store.project_dir()));
    assert_eq!(
        bound(f.store.read_connection()).unwrap().as_ref(),
        Some(owner.authority_epoch())
    );
    let spec = f.spec();
    let before = checkpoint(f.store.read_connection()).unwrap();
    // A normal new target proposal changes the pinned authority data checkpoint.
    target::propose_revision(&mut f.store, &owner, spec).unwrap();
    assert_eq!(checkpoint(f.store.read_connection()).unwrap(), before);
}

#[test]
fn late_completion_is_historical_and_cannot_change_the_restored_target() {
    let (mut f, owner, _w, a) = setup();
    let receipt = RecordId::parse(&reference('e')).unwrap();
    assert!(record_late_outcome(&f.store, &a, &receipt, "before revocation").is_err());
    let frozen = freeze(&mut f.store, owner.authority_epoch(), "revoke").unwrap();
    let before = checkpoint(f.store.read_connection()).unwrap();
    let observation = record_late_outcome(
        &f.store,
        &a,
        &receipt,
        "worker reported success; unverified",
    )
    .unwrap();
    assert_eq!(
        record_late_outcome(
            &f.store,
            &a,
            &receipt,
            "worker reported success; unverified"
        )
        .unwrap(),
        observation
    );
    assert!(
        record_late_outcome(
            &f.store,
            &a,
            &RecordId::parse(&reference('f')).unwrap(),
            "different"
        )
        .is_err()
    );
    assert_eq!(
        historical_outcomes(&f.store).unwrap(),
        vec![observation.clone()]
    );
    assert_eq!(checkpoint(f.store.read_connection()).unwrap(), before);
    resume(&mut f.store, &frozen.epoch).unwrap();
    let reopened = reopen(f._host.path());
    assert_eq!(historical_outcomes(&reopened).unwrap(), vec![observation]);
    assert!(wave::available(&mut f.store, &owner, "A", 1).is_err());
}

#[test]
fn first_authority_installation_excludes_previously_admitted_gc() {
    let mut f = Fixture::new();
    let held = archive_guard(&f.store).unwrap();
    let path = directory(&f.store).unwrap();
    assert!(!path.join("witness.json").exists());
    assert!(
        ExclusiveFileLock::try_acquire(open_lock_file(&path.join("ingress.lock")).unwrap())
            .unwrap()
            .is_none()
    );
    drop(held);
    let owner = target::acquire_controller(&mut f.store, "target", "appointed").unwrap();
    freeze(&mut f.store, owner.authority_epoch(), "stop after GC").unwrap();
}

#[test]
fn real_gc_apply_holds_ingress_before_first_controller_installation() {
    use crate::collaboration_gc as gc;
    let mut f = Fixture::new();
    for prefix in std::fs::read_dir(crate::collaboration_archive::objects_dir(&f.store)).unwrap() {
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
    let options = gc::GcOptions::default();
    let plan = gc::plan(&mut f.store, &options).unwrap();
    assert!(!plan.reclaimable.is_empty());
    let mut apply_store = reopen(f._host.path());
    let (ready_send, ready_recv) = std::sync::mpsc::channel();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let callback_barrier = barrier.clone();
    let hooks = gc::GcHooks {
        after_moves: Some((
            1,
            Box::new(move || {
                ready_send.send(()).unwrap();
                callback_barrier.wait();
            }),
        )),
        ..Default::default()
    };
    let path = directory(&f.store).unwrap();
    let held = std::thread::scope(|scope| {
        let work =
            scope.spawn(move || gc::apply_with(&mut apply_store, &plan.digest, &options, &hooks));
        ready_recv
            .recv_timeout(std::time::Duration::from_secs(30))
            .unwrap();
        let probe =
            ExclusiveFileLock::try_acquire(open_lock_file(&path.join("ingress.lock")).unwrap())
                .unwrap();
        let held = probe.is_none();
        drop(probe);
        // Release even if the protection was neutered; never strand our worker.
        barrier.wait();
        work.join().unwrap().unwrap();
        held
    });
    assert!(
        held,
        "actual reclamation must exclude first installation/freeze"
    );
    let owner = target::acquire_controller(&mut f.store, "target", "appointed").unwrap();
    freeze(
        &mut f.store,
        owner.authority_epoch(),
        "after completed reclamation",
    )
    .unwrap();
}
#[test]
#[ignore = "spawned by authority_process_survives_announced_restore"]
fn authority_worker_child() {
    let host =
        std::path::PathBuf::from(std::env::var_os("AETHYME_L5_AUTHORITY_CHILD_HOST").unwrap());
    let mut store = reopen(&host);
    let owner = target::acquire_controller(&mut store, "target", "appointed").unwrap();
    drop(store); // A worker handle survives; SQLite is quiesced for restore.
    std::fs::write(host.join("worker-ready"), b"ready").unwrap();
    for _ in 0..3000 {
        if host.join("worker-go").exists() {
            let mut restored = reopen(&host);
            assert!(wave::available(&mut restored, &owner, "A", 2).is_err());
            std::fs::write(
                host.join("worker-refused"),
                b"old controller mutation refused",
            )
            .unwrap();
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("parent never released worker");
}
struct ChildGuard(std::process::Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            if let Err(e) = self.0.kill() {
                eprintln!("own child cleanup: {e}");
            }
            if let Err(e) = self.0.wait() {
                eprintln!("own child wait: {e}");
            }
        }
    }
}
#[test]
fn authority_process_survives_announced_restore_and_cannot_mutate_current_data() {
    let (mut f, owner, _w, a) = setup();
    let backup = f._host.path().join("process-before.db");
    f.store
        .connection()
        .execute("VACUUM INTO ?1", [backup.to_str().unwrap()])
        .unwrap();
    let expected = owner.authority_epoch().clone();
    drop(owner);
    let mut child = ChildGuard(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "local_authority::tests::authority_worker_child",
                "--ignored",
                "--nocapture",
            ])
            .env("AETHYME_L5_AUTHORITY_CHILD_HOST", f._host.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let host = f._host.path().to_owned();
    for _ in 0..3000 {
        if host.join("worker-ready").exists() {
            break;
        }
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "worker exited before ready"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(host.join("worker-ready").exists());
    // Record cost after backup; a rewind must not make that history disappear.
    let owner_epoch = status(&f.store).unwrap().epoch;
    assert_eq!(owner_epoch, expected);
    f.store.connection().execute("UPDATE local_budget_reservations SET detail='external outcome may have happened' WHERE reservation=?1",[a.reservation().as_str()]).unwrap();
    let frozen = freeze(
        &mut f.store,
        &expected,
        "announced restore with surviving child",
    )
    .unwrap();
    assert!(child.0.try_wait().unwrap().is_none());
    let database = f.store.project_dir().join("state.db");
    drop(f.store);
    std::fs::copy(backup, database).unwrap();
    let mut restored = reopen(&host);
    assert!(matches!(
        resume(&mut restored, &frozen.epoch),
        Err(AuthorityError::Stale)
    ));
    std::fs::write(host.join("worker-go"), b"restore finished frozen").unwrap();
    for _ in 0..3000 {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(host.join("worker-refused").exists());
    assert_eq!(status(&restored).unwrap(), frozen);
}
#[test]
fn checkpoint_orders_equal_first_columns_independently_of_insertion_order() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE local_fixture(k TEXT,n INTEGER);INSERT INTO local_fixture VALUES('same',2),('same',1);").unwrap();
    let first = checkpoint(&c).unwrap();
    c.execute_batch(
        "DELETE FROM local_fixture;INSERT INTO local_fixture VALUES('same',1),('same',2);",
    )
    .unwrap();
    assert_eq!(checkpoint(&c).unwrap(), first);
}
#[test]
fn independent_checkpoint_vector_binds_table_types_and_row_bytes() {
    let vector: serde_json::Value =
        serde_json::from_str(include_str!("checkpoint-vector.json")).unwrap();
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE local_fixture(q TEXT,n INTEGER,payload BLOB);INSERT INTO local_fixture VALUES('slot',7,x'00ff');").unwrap();
    assert_eq!(checkpoint(&c).unwrap(), vector["id"].as_str().unwrap());
    c.execute_batch("UPDATE local_fixture SET n=8;").unwrap();
    assert_ne!(checkpoint(&c).unwrap(), vector["id"].as_str().unwrap());
}
