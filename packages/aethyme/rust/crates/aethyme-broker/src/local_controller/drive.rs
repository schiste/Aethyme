//! One bounded, explicitly appointed Local execution. Persisted dispatch is
//! never retried automatically, including after a lost response or restart.
use super::*;
use crate::collaboration_archive::{CommitOid, ObjectDigest};
use crate::execution_resources::{build_envelope, verify_envelope};
use crate::execution_snapshot::{self as snapshot, ExecutionSpec};
use crate::execution_supervisor::{DockerExecutor, Outcome};
use crate::execution_verifier::{self as verifier, ProducerPermit, VerificationOutcome};
use crate::local_budget::{self as budget, WorkKind};
use crate::local_source_transport as transport;
use crate::native_gates;
use rusqlite::OptionalExtension;
use std::sync::atomic::{AtomicBool, Ordering};

const RUN_SCHEMA: RecordSchema = RecordSchema {
    name: "aethyme.local-controller-run/experimental-v1",
    fields: &[FieldSpec {
        name: "spec",
        required: true,
        kind: FieldKind::Opaque,
        capability: None,
    }],
    capabilities: &[],
};

/// Operator-selected execution profile, never supplied by candidate source.
/// The candidate source field is replaced with the actual frozen wave subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DrivePolicy {
    pub execution: ExecutionSpec,
    pub wave_policy: wave::WavePolicy,
    pub max_wall_ms: i64,
    /// Explicit permission for the repository's no-command native policy.
    pub allow_empty_native_gates: bool,
}
impl DrivePolicy {
    pub(super) fn validate(&self) -> Result<(), ControllerError> {
        if !(1..=900_000).contains(&self.max_wall_ms)
            || self.execution.profile.build_limits.timeout_ms > 60_000
            || self.execution.profile.verifier_limits.timeout_ms > 60_000
            || self.execution.preparation.is_some()
        {
            return Err(ControllerError::Mismatch);
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Dispatching,
    Completed,
    Held,
    Unknown,
}
#[derive(Debug)]
pub enum DriveResult {
    Waiting(wave::Admission),
    Completed {
        completion: CompletionSpec,
        historical: bool,
    },
    Held {
        wave: RecordId,
        detail: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunSpec {
    target: String,
    revision: String,
    epoch: String,
    wave: String,
    policy: DrivePolicy,
    deadline_ms: i64,
}
fn canonical(spec: &RunSpec) -> Result<Record, ControllerError> {
    Record::decode(
        &serde_json::to_vec(&json!({"schema":RUN_SCHEMA.name,"spec":spec}))?,
        &[&RUN_SCHEMA],
    )
    .map_err(|error| ControllerError::Canonical(error.to_string()))
}
fn clock() -> Result<i64, ControllerError> {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| ControllerError::Canonical(e.to_string()))?
            .as_millis(),
    )
    .map_err(|_| ControllerError::Mismatch)
}
fn dispatch_deadline(
    started_ms: i64,
    max_wall_ms: i64,
    appointment_deadline: Option<i64>,
) -> Result<i64, ControllerError> {
    let local = started_ms
        .checked_add(max_wall_ms)
        .ok_or(ControllerError::Mismatch)?;
    let deadline = appointment_deadline.map_or(local, |appointed| local.min(appointed));
    if started_ms >= deadline {
        return Err(ControllerError::Mismatch);
    }
    Ok(deadline)
}
fn admit_before_deadline(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    policy: wave::WavePolicy,
    now_ms: i64,
    deadline_ms: i64,
    cancel: &AtomicBool,
) -> Result<wave::Admission, ControllerError> {
    if clock()? >= deadline_ms || cancel.load(Ordering::SeqCst) {
        return Err(ControllerError::Mismatch);
    }
    let admission = wave::admit(store, owner, policy, now_ms)?;
    // A concurrent writer can delay admission beyond the frozen expiry.
    // Its durable wave is preserved, but no execution journal or effect follows.
    if clock()? >= deadline_ms || cancel.load(Ordering::SeqCst) {
        return Err(ControllerError::Mismatch);
    }
    Ok(admission)
}
fn check(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    spec: &RunSpec,
    cancel: &AtomicBool,
) -> Result<(), ControllerError> {
    let _authority = target::authorize(store, owner)?;
    let current = target::status(store, owner.target())?;
    let active = wave::load(
        store,
        &RecordId::parse(&spec.wave).map_err(|_| ControllerError::Mismatch)?,
    )?;
    if spec.target != owner.target()
        || active.spec.target != owner.target()
        || active.spec.revision != spec.revision
        || cancel.load(Ordering::SeqCst)
        || clock()? >= spec.deadline_ms
        || current.revision.as_ref().map(RecordId::as_str) != Some(spec.revision.as_str())
        || spec.epoch != owner.authority_epoch().as_str()
        || !matches!(
            current.phase,
            Phase::WaveFrozen | Phase::Composing | Phase::Verifying | Phase::WaitingForInput
        )
    {
        return Err(ControllerError::Mismatch);
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
fn reserve(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    wave: &RecordId,
    recipe: &RecordId,
    label: &str,
    units: budget::Units,
    kind: WorkKind,
    deadline: i64,
) -> Result<budget::Attempt, ControllerError> {
    let nonce = ObjectDigest::of(
        format!(
            "controller:{wave}:{recipe}:{label}:{}",
            owner.authority_epoch().as_str()
        )
        .as_bytes(),
    );
    let r = budget::reserve(
        store,
        owner,
        wave,
        budget::Request {
            nonce: format!("sha256:{}", nonce.hex()),
            recipe: recipe.to_string(),
            kind,
            units,
            max_attempts: 1,
            deadline_ms: deadline,
        },
        clock()?,
    )?;
    Ok(budget::begin_attempt(store, owner, &r.id, clock()?)?)
}
fn close(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    attempt: &budget::Attempt,
) -> Result<(), ControllerError> {
    let r = budget::load(store, attempt.reservation())?;
    budget::close(store, owner, &r.id, r.version)?;
    Ok(())
}

/// Consume arrivals admitted to the current immutable target revision and
/// execute one wave using existing gate, build, verifier and provider adapters.
/// No callbacks or public receipt can supply the verification/acceptance proof.
/// A dispatch journal is written before composition; any interruption remains
/// unknown and holds its reservations rather than rerunning the workload.
#[allow(clippy::too_many_arguments)]
pub fn drive(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    broker: &mut Broker,
    carrier: &GitCarrier,
    executor: &DockerExecutor,
    permit: &ProducerPermit,
    policy: &DrivePolicy,
    cancel: &AtomicBool,
    now_ms: i64,
) -> Result<DriveResult, ControllerError> {
    drive_until(
        store, owner, broker, carrier, executor, permit, policy, cancel, now_ms, None, None,
    )
}

/// The service supplies its frozen absolute expiry; admission cannot extend it.
#[allow(clippy::too_many_arguments)]
pub(super) fn drive_until(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    broker: &mut Broker,
    carrier: &GitCarrier,
    executor: &DockerExecutor,
    permit: &ProducerPermit,
    policy: &DrivePolicy,
    cancel: &AtomicBool,
    now_ms: i64,
    appointment_deadline: Option<i64>,
    destination: Option<&super::bare_destination::BareDestination>,
) -> Result<DriveResult, ControllerError> {
    policy.validate()?;
    let deadline_ms = dispatch_deadline(clock()?, policy.max_wall_ms, appointment_deadline)?;
    if let Some(destination) = destination {
        carrier.check()?;
        destination.revalidate()?;
    }
    let _authority = target::authorize(store, owner)?;
    if cancel.load(Ordering::SeqCst) || clock()? >= deadline_ms {
        return Err(ControllerError::Mismatch);
    }
    let admission = admit_before_deadline(
        store,
        owner,
        policy.wave_policy,
        now_ms,
        deadline_ms,
        cancel,
    )?;
    let active = match admission {
        wave::Admission::Frozen(w) | wave::Admission::Existing(w) => w,
        other => return Ok(DriveResult::Waiting(other)),
    };
    let revision = RecordId::parse(&active.spec.revision).map_err(|_| ControllerError::Mismatch)?;
    let pinned = target::load_revision(store, &revision)?;
    if permit.policy().record()?.id().as_str() != pinned.spec.acceptance_policy
        || policy.execution.harness != permit.policy().harness
        || policy.execution.assertion_inventory != permit.policy().inventory
        || policy.execution.verify != permit.policy().command
        || policy.execution.profile.runtime_profile != executor.runtime().id()
        || policy.execution.profile.verifier_image != permit.policy().image
    {
        return Err(ControllerError::Mismatch);
    }
    let existing: Option<(String, Vec<u8>)> = store
        .read_connection()
        .query_row(
            "SELECT state,body FROM local_controller_runs WHERE wave=?1",
            [active.id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((state, body)) = existing {
        let record = Record::decode(&body, &[&RUN_SCHEMA])
            .map_err(|e| ControllerError::Canonical(e.to_string()))?;
        let old: RunSpec = serde_json::from_value(
            serde_json::from_slice::<serde_json::Value>(&body)?["spec"].clone(),
        )?;
        if record.canonical_bytes() != body
            || old.wave != active.id.as_str()
            || old.policy != *policy
        {
            return Err(ControllerError::Mismatch);
        }
        return Err(ControllerError::DispatchHeld(state));
    }
    let spec = RunSpec {
        target: owner.target().into(),
        revision: revision.to_string(),
        epoch: owner.authority_epoch().as_str().into(),
        wave: active.id.to_string(),
        policy: policy.clone(),
        deadline_ms,
    };
    let record = canonical(&spec)?;
    let _archive = archive_use(store)?;
    let object = archive::put_object(store, &record.canonical_bytes())?;
    target::retain_artifacts(store, owner, &revision, &[], &[object])?;
    store.connection().execute("INSERT INTO local_controller_runs(wave,id,epoch,state,body) VALUES(?1,?2,?3,'dispatching',?4)",params![active.id.as_str(),record.id().as_str(),spec.epoch,record.canonical_bytes()])?;
    let result = run(
        store,
        owner,
        broker,
        carrier,
        executor,
        permit,
        &spec,
        &active.id,
        cancel,
        destination,
    );
    let (state, detail) = match &result {
        Ok(DriveResult::Completed { completion, .. }) => {
            ("completed", serde_json::to_vec(completion)?)
        }
        Ok(DriveResult::Held { detail, .. }) => {
            ("held", serde_json::to_vec(&json!({"detail":detail}))?)
        }
        _ => (
            "unknown",
            serde_json::to_vec(&json!({"error":result.as_ref().err().map(ToString::to_string)}))?,
        ),
    };
    if store.connection().execute("UPDATE local_controller_runs SET state=?2,observation=?3 WHERE wave=?1 AND state='dispatching'",params![active.id.as_str(),state,detail])?!=1 {return Err(ControllerError::Mismatch);}
    result
}
fn require_fresh_native(job: &native_gates::Job) -> Result<(), ControllerError> {
    if job.state != "prepared" {
        return Err(ControllerError::DispatchHeld(format!(
            "native job {}",
            job.state
        )));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    broker: &mut Broker,
    carrier: &GitCarrier,
    executor: &DockerExecutor,
    permit: &ProducerPermit,
    spec: &RunSpec,
    wave_id: &RecordId,
    cancel: &AtomicBool,
    destination: Option<&super::bare_destination::BareDestination>,
) -> Result<DriveResult, ControllerError> {
    check(store, owner, spec, cancel)?;
    // Reserve the trusted native/control allowance before any external work.
    // This lane charges the whole conservative envelope, not measured usage.
    let before = wave::load(store, wave_id)?;
    let control_units = budget::Units {
        time_ms: spec.policy.max_wall_ms,
        memory_bytes: 512 * 1024 * 1024,
        disk_bytes: 768 * 1024 * 1024,
        slots: 1,
        ..Default::default()
    };
    let control = reserve(
        store,
        owner,
        wave_id,
        &canonical(spec)?.id(),
        "control",
        control_units,
        if before.phase == WavePhase::Frozen {
            WorkKind::Composition
        } else {
            WorkKind::Verification
        },
        spec.deadline_ms,
    )?;
    let active = wave::compose(store, owner, broker.repo_handle().root(), wave_id)?;
    if active.phase != WavePhase::Verifying {
        budget::settle(
            store,
            owner,
            &control,
            control_units,
            &canonical(spec)?.id(),
        )?;
        close(store, owner, &control)?;
        return Ok(DriveResult::Held {
            wave: wave_id.clone(),
            detail: format!("composition {:?}", active.phase),
        });
    }
    let held = target::status(store, owner.target())?;
    if held.phase == Phase::WaitingForInput && held.resume_phase == Some(Phase::Verifying) {
        target::resume(store, owner, held.version)?;
    }
    check(store, owner, spec, cancel)?;
    let native = native_gates::prepare(store, owner, broker, wave_id)?;
    require_fresh_native(&native)?;
    // Graph regeneration does not have this profile's process deadline.
    // Refuse that optional configuration before dispatching the native job.
    if native.spec.graph_config_sha256.is_some() {
        return Err(ControllerError::Mismatch);
    }
    let native = native_gates::run_fresh(store, owner, broker, &native.id)?;
    budget::settle(
        store,
        owner,
        &control,
        control_units,
        &canonical(spec)?.id(),
    )?;
    close(store, owner, &control)?;
    let observation = native.observation.ok_or(ControllerError::Mismatch)?;
    let status = observation["status"]
        .as_str()
        .ok_or(ControllerError::Mismatch)?;
    if observation["current_authority"] != true
        || !(status == "passed"
            || (spec.policy.allow_empty_native_gates
                && matches!(status, "no_configuration" | "no_gates_triggered")))
    {
        return Ok(DriveResult::Held {
            wave: wave_id.clone(),
            detail: format!("native gates {status}"),
        });
    }
    check(store, owner, spec, cancel)?;
    let mut execution_spec = spec.policy.execution.clone();
    execution_spec.candidate_source = active
        .candidate_snapshot
        .as_ref()
        .ok_or(ControllerError::Mismatch)?
        .to_string();
    let revision = RecordId::parse(&spec.revision).map_err(|_| ControllerError::Mismatch)?;
    let execution = snapshot::freeze(store, owner, &revision, execution_spec)?;
    let build_attempt = reserve(
        store,
        owner,
        wave_id,
        execution.id(),
        "build",
        build_envelope(&execution)?.units()?,
        WorkKind::Verification,
        spec.deadline_ms,
    )?;
    let built = executor.build(store, owner, &execution, &build_attempt, cancel)?;
    close(store, owner, &build_attempt)?;
    if built.outcome() != Outcome::Built {
        return Ok(DriveResult::Held {
            wave: wave_id.clone(),
            detail: format!("build {:?}", built.outcome()),
        });
    }
    let artifact = built.into_artifact().ok_or(ControllerError::Mismatch)?;
    check(store, owner, spec, cancel)?;
    let verify_attempt = reserve(
        store,
        owner,
        wave_id,
        execution.id(),
        "verification",
        verify_envelope(&execution)?.units()?,
        WorkKind::Verification,
        spec.deadline_ms,
    )?;
    let evidence = verifier::verify(
        store,
        owner,
        &execution,
        &artifact,
        permit,
        executor,
        &verify_attempt,
        cancel,
    )?;
    close(store, owner, &verify_attempt)?;
    if evidence.outcome() != VerificationOutcome::Passed {
        return Ok(DriveResult::Held {
            wave: wave_id.clone(),
            detail: format!("verification {:?}", evidence.outcome()),
        });
    }
    check(store, owner, spec, cancel)?;
    let expected = CommitOid::parse(&native.spec.base_commit)?;
    let nonce = ObjectDigest::of(
        format!("controller-publish:{}:{}", wave_id, canonical(spec)?.id()).as_bytes(),
    )
    .hex();
    let intent = transport::prepare(store, owner, carrier, wave_id, &nonce, Some(&expected))?;
    let binding = acceptance::appoint(
        store,
        owner,
        broker,
        carrier,
        &intent.id,
        acceptance::ProviderRoute::GenericGit,
        &carrier.destination,
        64,
    )?;
    check(store, owner, spec, cancel)?;
    if let Some(destination) = destination {
        carrier.check()?;
        destination.revalidate()?;
        check(store, owner, spec, cancel)?;
    }
    transport::publish(store, owner, carrier, broker, &intent.id)?;
    check(store, owner, spec, cancel)?;
    let grant = acceptance::qualify(
        store,
        owner,
        broker,
        carrier,
        binding.id(),
        &execution,
        &artifact,
        permit,
        &evidence,
    )?;
    check(store, owner, spec, cancel)?;
    let completion = complete_inner(
        store,
        owner,
        broker,
        carrier,
        wave_id,
        &grant,
        Some((spec.deadline_ms, cancel)),
    )?;
    Ok(DriveResult::Completed {
        completion,
        historical: false,
    })
}

#[cfg(all(test, unix))]
#[path = "drive_tests.rs"]
pub(crate) mod tests;
