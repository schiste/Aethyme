use super::*;
use crate::local_target::tests::Fixture;

fn units(n: i64) -> Units {
    Units {
        time_ms: n,
        memory_bytes: n,
        disk_bytes: n,
        model_requests: n,
        input_tokens: n,
        output_tokens: n,
        spend_microunits: n,
        slots: n,
        previews: n,
    }
}
fn reference(n: char) -> String {
    format!("sha256:{}", n.to_string().repeat(64))
}
fn policy(cap: i64, recovery: i64, mandatory: i64) -> ProjectPolicy {
    ProjectPolicy {
        authorization: reference('7'),
        price_profile: reference('8'),
        cap: units(cap),
        recovery_headroom: units(recovery),
        mandatory_headroom: units(mandatory),
        max_root_reservations: 32,
    }
}
fn request(n: char, amount: i64, kind: WorkKind) -> Request {
    Request {
        nonce: reference(n),
        recipe: reference('9'),
        kind,
        units: units(amount),
        max_attempts: 2,
        deadline_ms: 1000,
    }
}
fn setup() -> (Fixture, ControllerOwner, RecordId) {
    let mut f = Fixture::new();
    let owner = target::acquire_controller(&mut f.store, "target", "appointed").unwrap();
    let spec = f.spec();
    let revision = target::propose_revision(&mut f.store, &owner, spec).unwrap();
    target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
    wave::available(&mut f.store, &owner, "A", 0).unwrap();
    let admission = wave::admit(
        &mut f.store,
        &owner,
        wave::WavePolicy {
            debounce_ms: 0,
            ..wave::WavePolicy::default()
        },
        0,
    )
    .unwrap();
    let wave::Admission::Frozen(wave) = admission else {
        panic!("{admission:?}")
    };
    configure_project(&mut f.store, &owner, policy(10, 1, 3)).unwrap();
    authorize_objective(&mut f.store, &owner, units(10)).unwrap();
    (f, owner, wave.id)
}
fn receipt() -> RecordId {
    id(&reference('e')).unwrap()
}

#[test]
fn immutable_project_and_objective_caps_cannot_replenish_spending() {
    let (mut f, owner, wave) = setup();
    assert!(matches!(
        configure_project(&mut f.store, &owner, policy(100, 1, 3)),
        Err(BudgetError::Immutable)
    ));
    assert!(matches!(
        authorize_objective(&mut f.store, &owner, units(9)),
        Err(BudgetError::Immutable)
    ));
    let r = reserve(
        &mut f.store,
        &owner,
        &wave,
        request('a', 6, WorkKind::Composition),
        0,
    )
    .unwrap();
    let attempt = begin_attempt(&mut f.store, &owner, &r.id, 0).unwrap();
    let r = settle(&mut f.store, &owner, &attempt, units(5), &receipt()).unwrap();
    close(&mut f.store, &owner, &r.id, r.version).unwrap();
    authorize_objective(&mut f.store, &owner, units(10)).unwrap();
    configure_project(&mut f.store, &owner, policy(10, 1, 3)).unwrap();
    assert_eq!(charged(&f.store, &owner).unwrap(), units(5));
    assert!(matches!(
        reserve(
            &mut f.store,
            &owner,
            &wave,
            request('b', 6, WorkKind::Composition),
            0
        ),
        Err(BudgetError::Capacity(_))
    ));
}

#[test]
fn duplicate_reservation_and_dispatch_never_mint_another_allowance() {
    let (mut f, owner, wave) = setup();
    let request = request('a', 3, WorkKind::Composition);
    let r = reserve(&mut f.store, &owner, &wave, request.clone(), 0).unwrap();
    assert_eq!(
        reserve(&mut f.store, &owner, &wave, request.clone(), 1).unwrap(),
        r
    );
    let mut changed = request;
    changed.units = units(4);
    assert!(matches!(
        reserve(&mut f.store, &owner, &wave, changed, 2),
        Err(BudgetError::Immutable)
    ));
    let attempt = begin_attempt(&mut f.store, &owner, &r.id, 2).unwrap();
    assert_eq!(attempt.remaining(), units(3));
    assert!(matches!(
        begin_attempt(&mut f.store, &owner, &r.id, 3),
        Err(BudgetError::Pending)
    ));
    let r = settle(&mut f.store, &owner, &attempt, units(1), &receipt()).unwrap();
    assert!(matches!(
        settle(&mut f.store, &owner, &attempt, units(1), &receipt()),
        Err(BudgetError::Stale)
    ));
    assert_eq!(r.spent, units(1));
    assert_eq!(charged(&f.store, &owner).unwrap(), units(3));
}

#[test]
fn retry_preserves_spent_units_and_one_finite_lineage_allowance() {
    let (mut f, owner, wave) = setup();
    let r = reserve(
        &mut f.store,
        &owner,
        &wave,
        request('a', 3, WorkKind::Composition),
        0,
    )
    .unwrap();
    let one = begin_attempt(&mut f.store, &owner, &r.id, 0).unwrap();
    settle(&mut f.store, &owner, &one, units(1), &receipt()).unwrap();
    let two = begin_attempt(&mut f.store, &owner, &r.id, 1).unwrap();
    assert_eq!(two.number(), 2);
    assert_eq!(two.remaining(), units(2));
    let r = settle(&mut f.store, &owner, &two, units(1), &receipt()).unwrap();
    assert_eq!(r.spent, units(2));
    assert!(matches!(
        begin_attempt(&mut f.store, &owner, &r.id, 2),
        Err(BudgetError::AttemptsExhausted)
    ));
    let closed = close(&mut f.store, &owner, &r.id, r.version).unwrap();
    assert_eq!(closed.state, State::Consumed);
    assert_eq!(charged(&f.store, &owner).unwrap(), units(2));
    assert_eq!(
        reserve(
            &mut f.store,
            &owner,
            &wave,
            request('a', 3, WorkKind::Composition),
            3
        )
        .unwrap(),
        closed
    );
}

#[test]
fn optional_analysis_cannot_consume_mandatory_or_recovery_headroom() {
    let (mut f, owner, wave) = setup();
    reserve(
        &mut f.store,
        &owner,
        &wave,
        request('a', 6, WorkKind::OptionalAnalysis),
        0,
    )
    .unwrap();
    assert!(matches!(
        reserve(
            &mut f.store,
            &owner,
            &wave,
            request('b', 1, WorkKind::OptionalAnalysis),
            0
        ),
        Err(BudgetError::Capacity("mandatory headroom"))
    ));
    reserve(
        &mut f.store,
        &owner,
        &wave,
        request('c', 3, WorkKind::Composition),
        0,
    )
    .unwrap();
    assert!(matches!(
        reserve(
            &mut f.store,
            &owner,
            &wave,
            request('d', 1, WorkKind::Composition),
            0
        ),
        Err(BudgetError::Capacity("recovery headroom"))
    ));
    reserve(
        &mut f.store,
        &owner,
        &wave,
        request('e', 1, WorkKind::Recovery),
        0,
    )
    .unwrap();
    assert_eq!(charged(&f.store, &owner).unwrap(), units(10));
}

#[test]
fn delegated_allocations_partition_parent_and_share_retry_spend() {
    let (mut f, owner, wave) = setup();
    let root = reserve(
        &mut f.store,
        &owner,
        &wave,
        request('a', 6, WorkKind::Composition),
        0,
    )
    .unwrap();
    let child = delegate(
        &mut f.store,
        &owner,
        &root.id,
        request('b', 4, WorkKind::Composition),
        0,
    )
    .unwrap();
    assert_eq!(child.root, root.id);
    assert_eq!(charged(&f.store, &owner).unwrap(), units(6));
    assert!(matches!(
        delegate(
            &mut f.store,
            &owner,
            &root.id,
            request('c', 3, WorkKind::Composition),
            0
        ),
        Err(BudgetError::Capacity("parent"))
    ));
    assert!(matches!(
        begin_attempt(&mut f.store, &owner, &root.id, 0),
        Err(BudgetError::Pending)
    ));
    let attempt = begin_attempt(&mut f.store, &owner, &child.id, 0).unwrap();
    let child = settle(&mut f.store, &owner, &attempt, units(2), &receipt()).unwrap();
    close(&mut f.store, &owner, &child.id, child.version).unwrap();
    let attempt = begin_attempt(&mut f.store, &owner, &root.id, 0).unwrap();
    assert_eq!(attempt.remaining(), units(4));
    let root = settle(&mut f.store, &owner, &attempt, units(1), &receipt()).unwrap();
    assert!(matches!(
        begin_attempt(&mut f.store, &owner, &root.id, 0),
        Err(BudgetError::AttemptsExhausted)
    ));
    let root = close(&mut f.store, &owner, &root.id, root.version).unwrap();
    assert_eq!(root.spent, units(3));
    assert_eq!(charged(&f.store, &owner).unwrap(), units(3));
}

#[test]
fn unknown_consumption_survives_deadline_and_holds_retention() {
    let (mut f, owner, wave) = setup();
    let r = reserve(
        &mut f.store,
        &owner,
        &wave,
        request('a', 9, WorkKind::Composition),
        0,
    )
    .unwrap();
    let attempt = begin_attempt(&mut f.store, &owner, &r.id, 0).unwrap();
    let r = load(&f.store, &r.id).unwrap();
    assert_eq!(r.state, State::CompletionPending);
    let r = mark_unknown(
        &mut f.store,
        &owner,
        &r.id,
        r.version,
        "worker liveness unknown",
    )
    .unwrap();
    assert_eq!(charged(&f.store, &owner).unwrap(), units(9));
    assert!(matches!(
        close(&mut f.store, &owner, &r.id, r.version),
        Err(BudgetError::Pending)
    ));
    assert!(begin_attempt(&mut f.store, &owner, &r.id, 2000).is_err());
    wave::stop(&mut f.store, &owner, &wave).unwrap();
    let revision = id(&r.spec.revision).unwrap();
    assert!(matches!(
        target::release_revision(&mut f.store, &owner, &revision),
        Err(TargetError::Frozen)
    ));
    let mut spec = f.spec();
    spec.selected = vec!["B".into()];
    let next = target::propose_revision(&mut f.store, &owner, spec).unwrap();
    assert!(matches!(
        target::select_revision(&mut f.store, &owner, &next.id),
        Err(TargetError::Frozen)
    ));
    assert!(matches!(
        settle(&mut f.store, &owner, &attempt, units(1), &receipt()),
        Err(BudgetError::Stale)
    ));
    let r = reconcile(&mut f.store, &owner, &r.id, r.version, units(2), &receipt()).unwrap();
    let r = close(&mut f.store, &owner, &r.id, r.version).unwrap();
    assert_eq!(charged(&f.store, &owner).unwrap(), units(2));
    target::select_revision(&mut f.store, &owner, &next.id).unwrap();
    target::release_revision(&mut f.store, &owner, &revision).unwrap();
    assert_eq!(r.state, State::Consumed);
}

#[test]
fn usage_over_the_envelope_stays_unknown_instead_of_releasing_capacity() {
    let (mut f, owner, wave) = setup();
    let r = reserve(
        &mut f.store,
        &owner,
        &wave,
        request('a', 2, WorkKind::Composition),
        0,
    )
    .unwrap();
    let attempt = begin_attempt(&mut f.store, &owner, &r.id, 0).unwrap();
    assert!(matches!(
        settle(&mut f.store, &owner, &attempt, units(3), &receipt()),
        Err(BudgetError::UsageExceeded)
    ));
    let r = load(&f.store, &r.id).unwrap();
    assert_eq!(r.state, State::ConsumptionUnknown);
    assert_eq!(charged(&f.store, &owner).unwrap(), units(2));
    assert!(close(&mut f.store, &owner, &r.id, r.version).is_err());
}

#[test]
fn delegation_cannot_promote_priority_change_recipe_or_deadline() {
    let (mut f, owner, wave) = setup();
    let r = reserve(
        &mut f.store,
        &owner,
        &wave,
        request('a', 3, WorkKind::OptionalAnalysis),
        0,
    )
    .unwrap();
    let mut changed = request('b', 1, WorkKind::Recovery);
    assert!(delegate(&mut f.store, &owner, &r.id, changed.clone(), 0).is_err());
    changed.kind = WorkKind::OptionalAnalysis;
    changed.recipe = reference('f');
    assert!(delegate(&mut f.store, &owner, &r.id, changed.clone(), 0).is_err());
    changed.recipe = reference('9');
    changed.deadline_ms = 1001;
    assert!(delegate(&mut f.store, &owner, &r.id, changed.clone(), 0).is_err());
    changed.deadline_ms = 1000;
    changed.max_attempts = 3;
    assert!(delegate(&mut f.store, &owner, &r.id, changed, 0).is_err());
}

#[test]
fn bounds_and_missing_authorizations_refuse_before_any_reservation() {
    let (mut f, owner, wave) = setup();
    let mut bad = request('a', -1, WorkKind::Composition);
    assert!(reserve(&mut f.store, &owner, &wave, bad.clone(), 0).is_err());
    bad.units = units(1);
    bad.max_attempts = 9;
    assert!(reserve(&mut f.store, &owner, &wave, bad.clone(), 0).is_err());
    bad.max_attempts = 2;
    bad.deadline_ms = 100_000_000;
    assert!(reserve(&mut f.store, &owner, &wave, bad, 0).is_err());
    assert_eq!(charged(&f.store, &owner).unwrap(), Units::default());
}

#[test]
fn independent_project_identity_vector_binds_all_cap_dimensions() {
    let vector: serde_json::Value =
        serde_json::from_str(include_str!("project-vector.json")).unwrap();
    let bytes = vector["canonical"].as_str().unwrap().as_bytes();
    let r = Record::decode(bytes, &[&SCHEMA]).unwrap();
    assert_eq!(r.canonical_bytes(), bytes);
    assert_eq!(r.id().as_str(), vector["id"].as_str().unwrap());
    let spec: ProjectRecord = decode(r.id().as_str(), bytes).unwrap();
    spec.policy.validate().unwrap();
    let mut changed = spec;
    changed.policy.cap.spend_microunits += 1;
    assert_ne!(record(&changed).unwrap().id(), r.id());
}
