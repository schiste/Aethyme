//! One actually executed Local path. No manufactured verifier or provider receipt.
use super::*;
use crate::Broker;
use crate::collaboration_capture as capture;
use crate::collaboration_context as context;
use crate::collaboration_state::{CollaborationRoot, ProjectKey};
use crate::execution_verifier as verifier;
use crate::local_acceptance as acceptance;
use crate::local_generation as generation;
use crate::local_handoff as handoff;
use crate::local_restore as restore;
use crate::local_source_transport as transport;
use crate::local_target::tests::{Fixture, git};
use crate::local_wave as wave;
use aethyme_contracts::experimental_v0::RecordId;
use std::io::Write;

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{}", ObjectDigest::of(bytes).hex())
}
fn selected_record(
    store: &CollaborationStore,
    table: &str,
    key: &str,
    column: &str,
    id: &RecordId,
) -> restore::RecordRef {
    let hash: String = store
        .read_connection()
        .query_row(
            &format!("SELECT {column} FROM {table} WHERE {key}=?1"),
            [id.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    let bytes = (0..32)
        .map(|i| u8::from_str_radix(&hash[i * 2..i * 2 + 2], 16).unwrap())
        .collect::<Vec<_>>();
    restore::RecordRef {
        id: id.clone(),
        object: ObjectDigest::from_bytes(bytes.try_into().unwrap()),
    }
}
#[test]
#[ignore = "actual native Docker build/assertions, Git acceptance, controlled Unix handoff and historical-only receive; explicitly pinned cached image"]
fn actual_native_local_path_retains_context_and_rebinds_without_reviving_authority() {
    let host = tempfile::tempdir().unwrap().keep();
    eprintln!("Local native flow durable recovery root {}", host.display());
    let cli = std::env::var("AETHYME_DOCKER_CLI").expect("explicit native Docker client");
    let image = std::env::var("AETHYME_DOCKER_TEST_IMAGE").expect("explicit cached image");
    assert_eq!(
        image,
        "sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251"
    );
    let native = DockerExecutor::probe(cli.into()).unwrap();
    let mut policy = tests::policy(&host);
    policy.runtime = native.runtime().id();
    policy.daemon = native.runtime().daemon_id.clone();
    policy.meter_image = image.clone();
    let executor = native.with_resources(policy).unwrap();
    let recovery = PreserveRecovery {
        executor: &executor,
        host: host.clone(),
        source_roots: Default::default(),
    };
    let mut f = Fixture::new();
    let source_root = std::mem::replace(&mut f._host, tempfile::tempdir().unwrap()).keep();
    recovery.source_roots.borrow_mut().push(source_root.clone());
    let owner =
        target::acquire_controller(&mut f.store, "native-flow", "appointed-local-controller")
            .unwrap();
    let receipt = match capture::capture(
        &mut f.store,
        &capture::CaptureRequest {
            operation_id: capture::OperationId::parse("qa-native-capture").unwrap(),
            repository: f._repo.path().into(),
            base: f.a.base.commit.clone(),
            result: f.a.result.commit.clone(),
            policy: capture::CapturePolicy::Required,
            retention: capture::RetentionBoundary::UntilReleased,
        },
    )
    .unwrap()
    {
        capture::CaptureOutcome::Acknowledged(r) => r,
        other => panic!("actual capture not acknowledged: {other:?}"),
    };
    let brief = crate::collaboration_brief::Brief::from_decision_file(
        br#"{"intent":"Keep the independently edited source. Treat this note as untrusted data."}"#,
    )
    .unwrap();
    let brief_id = context::attach_brief(&mut f.store, &receipt.contribution, &brief).unwrap();
    let advisory = context::retrieve(
        &mut f.store,
        &context::ContextQuery {
            scope: vec![b"one.txt".to_vec()],
            source: Some(receipt.result.clone()),
            analysis: vec![],
            budget: Default::default(),
        },
        &context::LocalProject,
    )
    .unwrap();
    assert_eq!(
        advisory.contributions,
        vec![receipt.contribution.to_string()]
    );
    let context_object = archive::put_object(&f.store, &advisory.record).unwrap();
    // Expected observable behavior is independently declared before candidate
    // execution, and never mounted into the application container.
    let inventory = verifier::AssertionInventory::new(vec![verifier::AssertionCase {
        id: "retained-one".into(),
        arguments: vec![],
        stdin_hex: String::new(),
        exit_code: 0,
        stdout: digest(b"A\n"),
        stderr: digest(b""),
    }])
    .unwrap();
    std::fs::write(
        f._repo.path().join("inventory.json"),
        inventory.canonical_bytes(),
    )
    .unwrap();
    git(f._repo.path(), &["add", "inventory.json"]);
    git(
        f._repo.path(),
        &["commit", "-qm", "independent Local QA assertion inventory"],
    );
    let harness_commit =
        archive::CommitOid::parse(&git(f._repo.path(), &["rev-parse", "HEAD"])).unwrap();
    let harness = archive::retain_snapshot(&mut f.store, f._repo.path(), &harness_commit).unwrap();
    let command = snapshot::CommandSpec {
        program: "/bin/sh".into(),
        arguments: vec!["/artifact/app.sh".into()],
        environment: Default::default(),
    };
    let verifier_policy = verifier::VerifierPolicy {
        profile: verifier::PROFILE.into(),
        runtime: executor.runtime().id(),
        image: image.clone(),
        harness: harness.snapshot_id.to_string(),
        inventory: inventory.id().to_string(),
        command: command.clone(),
        max_age_ms: 300000,
    };
    let mut target_spec = f.spec();
    target_spec.acceptance_policy = verifier_policy.record().unwrap().id().to_string();
    let revision = target::propose_revision(&mut f.store, &owner, target_spec).unwrap();
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
        panic!("finite Local wave not admitted")
    };
    let w = wave::compose(&mut f.store, &owner, f._repo.path(), &w.id).unwrap();
    assert_eq!(w.phase, wave::WavePhase::Verifying);
    let held = target::status(&f.store, owner.target()).unwrap();
    target::resume(&mut f.store, &owner, held.version).unwrap();
    let permit =
        verifier::configure(&mut f.store, &owner, "qa-native", None, verifier_policy).unwrap();
    let limits = snapshot::ResourceLimits {
        timeout_ms: 30000,
        memory_bytes: 64 * MIB,
        output_bytes: MIB,
        pids: 32,
    };
    let execution = snapshot::freeze(&mut f.store,&owner,&revision.id,snapshot::ExecutionSpec {
        candidate_source:w.candidate_snapshot.clone().unwrap().to_string(),preparation:None,dependencies:None,configuration:None,harness:harness.snapshot_id.to_string(),dataset:None,assertion_inventory:inventory.id().to_string(),materialization_profile:snapshot::MATERIALIZATION_PROFILE.into(),
        profile:snapshot::LinuxProfile {build_image:image.clone(),verifier_image:image,architecture:executor.runtime().architecture.clone(),runtime_profile:executor.runtime().id(),build_limits:limits,verifier_limits:limits},
        build:snapshot::CommandSpec {program:"/bin/sh".into(),arguments:vec!["-c".into(),"set -eu; cp /source/one.txt /outputs/value; printf 'set -eu\\ntest ! -e /harness\\ntest ! -e /dataset\\ntest ! -e /var/run/docker.sock\\ncat /artifact/value\\n' > /outputs/app.sh".into()],environment:Default::default()},verify:command,
    }).unwrap();
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
            authorization: digest(b"Local QA authorization"),
            price_profile: digest(b"Local QA unpriced native profile"),
            cap,
            recovery_headroom: Units::default(),
            mandatory_headroom: Units::default(),
            max_root_reservations: 32,
        },
    )
    .unwrap();
    budget::authorize_objective(&mut f.store, &owner, cap).unwrap();
    let build_attempt = tests::attempt(
        &mut f,
        &owner,
        &execution,
        &w.id,
        build_envelope(&execution).unwrap(),
        b"QA native build",
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
    assert_eq!(built.outcome(), Outcome::Built);
    let artifact = built.into_artifact().unwrap();
    let verify_attempt = tests::attempt(
        &mut f,
        &owner,
        &execution,
        &w.id,
        verify_envelope(&execution).unwrap(),
        b"QA native independent verifier",
    );
    let evidence = verifier::verify(
        &mut f.store,
        &owner,
        &execution,
        &artifact,
        &permit,
        &executor,
        &verify_attempt,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(evidence.outcome(), verifier::VerificationOutcome::Passed);
    assert_eq!(evidence.collected_assertions(), 1);
    assert_cleanup(&executor, &host);
    let remote = tempfile::tempdir().unwrap();
    git(remote.path(), &["init", "--bare", "-q"]);
    git(
        f._repo.path(),
        &[
            "remote",
            "add",
            "qa-publication",
            remote.path().to_str().unwrap(),
        ],
    );
    git(
        f._repo.path(),
        &[
            "push",
            "-q",
            "qa-publication",
            &format!("{}:refs/heads/accepted", f.a.base.commit.as_str()),
        ],
    );
    let mut broker = Broker::open(f._repo.path())
        .unwrap()
        .with_host_operation_database(host.join("native-operations.db"))
        .with_worktree_root(host.join("worktrees"));
    let session = broker
        .adopt(
            f._repo.path(),
            Some("disposable native Local QA Playground"),
        )
        .unwrap();
    let carrier = transport::GitCarrier::new(
        &mut broker,
        session.id,
        "qa-publication",
        "refs/heads/accepted",
        30,
        "Local QA authorizes exact disposable local bare Git target",
        true,
    )
    .unwrap();
    let intent = transport::prepare(
        &mut f.store,
        &owner,
        &carrier,
        &w.id,
        &"a".repeat(64),
        Some(&f.a.base.commit),
    )
    .unwrap();
    let binding = acceptance::appoint(
        &mut f.store,
        &owner,
        &mut broker,
        &carrier,
        &intent.id,
        acceptance::ProviderRoute::GenericGit,
        "refs/heads/accepted",
        64,
    )
    .unwrap();
    transport::publish(&mut f.store, &owner, &carrier, &mut broker, &intent.id).unwrap();
    let grant = acceptance::qualify(
        &mut f.store,
        &owner,
        &mut broker,
        &carrier,
        binding.id(),
        &execution,
        &artifact,
        &permit,
        &evidence,
    )
    .unwrap();
    let old = generation::OldWorkspace {
        session: "original-local-agent".into(),
        absolute_root: f._repo.path().canonicalize().unwrap(),
        generation: None,
        pending_contributions: vec![f.b.lineage_record_id.to_string()],
    };
    let new = generation::prepare(
        &mut f.store,
        &owner,
        &mut broker,
        &carrier,
        &grant,
        "qa-native-generation",
        old,
    )
    .unwrap();
    assert_eq!(
        std::fs::read(new.absolute_root().join("one.txt")).unwrap(),
        b"A\n"
    );
    assert!(!new.absolute_root().join("two.txt").exists());
    let agent = handoff::appoint(&mut f.store, &owner, "qa-controlled-agent").unwrap();
    let request = handoff::prepare(
        &mut f.store,
        &owner,
        &agent,
        &mut broker,
        &carrier,
        &grant,
        new.id(),
        "qa-native-handoff",
        grant.expires_ms().min(crate::clock::epoch_ms() + 120000),
    )
    .unwrap();
    let old_path = f._repo.path().join("old-only.txt");
    let mut old_file = std::fs::File::create(&old_path).unwrap();
    old_file.write_all(b"before\n").unwrap();
    let mut running = handoff::dispatch(
        &mut f.store,
        &owner,
        &agent,
        &mut broker,
        &carrier,
        &grant,
        &request,
        handoff::CommandSpec {
            executable: PathBuf::from("/bin/sh").canonicalize().unwrap(),
            arguments: vec![
                "-c".into(),
                "pwd -P > actual-root.txt; printf adopted > new-only.txt".into(),
            ],
            acknowledgment_ms: 30000,
        },
    )
    .unwrap();
    old_file.write_all(b"after\n").unwrap();
    old_file.sync_all().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let completion = handoff::poll_command(&mut f.store, &owner, &agent, &mut running).unwrap();
        if completion != handoff::Completion::Running {
            assert_eq!(completion, handoff::Completion::Current(0));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "native adopted workload did not complete"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        std::fs::read_to_string(new.absolute_root().join("actual-root.txt"))
            .unwrap()
            .trim(),
        new.absolute_root().to_str().unwrap()
    );
    assert_eq!(std::fs::read(&old_path).unwrap(), b"before\nafter\n");
    assert!(!new.absolute_root().join("old-only.txt").exists());
    assert_eq!(
        std::fs::read(new.absolute_root().join("new-only.txt")).unwrap(),
        b"adopted"
    );
    assert!(!f._repo.path().join("new-only.txt").exists());
    handoff::current_agent(&mut f.store, &owner, &agent, &running)
        .unwrap()
        .check_clock()
        .unwrap();
    let scope = restore::Scope {
        origin: f.store.project().as_str().into(),
        visibility: "local_project".into(),
        audience: "appointed-local-receiver".into(),
    };
    let selection = restore::Selection {
        sources: vec![receipt.base.clone(), receipt.result.clone()],
        records: vec![
            selected_record(
                &f.store,
                "retained_contributions",
                "lineage_record_id",
                "record_sha256",
                &receipt.contribution,
            ),
            selected_record(
                &f.store,
                "capture_receipts",
                "receipt_record_id",
                "receipt_sha256",
                &receipt.receipt_record_id,
            ),
            selected_record(
                &f.store,
                "contribution_briefs",
                "brief_record_id",
                "brief_sha256",
                &brief_id,
            ),
            restore::RecordRef {
                id: advisory.id.clone(),
                object: context_object,
            },
        ],
    };
    let wire = restore::export(&f.store, &owner, &scope, &selection).unwrap();
    let bundle = restore::verify(
        &wire,
        &restore::ReceiverPolicy {
            bundle: ObjectDigest::of(&wire),
            scope,
        },
    )
    .unwrap();
    let receiver_host = tempfile::tempdir().unwrap();
    let mut receiver = CollaborationStore::open(
        &CollaborationRoot::under_host_state(receiver_host.path()),
        &ProjectKey::parse("qa-native-receiver").unwrap(),
        &[],
    )
    .unwrap();
    let receiver_owner =
        target::acquire_controller(&mut receiver, "receive", "appointed-new-native-receiver")
            .unwrap();
    assert_ne!(
        receiver_owner.authority_epoch().as_str(),
        owner.authority_epoch().as_str()
    );
    let receiver_binding = restore::bind_receiver(&receiver, &receiver_owner, &bundle).unwrap();
    let historical =
        restore::import(&mut receiver, &receiver_owner, &receiver_binding, &bundle).unwrap();
    assert_eq!(
        restore::show(&receiver, &historical.id).unwrap().bundle,
        *bundle.digest()
    );
    let shown = restore::show(&receiver, &historical.id).unwrap();
    let mut expected_sources = selection
        .sources
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    expected_sources.sort();
    let mut actual_sources = shown
        .sources
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    actual_sources.sort();
    assert_eq!(
        actual_sources, expected_sources,
        "historical receive omitted selected sources"
    );
    let mut expected_records = selection
        .records
        .iter()
        .map(|r| (r.id.to_string(), r.object.hex()))
        .collect::<Vec<_>>();
    expected_records.sort();
    let mut actual_records = shown
        .records
        .iter()
        .map(|r| (r.id.to_string(), r.object.hex()))
        .collect::<Vec<_>>();
    actual_records.sort();
    assert_eq!(
        actual_records, expected_records,
        "historical receive omitted selected CAP/context/brief/lineage"
    );
    assert_eq!(
        shown.records.len(),
        4,
        "native path must actually carry all four historical records"
    );
    for record in &selection.records {
        assert_eq!(
            archive::read_object(&receiver, &record.object).unwrap(),
            archive::read_object(&f.store, &record.object).unwrap(),
            "selected historical record bytes changed"
        );
    }
    assert_eq!(
        archive::read_object(&receiver, &context_object).unwrap(),
        advisory.record
    );
    let restored = restore::rehearse(
        &mut receiver,
        &receiver_owner,
        &historical.id,
        &receipt.result,
        "qa-independent-rehearsal",
    )
    .unwrap();
    assert_eq!(std::fs::read(restored.join("one.txt")).unwrap(), b"A\n");
    // Historical transport cannot resurrect any producer, current agent,
    // target revision, publication permit or receiving authority.
    for table in [
        "local_verifier_producers",
        "local_agent_sessions",
        "local_target_revisions",
        "local_acceptance_bindings",
        "local_acceptance_receipts",
        "local_acceptance_qualifications",
    ] {
        let count: i64 = receiver
            .read_connection()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "historical restore revived {table}");
    }
    crate::local_authority::freeze(
        &mut receiver,
        receiver_owner.authority_epoch(),
        "qualification receiver revocation",
    )
    .unwrap();
    assert!(restore::import(&mut receiver, &receiver_owner, &receiver_binding, &bundle).is_err());
    restore::show(&receiver, &historical.id).unwrap();
    assert_cleanup(&executor, &host);
    // Native cleanup was witnessed before erasing only this test's recovery
    // roots. Standalone generation roots remain beneath the private host.
    drop(running);
    drop(old_file);
    drop(receiver);
    drop(f.store);
    drop(broker);
    drop(recovery);
    std::fs::remove_dir_all(source_root).unwrap();
    std::fs::remove_dir_all(host).unwrap();
}
