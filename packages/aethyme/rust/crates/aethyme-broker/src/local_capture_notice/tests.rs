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
    ingest(
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
            let result = admit_for_owner(&mut store, &owner, wave_policy(), 0);
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
