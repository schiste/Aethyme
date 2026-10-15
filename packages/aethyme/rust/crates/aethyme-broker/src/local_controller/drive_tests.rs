use super::*;
use crate::execution_snapshot::{CommandSpec, LinuxProfile, ResourceLimits};
use crate::execution_verifier::{AssertionCase, AssertionInventory, VerifierPolicy};
use crate::local_target::tests::{Fixture, git};
fn digest(bytes: &[u8]) -> String {
    format!("sha256:{}", ObjectDigest::of(bytes).hex())
}
pub(crate) fn policy() -> DrivePolicy {
    let command = CommandSpec {
        program: "/bin/sh".into(),
        arguments: vec!["/artifact/app.sh".into()],
        environment: Default::default(),
    };
    let limits = ResourceLimits {
        timeout_ms: 30000,
        memory_bytes: 64 * 1024 * 1024,
        output_bytes: 1024 * 1024,
        pids: 32,
    };
    DrivePolicy {execution:ExecutionSpec {candidate_source:digest(b"placeholder"),preparation:None,dependencies:None,configuration:None,harness:digest(b"harness"),dataset:None,assertion_inventory:digest(b"inventory"),materialization_profile:snapshot::MATERIALIZATION_PROFILE.into(),profile:LinuxProfile {build_image:digest(b"image"),verifier_image:digest(b"image"),architecture:"aarch64".into(),runtime_profile:digest(b"runtime"),build_limits:limits,verifier_limits:limits},build:CommandSpec {program:"/bin/sh".into(),arguments:vec!["-c".into(),"set -eu; cp /source/one.txt /outputs/value; printf 'set -eu\\ntest ! -e /harness\\ntest ! -e /dataset\\ntest ! -e /var/run/docker.sock\\ncat /artifact/value\\n' > /outputs/app.sh".into()],environment:Default::default()},verify:command},wave_policy:wave::WavePolicy {debounce_ms:0,..Default::default()},max_wall_ms:300000,allow_empty_native_gates:true}
}
fn spec() -> RunSpec {
    RunSpec {
        target: "appointed".into(),
        revision: digest(b"revision"),
        epoch: digest(b"epoch"),
        wave: digest(b"wave"),
        policy: policy(),
        deadline_ms: 300000,
    }
}
#[test]
fn driver_configuration_is_finite_and_preparation_is_explicitly_refused() {
    let p = policy();
    p.validate().unwrap();
    for limit in [0, -1, 900001] {
        let mut bad = p.clone();
        bad.max_wall_ms = limit;
        assert!(bad.validate().is_err(), "controller deadline bound {limit}");
    }
    let mut bad = p.clone();
    bad.execution.profile.build_limits.timeout_ms = 60001;
    assert!(bad.validate().is_err(), "build deadline bound");
    let mut bad = p;
    bad.execution.profile.verifier_limits.timeout_ms = 60001;
    assert!(bad.validate().is_err(), "verifier deadline bound");
}
#[test]
fn dispatch_record_binds_every_operator_choice() {
    let s = spec();
    let old = canonical(&s).unwrap().id();
    let mut changes = Vec::new();
    let mut v = s.clone();
    v.target.push('x');
    changes.push(v);
    let mut v = s.clone();
    v.revision = digest(b"other-revision");
    changes.push(v);
    let mut v = s.clone();
    v.epoch = digest(b"other-epoch");
    changes.push(v);
    let mut v = s.clone();
    v.wave = digest(b"other-wave");
    changes.push(v);
    let mut v = s.clone();
    v.deadline_ms += 1;
    changes.push(v);
    let mut v = s.clone();
    v.policy.max_wall_ms += 1;
    changes.push(v);
    let mut v = s.clone();
    v.policy.allow_empty_native_gates = false;
    changes.push(v);
    let mut v = s.clone();
    v.policy.execution.build.arguments.push("changed".into());
    changes.push(v);
    let mut v = s.clone();
    v.policy.execution.harness = digest(b"changed-harness");
    changes.push(v);
    let mut v = s.clone();
    v.policy.wave_policy.batch_width += 1;
    changes.push(v);
    for v in changes {
        assert_ne!(
            old,
            canonical(&v).unwrap().id(),
            "dispatch identity changed"
        );
    }
}
#[test]
fn live_dispatch_guard_rejects_cancel_deadline_epoch_and_revision() {
    let mut f = Fixture::new();
    let owner = target::acquire_controller(&mut f.store, "appointed", "fixture-operator").unwrap();
    let revision_spec = f.spec();
    let revision = target::propose_revision(&mut f.store, &owner, revision_spec).unwrap();
    target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
    wave::available(&mut f.store, &owner, "A", 0).unwrap();
    let wave::Admission::Frozen(w) =
        wave::admit(&mut f.store, &owner, policy().wave_policy, 0).unwrap()
    else {
        panic!("actual wave not frozen")
    };
    let mut s = spec();
    s.revision = revision.id.to_string();
    s.epoch = owner.authority_epoch().as_str().into();
    s.wave = w.id.to_string();
    s.deadline_ms = clock().unwrap() + 300000;
    check(&f.store, &owner, &s, &AtomicBool::new(false)).unwrap();
    assert!(
        check(&f.store, &owner, &s, &AtomicBool::new(true)).is_err(),
        "cancel dispatch"
    );
    let mut v = s.clone();
    v.deadline_ms = clock().unwrap() - 1;
    assert!(
        check(&f.store, &owner, &v, &AtomicBool::new(false)).is_err(),
        "expired dispatch"
    );
    let mut v = s.clone();
    v.epoch = digest(b"stale");
    assert!(
        check(&f.store, &owner, &v, &AtomicBool::new(false)).is_err(),
        "stale epoch dispatch"
    );
    let mut v = s;
    v.revision = digest(b"stale-revision");
    assert!(
        check(&f.store, &owner, &v, &AtomicBool::new(false)).is_err(),
        "stale revision dispatch"
    );
}
#[test]
#[ignore = "actual appointed cached Docker build, independent verifier, native gates and disposable GenericGit acceptance"]
fn actual_appointed_driver_completes_without_a_manufactured_receipt() {
    let host = tempfile::tempdir().unwrap().keep();
    eprintln!("Controller actual native recovery root {}", host.display());
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o700)).unwrap();
    let cli = std::env::var("AETHYME_DOCKER_CLI").expect("appointed actual Docker client");
    let image = std::env::var("AETHYME_DOCKER_TEST_IMAGE").expect("appointed cached image");
    assert_eq!(
        image,
        "sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251"
    );
    let native = DockerExecutor::probe(cli.into()).unwrap();
    let resources = crate::execution_resources::ResourcePolicy {
        host_state: host.join("resources.db"),
        temporary_root: host.clone(),
        runtime: native.runtime().id(),
        daemon: native.runtime().daemon_id.clone(),
        meter_image: image.clone(),
        memory_mib: 2048,
        disk_mib: 2048,
        process_slots: 1024,
        job_slots: 2,
        host_floor_bytes: 1,
        daemon_floor_bytes: 1,
    };
    let executor = native.with_resources(resources).unwrap();
    let mut f = Fixture::new();
    let source_host = std::mem::replace(&mut f._host, tempfile::tempdir().unwrap()).keep();
    let repo = std::mem::replace(&mut f._repo, tempfile::tempdir().unwrap()).keep();
    eprintln!(
        "Controller retained source host {}, repository {}",
        source_host.display(),
        repo.display()
    );
    let owner = target::acquire_controller(
        &mut f.store,
        "native-controller",
        "appointed-local-operator",
    )
    .unwrap();
    let inventory = AssertionInventory::new(vec![AssertionCase {
        id: "retained-one".into(),
        arguments: vec![],
        stdin_hex: String::new(),
        exit_code: 0,
        stdout: digest(b"A\n"),
        stderr: digest(b""),
    }])
    .unwrap();
    std::fs::write(repo.join("inventory.json"), inventory.canonical_bytes()).unwrap();
    git(&repo, &["add", "inventory.json"]);
    git(
        &repo,
        &["commit", "-qm", "independent controller behavior inventory"],
    );
    let harness_commit = CommitOid::parse(&git(&repo, &["rev-parse", "HEAD"])).unwrap();
    let harness = archive::retain_snapshot(&mut f.store, &repo, &harness_commit).unwrap();
    let mut p = policy();
    p.execution.harness = harness.snapshot_id.to_string();
    p.execution.assertion_inventory = inventory.id().to_string();
    p.execution.profile.build_image = image.clone();
    p.execution.profile.verifier_image = image.clone();
    p.execution.profile.runtime_profile = executor.runtime().id();
    p.execution.profile.architecture = executor.runtime().architecture.clone();
    let verifier_policy = VerifierPolicy {
        profile: verifier::PROFILE.into(),
        runtime: executor.runtime().id(),
        image,
        harness: harness.snapshot_id.to_string(),
        inventory: inventory.id().to_string(),
        command: p.execution.verify.clone(),
        max_age_ms: 300000,
    };
    let mut selected = f.spec();
    selected.acceptance_policy = verifier_policy.record().unwrap().id().to_string();
    selected.selected = vec!["A".into(), "B".into()];
    p.wave_policy.batch_width = 1;
    let revision = target::propose_revision(&mut f.store, &owner, selected).unwrap();
    target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
    let permit = verifier::configure(
        &mut f.store,
        &owner,
        "controller-native",
        None,
        verifier_policy,
    )
    .unwrap();
    let cap = budget::Units {
        time_ms: 1000000000,
        memory_bytes: 64 * 1024 * 1024 * 1024,
        disk_bytes: 64 * 1024 * 1024 * 1024,
        slots: 100,
        ..Default::default()
    };
    budget::configure_project(
        &mut f.store,
        &owner,
        budget::ProjectPolicy {
            authorization: digest(b"controller authorization"),
            price_profile: digest(b"unpriced native execution"),
            cap,
            recovery_headroom: Default::default(),
            mandatory_headroom: Default::default(),
            max_root_reservations: 32,
        },
    )
    .unwrap();
    budget::authorize_objective(&mut f.store, &owner, cap).unwrap();
    let remote = tempfile::tempdir().unwrap().keep();
    git(&remote, &["init", "--bare", "-q"]);
    git(
        &repo,
        &[
            "remote",
            "add",
            "controller-accepted",
            remote.to_str().unwrap(),
        ],
    );
    git(
        &repo,
        &[
            "push",
            "-q",
            "controller-accepted",
            &format!("{}:refs/heads/accepted", f.a.base.commit.as_str()),
        ],
    );
    let original = git(&repo, &["rev-parse", "HEAD"]);
    let mut broker = Broker::open(&repo)
        .unwrap()
        .with_host_operation_database(host.join("operations.db"))
        .with_worktree_root(host.join("worktrees"));
    let session = broker
        .adopt(
            &repo,
            Some("appointed disposable Local controller Playground"),
        )
        .unwrap();
    let carrier = GitCarrier::new(
        &mut broker,
        session.id,
        "controller-accepted",
        "refs/heads/accepted",
        30,
        "Operator authorizes exact disposable local bare Git controller target",
        true,
    )
    .unwrap();
    wave::available(&mut f.store, &owner, "A", 0).unwrap();
    let appointment = super::super::service::ServicePolicy {
        nonce: "sustained-arrival-1".into(),
        drive: p.clone(),
        max_ticks: 4096,
        max_completions: 2,
        poll_ms: 20,
        max_elapsed_ms: 600000,
    };
    let arrival_host = source_host.clone();
    let service = std::thread::scope(|scope| {
        let owner_ref = &owner;
        let sender=scope.spawn(move || {
            let mut observer=CollaborationStore::open(&crate::collaboration_state::CollaborationRoot::under_host_state(&arrival_host),&crate::collaboration_state::ProjectKey::parse("local-target-test").unwrap(),&[]).unwrap();
            let until=std::time::Instant::now()+std::time::Duration::from_secs(300);
            loop {
                let completed:i64=observer.read_connection().query_row("SELECT count(*) FROM local_controller_completions WHERE target='native-controller'",[],|r|r.get(0)).unwrap();
                if completed==1 && target::status(&observer,owner_ref.target()).unwrap().phase==Phase::WaitingForInput {
                    // This persisted state witnesses an actual missing-input tick.
                    // A delay after completion alone can expire during cleanup.
                    wave::available(&mut observer,owner_ref,"B",clock().unwrap()).unwrap();
                    return;
                }
                if std::time::Instant::now()>=until {panic!("bounded arrival observer did not see first completion");}
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        });
        let report = super::super::service::serve(
            &mut f.store,
            &owner,
            &mut broker,
            &carrier,
            &executor,
            &permit,
            &appointment,
            &AtomicBool::new(false),
        );
        sender.join().unwrap();
        report.unwrap()
    });
    assert_eq!(
        service.outcome,
        super::super::service::ServiceOutcome::CompletionAllowance,
        "finite service must accept both separate arrivals"
    );
    assert_eq!(service.completions.len(), 2);
    assert!(
        service.ticks > 2,
        "timed service waits for the later durable arrival without redispatch"
    );
    let completion = service.completions[0].clone();
    let second = service.completions[1].clone();
    assert_eq!(
        target::status(&f.store, owner.target()).unwrap().phase,
        Phase::PublicationConfirmed,
        "private grant required for completion"
    );
    let wave_id = RecordId::parse(&completion.wave).unwrap();
    assert_eq!(
        wave::load(&f.store, &wave_id).unwrap().phase,
        WavePhase::Accepted
    );
    assert_eq!(
        target::dispositions(&f.store, &revision.id).unwrap()[0].status,
        target::Disposition::Delivered
    );
    assert_eq!(
        git(&remote, &["rev-parse", "refs/heads/accepted"]),
        second.accepted_commit
    );
    assert_eq!(
        git(&repo, &["rev-parse", "HEAD"]),
        original,
        "no original checkout/ref writes"
    );
    assert_eq!(
        load(&f.store, &completion.record().unwrap().id()).unwrap(),
        completion
    );
    let second_wave = wave::load(&f.store, &RecordId::parse(&second.wave).unwrap()).unwrap();
    assert_eq!(
        second_wave.spec.prior_completion.as_deref(),
        Some(completion.record().unwrap().id().as_str()),
        "prior private completion is baseline authority"
    );
    assert_eq!(second_wave.spec.baseline, completion.source);
    assert_eq!(
        git(
            &repo,
            &["show", &format!("{}:one.txt", second.candidate_commit)]
        ),
        "A",
        "later arrival retains earlier active obligation"
    );
    assert_eq!(
        git(
            &repo,
            &["show", &format!("{}:two.txt", second.candidate_commit)]
        ),
        "B",
        "later arrival delivers its own change"
    );
    assert_eq!(
        git(
            &repo,
            &["rev-parse", &format!("{}^", second.candidate_commit)]
        ),
        completion.accepted_commit,
        "candidate parent is exact prior acceptance"
    );
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), original);
    let runs: i64 = f
        .store
        .read_connection()
        .query_row(
            "SELECT count(*) FROM local_controller_runs WHERE state='completed'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(runs, 2);
    let pending:i64=f.store.read_connection().query_row("SELECT count(*) FROM local_budget_reservations WHERE phase IN ('reserved','partly_consumed','completion_pending','consumption_unknown')",[],|r|r.get(0)).unwrap();
    assert_eq!(pending, 0, "no unconfirmed budget released");
    let bytes=serde_json::to_vec_pretty(&json!({"profile":"aethyme.local-controller/experimental-v1","completion":completion,"second_completion":second,"service":service,"source_host":source_host,"source_repository":repo,"remote":remote,"host":host,"runtime":executor.runtime(),"completed_runs":runs,"pending_reservations":pending,"original_head":original})).unwrap();
    std::fs::write(host.join("report.json"), bytes).unwrap();
}

#[test]
fn completion_control_checks_cancel_and_expiry_at_commit_boundary() {
    control_check(None).unwrap();
    control_check(Some((clock().unwrap() + 300000, &AtomicBool::new(false)))).unwrap();
    assert!(
        control_check(Some((clock().unwrap() - 1, &AtomicBool::new(false)))).is_err(),
        "expired completion control"
    );
    assert!(
        control_check(Some((clock().unwrap() + 300000, &AtomicBool::new(true)))).is_err(),
        "cancelled completion control"
    );
}

#[test]
fn completed_native_job_is_history_and_cannot_approve_controller_dispatch() {
    let mut f = Fixture::new();
    let owner = target::acquire_controller(&mut f.store, "completed-gate", "operator").unwrap();
    let selected = f.spec();
    let revision = target::propose_revision(&mut f.store, &owner, selected).unwrap();
    target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
    wave::available(&mut f.store, &owner, "A", 0).unwrap();
    let wave::Admission::Frozen(w) =
        wave::admit(&mut f.store, &owner, policy().wave_policy, 0).unwrap()
    else {
        panic!("missing wave")
    };
    wave::compose(&mut f.store, &owner, f._repo.path(), &w.id).unwrap();
    let mut broker = Broker::open(f._repo.path())
        .unwrap()
        .with_host_operation_database(f._host.path().join("operations.db"))
        .with_worktree_root(f._host.path().join("worktrees"));
    let job = native_gates::prepare(&mut f.store, &owner, &mut broker, &w.id).unwrap();
    require_fresh_native(&job).unwrap();
    let job = native_gates::run(&mut f.store, &owner, &mut broker, &job.id).unwrap();
    assert_eq!(job.state, "completed");
    assert_eq!(job.observation.as_ref().unwrap()["current_authority"], true);
    assert!(
        require_fresh_native(&job).is_err(),
        "historical native result cannot approve new controller dispatch"
    );
    let mut restarted = job;
    restarted.state = "dispatching".into();
    assert!(
        require_fresh_native(&restarted).is_err(),
        "ambiguous native dispatch cannot be retried"
    );
}

#[test]
fn controller_migrations_adopt_existing_journals_without_erasing_rows() {
    let mut f = Fixture::new();
    let owner = target::acquire_controller(&mut f.store, "adoption", "fixture-operator").unwrap();
    let selected = f.spec();
    let revision = target::propose_revision(&mut f.store, &owner, selected).unwrap();
    target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
    wave::available(&mut f.store, &owner, "A", 0).unwrap();
    let wave::Admission::Frozen(w) =
        wave::admit(&mut f.store, &owner, policy().wave_policy, 0).unwrap()
    else {
        panic!("real wave missing");
    };
    let connection = f.store.connection();
    connection.execute("INSERT INTO local_controller_completions(id,wave,revision,target,body) VALUES('migration-sentinel',?1,?2,'adoption',?3)",params![w.id.as_str(),revision.id.as_str(),b"completion-preserved".as_slice()]).unwrap();
    connection.execute("INSERT INTO local_controller_completion_roots(completion,kind,reference) VALUES('migration-sentinel','source','retained-sentinel')",[]).unwrap();
    connection.execute("INSERT INTO local_controller_runs(wave,id,epoch,body,state,observation) VALUES(?1,'run-sentinel',?2,?3,'unknown',?4)",params![w.id.as_str(),owner.authority_epoch().as_str(),b"dispatch-preserved".as_slice(),b"unknown-preserved".as_slice()]).unwrap();
    connection
        .execute(
            "UPDATE meta SET value='18' WHERE key IN ('schema_version','min_compatible_schema')",
            [],
        )
        .unwrap();
    drop(owner);
    let host = f._host.path().to_path_buf();
    drop(f.store);
    let reopened = CollaborationStore::open(
        &crate::collaboration_state::CollaborationRoot::under_host_state(&host),
        &crate::collaboration_state::ProjectKey::parse("local-target-test").unwrap(),
        &[],
    )
    .unwrap();
    assert_eq!(
        reopened.schema_version(),
        crate::collaboration_state::COLLABORATION_STATE_SCHEMA_VERSION
    );
    let completion: Vec<u8> = reopened
        .read_connection()
        .query_row(
            "SELECT body FROM local_controller_completions WHERE id='migration-sentinel'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(completion, b"completion-preserved");
    let run: (Vec<u8>, String, Vec<u8>) = reopened
        .read_connection()
        .query_row(
            "SELECT body,state,observation FROM local_controller_runs WHERE id='run-sentinel'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        run,
        (
            b"dispatch-preserved".to_vec(),
            "unknown".into(),
            b"unknown-preserved".to_vec()
        )
    );
    let roots:i64=reopened.read_connection().query_row("SELECT count(*) FROM local_controller_completion_roots WHERE completion='migration-sentinel' AND reference='retained-sentinel'",[],|r|r.get(0)).unwrap();
    assert_eq!(roots, 1, "migration preserves independent retention roots");
}

#[test]
fn later_same_line_edit_then_third_wave_does_not_replay_accepted_deltas() {
    use crate::composer::{self, CompositionBudget, CompositionRequest, ContributionSpec};
    let mut f = Fixture::new();
    let repo = f._repo.path();
    git(
        repo,
        &["checkout", "-q", "--detach", f.a.result.commit.as_str()],
    );
    std::fs::write(repo.join("one.txt"), "B\n").unwrap();
    git(repo, &["commit", "-qam", "dependent same-line edit"]);
    let b_commit = archive::pin_commit(repo, "HEAD").unwrap();
    let b =
        archive::retain_contribution(&mut f.store, repo, &f.a.result.commit, &b_commit).unwrap();
    git(
        repo,
        &["checkout", "-q", "--detach", f.a.base.commit.as_str()],
    );
    std::fs::write(repo.join("three.txt"), "C\n").unwrap();
    git(repo, &["add", "three.txt"]);
    git(repo, &["commit", "-qm", "later independent arrival"]);
    let c_commit = archive::pin_commit(repo, "HEAD").unwrap();
    let c = archive::retain_contribution(&mut f.store, repo, &f.a.base.commit, &c_commit).unwrap();
    let contribution =
        |id: &str, c: &archive::RetainedContribution, requires: Vec<String>| ContributionSpec {
            id: id.into(),
            lineage: Some(c.lineage_record_id.clone()),
            requires,
            atomic_group: None,
            revision_of: None,
            derived_from: vec![],
        };
    let mut request = CompositionRequest {
        baseline: f.a.base.commit.clone(),
        accepted: vec![],
        catalog: vec![
            contribution("A", &f.a, vec![]),
            contribution("B", &b, vec!["A".into()]),
            contribution("C", &c, vec![]),
        ],
        deliveries: vec!["A".into()],
        budget: CompositionBudget::default(),
    };
    let first = composer::compose(&mut f.store, repo, &request)
        .unwrap()
        .outcome
        .candidate()
        .unwrap()
        .clone();
    request.baseline = first.commit.clone();
    request.accepted = vec![f.a.base.commit.clone()];
    request.deliveries.push("B".into());
    let controlled_proof = RecordId::parse(&digest(b"synthetic composition-only proof")).unwrap();
    let second = composer::compose_after_accepted(
        &mut f.store,
        repo,
        &request,
        controlled_proof.clone(),
        vec!["A".into()],
    )
    .unwrap()
    .outcome
    .candidate()
    .unwrap()
    .clone();
    assert_eq!(
        git(
            repo,
            &["show", &format!("{}:one.txt", second.commit.as_str())]
        ),
        "B"
    );
    request.baseline = second.commit.clone();
    request.deliveries.push("C".into());
    let third = composer::compose_after_accepted(
        &mut f.store,
        repo,
        &request,
        controlled_proof,
        vec!["A".into(), "B".into()],
    )
    .unwrap();
    let candidate = third
        .outcome
        .candidate()
        .expect("third wave must not replay the accepted A delta onto B");
    assert_eq!(
        git(
            repo,
            &["show", &format!("{}:one.txt", candidate.commit.as_str())]
        ),
        "B"
    );
    assert_eq!(
        git(
            repo,
            &["show", &format!("{}:three.txt", candidate.commit.as_str())]
        ),
        "C"
    );
    assert_eq!(
        git(
            repo,
            &["rev-parse", &format!("{}^", candidate.commit.as_str())]
        ),
        second.commit.as_str()
    );
    assert_eq!(
        third.order,
        vec!["A", "B", "C"],
        "all surviving constituents remain in provenance"
    );
    let recipe = composer::read_recipe(&f.store, third.recipe.as_ref().unwrap()).unwrap();
    assert!(
        recipe.get("accepted_baseline").is_some(),
        "recipe records the private wave adapter input"
    );
    static LEGACY: RecordSchema = RecordSchema {
        name: composer::COMPOSITION_RECIPE_SCHEMA_NAME,
        fields: &[],
        capabilities: &[],
    };
    assert!(
        Record::decode(&recipe.canonical_bytes(), &[&LEGACY]).is_err(),
        "old readers must refuse incremental recipes they cannot interpret"
    );
}

#[test]
fn delayed_admission_cannot_extend_an_appointed_absolute_deadline() {
    // Exercise the exact helper used before admission and stored in RunSpec.
    // A later admission time still uses the original frozen expiry.
    let frozen = dispatch_deadline(1000, 300000, Some(1010)).unwrap();
    assert_eq!(frozen, 1010);
    assert_eq!(dispatch_deadline(1008, 300000, Some(1010)).unwrap(), frozen);
    assert!(dispatch_deadline(1010, 300000, Some(1010)).is_err());
    assert!(dispatch_deadline(1011, 300000, Some(1010)).is_err());
    assert_eq!(dispatch_deadline(1000, 5, Some(1010)).unwrap(), 1005);
    assert_eq!(dispatch_deadline(1000, 5, None).unwrap(), 1005);
}

#[test]
fn actual_delayed_database_admission_refuses_dispatch_after_frozen_expiry() {
    let mut f = Fixture::new();
    let owner = target::acquire_controller(&mut f.store, "delayed-admission", "operator").unwrap();
    let selected = f.spec();
    let revision = target::propose_revision(&mut f.store, &owner, selected).unwrap();
    target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
    wave::available(&mut f.store, &owner, "A", 0).unwrap();
    let path = f.store.project_dir().join("state.db");
    let (sender, receiver) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let db = rusqlite::Connection::open(path).unwrap();
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        sender.send(()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        db.execute_batch("COMMIT").unwrap();
    });
    receiver
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    let deadline = clock().unwrap() + 75;
    let result = admit_before_deadline(
        &mut f.store,
        &owner,
        policy().wave_policy,
        0,
        deadline,
        &AtomicBool::new(false),
    );
    writer.join().unwrap();
    assert!(clock().unwrap() >= deadline);
    assert!(
        matches!(result, Err(ControllerError::Mismatch)),
        "actual delayed admission must refuse, got {result:?}"
    );
    let waves: i64 = f
        .store
        .read_connection()
        .query_row("SELECT count(*) FROM local_waves", [], |r| r.get(0))
        .unwrap();
    assert_eq!(waves, 1, "the delayed actual wave is retained");
    let runs: i64 = f
        .store
        .read_connection()
        .query_row("SELECT count(*) FROM local_controller_runs", [], |r| {
            r.get(0)
        })
        .unwrap();
    let completions: i64 = f
        .store
        .read_connection()
        .query_row(
            "SELECT count(*) FROM local_controller_completions",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        (runs, completions),
        (0, 0),
        "expired admission cannot start or complete execution"
    );
}

#[test]
fn only_known_successful_publication_dispatch_can_reach_qualification() {
    use crate::OperationStatus;
    assert!(dispatch_succeeded(OperationStatus::Succeeded));
    for outcome in [
        OperationStatus::Prepared,
        OperationStatus::Running,
        OperationStatus::Failed,
        OperationStatus::OutcomeUnknown,
        OperationStatus::ReconciledSucceeded,
        OperationStatus::ReconciledFailed,
    ] {
        assert!(
            !dispatch_succeeded(outcome),
            "fresh publication status {outcome:?} must hold regardless of later source availability"
        );
    }
}
