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
pub(super) fn policy(host: &Path) -> ResourcePolicy {
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
pub(super) fn subject(
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
pub(super) fn attempt(
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
    p.host_floor_bytes = u64::MAX / 2;
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
    run.created(ObjectKind::Volume, &name, name.clone())
        .unwrap();
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
#[test]
fn committed_host_acquisition_is_recovered_without_reallocation() {
    for state in ["active", "quarantined", "expired", "released"] {
        let host = tempfile::tempdir().unwrap();
        let p = policy(host.path());
        let (mut f, owner, execution, wave) = subject(p.runtime.clone(), p.meter_image.clone());
        let a = attempt(
            &mut f,
            &owner,
            &execution,
            &wave,
            envelope(),
            b"crash-window",
        );
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
        let grant = run.journal.grant.clone().unwrap();
        if state == "released" {
            run.coordinator
                .release(
                    &grant.lease.lease_id,
                    grant.lease.generation,
                    &grant.ownership_token,
                )
                .unwrap();
        }
        if state == "quarantined" {
            run.coordinator
                .quarantine(
                    &grant.lease.lease_id,
                    grant.lease.generation,
                    &grant.ownership_token,
                )
                .unwrap();
        }
        if state == "expired" {
            rusqlite::Connection::open(&p.host_state)
                .unwrap()
                .execute("UPDATE resource_leases SET expires_at=0", [])
                .unwrap();
        }
        // Model a process dying after acquire committed but before its next
        // durable journal save. Drop sees the pre-save journal too.
        run.journal.grant = None;
        save(&run.root, &run.journal).unwrap();
        drop(run);
        let mut recovery = Run::recover(&p, &id).unwrap();
        let recovered = recovery.journal.grant.as_ref().unwrap();
        assert_eq!(recovered.lease.lease_id, grant.lease.lease_id);
        assert_eq!(recovered.lease.generation, grant.lease.generation);
        assert_eq!(recovered.ownership_token, grant.ownership_token);
        recovery.cleaned().unwrap();
        recovery.release_clean().unwrap();
        recovery.release_clean().unwrap();
        let leases = recovery.coordinator.list(true).unwrap();
        assert_eq!(leases.len(), 1);
        assert_eq!(leases[0].state, crate::resources::HostLeaseState::Released);
        // An intent that crashed before coordinator acquisition must never
        // acquire on cleanup, or leave an invented lease to reconcile.
        let mut missing: Journal =
            serde_json::from_value(serde_json::to_value(&recovery.journal).unwrap()).unwrap();
        missing.run = "b".repeat(64);
        missing.request.run_id = missing.run.clone();
        missing.request.request_id = format!("local-exec:{}", missing.run);
        missing.grant = None;
        missing.cleaned = false;
        save(&recovery.root, &missing).unwrap();
        let mut before_acquire = Run::recover(&p, &missing.run).unwrap();
        assert!(before_acquire.journal.grant.is_none());
        before_acquire.cleaned().unwrap();
        before_acquire.release_clean().unwrap();
        assert_eq!(before_acquire.coordinator.list(true).unwrap().len(), 1);
    }
}
#[test]
fn pending_execution_is_blocked_by_another_reservations_overrun() {
    let host = tempfile::tempdir().unwrap();
    let p = policy(host.path());
    let (mut f, owner, execution, wave) = subject(p.runtime.clone(), p.meter_image.clone());
    let pending = attempt(
        &mut f,
        &owner,
        &execution,
        &wave,
        envelope(),
        b"pending-before-overrun",
    );
    let other = attempt(
        &mut f,
        &owner,
        &execution,
        &wave,
        envelope(),
        b"other-overrun",
    );
    let mut too_much = envelope().units().unwrap();
    too_much.time_ms += 1;
    let receipt =
        aethyme_contracts::experimental_v0::RecordId::parse(&digest(b"overrun receipt")).unwrap();
    assert!(matches!(
        budget::settle(&mut f.store, &owner, &other, too_much, &receipt),
        Err(budget::BudgetError::UsageExceeded)
    ));
    assert!(matches!(
        budget::authorize_execution(
            &f.store,
            &owner,
            &pending,
            &execution,
            envelope().units().unwrap()
        ),
        Err(budget::BudgetError::Capacity("unreconciled overrun"))
    ));
    assert!(matches!(
        budget::execution_guard(
            &f.store,
            &owner,
            &pending,
            &execution,
            envelope().units().unwrap()
        ),
        Err(budget::BudgetError::Capacity("unreconciled overrun"))
    ));
    assert!(
        Run::admit(
            &p,
            &f.store,
            &owner,
            &execution,
            &pending,
            "a".repeat(64),
            "unix:///test".into(),
            envelope()
        )
        .is_err()
    );
    assert_eq!(
        budget::load(&f.store, pending.reservation()).unwrap().state,
        budget::State::CompletionPending
    );
    assert_eq!(
        std::fs::read_dir(host.path().join("local-execution-journal"))
            .unwrap()
            .count(),
        0
    );
}
#[test]
#[ignore = "requires explicitly selected cached public Linux Docker image"]
fn public_admitted_build_and_verifier_wait_for_delayed_start_acknowledgement() {
    use crate::execution_supervisor::DockerExecutor;
    use crate::execution_verifier as verifier;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::AtomicBool;
    let host = tempfile::tempdir().unwrap();
    let wrapper_dir = tempfile::tempdir().unwrap();
    let cli = std::env::var("AETHYME_DOCKER_CLI").expect("trusted cached Docker client");
    let image = std::env::var("AETHYME_DOCKER_TEST_IMAGE").expect("cached public image digest");
    assert!(!cli.contains('\''));
    let wrapper = wrapper_dir.path().join("docker-delayed-start");
    let marker = wrapper_dir.path().join("delayed");
    let lose_remove = wrapper_dir.path().join("lose-remove-response");
    let pre_ack = wrapper_dir.path().join("observed-before-ack");
    std::fs::write(
        &wrapper,
        format!(
            r#"#!/bin/sh
set -eu
if test "${{3-}} ${{4-}} ${{5-}} ${{6-}}" = 'container start --attach --interactive'; then
  name=$('{}' --host "$2" container inspect --format '{{{{.Name}}}}' "$7")
  case "$name" in *-check) printf delayed >> '{}'; sleep 1;; esac
fi
if test "${{3-}} ${{4-}}" = 'container inspect' && test -e '{}'; then
  state=$('{}' --host "$2" container inspect --format '{{{{.State.StartedAt}}}}|{{{{.Name}}}}' "$5")
  case "$state" in 0001-*-check) printf unacknowledged >> '{}';; esac
fi
if test "${{3-}} ${{4-}}" = 'container rm' && test -e '{}'; then rm '{}'; '{}' "$@"; exit 1; fi
exec '{}' "$@"
"#,
            cli,
            marker.display(),
            marker.display(),
            cli,
            pre_ack.display(),
            lose_remove.display(),
            lose_remove.display(),
            cli,
            cli
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let executor = DockerExecutor::probe(wrapper.clone()).unwrap();
    let mut p = policy(host.path());
    p.runtime = executor.runtime().id();
    p.daemon = executor.runtime().daemon_id.clone();
    p.meter_image = image.clone();
    let executor = executor.with_resources(p.clone()).unwrap();
    let mut f = Fixture::new();
    let inventory = verifier::AssertionInventory::new(vec![verifier::AssertionCase {
        id: "echo".into(),
        arguments: vec![],
        stdin_hex: "68656c6c6f0a".into(),
        exit_code: 0,
        stdout: digest(b"hello\n"),
        stderr: digest(b""),
    }])
    .unwrap();
    std::fs::write(
        f._repo.path().join("inventory.json"),
        inventory.canonical_bytes(),
    )
    .unwrap();
    crate::local_target::tests::git(f._repo.path(), &["add", "inventory.json"]);
    crate::local_target::tests::git(
        f._repo.path(),
        &["commit", "-qm", "public resource fixture harness"],
    );
    let head = crate::collaboration_archive::CommitOid::parse(&crate::local_target::tests::git(
        f._repo.path(),
        &["rev-parse", "HEAD"],
    ))
    .unwrap();
    let harness =
        crate::collaboration_archive::retain_snapshot(&mut f.store, f._repo.path(), &head)
            .unwrap()
            .snapshot_id
            .to_string();
    let command = CommandSpec {
        program: "/bin/sh".into(),
        arguments: vec!["/artifact/app.sh".into()],
        environment: Default::default(),
    };
    let verifier_policy = verifier::VerifierPolicy {
        profile: verifier::PROFILE.into(),
        runtime: executor.runtime().id(),
        image: image.clone(),
        harness: harness.clone(),
        inventory: inventory.id().to_string(),
        command: command.clone(),
        max_age_ms: 60_000,
    };
    let owner =
        target::acquire_controller(&mut f.store, "admitted-resource-test", "appointed").unwrap();
    let mut ts = f.spec();
    ts.selected = vec!["A".into()];
    ts.acceptance_policy = verifier_policy.record().unwrap().id().to_string();
    let revision = target::propose_revision(&mut f.store, &owner, ts).unwrap();
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
    let held = target::status(&f.store, owner.target()).unwrap();
    target::resume(&mut f.store, &owner, held.version).unwrap();
    let permit =
        verifier::configure(&mut f.store, &owner, "native", None, verifier_policy).unwrap();
    let limits = ResourceLimits {
        timeout_ms: 10_000,
        memory_bytes: 64 * MIB,
        output_bytes: MIB,
        pids: 32,
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
            harness,
            dataset: None,
            assertion_inventory: inventory.id().to_string(),
            materialization_profile: snapshot::MATERIALIZATION_PROFILE.into(),
            profile: LinuxProfile {
                build_image: image.clone(),
                verifier_image: image,
                architecture: executor.runtime().architecture.clone(),
                runtime_profile: executor.runtime().id(),
                build_limits: limits,
                verifier_limits: limits,
            },
            build: CommandSpec {
                program: "/bin/sh".into(),
                arguments: vec![
                    "-c".into(),
                    "printf '#!/bin/sh\ncat\n' > /outputs/app.sh".into(),
                ],
                environment: Default::default(),
            },
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
            authorization: digest(b"live operator"),
            price_profile: digest(b"price"),
            cap,
            recovery_headroom: Units::default(),
            mandatory_headroom: Units::default(),
            max_root_reservations: 32,
        },
    )
    .unwrap();
    budget::authorize_objective(&mut f.store, &owner, cap).unwrap();
    let build_attempt = attempt(
        &mut f,
        &owner,
        &execution,
        &w.id,
        build_envelope(&execution).unwrap(),
        b"live build",
    );
    let built = executor
        .build(
            &mut f.store,
            &owner,
            &execution,
            &build_attempt,
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(
        built.artifact().is_some(),
        "{:?}: {:?}",
        built.outcome(),
        built.detail()
    );
    let artifact = built.into_artifact().unwrap();
    let verification_attempt = attempt(
        &mut f,
        &owner,
        &execution,
        &w.id,
        verify_envelope(&execution).unwrap(),
        b"live independent verify",
    );
    let evidence = verifier::verify(
        &mut f.store,
        &owner,
        &execution,
        &artifact,
        &permit,
        &executor,
        &verification_attempt,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(evidence.outcome(), verifier::VerificationOutcome::Passed);
    verifier::applicable(&f.store, &owner, &execution, &artifact, &permit, &evidence).unwrap();
    assert!(
        marker.exists(),
        "the controlled acknowledgement delay must execute"
    );
    assert!(
        pre_ack.exists(),
        "the native verifier must observe an unacknowledged dispatch while its ledger guard is held"
    );
    for a in [&build_attempt, &verification_attempt] {
        let reservation = budget::load(&f.store, a.reservation()).unwrap();
        assert_eq!(reservation.state, budget::State::PartlyConsumed);
        assert_eq!(reservation.spent, a.remaining());
    }
    assert!(
        HostResourceCoordinator::open(&p.host_state)
            .unwrap()
            .list(false)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        target::status(&f.store, owner.target()).unwrap().phase,
        target::Phase::Verifying
    );
    for entry in std::fs::read_dir(host.path().join("local-execution-journal")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "json") {
            let journal: Journal = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert!(journal.cleaned && journal.settled);
            assert!(journal.objects.iter().all(|o| o.removed && o.id.is_some()));
            executor.recover_resources(&journal.run).unwrap();
        }
    } // Simulate a dead client with an acknowledged stopped container and volume,
    // then lose a successful remove response. Only exact owned resources may
    // disappear; retry must inspect absence, never rerun candidate work.
    let recovery_attempt = attempt(
        &mut f,
        &owner,
        &execution,
        &w.id,
        envelope(),
        b"interrupted native cleanup",
    );
    let id: String = f
        .store
        .read_connection()
        .query_row("SELECT lower(hex(randomblob(32)))", [], |r| r.get(0))
        .unwrap();
    let mut run = Run::admit(
        &p,
        &f.store,
        &owner,
        &execution,
        &recovery_attempt,
        id.clone(),
        executor.endpoint().into(),
        envelope(),
    )
    .unwrap();
    let volume = format!("aethyme-{id}-recovery-volume");
    let name = format!("aethyme-{id}-recovery-container");
    let label = format!("aethyme.local.generation={id}");
    let docker = |args: &[&str]| {
        let output = std::process::Command::new(&cli)
            .args(["--host", executor.endpoint()])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    };
    run.intent(ObjectKind::Volume, volume.clone(), id.clone())
        .unwrap();
    let volume_id = docker(&[
        "volume", "create", "--driver", "local", "--label", &label, &volume,
    ]);
    run.created(ObjectKind::Volume, &volume, volume_id).unwrap();
    run.intent(ObjectKind::Container, name.clone(), id).unwrap();
    let container_id = docker(&[
        "container",
        "create",
        "--name",
        &name,
        "--label",
        &label,
        "--network",
        "none",
        "--mount",
        &format!("type=volume,source={volume},target=/empty,readonly"),
        &p.meter_image,
        "/bin/true",
    ]);
    run.created(ObjectKind::Container, &name, container_id.clone())
        .unwrap();
    assert!(
        executor.recover_resources(&run.journal.run).is_err(),
        "live reader fences recovery"
    );
    let recovery_id = run.journal.run.clone();
    drop(run);
    std::fs::write(&lose_remove, b"lose only this test removal response").unwrap();
    assert!(executor.recover_resources(&recovery_id).is_err());
    // Container removal happened; its acknowledgement was lost. The volume and
    // capacity remain until exact absence and subsequent cleanup are observed.
    assert!(
        !HostResourceCoordinator::open(&p.host_state)
            .unwrap()
            .list(false)
            .unwrap()
            .is_empty()
    );
    executor.recover_resources(&recovery_id).unwrap();
    executor.recover_resources(&recovery_id).unwrap();
    assert!(
        HostResourceCoordinator::open(&p.host_state)
            .unwrap()
            .list(false)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        budget::load(&f.store, recovery_attempt.reservation())
            .unwrap()
            .state,
        budget::State::CompletionPending
    );
    assert!(
        docker(&[
            "container",
            "ls",
            "--all",
            "--quiet",
            "--no-trunc",
            "--filter",
            &format!("id={container_id}")
        ])
        .is_empty()
    );
    assert!(
        docker(&[
            "volume",
            "ls",
            "--quiet",
            "--filter",
            &format!("name=^{volume}$")
        ])
        .is_empty()
    ); // Physical daemon labels are independently observed, not taken from the
    // valid journal. Wrong labels must not authorize deletion or capacity release.
    for kind in [ObjectKind::Container, ObjectKind::Volume] {
        let recovery_attempt = attempt(
            &mut f,
            &owner,
            &execution,
            &w.id,
            envelope(),
            format!("wrong-label-{kind:?}").as_bytes(),
        );
        let id: String = f
            .store
            .read_connection()
            .query_row("SELECT lower(hex(randomblob(32)))", [], |r| r.get(0))
            .unwrap();
        let mut run = Run::admit(
            &p,
            &f.store,
            &owner,
            &execution,
            &recovery_attempt,
            id.clone(),
            executor.endpoint().into(),
            envelope(),
        )
        .unwrap();
        let name = format!("aethyme-{id}-wrong-label");
        run.intent(kind, name.clone(), id.clone()).unwrap();
        let foreign_label = "aethyme.local.generation=another-actual-generation";
        let object = match kind {
            ObjectKind::Container => docker(&[
                "container",
                "create",
                "--name",
                &name,
                "--label",
                foreign_label,
                "--network",
                "none",
                &p.meter_image,
                "/bin/true",
            ]),
            ObjectKind::Volume => docker(&[
                "volume",
                "create",
                "--driver",
                "local",
                "--label",
                foreign_label,
                &name,
            ]),
        };
        run.created(kind, &name, object.clone()).unwrap();
        drop(run);
        assert!(
            executor.recover_resources(&id).is_err(),
            "actual label mismatch must refuse cleanup for {kind:?}"
        );
        assert_eq!(
            HostResourceCoordinator::open(&p.host_state)
                .unwrap()
                .list(false)
                .unwrap()
                .len(),
            1
        );
        let mut recovery = Run::recover(&p, &id).unwrap();
        assert!(!recovery.journal.objects[0].removed);
        let original_endpoint = recovery.journal.endpoint.clone();
        recovery.journal.endpoint = "unix:///another-docker-endpoint".into();
        save(&recovery.root, &recovery.journal).unwrap();
        drop(recovery);
        assert!(executor.recover_resources(&id).is_err());
        let mut recovery = Run::recover(&p, &id).unwrap();
        recovery.journal.endpoint = original_endpoint;
        save(&recovery.root, &recovery.journal).unwrap();
        drop(recovery);
        // The trusted fixture owns these exact IDs. Explicit manual cleanup is
        // distinct from granting a mismatching native recovery authority.
        match kind {
            ObjectKind::Container => {
                docker(&["container", "rm", &object]);
            }
            ObjectKind::Volume => {
                docker(&["volume", "rm", &object]);
            }
        }
        executor.recover_resources(&id).unwrap();
        assert!(
            HostResourceCoordinator::open(&p.host_state)
                .unwrap()
                .list(false)
                .unwrap()
                .is_empty()
        );
    }
    // A cgroup OOM is an infrastructure failure, not a candidate code verdict.
    // This allocates only inside this workload's 64 MiB cgroup.
    let mut oom_spec = execution.spec().clone();
    oom_spec.build.arguments[1] =
        "awk 'BEGIN {s=\"x\";for(i=0;i<28;i++)s=s s;print length(s)}'".into();
    let oom_execution =
        snapshot::freeze(&mut f.store, &owner, execution.target_revision(), oom_spec).unwrap();
    let oom_attempt = attempt(
        &mut f,
        &owner,
        &oom_execution,
        &w.id,
        build_envelope(&oom_execution).unwrap(),
        b"native cgroup OOM",
    );
    let oom = executor
        .build(
            &mut f.store,
            &owner,
            &oom_execution,
            &oom_attempt,
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(
        oom.outcome(),
        crate::execution_supervisor::Outcome::InfrastructureDeferred,
        "{:?}",
        oom.detail()
    );
    assert!(oom.artifact().is_none());
    assert!(oom.detail().is_some_and(|d| d.contains("OOM")));
    assert!(
        HostResourceCoordinator::open(&p.host_state)
            .unwrap()
            .list(false)
            .unwrap()
            .is_empty()
    );
    // Exercise a daemon disk-floor failure without exhausting the real host.
    let mut high_floor = p.clone();
    high_floor.daemon_floor_bytes = u64::MAX / 2;
    let constrained = DockerExecutor::probe(wrapper)
        .unwrap()
        .with_resources(high_floor)
        .unwrap();
    let floor_attempt = attempt(
        &mut f,
        &owner,
        &execution,
        &w.id,
        build_envelope(&execution).unwrap(),
        b"native daemon floor",
    );
    let failed = constrained.build(
        &mut f.store,
        &owner,
        &execution,
        &floor_attempt,
        &AtomicBool::new(false),
    );
    assert!(
        matches!(failed, Err(crate::execution_supervisor::SupervisorError::Infrastructure(ref d)) if d.contains("disk floor"))
    );
    assert!(
        HostResourceCoordinator::open(&p.host_state)
            .unwrap()
            .list(false)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        budget::load(&f.store, floor_attempt.reservation())
            .unwrap()
            .spent,
        floor_attempt.remaining()
    );
}
#[test]
fn journal_object_identity_includes_kind_and_recovery_rejects_generation_drift() {
    let host = tempfile::tempdir().unwrap();
    let p = policy(host.path());
    let (mut f, owner, execution, wave) = subject(p.runtime.clone(), p.meter_image.clone());
    let a = attempt(&mut f, &owner, &execution, &wave, envelope(), b"namespaces");
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
    let name = format!("aethyme-{id}-same-name");
    run.intent(ObjectKind::Volume, name.clone(), id.clone())
        .unwrap();
    run.intent(ObjectKind::Container, name.clone(), id.clone())
        .unwrap();
    assert!(
        run.intent(ObjectKind::Container, name.clone(), id.clone())
            .is_err()
    );
    run.created(ObjectKind::Container, &name, "b".repeat(64))
        .unwrap();
    assert!(run.journal.objects[0].id.is_none());
    run.created(ObjectKind::Volume, &name, name.clone())
        .unwrap();
    assert_eq!(
        run.journal.objects[1].id.as_deref(),
        Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
    );
    let original_request = run.journal.request.clone();
    let coordinator = HostResourceCoordinator::open(&p.host_state).unwrap();
    let mut changed_request = original_request;
    changed_request.repository = "other project".into();
    assert!(coordinator.recover_request(&changed_request).is_err());
    run.journal.objects[0].generation = "another generation".into();
    save(&run.root, &run.journal).unwrap();
    drop(run);
    assert!(Run::recover(&p, &id).is_err());
}
#[test]
fn host_journals_cannot_enter_the_project_backup_root() {
    use std::os::unix::fs::PermissionsExt;
    let host = tempfile::tempdir().unwrap();
    let mut p = policy(host.path());
    let (mut f, owner, execution, wave) = subject(p.runtime.clone(), p.meter_image.clone());
    let a = attempt(
        &mut f,
        &owner,
        &execution,
        &wave,
        envelope(),
        b"backup-root",
    );
    std::fs::set_permissions(
        f.store.project_dir(),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    p.host_state = f.store.project_dir().join("host-resources.db");
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
    assert!(!p.host_state.exists());
    assert!(
        !f.store
            .project_dir()
            .join("local-execution-journal")
            .exists()
    );
}

type ReservationLedgerRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    String,
    String,
    String,
    i64,
    i64,
    Option<String>,
    Option<String>,
    String,
);
type ReceiptLedgerRow = (String, String, i64, String);

fn admission_ledger(
    f: &Fixture,
    owner: &ControllerOwner,
) -> (Vec<ReservationLedgerRow>, Vec<ReceiptLedgerRow>, Units) {
    let mut statement = f.store.read_connection().prepare(
        "SELECT reservation,nonce,target,revision,wave,root,parent,hex(body),phase,hex(spent),attempts,version,receipt,detail,coalesce(hex(observed_overrun),'') FROM local_budget_reservations ORDER BY reservation",
    ).unwrap();
    let reservations = statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
                row.get(10)?,
                row.get(11)?,
                row.get(12)?,
                row.get(13)?,
                row.get(14)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let mut statement = f.store.read_connection().prepare(
        "SELECT receipt,reservation,attempt,hex(usage) FROM local_budget_receipts ORDER BY receipt",
    ).unwrap();
    let receipts = statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let charged = budget::charged(&f.store, owner).unwrap();
    (reservations, receipts, charged)
}

fn admission_request(
    execution: &ExecutionSnapshot,
    nonce: &[u8],
    units: Units,
    deadline_ms: i64,
) -> budget::Request {
    budget::Request {
        nonce: digest(nonce),
        recipe: execution.id().to_string(),
        kind: WorkKind::Verification,
        units,
        max_attempts: 1,
        deadline_ms,
    }
}

fn parent_request(execution: &ExecutionSnapshot, deadline_ms: i64) -> budget::Request {
    let child = envelope().units().unwrap();
    budget::Request {
        nonce: digest(b"admission-parent"),
        recipe: execution.id().to_string(),
        kind: WorkKind::Verification,
        units: Units {
            time_ms: child.time_ms * 4,
            memory_bytes: child.memory_bytes * 4,
            disk_bytes: child.disk_bytes * 4,
            model_requests: child.model_requests * 4,
            input_tokens: child.input_tokens * 4,
            output_tokens: child.output_tokens * 4,
            spend_microunits: child.spend_microunits * 4,
            slots: child.slots * 4,
            previews: child.previews * 4,
        },
        max_attempts: 2,
        deadline_ms,
    }
}

#[test]
fn shared_admission_routes_keep_one_nonce_and_reach_the_execution_guard() {
    // Direct admission first: the same nonce cannot be admitted a second time
    // through a parent, and the failed route leaves every ledger row unchanged.
    let host = tempfile::tempdir().unwrap();
    let p = policy(host.path());
    let (mut f, owner, execution, wave) = subject(p.runtime.clone(), p.meter_image.clone());
    let deadline = now() + 3_600_000;
    let parent = budget::reserve(
        &mut f.store,
        &owner,
        &wave,
        parent_request(&execution, deadline),
        now(),
    )
    .unwrap();
    let request = admission_request(
        &execution,
        b"same-logical-work-direct-first",
        envelope().units().unwrap(),
        deadline,
    );
    let direct = budget::admit(
        &mut f.store,
        &owner,
        &wave,
        request.clone(),
        budget::AdmissionRoute::Direct,
        || Ok(now()),
    )
    .unwrap();
    let guard = budget::execution_guard(
        &f.store,
        &owner,
        &direct,
        &execution,
        envelope().units().unwrap(),
    )
    .unwrap();
    drop(guard);
    let direct_ledger = admission_ledger(&f, &owner);
    assert!(matches!(
        budget::admit(
            &mut f.store,
            &owner,
            &wave,
            request.clone(),
            budget::AdmissionRoute::Delegated {
                parent: parent.id.clone(),
            },
            || Ok(now()),
        ),
        Err(budget::BudgetError::Immutable)
    ));
    assert_eq!(admission_ledger(&f, &owner), direct_ledger);

    // Delegated admission first: the same nonce cannot create a second root.
    // The child reaches the same real guard; while it is pending the parent
    // cannot start an overlapping attempt.
    let host = tempfile::tempdir().unwrap();
    let p = policy(host.path());
    let (mut f, owner, execution, wave) = subject(p.runtime.clone(), p.meter_image.clone());
    let deadline = now() + 3_600_000;
    let parent = budget::reserve(
        &mut f.store,
        &owner,
        &wave,
        parent_request(&execution, deadline),
        now(),
    )
    .unwrap();
    let request = admission_request(
        &execution,
        b"same-logical-work-delegated-first",
        envelope().units().unwrap(),
        deadline,
    );
    let delegated = budget::admit(
        &mut f.store,
        &owner,
        &wave,
        request.clone(),
        budget::AdmissionRoute::Delegated {
            parent: parent.id.clone(),
        },
        || Ok(now()),
    )
    .unwrap();
    let parent_key: Option<String> = f
        .store
        .read_connection()
        .query_row(
            "SELECT parent FROM local_budget_reservations WHERE reservation=?1",
            [delegated.reservation().as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(parent_key.as_deref(), Some(parent.id.as_str()));
    let guard = budget::execution_guard(
        &f.store,
        &owner,
        &delegated,
        &execution,
        envelope().units().unwrap(),
    )
    .unwrap();
    drop(guard);
    assert!(matches!(
        budget::begin_attempt(&mut f.store, &owner, &parent.id, now()),
        Err(budget::BudgetError::Pending)
    ));
    let delegated_ledger = admission_ledger(&f, &owner);
    assert!(matches!(
        budget::admit(
            &mut f.store,
            &owner,
            &wave,
            request,
            budget::AdmissionRoute::Direct,
            || Ok(now()),
        ),
        Err(budget::BudgetError::Immutable)
    ));
    assert_eq!(admission_ledger(&f, &owner), delegated_ledger);
}

#[test]
fn admission_refusals_leave_the_ledger_unchanged() {
    let host = tempfile::tempdir().unwrap();
    let p = policy(host.path());
    let (mut f, owner, execution, wave) = subject(p.runtime.clone(), p.meter_image.clone());
    let deadline = now() + 3_600_000;
    let parent = budget::reserve(
        &mut f.store,
        &owner,
        &wave,
        parent_request(&execution, deadline),
        now(),
    )
    .unwrap();
    let initial = admission_ledger(&f, &owner);

    assert!(matches!(
        budget::admit(
            &mut f.store,
            &owner,
            &aethyme_contracts::experimental_v0::RecordId::parse(&digest(b"wrong-wave")).unwrap(),
            admission_request(
                &execution,
                b"wrong-wave-child",
                envelope().units().unwrap(),
                deadline,
            ),
            budget::AdmissionRoute::Delegated {
                parent: parent.id.clone(),
            },
            || Ok(now()),
        ),
        Err(budget::BudgetError::Immutable)
    ));
    assert_eq!(admission_ledger(&f, &owner), initial);

    let missing_parent =
        aethyme_contracts::experimental_v0::RecordId::parse(&digest(b"unowned-parent")).unwrap();
    assert!(matches!(
        budget::admit(
            &mut f.store,
            &owner,
            &wave,
            admission_request(
                &execution,
                b"missing-parent-child",
                envelope().units().unwrap(),
                deadline,
            ),
            budget::AdmissionRoute::Delegated {
                parent: missing_parent,
            },
            || Ok(now()),
        ),
        Err(budget::BudgetError::Missing)
    ));
    assert_eq!(admission_ledger(&f, &owner), initial);

    let mut other = Fixture::new();
    let other_owner =
        target::acquire_controller(&mut other.store, "resource-target", "appointed").unwrap();
    assert!(matches!(
        budget::admit(
            &mut f.store,
            &other_owner,
            &wave,
            admission_request(
                &execution,
                b"wrong-owner-direct",
                envelope().units().unwrap(),
                deadline,
            ),
            budget::AdmissionRoute::Direct,
            || Ok(now()),
        ),
        Err(budget::BudgetError::Target(
            target::TargetError::Unauthorized
        ))
    ));
    assert_eq!(admission_ledger(&f, &owner), initial);

    let mut replacement = f.spec();
    replacement.composer_profile.push_str("-other-revision");
    let replacement = target::propose_revision(&mut f.store, &owner, replacement).unwrap();
    f.store
        .connection()
        .execute(
            "UPDATE local_targets SET active_revision=?1 WHERE target=?2",
            (replacement.id.as_str(), owner.target()),
        )
        .unwrap();
    let stale_ledger = admission_ledger(&f, &owner);
    assert!(matches!(
        budget::admit(
            &mut f.store,
            &owner,
            &wave,
            admission_request(
                &execution,
                b"stale-parent-child",
                envelope().units().unwrap(),
                deadline,
            ),
            budget::AdmissionRoute::Delegated { parent: parent.id },
            || Ok(now()),
        ),
        Err(budget::BudgetError::Stale)
    ));
    assert_eq!(admission_ledger(&f, &owner), stale_ledger);
}

#[test]
fn admission_second_clock_error_preserves_reserved_work_without_attempt() {
    let host = tempfile::tempdir().unwrap();
    let p = policy(host.path());
    let (mut f, owner, execution, wave) = subject(p.runtime.clone(), p.meter_image.clone());
    let first_now = now();
    let deadline = first_now + 3_600_000;
    let request = admission_request(
        &execution,
        b"second-clock-failure",
        envelope().units().unwrap(),
        deadline,
    );
    let expected_units = request.units;
    let mut samples = 0;
    let result = budget::admit(
        &mut f.store,
        &owner,
        &wave,
        request,
        budget::AdmissionRoute::Direct,
        || {
            samples += 1;
            if samples == 1 {
                Ok(first_now)
            } else {
                Err(budget::BudgetError::Invalid("clock unavailable".into()))
            }
        },
    );
    assert!(matches!(
        result,
        Err(budget::BudgetError::Invalid(ref detail)) if detail == "clock unavailable"
    ));
    assert_eq!(samples, 2);
    let rows = admission_ledger(&f, &owner);
    assert_eq!(rows.0.len(), 1);
    assert!(rows.1.is_empty());
    assert_eq!(rows.2, expected_units);
    assert_eq!(rows.0[0].8, "reserved");
    assert_eq!(rows.0[0].10, 0);
}
