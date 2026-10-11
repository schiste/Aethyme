use super::*;
use crate::execution_snapshot::{
    self as snapshot, CommandSpec, ExecutionSpec, LinuxProfile, ResourceLimits,
};
use crate::local_target::tests::Fixture;
use crate::local_wave as wave;
fn digest(bytes: &[u8]) -> String {
    format!("sha256:{}", ObjectDigest::of(bytes).hex())
}
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
fn envelope() -> Envelope {
    Envelope {
        memory: 64 * MIB,
        disk: MIB,
        pids: 8,
        jobs: 1,
        time_ms: 1000,
    }
}
fn policy(host: &Path) -> ResourcePolicy {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(host, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    ResourcePolicy {
        host_state: host.join("host-resources.db"),
        temporary_root: host.into(),
        runtime: digest(b"runtime"),
        daemon: "physical-daemon".into(),
        meter_image: digest(b"trusted meter"),
        memory_mib: 2048,
        disk_mib: 2048,
        process_slots: 1024,
        job_slots: 2,
        host_floor_bytes: 1,
        daemon_floor_bytes: 1,
    }
}
fn subject(
    runtime: String,
    image: String,
) -> (
    Fixture,
    ControllerOwner,
    ExecutionSnapshot,
    aethyme_contracts::experimental_v0::RecordId,
) {
    let mut f = Fixture::new();
    let owner = target::acquire_controller(&mut f.store, "resource-target", "appointed").unwrap();
    let mut spec = f.spec();
    spec.selected = vec!["A".into()];
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
        panic!("wave")
    };
    let w = wave::compose(&mut f.store, &owner, f._repo.path(), &w.id).unwrap();
    assert_eq!(w.phase, wave::WavePhase::Verifying);
    let held = target::status(&f.store, owner.target()).unwrap();
    assert_eq!(held.resume_phase, Some(target::Phase::Verifying));
    target::resume(&mut f.store, &owner, held.version).unwrap();
    let limits = ResourceLimits {
        timeout_ms: 1000,
        memory_bytes: 64 * MIB,
        output_bytes: MIB,
        pids: 32,
    };
    let command = CommandSpec {
        program: "/bin/sh".into(),
        arguments: vec!["-c".into(), "printf actual > /outputs/artifact".into()],
        environment: Default::default(),
    };
    let execution = snapshot::freeze(
        &mut f.store,
        &owner,
        &revision.id,
        ExecutionSpec {
            candidate_source: w.candidate_snapshot.clone().unwrap().to_string(),
            preparation: None,
            dependencies: None,
            configuration: None,
            harness: f.b.result.snapshot_id.to_string(),
            dataset: None,
            assertion_inventory: digest(b"inventory"),
            materialization_profile: snapshot::MATERIALIZATION_PROFILE.into(),
            profile: LinuxProfile {
                build_image: image.clone(),
                verifier_image: image,
                architecture: "arm64".into(),
                runtime_profile: runtime,
                build_limits: limits,
                verifier_limits: limits,
            },
            build: command.clone(),
            verify: command,
        },
    )
    .unwrap();
    let cap = Units {
        time_ms: 1_000_000_000,
        memory_bytes: 64 * 1024 * MIB as i64,
        disk_bytes: 64 * 1024 * MIB as i64,
        slots: 100,
        ..Default::default()
    };
    budget::configure_project(
        &mut f.store,
        &owner,
        budget::ProjectPolicy {
            authorization: digest(b"authorization"),
            price_profile: digest(b"pricing"),
            cap,
            recovery_headroom: Units::default(),
            mandatory_headroom: Units::default(),
            max_root_reservations: 32,
        },
    )
    .unwrap();
    budget::authorize_objective(&mut f.store, &owner, cap).unwrap();
    (f, owner, execution, w.id)
}
fn attempt(
    f: &mut Fixture,
    owner: &ControllerOwner,
    execution: &ExecutionSnapshot,
    wave: &aethyme_contracts::experimental_v0::RecordId,
    envelope: Envelope,
    nonce: &[u8],
) -> Attempt {
    let r = budget::reserve(
        &mut f.store,
        owner,
        wave,
        budget::Request {
            nonce: digest(nonce),
            recipe: execution.id().to_string(),
            kind: budget::WorkKind::Verification,
            units: envelope.units().unwrap(),
            max_attempts: 1,
            deadline_ms: now() + 3_600_000,
        },
        now(),
    )
    .unwrap();
    budget::begin_attempt(&mut f.store, owner, &r.id, now()).unwrap()
}
#[test]
fn host_capacity_is_shared_across_runtime_binaries_and_unknown_holders() {
    let host = tempfile::tempdir().unwrap();
    let p = policy(host.path());
    let f = Fixture::new();
    let mut coordinator = HostResourceCoordinator::open(&p.host_state).unwrap();
    let a = request(
        &p,
        &f.store,
        &"a".repeat(64),
        Envelope {
            jobs: 2,
            ..envelope()
        },
    )
    .unwrap();
    let grant = coordinator.acquire(&a).unwrap();
    let mut changed = p.clone();
    changed.runtime = digest(b"new binary");
    let b = request(
        &changed,
        &f.store,
        &"b".repeat(64),
        Envelope {
            jobs: 2,
            ..envelope()
        },
    )
    .unwrap();
    assert!(matches!(
        coordinator.acquire(&b),
        Err(crate::resources::HostResourceError::Conflict { .. })
    ));
    coordinator
        .quarantine(
            &grant.lease.lease_id,
            grant.lease.generation,
            &grant.ownership_token,
        )
        .unwrap();
    coordinator.reap_dead_holders().unwrap();
    assert!(coordinator.acquire(&b).is_err());
    coordinator
        .release(
            &grant.lease.lease_id,
            grant.lease.generation,
            &grant.ownership_token,
        )
        .unwrap();
    coordinator.acquire(&b).unwrap();
}
#[test]
fn disk_floor_and_insufficient_budget_refuse_before_daemon_intents() {
    let host = tempfile::tempdir().unwrap();
    let mut p = policy(host.path());
    let (mut f, owner, execution, wave) = subject(p.runtime.clone(), p.meter_image.clone());
    let a = attempt(&mut f, &owner, &execution, &wave, envelope(), b"floor");
    p.host_floor_bytes = u64::MAX - 10;
    assert!(
        Run::admit(
            &p,
            &f.store,
            &owner,
            &execution,
            &a,
            "a".repeat(64),
            "unix:///test".into(),
            envelope()
        )
        .is_err()
    );
    let root = host.path().join("local-execution-journal");
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    p.host_floor_bytes = 1;
    assert!(
        Run::admit(
            &p,
            &f.store,
            &owner,
            &execution,
            &a,
            "a".repeat(64),
            "unix:///test".into(),
            Envelope {
                memory: 128 * MIB,
                ..envelope()
            }
        )
        .is_err()
    );
    assert_eq!(std::fs::read_dir(root).unwrap().count(), 0);
}
#[test]
fn live_reader_fences_cleanup_and_attempt_cannot_be_dispatched_twice() {
    let host = tempfile::tempdir().unwrap();
    let p = policy(host.path());
    let (mut f, owner, execution, wave) = subject(p.runtime.clone(), p.meter_image.clone());
    let a = attempt(
        &mut f,
        &owner,
        &execution,
        &wave,
        envelope(),
        b"single-dispatch",
    );
    let run = Run::admit(
        &p,
        &f.store,
        &owner,
        &execution,
        &a,
        "a".repeat(64),
        "unix:///test".into(),
        envelope(),
    )
    .unwrap();
    assert!(Run::recover(&p, &run.journal.run).is_err());
    assert!(
        Run::admit(
            &p,
            &f.store,
            &owner,
            &execution,
            &a,
            "b".repeat(64),
            "unix:///test".into(),
            envelope()
        )
        .is_err()
    );
    drop(run);
    let mut recovery = Run::recover(&p, &"a".repeat(64)).unwrap();
    recovery.cleaned().unwrap();
    recovery.release_clean().unwrap();
    recovery.release_clean().unwrap();
    assert!(
        Run::admit(
            &p,
            &f.store,
            &owner,
            &execution,
            &a,
            "c".repeat(64),
            "unix:///test".into(),
            envelope()
        )
        .is_err()
    );
}
#[test]
fn unknown_creation_remains_held_and_retained_sources_are_untouched() {
    let host = tempfile::tempdir().unwrap();
    let p = policy(host.path());
    let (mut f, owner, execution, wave) = subject(p.runtime.clone(), p.meter_image.clone());
    let a = attempt(&mut f, &owner, &execution, &wave, envelope(), b"unknown");
    let before = crate::collaboration_archive::read_object(
        &f.store,
        &ObjectDigest::of_snapshot(&f.a.result.snapshot_id),
    )
    .unwrap();
    let id = "a".repeat(64);
    let mut run = Run::admit(
        &p,
        &f.store,
        &owner,
        &execution,
        &a,
        id.clone(),
        "unix:///test".into(),
        envelope(),
    )
    .unwrap();
    run.intent(
        ObjectKind::Volume,
        format!("aethyme-{id}-source"),
        id.clone(),
    )
    .unwrap();
    assert!(run.cleaned().is_err());
    drop(run);
    let mut recovery = Run::recover(&p, &id).unwrap();
    assert!(recovery.cleaned().is_err());
    assert!(recovery.release_clean().is_err());
    assert_eq!(
        before,
        crate::collaboration_archive::read_object(
            &f.store,
            &ObjectDigest::of_snapshot(&f.a.result.snapshot_id)
        )
        .unwrap()
    );
}
#[test]
fn exact_cleanup_receipt_settles_once_and_does_not_replenish_budget() {
    let host = tempfile::tempdir().unwrap();
    let p = policy(host.path());
    let (mut f, owner, execution, wave) = subject(p.runtime.clone(), p.meter_image.clone());
    let a = attempt(&mut f, &owner, &execution, &wave, envelope(), b"settle");
    let id = "a".repeat(64);
    let mut run = Run::admit(
        &p,
        &f.store,
        &owner,
        &execution,
        &a,
        id.clone(),
        "unix:///test".into(),
        envelope(),
    )
    .unwrap();
    let name = format!("aethyme-{id}-source");
    run.intent(ObjectKind::Volume, name.clone(), id).unwrap();
    run.created(&name, name.clone()).unwrap();
    assert!(run.cleaned().is_err());
    run.removed(ObjectKind::Volume, &name).unwrap();
    run.cleaned().unwrap();
    run.finish(&mut f.store, &owner, &a, "infrastructure_deferred")
        .unwrap();
    let charged = budget::load(&f.store, a.reservation()).unwrap();
    assert_eq!(charged.spent, envelope().units().unwrap());
    assert!(run.finish(&mut f.store, &owner, &a, "repeat").is_err());
    assert!(budget::begin_attempt(&mut f.store, &owner, a.reservation(), now()).is_err());
}
#[test]
fn cache_keys_are_exact_and_every_result_reuse_is_disabled() {
    let subject = CacheSubject {
        execution: "exec",
        artifact: "artifact",
        producer: "native",
        producer_generation: 1,
        policy: "policy",
        harness: "harness",
        collection: "collector",
        authority_epoch: "epoch",
        target_revision: "revision",
        result_class: "passed",
    };
    let key = subject.key().unwrap();
    assert_eq!(
        key,
        "sha256:2de980c9c36a48464a6a940e1186949f191264ec27e7e2a967e884f37083f9c2"
    );
    assert!(matches!(subject.reuse(), Err(ResourceError::CacheDisabled)));
    let original = serde_json::to_value(&subject).unwrap();
    assert_eq!(
        ObjectDigest::of(&serde_json::to_vec(&original).unwrap()).hex(),
        key.trim_start_matches("sha256:")
    );
    for field in original.as_object().unwrap().keys() {
        let mut changed = original.clone();
        changed[field] = if field == "producer_generation" {
            serde_json::json!(2)
        } else {
            serde_json::json!("different")
        };
        assert_ne!(
            ObjectDigest::of(&serde_json::to_vec(&changed).unwrap()).hex(),
            key.trim_start_matches("sha256:")
        );
    }
}
