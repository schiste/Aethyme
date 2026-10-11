use super::*;
use crate::collaboration_state::{CollaborationRoot, ProjectKey};
use crate::local_budget::{self as budget, ProjectPolicy, Request, State, Units, WorkKind};
use crate::local_target::{self as target, tests::Fixture};
use crate::local_wave::{self as wave, Admission, WavePhase, WavePolicy};

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
        version: 1,
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
