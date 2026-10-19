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
    setup_with_cap(10)
}
fn setup_with_cap(cap: i64) -> (Fixture, ControllerOwner, RecordId) {
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
    authorize_objective(&mut f.store, &owner, units(cap)).unwrap();
    (f, owner, wave.id)
}

#[test]
fn generic_reserve_refuses_resolution_without_ledger_mutation() {
    let (mut f, owner, wave) = setup();
    let before: (i64, i64) = f
        .store
        .read_connection()
        .query_row(
            "SELECT
                 (SELECT count(*) FROM local_budget_reservations),
                 (SELECT count(*) FROM resolution_attempts)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();

    let error = reserve(
        &mut f.store,
        &owner,
        &wave,
        request('a', 3, WorkKind::Resolution),
        0,
    )
    .unwrap_err();
    assert!(matches!(error, BudgetError::ResolutionRoute));

    let after: (i64, i64) = f
        .store
        .read_connection()
        .query_row(
            "SELECT
                 (SELECT count(*) FROM local_budget_reservations),
                 (SELECT count(*) FROM resolution_attempts)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(after, before);
    assert_eq!(charged(&f.store, &owner).unwrap(), Units::default());
}

#[test]
fn generic_resolution_mutators_refuse_without_changing_pending_reservation() {
    let (mut f, owner, wave_id) = setup();
    let current_wave = wave::load(&f.store, &wave_id).unwrap();
    let (project_id, _) = project(f.store.read_connection(), owner.issuer()).unwrap();
    let (objective_id, _) = objective(
        f.store.read_connection(),
        owner.issuer(),
        &current_wave.spec.objective_authorization,
        &project_id,
    )
    .unwrap();
    let mut resolution_request = request('a', 3, WorkKind::Resolution);
    resolution_request.max_attempts = 1;
    let spec = ReservationSpec {
        issuer: owner.issuer().into(),
        target: owner.target().into(),
        revision: current_wave.spec.revision,
        wave: wave_id.to_string(),
        objective: objective_id.to_string(),
        project_policy: project_id.to_string(),
        objective_policy: objective_id.to_string(),
        request: resolution_request,
        parent: None,
    };
    let reservation_record = record(&spec).unwrap();
    let reservation_id = reservation_record.id();
    insert(
        f.store.connection(),
        &reservation_record,
        &spec,
        &reservation_id,
    )
    .unwrap();
    f.store
        .connection()
        .execute(
            "UPDATE local_budget_reservations
             SET phase='completion_pending',attempts=1,version=1
             WHERE reservation=?1",
            [reservation_id.as_str()],
        )
        .unwrap();

    let attempt = Attempt {
        reservation: reservation_id.clone(),
        version: 1,
        number: 1,
        remaining: units(3),
        epoch: owner.authority_epoch().clone(),
        project_dir: f.store.project_dir().to_owned(),
    };
    let before: (String, i64, i64, i64) = f
        .store
        .read_connection()
        .query_row(
            "SELECT phase,version,attempts,
                    (SELECT count(*) FROM local_budget_receipts WHERE reservation=?1)
             FROM local_budget_reservations WHERE reservation=?1",
            [reservation_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();

    let generic_attempt = begin_attempt(&mut f.store, &owner, &reservation_id, 0);
    assert!(
        matches!(&generic_attempt, Err(BudgetError::ResolutionRoute)),
        "generic resolution attempt returned an unexpected error: {generic_attempt:?}"
    );
    assert!(matches!(
        settle(&mut f.store, &owner, &attempt, units(1), &receipt()),
        Err(BudgetError::ResolutionRoute)
    ));
    assert!(matches!(
        reconcile(
            &mut f.store,
            &owner,
            &reservation_id,
            1,
            units(1),
            &receipt()
        ),
        Err(BudgetError::ResolutionRoute)
    ));
    assert!(matches!(
        close(&mut f.store, &owner, &reservation_id, 1),
        Err(BudgetError::ResolutionRoute)
    ));
    assert!(matches!(
        delegate(
            &mut f.store,
            &owner,
            &reservation_id,
            request('b', 1, WorkKind::Composition),
            0
        ),
        Err(BudgetError::ResolutionRoute)
    ));

    let after: (String, i64, i64, i64) = f
        .store
        .read_connection()
        .query_row(
            "SELECT phase,version,attempts,
                    (SELECT count(*) FROM local_budget_receipts WHERE reservation=?1)
             FROM local_budget_reservations WHERE reservation=?1",
            [reservation_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(after, before);
}

fn receipt() -> RecordId {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0x10000);
    let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    id(&format!("sha256:{sequence:064x}")).unwrap()
}

#[test]
fn nonce_status_recovers_pending_dispatch_without_replaying_it() {
    let (mut f, owner, wave) = setup();
    let request = request('a', 3, WorkKind::Composition);
    let reserved = reserve(&mut f.store, &owner, &wave, request.clone(), 0).unwrap();
    let attempt = begin_attempt(&mut f.store, &owner, &reserved.id, 0).unwrap();

    let initial_status = status(
        &f.store,
        owner.target(),
        owner.issuer(),
        None,
        Some(&request.nonce),
    )
    .unwrap();
    let lookup = initial_status.nonce_lookup.unwrap();
    assert_eq!(lookup.outcome, NonceLookupOutcome::Found);
    assert_eq!(lookup.matches.len(), 1);
    assert_eq!(lookup.matches[0].id, reserved.id.as_str());
    assert_eq!(lookup.matches[0].phase, State::CompletionPending);
    assert_eq!(lookup.matches[0].attempts, 1);
    assert_eq!(lookup.matches[0].version, attempt.version);

    // Reconciliation is read-only: a second lookup reports the same pending
    // attempt and does not begin another attempt or change its version.
    let repeated = status(
        &f.store,
        owner.target(),
        owner.issuer(),
        None,
        Some(&request.nonce),
    )
    .unwrap()
    .nonce_lookup
    .unwrap();
    assert_eq!(repeated, lookup);

    let absent = status(
        &f.store,
        owner.target(),
        owner.issuer(),
        None,
        Some(&reference('f')),
    )
    .unwrap()
    .nonce_lookup
    .unwrap();
    assert_eq!(absent.outcome, NonceLookupOutcome::NotFound);
    assert!(absent.matches.is_empty());
}

#[test]
fn duplicate_nonce_matches_are_classified_as_ambiguous() {
    assert_eq!(nonce_lookup_outcome(0), NonceLookupOutcome::NotFound);
    assert_eq!(nonce_lookup_outcome(1), NonceLookupOutcome::Found);
    assert_eq!(nonce_lookup_outcome(2), NonceLookupOutcome::Ambiguous);
}

#[test]
fn nonce_status_finds_historical_reservation_after_revision_and_epoch_change() {
    let (mut f, owner, wave_id) = setup();
    let request = request('a', 3, WorkKind::Composition);
    let reservation = reserve(&mut f.store, &owner, &wave_id, request.clone(), 0).unwrap();
    let original_revision = reservation.spec.revision.clone();
    let attempt = begin_attempt(&mut f.store, &owner, &reservation.id, 0).unwrap();
    let settled = settle(&mut f.store, &owner, &attempt, units(1), &receipt()).unwrap();
    close(&mut f.store, &owner, &reservation.id, settled.version).unwrap();

    crate::local_wave::stop(&mut f.store, &owner, &wave_id).unwrap();
    let previous = target::status(&f.store, owner.target())
        .unwrap()
        .revision
        .unwrap();
    let mut next_spec = f.spec();
    next_spec.objective = "a later target revision".into();
    let next = target::propose_revision(&mut f.store, &owner, next_spec).unwrap();
    target::select_revision(&mut f.store, &owner, &next.id).unwrap();
    assert_ne!(previous, next.id);

    let witness = crate::local_authority::status(&f.store).unwrap();
    let frozen = crate::local_authority::freeze(
        &mut f.store,
        &witness.epoch,
        "rotate the target authority epoch",
    )
    .unwrap();
    crate::local_authority::resume(&mut f.store, &frozen.epoch).unwrap();

    let status = status(
        &f.store,
        owner.target(),
        owner.issuer(),
        None,
        Some(&request.nonce),
    )
    .unwrap();
    let lookup = status.nonce_lookup.unwrap();
    assert_eq!(lookup.outcome, NonceLookupOutcome::Found);
    assert_eq!(lookup.matches.len(), 1);
    assert_eq!(lookup.matches[0].id, reservation.id.as_str());
    assert_eq!(lookup.matches[0].revision, original_revision);
    assert_eq!(
        target::status(&f.store, owner.target()).unwrap().revision,
        Some(next.id)
    );
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
fn direct_and_delegated_paths_share_one_nonce_identity() {
    let reservation_rows = |store: &CollaborationStore| {
        let connection = store.read_connection();
        let mut statement = connection
            .prepare("SELECT * FROM local_budget_reservations ORDER BY reservation")
            .unwrap();
        let columns = statement.column_count();
        statement
            .query_map([], |row| {
                (0..columns)
                    .map(|column| row.get::<_, rusqlite::types::Value>(column))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    let ledger_counts = |store: &CollaborationStore| {
        let connection = store.read_connection();
        let roots = connection
            .query_row(
                "SELECT count(*) FROM local_budget_reservations WHERE parent IS NULL",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        let children = connection
            .query_row(
                "SELECT count(*) FROM local_budget_reservations WHERE parent IS NOT NULL",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        let attempts = connection
            .query_row(
                "SELECT coalesce(sum(attempts), 0) FROM local_budget_reservations",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        let receipts = connection
            .query_row("SELECT count(*) FROM local_budget_receipts", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap();
        (roots, children, attempts, receipts)
    };

    // The direct reservation exists first. Delegation with its nonce must
    // refuse before any parent or project capacity accounting can change.
    let (mut direct_first, direct_owner, direct_wave) = setup();
    let parent = reserve(
        &mut direct_first.store,
        &direct_owner,
        &direct_wave,
        request('a', 4, WorkKind::Composition),
        0,
    )
    .unwrap();
    let shared = request('b', 1, WorkKind::Composition);
    let direct = reserve(
        &mut direct_first.store,
        &direct_owner,
        &direct_wave,
        shared.clone(),
        0,
    )
    .unwrap();
    assert_ne!(parent.id, direct.id);
    assert_eq!(direct.root, direct.id);
    assert!(shared.units.within(parent.allocated));
    let direct_rows = reservation_rows(&direct_first.store);
    let direct_charge = charged(&direct_first.store, &direct_owner).unwrap();
    let direct_counts = ledger_counts(&direct_first.store);
    assert_eq!(direct_charge, units(5));
    assert!(direct_charge.add(units(1)).unwrap().within(units(9)));
    assert_eq!(direct_counts, (2, 0, 0, 0));
    assert!(matches!(
        delegate(
            &mut direct_first.store,
            &direct_owner,
            &parent.id,
            shared.clone(),
            0
        ),
        Err(BudgetError::Immutable)
    ));
    assert_eq!(
        reserve(
            &mut direct_first.store,
            &direct_owner,
            &direct_wave,
            shared,
            1
        )
        .unwrap(),
        direct
    );
    assert_eq!(reservation_rows(&direct_first.store), direct_rows);
    assert_eq!(
        charged(&direct_first.store, &direct_owner).unwrap(),
        direct_charge
    );
    assert_eq!(ledger_counts(&direct_first.store), direct_counts);

    // The delegated reservation exists first. Reserving its nonce as a new
    // root also has spare project capacity, but remains the same identity.
    let (mut delegated_first, delegated_owner, delegated_wave) = setup();
    let parent = reserve(
        &mut delegated_first.store,
        &delegated_owner,
        &delegated_wave,
        request('a', 4, WorkKind::Composition),
        0,
    )
    .unwrap();
    let shared = request('b', 1, WorkKind::Composition);
    let child = delegate(
        &mut delegated_first.store,
        &delegated_owner,
        &parent.id,
        shared.clone(),
        0,
    )
    .unwrap();
    assert_eq!(child.root, parent.id);
    assert_eq!(child.spec.parent.as_deref(), Some(parent.id.as_str()));
    let delegated_rows = reservation_rows(&delegated_first.store);
    let delegated_charge = charged(&delegated_first.store, &delegated_owner).unwrap();
    let delegated_counts = ledger_counts(&delegated_first.store);
    assert_eq!(delegated_charge, units(4));
    assert!(delegated_charge.add(units(1)).unwrap().within(units(9)));
    assert_eq!(delegated_counts, (1, 1, 0, 0));
    assert!(matches!(
        reserve(
            &mut delegated_first.store,
            &delegated_owner,
            &delegated_wave,
            shared.clone(),
            0
        ),
        Err(BudgetError::Immutable)
    ));
    assert_eq!(
        delegate(
            &mut delegated_first.store,
            &delegated_owner,
            &parent.id,
            shared,
            1
        )
        .unwrap(),
        child
    );
    assert_eq!(reservation_rows(&delegated_first.store), delegated_rows);
    assert_eq!(
        charged(&delegated_first.store, &delegated_owner).unwrap(),
        delegated_charge
    );
    assert_eq!(ledger_counts(&delegated_first.store), delegated_counts);

    // Reacquiring the appointed target under a different issuer still fails.
    let before_wrong_issuer = reservation_rows(&direct_first.store);
    let mut reopened = reopen(&direct_first);
    drop(direct_owner);
    assert!(matches!(
        target::acquire_controller(&mut reopened, "target", "intruder"),
        Err(TargetError::Unauthorized)
    ));
    assert_eq!(reservation_rows(&reopened), before_wrong_issuer);
    assert_eq!(ledger_counts(&reopened), direct_counts);
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

fn reopen(f: &Fixture) -> CollaborationStore {
    use crate::collaboration_state::{CollaborationRoot, ProjectKey};
    CollaborationStore::open(
        &CollaborationRoot::under_host_state(f._host.path()),
        &ProjectKey::parse("local-target-test").unwrap(),
        &[],
    )
    .unwrap()
}
fn another_target(f: &mut Fixture) -> (ControllerOwner, RecordId) {
    let owner = target::acquire_controller(&mut f.store, "other", "appointed").unwrap();
    let mut spec = f.spec();
    spec.objective_authorization = reference('2');
    let revision = target::propose_revision(&mut f.store, &owner, spec).unwrap();
    target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
    wave::available(&mut f.store, &owner, "A", 0).unwrap();
    let wave::Admission::Frozen(w) = wave::admit(
        &mut f.store,
        &owner,
        wave::WavePolicy {
            debounce_ms: 0,
            ..Default::default()
        },
        0,
    )
    .unwrap() else {
        panic!("not frozen")
    };
    authorize_objective(&mut f.store, &owner, units(10)).unwrap();
    (owner, w.id)
}
#[test]
fn concurrent_targets_and_new_objectives_share_one_project_cap() {
    let (mut f, one, w1) = setup();
    let (two, w2) = another_target(&mut f);
    let mut s1 = reopen(&f);
    let mut s2 = reopen(&f);
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let b = &barrier;
        let a = scope.spawn(move || {
            b.wait();
            reserve(&mut s1, &one, &w1, request('a', 6, WorkKind::Recovery), 0)
        });
        let b = &barrier;
        let c = scope.spawn(move || {
            b.wait();
            reserve(&mut s2, &two, &w2, request('b', 6, WorkKind::Recovery), 0)
        });
        [a.join().unwrap(), c.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(BudgetError::Capacity("project"))))
            .count(),
        1
    );
    let total: i64 = f
        .store
        .read_connection()
        .query_row("SELECT count(*) FROM local_budget_reservations", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(total, 1);
}
#[test]
fn reopen_preserves_pending_consumption_and_retry_lineage() {
    let (mut f, owner, w) = setup();
    let r = reserve(
        &mut f.store,
        &owner,
        &w,
        request('a', 3, WorkKind::Composition),
        0,
    )
    .unwrap();
    let attempt = begin_attempt(&mut f.store, &owner, &r.id, 0).unwrap();
    let mut restored = reopen(&f);
    let r = load(&restored, &r.id).unwrap();
    assert_eq!(r.state, State::CompletionPending);
    assert_eq!(r.attempts, 1);
    assert_eq!(charged(&restored, &owner).unwrap(), units(3));
    assert!(matches!(
        begin_attempt(&mut restored, &owner, &r.id, 1),
        Err(BudgetError::Pending)
    ));
    let r = settle(&mut restored, &owner, &attempt, units(1), &receipt()).unwrap();
    let r = close(&mut restored, &owner, &r.id, r.version).unwrap();
    assert_eq!(load(&f.store, &r.id).unwrap().spent, units(1));
    assert_eq!(charged(&f.store, &owner).unwrap(), units(1));
}
#[test]
fn grandchild_siblings_share_each_ancestor_retry_allowance() {
    let (mut f, owner, w) = setup();
    let mut req = request('a', 8, WorkKind::Composition);
    req.max_attempts = 8;
    let root = reserve(&mut f.store, &owner, &w, req, 0).unwrap();
    let child = delegate(
        &mut f.store,
        &owner,
        &root.id,
        request('b', 4, WorkKind::Composition),
        0,
    )
    .unwrap();
    let x = delegate(
        &mut f.store,
        &owner,
        &child.id,
        request('c', 2, WorkKind::Composition),
        0,
    )
    .unwrap();
    let y = delegate(
        &mut f.store,
        &owner,
        &child.id,
        request('d', 2, WorkKind::Composition),
        0,
    )
    .unwrap();
    for _ in 0..2 {
        let a = begin_attempt(&mut f.store, &owner, &x.id, 0).unwrap();
        settle(&mut f.store, &owner, &a, units(1), &receipt()).unwrap();
    }
    let x = load(&f.store, &x.id).unwrap();
    close(&mut f.store, &owner, &x.id, x.version).unwrap();
    assert!(matches!(
        begin_attempt(&mut f.store, &owner, &y.id, 0),
        Err(BudgetError::AttemptsExhausted)
    ));
}
#[test]
fn overrun_observation_cannot_be_hidden_by_a_smaller_receipt_or_new_objective() {
    let (mut f, owner, w) = setup();
    let r = reserve(
        &mut f.store,
        &owner,
        &w,
        request('a', 2, WorkKind::Composition),
        0,
    )
    .unwrap();
    let observed_receipt = receipt();
    let a = begin_attempt(&mut f.store, &owner, &r.id, 0).unwrap();
    assert!(matches!(
        settle(&mut f.store, &owner, &a, units(3), &observed_receipt),
        Err(BudgetError::UsageExceeded)
    ));
    let mut restored = reopen(&f);
    let r = load(&restored, &r.id).unwrap();
    assert_eq!(r.observed_overrun, Some(units(3)));
    assert_eq!(r.receipt, Some(observed_receipt));
    assert!(matches!(
        reconcile(
            &mut restored,
            &owner,
            &r.id,
            r.version,
            units(1),
            &receipt()
        ),
        Err(BudgetError::UsageExceeded)
    ));
    let (other, w2) = another_target(&mut f);
    assert!(matches!(
        reserve(
            &mut f.store,
            &other,
            &w2,
            request('b', 1, WorkKind::Recovery),
            0
        ),
        Err(BudgetError::Capacity("unreconciled overrun"))
    ));
}
#[test]
fn correctly_rehashed_delegation_cannot_change_pinned_authority() {
    let (mut f, owner, w) = setup();
    let root = reserve(
        &mut f.store,
        &owner,
        &w,
        request('a', 3, WorkKind::OptionalAnalysis),
        0,
    )
    .unwrap();
    let child = delegate(
        &mut f.store,
        &owner,
        &root.id,
        request('b', 1, WorkKind::OptionalAnalysis),
        0,
    )
    .unwrap();
    let mut spec = child.spec;
    spec.request.kind = WorkKind::Recovery;
    let forged = record(&spec).unwrap();
    f.store
        .connection()
        .execute(
            "UPDATE local_budget_reservations SET reservation=?2,body=?3 WHERE reservation=?1",
            params![
                child.id.as_str(),
                forged.id().as_str(),
                forged.canonical_bytes()
            ],
        )
        .unwrap();
    assert!(matches!(
        load(&f.store, &forged.id()),
        Err(BudgetError::Invalid(_))
    ));
}
#[test]
fn zero_spend_delegation_recycling_has_a_finite_record_limit() {
    let (mut f, owner, w) = setup();
    let root = reserve(
        &mut f.store,
        &owner,
        &w,
        request('a', 3, WorkKind::Composition),
        0,
    )
    .unwrap();
    // Saturate the model count directly; its rows need not grant dispatch.
    let tx = f.store.connection().transaction().unwrap();
    for i in 1..MAX_LINEAGE_RECORDS {
        let mut spec = root.spec.clone();
        spec.parent = Some(root.id.to_string());
        spec.request.nonce = format!("sha256:{i:064x}");
        spec.request.units = units(1);
        let r = record(&spec).unwrap();
        insert(&tx, &r, &spec, &root.id).unwrap();
        tx.execute(
            "UPDATE local_budget_reservations SET phase='released_unused' WHERE reservation=?1",
            [r.id().as_str()],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    assert!(matches!(
        delegate(
            &mut f.store,
            &owner,
            &root.id,
            request('f', 1, WorkKind::Composition),
            0
        ),
        Err(BudgetError::Capacity("lineage records"))
    ));
}

#[test]
fn receipt_ids_cannot_be_reused_across_attempts_or_allocations() {
    let (mut f, owner, w) = setup();
    let r = reserve(
        &mut f.store,
        &owner,
        &w,
        request('a', 3, WorkKind::Composition),
        0,
    )
    .unwrap();
    let receipt = receipt();
    let one = begin_attempt(&mut f.store, &owner, &r.id, 0).unwrap();
    settle(&mut f.store, &owner, &one, units(1), &receipt).unwrap();
    let two = begin_attempt(&mut f.store, &owner, &r.id, 1).unwrap();
    assert!(matches!(
        settle(&mut f.store, &owner, &two, units(1), &receipt),
        Err(BudgetError::ReceiptReplay)
    ));
    assert_eq!(load(&f.store, &r.id).unwrap().spent, units(1));
    assert_eq!(
        load(&f.store, &r.id).unwrap().state,
        State::CompletionPending
    );
}
#[test]
fn exact_root_and_delegation_replays_return_history_after_stop_and_expiry() {
    let (mut f, owner, w) = setup();
    let req = request('a', 3, WorkKind::Composition);
    let root = reserve(&mut f.store, &owner, &w, req.clone(), 0).unwrap();
    let creq = request('b', 1, WorkKind::Composition);
    let child = delegate(&mut f.store, &owner, &root.id, creq.clone(), 0).unwrap();
    let child = close(&mut f.store, &owner, &child.id, child.version).unwrap();
    let root = load(&f.store, &root.id).unwrap();
    let root = close(&mut f.store, &owner, &root.id, root.version).unwrap();
    wave::stop(&mut f.store, &owner, &w).unwrap();
    assert_eq!(reserve(&mut f.store, &owner, &w, req, 2000).unwrap(), root);
    assert_eq!(
        delegate(&mut f.store, &owner, &root.id, creq, 2000).unwrap(),
        child
    );
    assert!(matches!(
        begin_attempt(&mut f.store, &owner, &root.id, 2000),
        Err(BudgetError::Stale)
    ));
}

#[test]
fn objective_caps_and_each_unit_dimension_are_enforced() {
    let (mut f, owner, w) = setup_with_cap(4);
    let mut req = request('a', 1, WorkKind::Recovery);
    req.units.spend_microunits = 5;
    assert!(matches!(
        reserve(&mut f.store, &owner, &w, req, 0),
        Err(BudgetError::Capacity("objective"))
    ));
    let r = reserve(
        &mut f.store,
        &owner,
        &w,
        request('b', 4, WorkKind::Recovery),
        0,
    )
    .unwrap();
    assert_eq!(r.allocated, units(4));
    assert!(matches!(
        reserve(
            &mut f.store,
            &owner,
            &w,
            request('c', 1, WorkKind::Recovery),
            0
        ),
        Err(BudgetError::Capacity("objective"))
    ));
}

#[test]
fn repeated_overrun_reconciliation_cannot_mint_unbounded_receipt_rows() {
    let (mut f, owner, w) = setup();
    let r = reserve(
        &mut f.store,
        &owner,
        &w,
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
    for _ in 0..3 {
        assert!(matches!(
            reconcile(&mut f.store, &owner, &r.id, r.version, units(3), &receipt()),
            Err(BudgetError::UsageExceeded)
        ));
        assert_eq!(load(&f.store, &r.id).unwrap(), r);
    }
    let count: i64 = f
        .store
        .read_connection()
        .query_row("SELECT count(*) FROM local_budget_receipts", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn an_observed_overrun_blocks_new_dispatch_and_delegation_of_existing_grants() {
    let (mut f, owner, w) = setup();
    let bad = reserve(
        &mut f.store,
        &owner,
        &w,
        request('a', 2, WorkKind::Composition),
        0,
    )
    .unwrap();
    let pending = reserve(
        &mut f.store,
        &owner,
        &w,
        request('b', 2, WorkKind::Composition),
        0,
    )
    .unwrap();
    let a = begin_attempt(&mut f.store, &owner, &bad.id, 0).unwrap();
    assert!(matches!(
        settle(&mut f.store, &owner, &a, units(3), &receipt()),
        Err(BudgetError::UsageExceeded)
    ));
    assert!(matches!(
        begin_attempt(&mut f.store, &owner, &pending.id, 1),
        Err(BudgetError::Capacity("unreconciled overrun"))
    ));
    assert!(matches!(
        delegate(
            &mut f.store,
            &owner,
            &pending.id,
            request('c', 1, WorkKind::Composition),
            1
        ),
        Err(BudgetError::Capacity("unreconciled overrun"))
    ));
    assert_eq!(load(&f.store, &pending.id).unwrap().state, State::Reserved);
}
#[test]
fn an_inactive_unknown_reservation_still_pins_its_original_revision() {
    let (mut f, owner, w) = setup();
    let r = reserve(
        &mut f.store,
        &owner,
        &w,
        request('a', 2, WorkKind::Composition),
        0,
    )
    .unwrap();
    let a = begin_attempt(&mut f.store, &owner, &r.id, 0).unwrap();
    let r = load(&f.store, a.reservation()).unwrap();
    mark_unknown(&mut f.store, &owner, &r.id, r.version, "unknown worker").unwrap();
    wave::stop(&mut f.store, &owner, &w).unwrap();
    // Fault injection models a rewound or older caller's changed active pointer.
    // The reservation must retain this revision independently of that pointer.
    f.store
        .connection()
        .execute(
            "UPDATE local_targets SET active_revision=NULL WHERE target='target'",
            [],
        )
        .unwrap();
    assert!(matches!(
        target::release_revision(&mut f.store, &owner, &id(&r.spec.revision).unwrap()),
        Err(TargetError::Frozen)
    ));
}
