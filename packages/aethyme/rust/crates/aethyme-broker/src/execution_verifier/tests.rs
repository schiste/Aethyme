use super::*;
use crate::local_target::tests::Fixture;
fn d(bytes: &[u8]) -> String {
    format!("sha256:{}", ObjectDigest::of(bytes).hex())
}
fn case() -> AssertionCase {
    AssertionCase {
        id: "echo".into(),
        arguments: vec![],
        stdin_hex: "68656c6c6f0a".into(),
        exit_code: 0,
        stdout: d(b"hello\n"),
        stderr: d(b""),
    }
}
fn policy() -> VerifierPolicy {
    VerifierPolicy {
        profile: PROFILE.into(),
        runtime: d(b"runtime"),
        image: d(b"image"),
        harness: d(b"harness"),
        inventory: AssertionInventory::new(vec![case()])
            .unwrap()
            .id()
            .to_string(),
        command: CommandSpec {
            program: "/bin/sh".into(),
            arguments: vec!["/artifact/app.sh".into()],
            environment: Default::default(),
        },
        max_age_ms: 1000,
    }
}
#[test]
fn inventory_identity_is_canonical_and_independently_calculated() {
    let inventory = AssertionInventory::new(vec![case()]).unwrap();
    assert_eq!(
        inventory.id().as_str(),
        "sha256:37c55ec41f1b8f27f9f26f138d092dec7289b7a25ad49e6488530996325757d9"
    );
    assert_eq!(
        AssertionInventory::decode(&inventory.canonical_bytes()).unwrap(),
        inventory
    );
    let mut changed = case();
    changed.arguments.push("other".into());
    assert_ne!(
        AssertionInventory::new(vec![changed]).unwrap().id(),
        inventory.id()
    );
    let mut noncanonical = inventory.canonical_bytes();
    noncanonical.push(b'\n');
    assert!(AssertionInventory::decode(&noncanonical).is_err());
}
#[test]
fn missing_duplicate_invalid_and_oversized_inventories_are_refused() {
    assert!(AssertionInventory::new(vec![]).is_err());
    assert!(AssertionInventory::new(vec![case(), case()]).is_err());
    for change in 0..6 {
        let mut c = case();
        match change {
            0 => c.id = "../unsafe".into(),
            1 => c.stdin_hex = "GG".into(),
            2 => c.stdin_hex = "1".into(),
            3 => c.stdout = "sha256:unknown".into(),
            4 => c.arguments = vec!["arg".into(); 33],
            5 => c.stdin_hex = "00".repeat(65537),
            _ => unreachable!(),
        };
        assert!(AssertionInventory::new(vec![c]).is_err(), "{change}");
    }
    let cases = (0..33)
        .map(|i| {
            let mut c = case();
            c.id = format!("case{i}");
            c
        })
        .collect();
    assert!(AssertionInventory::new(cases).is_err());
}
#[test]
fn trusted_operator_rotates_and_revokes_exact_producer_generations() {
    let mut f = Fixture::new();
    let owner = target::acquire_controller(&mut f.store, "verifier", "appointed").unwrap();
    let policy = policy();
    let mut spec = f.spec();
    spec.acceptance_policy = policy.record().unwrap().id().to_string();
    let revision = target::propose_revision(&mut f.store, &owner, spec).unwrap();
    target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
    let first = configure(&mut f.store, &owner, "native", None, policy.clone()).unwrap();
    producer_guard(&f.store, &owner, &first).unwrap();
    assert!(matches!(
        configure(&mut f.store, &owner, "native", None, policy.clone()),
        Err(VerificationError::Stale)
    ));
    assert!(matches!(
        revoke(&mut f.store, &owner, "native", 2),
        Err(VerificationError::Stale)
    ));
    revoke(&mut f.store, &owner, "native", 1).unwrap();
    assert!(matches!(
        producer_guard(&f.store, &owner, &first),
        Err(VerificationError::Producer)
    ));
    assert!(matches!(
        configure(&mut f.store, &owner, "native", Some(1), policy.clone()),
        Err(VerificationError::Stale)
    ));
    let next = configure(&mut f.store, &owner, "native", Some(2), policy).unwrap();
    assert_eq!(next.generation(), 3);
    producer_guard(&f.store, &owner, &next).unwrap();
    crate::local_authority::freeze(
        &mut f.store,
        owner.authority_epoch(),
        "explicit verifier test freeze",
    )
    .unwrap();
    assert!(producer_guard(&f.store, &owner, &next).is_err());
}
#[test]
fn producer_policy_profile_bounds_and_target_policy_must_match() {
    for change in 0..5 {
        let mut p = policy();
        match change {
            0 => p.profile = "uploaded-json".into(),
            1 => p.max_age_ms = 0,
            2 => p.command.program = "relative".into(),
            3 => p.image = "latest".into(),
            4 => {
                p.command
                    .environment
                    .insert("LD-PRELOAD".into(), "bad".into());
            }
            _ => unreachable!(),
        };
        assert!(p.record().is_err(), "{change}");
    }
    let mut f = Fixture::new();
    let owner = target::acquire_controller(&mut f.store, "wrong-policy", "appointed").unwrap();
    let spec = f.spec();
    let revision = target::propose_revision(&mut f.store, &owner, spec).unwrap();
    target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
    assert!(matches!(
        configure(&mut f.store, &owner, "native", None, policy()),
        Err(VerificationError::Mismatch)
    ));
}
struct Subject {
    f: Fixture,
    owner: ControllerOwner,
    execution: ExecutionSnapshot,
    artifact: SealedArtifact,
    permit: ProducerPermit,
    inventory: AssertionInventory,
}
impl Subject {
    fn fake() -> Self {
        use aethyme_contracts::experimental_v0::SourceSnapshot;
        use aethyme_contracts::experimental_v0::source_snapshot::{EntryKind, SourceEntry};
        let mut f = Fixture::new();
        let owner = target::acquire_controller(&mut f.store, "evidence", "appointed").unwrap();
        let inventory = AssertionInventory::new(vec![case()]).unwrap();
        let policy = policy();
        let mut policy = policy;
        policy.harness = f.b.result.snapshot_id.to_string();
        // Policy must be frozen before selecting the target; use the actual
        // harness identity in the target's appointed policy too.
        let mut ts = f.spec();
        ts.acceptance_policy = policy.record().unwrap().id().to_string();
        ts.objective = "exact subject".into();
        let revision = target::propose_revision(&mut f.store, &owner, ts).unwrap();
        target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
        for phase in [
            target::Phase::Admitting,
            target::Phase::WaveFrozen,
            target::Phase::Composing,
            target::Phase::Verifying,
        ] {
            let state = target::status(&f.store, owner.target()).unwrap();
            target::transition(&mut f.store, &owner, state.version, phase).unwrap();
        }
        let permit = configure(&mut f.store, &owner, "native", None, policy.clone()).unwrap();
        let limits = snapshot::ResourceLimits {
            timeout_ms: 5000,
            memory_bytes: 67108864,
            output_bytes: 1048576,
            pids: 32,
        };
        let spec = snapshot::ExecutionSpec {
            candidate_source: f.a.result.snapshot_id.to_string(),
            preparation: None,
            dependencies: None,
            configuration: None,
            harness: policy.harness.clone(),
            dataset: None,
            assertion_inventory: inventory.id().to_string(),
            materialization_profile: snapshot::MATERIALIZATION_PROFILE.into(),
            profile: snapshot::LinuxProfile {
                build_image: d(b"image"),
                verifier_image: policy.image.clone(),
                architecture: "arm64".into(),
                runtime_profile: policy.runtime.clone(),
                build_limits: limits,
                verifier_limits: limits,
            },
            build: CommandSpec {
                program: "/bin/true".into(),
                arguments: vec![],
                environment: Default::default(),
            },
            verify: policy.command,
        };
        let execution = snapshot::freeze(&mut f.store, &owner, &revision.id, spec).unwrap();
        let bytes = b"sealed".to_vec();

        archive::put_object(&f.store, &bytes).unwrap();
        let manifest = SourceSnapshot::new(vec![SourceEntry::from_content(
            b"app".to_vec(),
            EntryKind::Regular,
            &bytes,
        )])
        .unwrap();
        archive::put_object(&f.store, &manifest.manifest_bytes()).unwrap();
        let packaging = d(&serde_json::to_vec(&execution.spec().build).unwrap());
        let raw = serde_json::json!({"schema":crate::execution_supervisor::ARTIFACT_SCHEMA,"execution":execution.id().as_str(),"target_revision":revision.id.as_str(),"output":manifest.id().as_str(),"packaging":packaging,"runtime":policy.runtime,"generation":"test"});
        // Independent canonical encoding for the test-only native capability.
        let encoded = serde_json::to_vec(&raw).unwrap();
        let logical = RecordId::parse(&d(
            &[b"aethyme record v0\0".as_slice(), encoded.as_slice()].concat()
        ))
        .unwrap();
        let record = archive::put_object(&f.store, &encoded).unwrap();
        target::retain_artifacts(&mut f.store, &owner, &revision.id, &[], &[record]).unwrap();
        let artifact = SealedArtifact {
            id: logical,
            record,
            output: crate::execution_supervisor::artifact::Output {
                manifest,
                files: std::collections::BTreeMap::from([(b"app".to_vec(), bytes)]),
            },
            execution: execution.id().clone(),
            generation: "test".into(),
            runtime: policy.runtime,
            epoch: owner.authority_epoch().clone(),
        };
        Self {
            f,
            owner,
            execution,
            artifact,
            permit,
            inventory,
        }
    }
    fn evidence(&mut self) -> TrustedEvidence {
        let assertions = self
            .inventory
            .cases
            .iter()
            .map(|c| AssertionObservation {
                id: c.id.clone(),
                outcome: VerificationOutcome::Passed,
                exit_code: Some(i32::from(c.exit_code)),
                stdout: Some(c.stdout.clone()),
                stderr: Some(c.stderr.clone()),
            })
            .collect();
        let body = expected_body(
            &self.owner,
            &self.execution,
            &self.artifact,
            &self.permit,
            "a".repeat(64),
            assertions,
            VerificationOutcome::Passed,
        )
        .unwrap();
        retain_evidence(
            &mut self.f.store,
            &self.owner,
            body,
            self.permit.policy.max_age_ms,
        )
        .unwrap()
    }
    fn check(&self, e: &TrustedEvidence, at: i64) -> Result<(), VerificationError> {
        applicable_at(
            &self.f.store,
            &self.owner,
            &self.execution,
            &self.artifact,
            &self.permit,
            e,
            &self.inventory,
            at,
        )
    }
}
#[test]
fn incomplete_wrong_subject_expired_and_revoked_evidence_cannot_pass() {
    let mut s = Subject::fake();
    let mut evidence = s.evidence();
    s.check(&evidence, evidence.issued_ms).unwrap();
    assert!(s.check(&evidence, evidence.issued_ms - 1).is_err());
    assert!(s.check(&evidence, evidence.expires_ms).is_err());
    let original = evidence.body.clone();
    for field in 0..20 {
        evidence.body = original.clone();
        match field {
            0 => evidence.body.producer.push('x'),
            1 => evidence.body.generation += 1,
            2 => evidence.body.epoch = d(b"other"),
            3 => evidence.body.revision = d(b"other"),
            4 => evidence.body.execution = d(b"other"),
            5 => evidence.body.candidate = d(b"other"),
            6 => evidence.body.artifact = d(b"other"),
            7 => evidence.body.output = d(b"other"),
            8 => evidence.body.policy = d(b"other"),
            9 => evidence.body.runtime = d(b"other"),
            10 => evidence.body.image = d(b"other"),
            11 => evidence.body.harness = d(b"other"),
            12 => evidence.body.inventory = d(b"other"),
            13 => evidence.body.profile = "uploaded".into(),
            14 => evidence.body.assertions.clear(),
            15 => evidence
                .body
                .assertions
                .push(evidence.body.assertions[0].clone()),
            16 => evidence.body.assertions[0].id = "different".into(),
            17 => evidence.body.assertions[0].stdout = Some(d(b"wrong")),
            18 => evidence.body.result = VerificationOutcome::Inconclusive,
            19 => evidence.body.verification = "unknown".into(),
            _ => unreachable!(),
        };
        assert!(
            s.check(&evidence, evidence.issued_ms).is_err(),
            "field {field}"
        );
    }
    evidence.body = original;
    revoke(&mut s.f.store, &s.owner, "native", s.permit.generation).unwrap();
    assert!(s.check(&evidence, evidence.issued_ms).is_err());
}
fn capture(f: &mut Fixture, path: &str, bytes: &[u8]) -> String {
    use crate::local_target::tests::git;
    std::fs::write(f._repo.path().join(path), bytes).unwrap();
    git(f._repo.path(), &["add", path]);
    git(f._repo.path(), &["commit", "-qm", "verifier fixture input"]);
    let head = archive::CommitOid::parse(&git(f._repo.path(), &["rev-parse", "HEAD"])).unwrap();
    archive::retain_snapshot(&mut f.store, f._repo.path(), &head)
        .unwrap()
        .snapshot_id
        .to_string()
}
#[test]
#[ignore = "requires explicitly selected cached public Linux Docker image"]
fn real_cli_assertions_observe_behavior_and_refuse_forged_or_stale_receipts() {
    let executor = DockerExecutor::probe(
        std::env::var_os("AETHYME_DOCKER_CLI")
            .expect("trusted Docker client")
            .into(),
    )
    .unwrap();
    let image = std::env::var("AETHYME_DOCKER_TEST_IMAGE").expect("pinned cached public image");
    let mut f = Fixture::new();
    let mut hello = case();
    hello.stdin_hex = "410a".into();
    hello.stdout = d(b"Hello,A\n");
    hello.id = "A".into();
    let mut second = hello.clone();
    second.id = "B".into();
    second.stdin_hex = "420a".into();
    second.stdout = d(b"Hello,B\n");
    let inventory = AssertionInventory::new(vec![hello, second]).unwrap();
    let harness = capture(&mut f, "inventory.json", &inventory.canonical_bytes());
    // Independent expected digests/inventory stay on the host; the artifact is
    // the only mounted input to the app under test. A success file is ignored.
    crate::local_target::tests::git(f._repo.path(), &["rm", "inventory.json"]);
    let source=capture(&mut f,"app.sh",b"set -eu\ntest ! -e /harness\ntest ! -e /dataset\ntest ! -e /var/run/docker.sock\ntest -z \"$HOME\"\nif printf tamper 2>/dev/null > /artifact/app.sh; then exit 77; fi\nread -r name\nprintf 'Hello,%s\\n' \"$name\"\n");
    let limits = snapshot::ResourceLimits {
        timeout_ms: 5000,
        memory_bytes: 67108864,
        output_bytes: 1048576,
        pids: 32,
    };
    let command = CommandSpec {
        program: "/bin/sh".into(),
        arguments: vec!["/artifact/app.sh".into()],
        environment: Default::default(),
    };
    let policy = VerifierPolicy {
        profile: PROFILE.into(),
        runtime: executor.runtime().id(),
        image: image.clone(),
        harness: harness.clone(),
        inventory: inventory.id().to_string(),
        command: command.clone(),
        max_age_ms: 60000,
    };
    let owner = target::acquire_controller(&mut f.store, "real-verifier", "appointed").unwrap();
    let mut ts = f.spec();
    ts.acceptance_policy = policy.record().unwrap().id().to_string();
    let revision = target::propose_revision(&mut f.store, &owner, ts).unwrap();
    target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
    for phase in [
        target::Phase::Admitting,
        target::Phase::WaveFrozen,
        target::Phase::Composing,
        target::Phase::Verifying,
    ] {
        let state = target::status(&f.store, owner.target()).unwrap();
        target::transition(&mut f.store, &owner, state.version, phase).unwrap();
    }
    let permit = configure(&mut f.store, &owner, "native", None, policy).unwrap();
    let mut spec = snapshot::ExecutionSpec {
        candidate_source: source,
        preparation: None,
        dependencies: None,
        configuration: None,
        harness,
        dataset: None,
        assertion_inventory: inventory.id().to_string(),
        materialization_profile: snapshot::MATERIALIZATION_PROFILE.into(),
        profile: snapshot::LinuxProfile {
            build_image: image.clone(),
            verifier_image: image,
            architecture: executor.runtime().architecture.clone(),
            runtime_profile: executor.runtime().id(),
            build_limits: limits,
            verifier_limits: limits,
        },
        build: CommandSpec {
            program: "/bin/sh".into(),
            arguments: vec!["-c".into(), "cp /source/app.sh /outputs/app.sh".into()],
            environment: Default::default(),
        },
        verify: command,
    };
    let execution = snapshot::freeze(&mut f.store, &owner, &revision.id, spec.clone()).unwrap();
    let artifact = executor
        .build(&mut f.store, &owner, &execution, &AtomicBool::new(false))
        .unwrap()
        .into_artifact()
        .unwrap();
    let evidence = verify(
        &mut f.store,
        &owner,
        &execution,
        &artifact,
        &permit,
        &executor,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(
        evidence.outcome(),
        VerificationOutcome::Passed,
        "{:?}",
        evidence.body
    );
    assert_eq!(evidence.collected_assertions(), 2);
    applicable(&f.store, &owner, &execution, &artifact, &permit, &evidence).unwrap();
    load_evidence(
        &f.store,
        &owner,
        &execution,
        &artifact,
        &permit,
        evidence.reference(),
    )
    .unwrap();
    let again = verify(
        &mut f.store,
        &owner,
        &execution,
        &artifact,
        &permit,
        &executor,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_ne!(again.reference().id, evidence.reference().id);
    let mut fake = evidence.reference().clone();
    fake.id = RecordId::parse(&d(b"app uploaded success")).unwrap();
    assert!(load_evidence(&f.store, &owner, &execution, &artifact, &permit, &fake).is_err());
    let cancelled = verify(
        &mut f.store,
        &owner,
        &execution,
        &artifact,
        &permit,
        &executor,
        &AtomicBool::new(true),
    )
    .unwrap();
    assert_eq!(cancelled.outcome(), VerificationOutcome::Cancelled);
    assert!(applicable(&f.store, &owner, &execution, &artifact, &permit, &cancelled).is_err());
    let phase = target::status(&f.store, owner.target()).unwrap().phase;
    assert_eq!(
        phase,
        target::Phase::Verifying,
        "verification must not authorize acceptance"
    );
    // Self-reported passed JSON and exit zero cannot replace actual behavior.
    spec.build.arguments[1] =
        "printf 'printf success > /tmp/result.json; printf wrong' > /outputs/app.sh".into();
    let wrong = snapshot::freeze(&mut f.store, &owner, &revision.id, spec).unwrap();
    let wrong_artifact = executor
        .build(&mut f.store, &owner, &wrong, &AtomicBool::new(false))
        .unwrap()
        .into_artifact()
        .unwrap();
    let failure = verify(
        &mut f.store,
        &owner,
        &wrong,
        &wrong_artifact,
        &permit,
        &executor,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(failure.outcome(), VerificationOutcome::AssertionFailed);
    assert_eq!(failure.collected_assertions(), 1);
    assert!(applicable(&f.store, &owner, &wrong, &wrong_artifact, &permit, &failure).is_err());
    assert!(
        applicable(
            &f.store,
            &owner,
            &wrong,
            &wrong_artifact,
            &permit,
            &evidence
        )
        .is_err()
    );
    revoke(&mut f.store, &owner, "native", permit.generation()).unwrap();
    assert!(
        load_evidence(
            &f.store,
            &owner,
            &execution,
            &artifact,
            &permit,
            evidence.reference()
        )
        .is_err()
    );
}
