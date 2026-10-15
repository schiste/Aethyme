use super::*;
use crate::collaboration_capture::{CaptureOutcome, CapturePolicy, CaptureRequest};
use crate::collaboration_state::{CollaborationRoot, ProjectKey};
use crate::local_target::tests::Fixture;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

fn open_store(host: &Path) -> CollaborationStore {
    CollaborationStore::open(
        &CollaborationRoot::under_host_state(host),
        &ProjectKey::parse("local-target-test").unwrap(),
        &[],
    )
    .unwrap()
}

fn capture(f: &mut Fixture, contribution: &archive::RetainedContribution) -> OperationId {
    let operation = OperationId::mint().unwrap();
    let outcome = capture::capture(
        &mut f.store,
        &CaptureRequest {
            operation_id: operation.clone(),
            repository: f._repo.path().into(),
            base: contribution.base.commit.clone(),
            result: contribution.result.commit.clone(),
            policy: CapturePolicy::Required,
            retention: RetentionBoundary::UntilReleased,
        },
    )
    .unwrap();
    assert!(matches!(outcome, CaptureOutcome::Acknowledged(_)));
    operation
}

fn wave_policy() -> crate::local_wave::WavePolicy {
    crate::local_wave::WavePolicy {
        debounce_ms: 0,
        age_promotion_ms: 0,
        ..Default::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AdmissionEffects {
    notices: i64,
    outcomes: i64,
    arrivals: i64,
    waves: i64,
    budget_projects: i64,
    budget_objectives: i64,
    budget_reservations: i64,
    budget_receipts: i64,
}

fn admission_effects(store: &CollaborationStore) -> AdmissionEffects {
    let connection = store.read_connection();
    let count = |table: &str| {
        connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    };
    AdmissionEffects {
        notices: count("local_capture_notices"),
        outcomes: count("local_capture_notice_outcomes"),
        arrivals: count("local_wave_arrivals"),
        waves: count("local_waves"),
        budget_projects: count("local_budget_project"),
        budget_objectives: count("local_budget_objectives"),
        budget_reservations: count("local_budget_reservations"),
        budget_receipts: count("local_budget_receipts"),
    }
}

#[test]
fn notice_ingress_refuses_unbound_issuer_revision_and_capture_without_admission_effects() {
    let mut f = Fixture::new();
    let owner = target::acquire_controller(&mut f.store, "target", "appointed").unwrap();
    let spec = f.spec();
    let revision = target::propose_revision(&mut f.store, &owner, spec).unwrap();
    target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
    let retained_a = f.a.clone();
    let retained_b = f.b.clone();
    let operation_a = capture(&mut f, &retained_a);
    let operation_b = capture(&mut f, &retained_b);

    let before = admission_effects(&f.store);
    let wrong_target = ingest(
        &mut f.store,
        "other-target",
        owner.issuer(),
        &revision.id,
        "A",
        &operation_a,
    );
    assert!(matches!(wrong_target, Err(NoticeError::Stale)));
    assert_eq!(admission_effects(&f.store), before);

    let wrong_issuer = ingest(
        &mut f.store,
        owner.target(),
        "different-issuer",
        &revision.id,
        "A",
        &operation_a,
    );
    assert!(matches!(wrong_issuer, Err(NoticeError::Stale)));
    assert_eq!(admission_effects(&f.store), before);

    // B is retained and present in the revision catalog, but only A is
    // selected. A retained object alone cannot make an unselected alias
    // available to the target.
    let unselected_alias = ingest(
        &mut f.store,
        owner.target(),
        owner.issuer(),
        &revision.id,
        "B",
        &operation_b,
    );
    assert!(matches!(unselected_alias, Err(NoticeError::Stale)));
    assert_eq!(admission_effects(&f.store), before);

    // A valid capture for B cannot be relabeled as the selected A input.
    let mismatched_capture = ingest(
        &mut f.store,
        owner.target(),
        owner.issuer(),
        &revision.id,
        "A",
        &operation_b,
    );
    assert!(matches!(mismatched_capture, Err(NoticeError::Stale)));
    assert_eq!(admission_effects(&f.store), before);

    let mut next_spec = f.spec();
    next_spec.objective = "reject notices for a superseded revision".into();
    let next_revision = target::propose_revision(&mut f.store, &owner, next_spec).unwrap();
    target::select_revision(&mut f.store, &owner, &next_revision.id).unwrap();
    let before_stale_revision = admission_effects(&f.store);
    let stale_revision = ingest(
        &mut f.store,
        owner.target(),
        owner.issuer(),
        &revision.id,
        "A",
        &operation_a,
    );
    assert!(matches!(stale_revision, Err(NoticeError::Stale)));
    assert_eq!(admission_effects(&f.store), before_stale_revision);
    assert_eq!(before_stale_revision.notices, 0);
    assert_eq!(before_stale_revision.outcomes, 0);
    assert_eq!(before_stale_revision.arrivals, 0);
    assert_eq!(before_stale_revision.waves, 0);
    assert_eq!(before_stale_revision.budget_reservations, 0);
}

#[test]
fn duplicate_ingress_and_owner_ticks_are_idempotent_for_one_arrival() {
    let mut f = Fixture::new();
    let owner = target::acquire_controller(&mut f.store, "target", "appointed").unwrap();
    let spec = f.spec();
    let revision = target::propose_revision(&mut f.store, &owner, spec).unwrap();
    target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
    let retained_a = f.a.clone();
    let operation = capture(&mut f, &retained_a);

    let first = ingest(
        &mut f.store,
        owner.target(),
        owner.issuer(),
        &revision.id,
        "A",
        &operation,
    )
    .unwrap();
    let duplicate = ingest(
        &mut f.store,
        owner.target(),
        owner.issuer(),
        &revision.id,
        "A",
        &operation,
    )
    .unwrap();
    assert_eq!(duplicate, first);
    assert_eq!(first.status, "pending");
    assert_eq!(admission_effects(&f.store).notices, 1);
    assert_eq!(admission_effects(&f.store).outcomes, 1);

    let _ = admit_for_owner(&mut f.store, &owner, wave_policy(), 0).unwrap();
    let after_first_tick = admission_effects(&f.store);
    assert_eq!(after_first_tick.notices, 1);
    assert_eq!(after_first_tick.outcomes, 1);
    assert_eq!(after_first_tick.arrivals, 1);
    let _ = admit_for_owner(&mut f.store, &owner, wave_policy(), 1).unwrap();
    assert_eq!(admission_effects(&f.store), after_first_tick);
}

#[test]
fn pending_notice_limit_accepts_512_and_refuses_513_without_effects() {
    let mut f = Fixture::new();
    let owner = target::acquire_controller(&mut f.store, "target", "appointed").unwrap();
    let spec = f.spec();
    let revision = target::propose_revision(&mut f.store, &owner, spec).unwrap();
    target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
    let retained_a = f.a.clone();
    let operation = capture(&mut f, &retained_a);
    let duplicate_operation = capture(&mut f, &retained_a);

    // Seed canonical, schema-valid pending rows to isolate the project-wide
    // capacity check without making 512 unrelated capture operations. These
    // deliberately never reach an owner tick; only the pending counter is
    // under test here.
    let capture_receipt = capture::receipt(&mut f.store, &operation).unwrap().unwrap();
    let retained = archive::retained_contribution(&f.store, &capture_receipt.contribution)
        .unwrap()
        .unwrap();
    let epoch = crate::local_authority::status(&f.store)
        .unwrap()
        .epoch
        .as_str()
        .to_owned();
    let tx = f
        .store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    for index in 0..MAX_PENDING_NOTICES - 1 {
        let body = NoticeBody {
            target: owner.target().into(),
            issuer: owner.issuer().into(),
            revision: revision.id.as_str().into(),
            alias: format!("seed-{index:03}"),
            operation: operation.as_str().into(),
            lineage: capture_receipt.contribution.as_str().into(),
            receipt: capture_receipt.receipt_record_id.as_str().into(),
            authority_epoch: epoch.clone(),
            base_snapshot: capture_receipt.base.as_str().into(),
            base_commit: retained.base.commit.as_str().into(),
            result_snapshot: capture_receipt.result.as_str().into(),
            result_commit: retained.result.commit.as_str().into(),
            received_ms: index,
        };
        let body_record = record(&body).unwrap();
        let notice_id = body_record.id().as_str().to_owned();
        tx.execute(
            "INSERT INTO local_capture_notices
             (notice_id,target,revision,operation_id,alias,lineage_record_id,receipt_record_id,
              authority_epoch,base_snapshot,base_commit,result_snapshot,result_commit,received_ms,body)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            params![
                notice_id,
                body.target,
                body.revision,
                body.operation,
                body.alias,
                body.lineage,
                body.receipt,
                body.authority_epoch,
                body.base_snapshot,
                body.base_commit,
                body.result_snapshot,
                body.result_commit,
                body.received_ms,
                body_record.canonical_bytes(),
            ],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO local_capture_notice_outcomes(notice_id,status,detail,changed_ms)
             VALUES (?1,'pending',NULL,?2)",
            params![notice_id, body.received_ms],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    assert_eq!(admission_effects(&f.store).notices, MAX_PENDING_NOTICES - 1);

    let receipt = ingest(
        &mut f.store,
        owner.target(),
        owner.issuer(),
        &revision.id,
        "A",
        &operation,
    )
    .unwrap();
    assert_eq!(receipt.status, "pending");
    let at_limit = admission_effects(&f.store);
    assert_eq!(at_limit.notices, MAX_PENDING_NOTICES);
    assert_eq!(at_limit.outcomes, MAX_PENDING_NOTICES);

    let over_limit = ingest(
        &mut f.store,
        owner.target(),
        owner.issuer(),
        &revision.id,
        "A",
        &duplicate_operation,
    );
    assert!(matches!(over_limit, Err(NoticeError::Full)));
    assert_eq!(admission_effects(&f.store), at_limit);
}

#[test]
fn owner_tick_marks_notice_from_old_epoch_stale_without_admission_effects() {
    let mut f = Fixture::new();
    let owner = target::acquire_controller(&mut f.store, "target", "appointed").unwrap();
    let spec = f.spec();
    let revision = target::propose_revision(&mut f.store, &owner, spec).unwrap();
    target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
    let retained_a = f.a.clone();
    let operation = capture(&mut f, &retained_a);
    let receipt = ingest(
        &mut f.store,
        owner.target(),
        owner.issuer(),
        &revision.id,
        "A",
        &operation,
    )
    .unwrap();
    assert_eq!(receipt.status, "pending");
    let old_epoch = crate::local_authority::status(&f.store).unwrap().epoch;
    drop(owner);

    let frozen = crate::local_authority::freeze(
        &mut f.store,
        &old_epoch,
        "exercise stale retained capture notice after epoch rotation",
    )
    .unwrap();
    let resumed = crate::local_authority::resume(&mut f.store, &frozen.epoch).unwrap();
    assert_ne!(resumed.epoch, old_epoch);

    let next_owner = target::acquire_controller(&mut f.store, "target", "appointed").unwrap();
    let admission = admit_for_owner(
        &mut f.store,
        &next_owner,
        wave_policy(),
        receipt.received_ms,
    );
    assert!(matches!(
        admission,
        Err(NoticeError::Wave(crate::local_wave::WaveError::Target(
            target::TargetError::Frozen
        )))
    ));

    let notices = list_for_target(&f.store, "target", 16).unwrap();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].id, receipt.id.as_str());
    assert_eq!(notices[0].status, "stale");
    assert_eq!(
        notices[0].detail.as_deref(),
        Some("authority epoch or selected target revision changed")
    );
    let after = admission_effects(&f.store);
    assert_eq!(after.notices, 1);
    assert_eq!(after.outcomes, 1);
    assert_eq!(after.arrivals, 0);
    assert_eq!(after.waves, 0);
    assert_eq!(after.budget_projects, 0);
    assert_eq!(after.budget_objectives, 0);
    assert_eq!(after.budget_reservations, 0);
    assert_eq!(after.budget_receipts, 0);
}

#[test]
fn ingress_cannot_interleave_between_notice_drain_and_wave_admission() {
    let mut f = Fixture::new();
    let owner = target::acquire_controller(&mut f.store, "target", "appointed").unwrap();
    let mut spec = f.spec();
    spec.selected.push("B".into());
    let revision = target::propose_revision(&mut f.store, &owner, spec).unwrap();
    target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
    let retained_a = f.a.clone();
    let retained_b = f.b.clone();
    let operation_a = capture(&mut f, &retained_a);
    let operation_b = capture(&mut f, &retained_b);
    let receipt_a = ingest(
        &mut f.store,
        owner.target(),
        owner.issuer(),
        &revision.id,
        "A",
        &operation_a,
    )
    .unwrap();

    let host = f._host.path().to_path_buf();
    let revision_id = revision.id.clone();
    let operation_b_for_ingress = operation_b.clone();
    let owner_target = owner.target().to_owned();
    let owner_issuer = owner.issuer().to_owned();
    let (drained_tx, drained_rx) = mpsc::sync_channel(1);
    let (resume_tx, resume_rx) = mpsc::sync_channel(1);
    let (admission_tx, admission_rx) = mpsc::sync_channel(1);
    std::thread::scope(|scope| {
        let tick = scope.spawn(|| {
            let _probe = observe_owner_tick(drained_tx, resume_rx);
            let mut store = open_store(&host);
            let result = admit_for_owner(&mut store, &owner, wave_policy(), receipt_a.received_ms);
            admission_tx.send(result).unwrap();
        });

        drained_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("owner tick drained its initial notice");

        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (ingested_tx, ingested_rx) = mpsc::sync_channel(1);
        let ingress_host = host.clone();
        let ingress = scope.spawn(move || {
            let mut store = open_store(&ingress_host);
            started_tx.send(()).unwrap();
            let result = ingest(
                &mut store,
                &owner_target,
                &owner_issuer,
                &revision_id,
                "B",
                &operation_b_for_ingress,
            );
            ingested_tx.send(result).unwrap();
        });
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("concurrent ingress reached the operation");

        let lock_path = f
            .store
            .project_dir()
            .join("spool/capture-notice-ingress.lock");
        assert!(
            ExclusiveFileLock::try_acquire(open_lock_file(&lock_path).unwrap())
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            ingested_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        resume_tx.send(()).unwrap();
        let admission = admission_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("owner tick completed admission")
            .unwrap();
        assert!(matches!(admission, crate::local_wave::Admission::Frozen(_)));
        ingested_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("ingress continued after admission")
            .unwrap();
        tick.join().unwrap();
        ingress.join().unwrap();
    });

    let status = list_for_target(&f.store, "target", 16).unwrap();
    assert_eq!(
        status
            .iter()
            .find(|notice| notice.operation == operation_a.as_str())
            .unwrap()
            .status,
        "available"
    );
    assert_eq!(
        status
            .iter()
            .find(|notice| notice.operation == operation_b.as_str())
            .unwrap()
            .status,
        "pending"
    );

    // The second owner tick consumes the notice queued after the first
    // admission. It remains durable even though the first wave is frozen.
    let _ = admit_for_owner(&mut f.store, &owner, wave_policy(), 1).unwrap();
    let status = list_for_target(&f.store, "target", 16).unwrap();
    assert_eq!(
        status
            .iter()
            .find(|notice| notice.operation == operation_b.as_str())
            .unwrap()
            .status,
        "available"
    );
}
