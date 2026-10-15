use crate::collaboration_archive::{self as archive, CommitOid};
use crate::execution_resources::ResourcePolicy;
use crate::execution_supervisor::DockerExecutor;
use crate::execution_verifier::{
    self as verifier, AssertionCase, AssertionInventory, VerifierPolicy,
};
use crate::local_budget as budget;
use crate::local_controller::ControllerError;
use crate::local_controller::drive as driver;
use crate::local_controller::service::{self as controller_service, ServiceOutcome, ServicePolicy};
use crate::local_source_transport::{self as transport, GitCarrier};
use crate::local_target::tests::{Fixture, git};
use crate::local_target::{self as target, Phase};
use crate::local_wave::{self as wave, WavePhase};
use crate::native_gates;
use crate::{
    Broker, CoordinatedOperation, OperationHistoryQuery, OperationProvider, OperationStatus,
};
use aethyme_contracts::experimental_v0::RecordId;
use rusqlite::params;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::atomic::AtomicBool;

const PINNED_IMAGE: &str =
    "sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251";

fn publication_operations(
    broker: &Broker,
    session_id: i64,
    candidate: &str,
) -> Vec<CoordinatedOperation> {
    let page = broker
        .store_ref()
        .operation_history(&OperationHistoryQuery {
            limit: 100,
            session_id: Some(session_id),
            provider: Some(OperationProvider::Git),
            ..Default::default()
        })
        .unwrap();
    let pushed_ref = format!("{candidate}:refs/heads/accepted");
    page.operations
        .into_iter()
        .filter(|operation| {
            let args: Vec<String> =
                serde_json::from_str(&operation.command_json).unwrap_or_default();
            args.get(1).is_some_and(|arg| arg == "push")
                && args.iter().any(|arg| arg == &pushed_ref)
        })
        .collect()
}

fn shell_quote(path: &Path) -> String {
    let path = path.to_str().expect("temporary path is UTF-8");
    format!("'{}'", path.replace('\'', "'\\''"))
}

#[test]
#[ignore = "actual appointed Docker build, independent verifier, native gates, and timed disposable bare-Git publication"]
fn actual_unknown_publication_after_receiver_ref_update_never_completes() {
    use std::os::unix::fs::PermissionsExt;

    let host = tempfile::tempdir().unwrap().keep();
    eprintln!(
        "Controller actual unknown-publication evidence root {}",
        host.display()
    );
    std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o700)).unwrap();
    let cli = std::env::var("AETHYME_DOCKER_CLI").expect("appointed actual Docker client");
    let image = std::env::var("AETHYME_DOCKER_TEST_IMAGE").expect("appointed cached image");
    assert_eq!(image, PINNED_IMAGE, "use the reviewed image digest");
    let native = DockerExecutor::probe(cli.into()).unwrap();
    let executor = native
        .with_resources(ResourcePolicy {
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
        })
        .unwrap();

    let mut f = Fixture::new();
    let source_host = std::mem::replace(&mut f._host, tempfile::tempdir().unwrap()).keep();
    let repo = std::mem::replace(&mut f._repo, tempfile::tempdir().unwrap()).keep();
    eprintln!(
        "Controller unknown-publication source host {}, repository {}",
        source_host.display(),
        repo.display()
    );
    let owner = target::acquire_controller(
        &mut f.store,
        "native-unknown-publication",
        "appointed-local-operator",
    )
    .unwrap();

    let inventory = AssertionInventory::new(vec![AssertionCase {
        id: "retained-one".into(),
        arguments: vec![],
        stdin_hex: String::new(),
        exit_code: 0,
        stdout: super::digest(b"A\n"),
        stderr: super::digest(b""),
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

    let mut drive = super::policy();
    drive.execution.harness = harness.snapshot_id.to_string();
    drive.execution.assertion_inventory = inventory.id().to_string();
    drive.execution.profile.build_image = image.clone();
    drive.execution.profile.verifier_image = image.clone();
    drive.execution.profile.runtime_profile = executor.runtime().id();
    drive.execution.profile.architecture = executor.runtime().architecture.clone();
    drive.wave_policy.batch_width = 1;
    let verifier_policy = VerifierPolicy {
        profile: verifier::PROFILE.into(),
        runtime: executor.runtime().id(),
        image: image.clone(),
        harness: harness.snapshot_id.to_string(),
        inventory: inventory.id().to_string(),
        command: drive.execution.verify.clone(),
        max_age_ms: 300000,
    };
    let mut selected = f.spec();
    selected.acceptance_policy = verifier_policy.record().unwrap().id().to_string();
    selected.selected = vec!["A".into()];
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
            authorization: super::digest(b"controller authorization"),
            price_profile: super::digest(b"unpriced native execution"),
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
    let baseline = f.a.base.commit.as_str().to_owned();
    git(
        &repo,
        &[
            "push",
            "-q",
            "controller-accepted",
            &format!("{baseline}:refs/heads/accepted"),
        ],
    );
    let hook_record = remote.join("hooks/post-receive-record");
    let hook = remote.join("hooks/post-receive");
    std::fs::write(
        &hook,
        format!(
            "#!/bin/sh\nset -eu\n/usr/bin/cat > {}\nexec /bin/sleep 5\n",
            shell_quote(&hook_record)
        ),
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();

    let mut broker = Broker::open(&repo)
        .unwrap()
        .with_host_operation_database(host.join("operations.db"))
        .with_worktree_root(host.join("worktrees"));
    let session = broker
        .adopt(
            &repo,
            Some("actual receiver response timeout after ref update"),
        )
        .unwrap();
    let carrier = GitCarrier::new(
        &mut broker,
        session.id,
        "controller-accepted",
        "refs/heads/accepted",
        2,
        "Operator authorizes exact disposable local bare Git controller target",
        true,
    )
    .unwrap();

    wave::available(&mut f.store, &owner, "A", 0).unwrap();
    let appointment = ServicePolicy {
        nonce: "post-receive-timeout-1".into(),
        drive: drive.clone(),
        max_ticks: 32,
        max_completions: 1,
        poll_ms: 20,
        max_elapsed_ms: 600000,
    };
    let service = controller_service::serve(
        &mut f.store,
        &owner,
        &mut broker,
        &carrier,
        &executor,
        &permit,
        &appointment,
        &AtomicBool::new(false),
    )
    .unwrap();

    assert_eq!(service.outcome, ServiceOutcome::Held);
    assert!(service.completions.is_empty());
    assert_eq!(service.ticks, 1);
    let intent_id: String = f
        .store
        .read_connection()
        .query_row(
            "SELECT intent FROM local_source_transports WHERE target=?1 AND revision=?2",
            params![owner.target(), revision.id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    let intent = transport::load(&f.store, &RecordId::parse(&intent_id).unwrap()).unwrap();
    let receiver_record = std::fs::read_to_string(&hook_record)
        .expect("receiver-owned post-receive hook must record the moved ref before delaying");
    let receiver_update: Vec<&str> = receiver_record.split_whitespace().collect();
    assert_eq!(
        receiver_update,
        vec![
            baseline.as_str(),
            intent.spec.commit.as_str(),
            "refs/heads/accepted"
        ],
        "Git invokes post-receive after applying this exact update"
    );
    let remote_commit = git(&remote, &["rev-parse", "refs/heads/accepted"]);
    assert_eq!(remote_commit, intent.spec.commit);

    let publication = publication_operations(&broker, session.id, &intent.spec.commit);
    assert_eq!(
        publication.len(),
        1,
        "one actual managed push was dispatched"
    );
    let operation = &publication[0];
    assert_eq!(operation.status, OperationStatus::OutcomeUnknown);
    assert!(
        operation.exit_code.is_none(),
        "the bounded caller timed out"
    );
    let operation_details: Value =
        serde_json::from_str(operation.details_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        operation_details["failure_class"],
        "coordinated_operation_timeout"
    );
    assert_eq!(
        operation_details["stage"],
        "executing coordinated operation"
    );
    assert_eq!(operation_details["remote_outcome"], "unknown");

    let (dispatched, observation, receipt): (i64, String, Option<Vec<u8>>) = f
        .store
        .read_connection()
        .query_row(
            "SELECT dispatched,observation,receipt FROM local_source_transports WHERE intent=?1",
            [&intent_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(dispatched, 1, "transport intent remains durably dispatched");
    assert_eq!(
        observation, "unobserved",
        "unknown write was never inspected"
    );
    assert!(
        receipt.is_none(),
        "no read receipt or private grant was manufactured"
    );
    let transport_receipts: i64 = f
        .store
        .read_connection()
        .query_row(
            "SELECT count(*) FROM local_source_transport_receipts WHERE intent=?1",
            [&intent_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(transport_receipts, 0);

    let (wave_id, run_state, run_observation): (String, String, Option<Vec<u8>>) = f
        .store
        .read_connection()
        .query_row(
            "SELECT wave,state,observation FROM local_controller_runs WHERE wave IN (SELECT wave FROM local_waves WHERE target=?1)",
            [owner.target()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        run_state, "unknown",
        "the actual native dispatch history is retained"
    );
    assert!(run_observation.is_some());
    let wave_id = RecordId::parse(&wave_id).unwrap();
    assert_eq!(
        wave::load(&f.store, &wave_id).unwrap().phase,
        WavePhase::Verifying
    );
    assert_eq!(
        target::status(&f.store, owner.target()).unwrap().phase,
        Phase::Verifying
    );

    let native_id: String = f
        .store
        .read_connection()
        .query_row(
            "SELECT id FROM local_native_gate_jobs WHERE wave=?1",
            [wave_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    let native_job = native_gates::load(&f.store, &RecordId::parse(&native_id).unwrap()).unwrap();
    assert_eq!(
        native_job.state, "completed",
        "real native gate job remains historical"
    );
    assert_eq!(
        native_job.observation.as_ref().unwrap()["current_authority"],
        true
    );
    let native_roots: i64 = f
        .store
        .read_connection()
        .query_row(
            "SELECT count(*) FROM local_native_gate_roots WHERE job=?1",
            [&native_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(native_roots > 0, "native job roots remain retained");
    let (target_roots, object_roots): (i64, i64) = f
        .store
        .read_connection()
        .query_row(
            "SELECT count(*),coalesce(sum(CASE WHEN kind='object' THEN 1 ELSE 0 END),0) FROM local_target_roots WHERE revision=?1",
            [revision.id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(
        target_roots > 0 && object_roots > 0,
        "candidate and run evidence remain rooted"
    );

    let (budget_rows, consumed_rows, attempts, budget_receipts): (i64, i64, i64, i64) = f
        .store
        .read_connection()
        .query_row(
            "SELECT count(*),sum(CASE WHEN phase='consumed' THEN 1 ELSE 0 END),coalesce(sum(attempts),0),(SELECT count(*) FROM local_budget_receipts WHERE reservation IN (SELECT reservation FROM local_budget_reservations WHERE wave=?1)) FROM local_budget_reservations WHERE wave=?1",
            [wave_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        (budget_rows, consumed_rows, attempts, budget_receipts),
        (3, 3, 3, 3)
    );
    let pending_reservations: i64 = f
        .store
        .read_connection()
        .query_row(
            "SELECT count(*) FROM local_budget_reservations WHERE wave=?1 AND phase IN ('reserved','partly_consumed','completion_pending','consumption_unknown')",
            [wave_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pending_reservations, 0);

    let (completion_rows, qualification_rows, acceptance_receipts): (i64, i64, i64) = f
        .store
        .read_connection()
        .query_row(
            "SELECT (SELECT count(*) FROM local_controller_completions WHERE wave=?1),(SELECT count(*) FROM local_acceptance_qualifications),(SELECT count(*) FROM local_acceptance_receipts)",
            [wave_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (completion_rows, qualification_rows, acceptance_receipts),
        (0, 0, 0)
    );
    let (service_state, service_report): (String, Vec<u8>) = f
        .store
        .read_connection()
        .query_row(
            "SELECT state,report FROM local_controller_services WHERE id=?1",
            [&service.appointment],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(service_state, "held");
    assert_eq!(
        serde_json::from_slice::<Value>(&service_report).unwrap(),
        serde_json::to_value(&service).unwrap()
    );

    let replay = driver::drive(
        &mut f.store,
        &owner,
        &mut broker,
        &carrier,
        &executor,
        &permit,
        &drive,
        &AtomicBool::new(false),
        super::clock().unwrap(),
    );
    assert!(
        matches!(&replay, Err(ControllerError::DispatchHeld(state)) if state == "unknown"),
        "a second native drive must stop at the retained unknown run: {replay:?}"
    );
    assert_eq!(
        publication_operations(&broker, session.id, &intent.spec.commit).len(),
        1
    );
    assert_eq!(
        f.store
            .read_connection()
            .query_row(
                "SELECT count(*) FROM local_controller_runs WHERE wave=?1",
                [wave_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1,
        "replay cannot create another native controller run"
    );
    assert_eq!(
        git(&remote, &["rev-parse", "refs/heads/accepted"]),
        intent.spec.commit,
        "receiver ref remains the exact candidate after replay refusal"
    );

    let evidence = json!({
        "profile": "aethyme.local-controller/actual-publication-outcome-unknown-v1",
        "service": service,
        "operation": operation,
        "native_job": {
            "id": native_job.id,
            "spec": native_job.spec,
            "state": native_job.state,
            "observation": native_job.observation,
        },
        "source_transport": {
            "intent": intent_id,
            "dispatched": dispatched,
            "observation": observation,
            "receipt_present": receipt.is_some(),
            "candidate": intent.spec.commit,
        },
        "receiver_update": receiver_record.trim(),
        "remote_ref": remote_commit,
        "run_state": run_state,
        "native_roots": native_roots,
        "target_roots": target_roots,
        "object_roots": object_roots,
        "budget": {
            "reservations": budget_rows,
            "consumed": consumed_rows,
            "attempts": attempts,
            "receipts": budget_receipts,
            "pending": pending_reservations,
        },
        "private_acceptance": {
            "completions": completion_rows,
            "qualifications": qualification_rows,
            "receipts": acceptance_receipts,
        },
        "replay_refused": true,
        "source_host": source_host,
        "repository": repo,
        "receiver": remote,
        "evidence_root": host,
        "runtime": executor.runtime(),
    });
    std::fs::write(
        host.join("report.json"),
        serde_json::to_vec_pretty(&evidence).unwrap(),
    )
    .unwrap();
    eprintln!(
        "Controller actual unknown-publication report {}",
        host.join("report.json").display()
    );
}
