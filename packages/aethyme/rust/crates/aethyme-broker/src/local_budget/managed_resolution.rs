//! The managed Local resolution route. Only code that has already validated
//! the current conflict source may construct `ValidatedConflict`; that value
//! is consumed by one Immediate transaction which writes the shared charge,
//! root reservation, retained inputs, proof, roots, and attempt binding.

#[cfg(test)]
use std::cell::Cell;
use std::collections::BTreeSet;
use std::path::Path;

use aethyme_contracts::experimental_v0::RecordId;
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{
    BudgetError, ManagedResolutionCommand, ManagedResolutionOutcome, ManagedResolutionStatus,
    Request, ReservationSpec, State, Units, WorkKind, charge, children, decode, guard, insert,
    objective, project, reject_overrun, rows, sum,
};
use crate::collaboration_archive::{self as archive, ObjectDigest};
use crate::collaboration_capture::OperationId;
use crate::collaboration_gc;
use crate::collaboration_state::CollaborationStore;
use crate::local_target::{self, ControllerOwner};
use crate::local_wave::{self, Wave, WavePhase};
use crate::resolution::{self, ResolutionRequest, Trigger};

const MANAGED_ROUTE: &str = "local_managed_resolution_v1";

/// Data assembled only after the typed current conflict reader validates the
/// wave, request, source wrapper, and frozen plan. Selected roots are derived
/// from that archived plan; policies are read again in the admission transaction.
pub(super) struct ValidatedConflict<'a> {
    pub command: &'a ManagedResolutionCommand,
    pub wave: &'a Wave,
    pub request: &'a ResolutionRequest,
    pub trigger_sha256: &'a str,
    pub selected_roots: &'a BTreeSet<RecordId>,
    pub input_manifest: ObjectDigest,
    pub now_ms: i64,
}

/// An opaque single-use identity produced only after the atomic admission
/// transaction commits. It does not represent measured usage or settlement.
#[derive(Debug)]
struct ResolutionDispatchPermit {
    reservation: RecordId,
    binding_id: RecordId,
    attempt_id: i64,
    async_token: crate::collaboration_capture::OperationId,
    reservation_version: i64,
    shared_attempt: u32,
    owner_target: String,
    owner_issuer: String,
    authority_epoch: crate::local_authority::Epoch,
    wave: RecordId,
    expected_version: i64,
    request: resolution::RecordRef,
    source: resolution::RecordRef,
    manifest: ObjectDigest,
    limits: resolution::DispatchLimits,
    deadline_ms: i64,
    roots: BTreeSet<RecordId>,
}

type AdapterAcknowledge = fn(&ResolutionDispatchPermit) -> Result<(), BudgetError>;

#[cfg(test)]
thread_local! {
    static TEST_ADAPTER: Cell<Option<AdapterAcknowledge>> = const { Cell::new(None) };
    static TEST_AFTER_ADMISSION: Cell<Option<fn(&mut CollaborationStore)>> = const { Cell::new(None) };
}

/// This production build deliberately has no registered paid or external
/// adapter. The function pointer is private so callers cannot supply code.
fn registered_adapter() -> Option<AdapterAcknowledge> {
    #[cfg(test)]
    if let Some(adapter) = TEST_ADAPTER.with(Cell::get) {
        return Some(adapter);
    }
    None
}

#[cfg(test)]
struct TestAdapterRegistration(Option<AdapterAcknowledge>);

#[cfg(test)]
impl Drop for TestAdapterRegistration {
    fn drop(&mut self) {
        TEST_ADAPTER.with(|slot| slot.set(self.0));
    }
}

#[cfg(test)]
fn register_test_adapter(adapter: AdapterAcknowledge) -> TestAdapterRegistration {
    let previous = TEST_ADAPTER.with(|slot| slot.replace(Some(adapter)));
    TestAdapterRegistration(previous)
}

#[cfg(test)]
struct TestAdmissionMutation(Option<fn(&mut CollaborationStore)>);

#[cfg(test)]
impl Drop for TestAdmissionMutation {
    fn drop(&mut self) {
        TEST_AFTER_ADMISSION.with(|slot| slot.set(self.0));
    }
}

#[cfg(test)]
fn register_after_admission(mutation: fn(&mut CollaborationStore)) -> TestAdmissionMutation {
    let previous = TEST_AFTER_ADMISSION.with(|slot| slot.replace(Some(mutation)));
    TestAdmissionMutation(previous)
}

/// Resolve an existing key before checking current ownership or policy. New
/// commands validate the exact archived conflict and hold archive, response,
/// and active-authority barriers through admission and private acknowledgement.
pub(super) fn begin(
    store: &mut CollaborationStore,
    repo: &Path,
    owner: &ControllerOwner,
    command: &ManagedResolutionCommand,
) -> Result<ManagedResolutionOutcome, BudgetError> {
    let initial_now_ms = wall_clock_ms()?;
    let _archive_guard = collaboration_gc::archive_use(store)
        .map_err(|error| BudgetError::Invalid(format!("archive use: {error}")))?;
    let _response_guard = crate::collaboration_sensitive::response_use(store)?;

    if let Some(outcome) = existing_key_status(store, repo, owner, command)? {
        return Ok(outcome);
    }

    let authority_guard = local_target::authorize(store, owner)?;
    let binding = local_wave::current_conflict_request_binding(
        store,
        repo,
        owner,
        &command.wave,
        command.expected_version,
        &command.request,
    )?;
    if binding.request != command.request || binding.source != command.source {
        return Err(BudgetError::Immutable);
    }
    let wave = local_wave::load(store, &command.wave)?;
    let request = resolution::load_request(store, repo, &command.request)?;
    let trigger_sha256 = conflict_trigger_digest(request.trigger())?;
    let selected_roots = selected_roots(&binding.plan_payload.order)?;
    if selected_roots.is_empty()
        || request.record() != &command.request
        || !matches!(request.trigger(), Trigger::Conflict(conflicts) if !conflicts.is_empty())
        || binding.plan_payload.wave != wave.id.as_str()
        || binding.plan_payload.revision != wave.spec.revision
        || binding.plan_payload.selected != wave.spec.selected
    {
        return Err(BudgetError::Stale);
    }
    command.envelope.validate()?;
    command.limits.validate()?;
    if initial_now_ms >= command.deadline_ms
        || command.deadline_ms.saturating_sub(initial_now_ms) > 86_400_000
        || command.envelope.zero()
    {
        return Err(BudgetError::Invalid("managed resolution bounds".into()));
    }
    let (manifest_bytes, input_manifest) =
        resolution::build_input_manifest(store, &request, command.limits)?;
    let manifest_object = ObjectDigest::of(&manifest_bytes);
    if input_manifest.sha256 != manifest_object.hex() {
        return Err(BudgetError::Stale);
    }

    let Some(adapter) = registered_adapter() else {
        return Ok(ManagedResolutionOutcome::AdapterUnavailable);
    };

    let now_ms = wall_clock_ms()?;
    if now_ms >= command.deadline_ms || command.deadline_ms.saturating_sub(now_ms) > 86_400_000 {
        return Err(BudgetError::Invalid("managed resolution deadline".into()));
    }
    let stored_manifest = archive::put_object(store, &manifest_bytes)
        .map_err(|error| BudgetError::Invalid(format!("archive object: {error}")))?;
    if stored_manifest != manifest_object {
        return Err(BudgetError::Stale);
    }
    let validated = ValidatedConflict {
        command,
        wave: &wave,
        request: &request,
        trigger_sha256: &trigger_sha256,
        selected_roots: &selected_roots,
        input_manifest: manifest_object,
        now_ms,
    };
    let permit = match admit_new(store, owner, validated) {
        Ok(permit) => permit,
        Err(BudgetError::Stale) => {
            if let Some(outcome) = existing_key_status(store, repo, owner, command)? {
                return Ok(outcome);
            }
            return Err(BudgetError::Stale);
        }
        Err(error) => return Err(error),
    };
    #[cfg(test)]
    if let Some(mutation) = TEST_AFTER_ADMISSION.with(Cell::get) {
        mutation(store);
    }
    dispatch_acknowledgement(
        store,
        owner,
        &authority_guard,
        command,
        &wave,
        &request,
        &trigger_sha256,
        &selected_roots,
        permit,
        adapter,
    )?;
    let status = existing_key_status(store, repo, owner, command)?
        .and_then(|outcome| match outcome {
            ManagedResolutionOutcome::StatusOnly(status) => Some(status),
            _ => None,
        })
        .ok_or(BudgetError::Stale)?;
    Ok(ManagedResolutionOutcome::DispatchAcknowledged(status))
}

struct ReservationProofRow {
    reservation: String,
    target: String,
    issuer: String,
    authority_epoch: String,
    client_key: String,
    wave: String,
    expected_version: i64,
    request_record_id: String,
    request_object_sha256: String,
    source_record_id: String,
    source_object_sha256: String,
    input_manifest_sha256: String,
    trigger_sha256: String,
    project_policy: String,
    objective_policy: String,
    price_profile: String,
    dispatch_limits: String,
    reserved_envelope: Vec<u8>,
    reservation_attempt_limit: u32,
    deadline_ms: i64,
    created_ms: i64,
}

fn existing_key_status(
    store: &mut CollaborationStore,
    repo: &Path,
    owner: &ControllerOwner,
    command: &ManagedResolutionCommand,
) -> Result<Option<ManagedResolutionOutcome>, BudgetError> {
    let proof_row = store
        .read_connection()
        .query_row(
            "SELECT reservation,target,issuer,authority_epoch,client_key,wave,
                    expected_version,request_record_id,request_object_sha256,
                    composition_source_record_id,composition_source_object_sha256,
                    input_manifest_object_sha256,trigger_sha256,project_policy,
                    objective_policy,price_profile,dispatch_limits,reserved_envelope,
                    reservation_attempt_limit,deadline_ms,created_ms
             FROM local_resolution_reservation_proofs
             WHERE target=?1 AND issuer=?2 AND client_key=?3",
            params![owner.target(), owner.issuer(), command.client_key.as_str()],
            |row| {
                Ok(ReservationProofRow {
                    reservation: row.get(0)?,
                    target: row.get(1)?,
                    issuer: row.get(2)?,
                    authority_epoch: row.get(3)?,
                    client_key: row.get(4)?,
                    wave: row.get(5)?,
                    expected_version: row.get(6)?,
                    request_record_id: row.get(7)?,
                    request_object_sha256: row.get(8)?,
                    source_record_id: row.get(9)?,
                    source_object_sha256: row.get(10)?,
                    input_manifest_sha256: row.get(11)?,
                    trigger_sha256: row.get(12)?,
                    project_policy: row.get(13)?,
                    objective_policy: row.get(14)?,
                    price_profile: row.get(15)?,
                    dispatch_limits: row.get(16)?,
                    reserved_envelope: row.get(17)?,
                    reservation_attempt_limit: row.get(18)?,
                    deadline_ms: row.get(19)?,
                    created_ms: row.get(20)?,
                })
            },
        )
        .optional();
    let proof = match proof_row {
        Ok(Some(proof)) => proof,
        Ok(None) => {
            return if orphaned_managed_key(store, owner, &command.client_key)? {
                Ok(Some(ManagedResolutionOutcome::StaleUnknown))
            } else {
                Ok(None)
            };
        }
        Err(error) if corrupt_row_encoding(&error) => {
            return Ok(Some(ManagedResolutionOutcome::StaleUnknown));
        }
        Err(error) => return Err(error.into()),
    };
    let retained_command = match command_from_proof(&proof) {
        Ok(command) => command,
        Err(_) => return Ok(Some(ManagedResolutionOutcome::StaleUnknown)),
    };
    let status = match retained_status(store, repo, owner, &retained_command, &proof) {
        Ok(status) => status,
        Err(_) => return Ok(Some(ManagedResolutionOutcome::StaleUnknown)),
    };
    if !same_command_identity(command, &retained_command) {
        return Err(BudgetError::Immutable);
    }
    Ok(Some(ManagedResolutionOutcome::StatusOnly(status)))
}

fn orphaned_managed_key(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    key: &OperationId,
) -> Result<bool, BudgetError> {
    let nonce = reservation_nonce_for(owner.target(), owner.issuer(), key);
    Ok(store.read_connection().query_row(
        "SELECT EXISTS(
             SELECT 1 FROM local_budget_reservations WHERE nonce=?1
         ) OR EXISTS(
             SELECT 1 FROM resolution_attempts a
             JOIN local_waves w
               ON w.resolution_request_record_id=a.request_record_id
              AND w.resolution_request_object=a.request_sha256
             WHERE a.client_key=?2 AND a.resolver='local-managed-resolution'
               AND w.target=?3
         )",
        params![nonce, key.as_str(), owner.target()],
        |row| row.get(0),
    )?)
}

fn corrupt_row_encoding(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::FromSqlConversionFailure(..)
            | rusqlite::Error::IntegralValueOutOfRange(..)
            | rusqlite::Error::InvalidColumnType(..)
    )
}

/// Reconstruct the full immutable identity from the retained proof before
/// classifying a caller's same-key reuse as a conflict. A malformed proof is
/// stale/unknown; it must never turn a corrupt binding into a fresh permit.
fn command_from_proof(
    proof: &ReservationProofRow,
) -> Result<ManagedResolutionCommand, BudgetError> {
    validate_digest(&proof.request_object_sha256)?;
    validate_digest(&proof.source_object_sha256)?;
    validate_digest(&proof.input_manifest_sha256)?;
    validate_digest(&proof.trigger_sha256)?;
    if proof.reservation_attempt_limit != 1
        || proof.expected_version < 0
        || proof.created_ms < 0
        || proof.deadline_ms <= proof.created_ms
        || proof.deadline_ms.saturating_sub(proof.created_ms) > 86_400_000
    {
        return Err(BudgetError::Stale);
    }
    RecordId::parse(&proof.reservation).map_err(|_| BudgetError::Stale)?;
    RecordId::parse(&proof.authority_epoch).map_err(|_| BudgetError::Stale)?;
    RecordId::parse(&proof.project_policy).map_err(|_| BudgetError::Stale)?;
    RecordId::parse(&proof.objective_policy).map_err(|_| BudgetError::Stale)?;
    RecordId::parse(&proof.price_profile).map_err(|_| BudgetError::Stale)?;
    let client_key = OperationId::parse(&proof.client_key).map_err(|_| BudgetError::Stale)?;
    let wave = RecordId::parse(&proof.wave).map_err(|_| BudgetError::Stale)?;
    let request = resolution::RecordRef {
        id: RecordId::parse(&proof.request_record_id).map_err(|_| BudgetError::Stale)?,
        object: archive::parse_digest(&proof.request_object_sha256).ok_or(BudgetError::Stale)?,
    };
    let source = resolution::RecordRef {
        id: RecordId::parse(&proof.source_record_id).map_err(|_| BudgetError::Stale)?,
        object: archive::parse_digest(&proof.source_object_sha256).ok_or(BudgetError::Stale)?,
    };
    let envelope: Units = serde_json::from_slice(&proof.reserved_envelope)?;
    envelope.validate()?;
    if serde_json::to_vec(&envelope)? != proof.reserved_envelope {
        return Err(BudgetError::Stale);
    }
    let limits = resolution::DispatchLimits::from_canonical_ledger_value(&proof.dispatch_limits)?;
    limits.validate()?;
    Ok(ManagedResolutionCommand {
        wave,
        expected_version: proof.expected_version,
        request,
        source,
        client_key,
        envelope,
        limits,
        deadline_ms: proof.deadline_ms,
    })
}

fn same_command_identity(
    left: &ManagedResolutionCommand,
    right: &ManagedResolutionCommand,
) -> bool {
    left.wave == right.wave
        && left.expected_version == right.expected_version
        && left.request == right.request
        && left.source == right.source
        && left.client_key == right.client_key
        && left.envelope == right.envelope
        && left.limits == right.limits
        && left.deadline_ms == right.deadline_ms
}

fn proof_for_reservation(
    connection: &rusqlite::Connection,
    reservation: &RecordId,
) -> Result<ReservationProofRow, BudgetError> {
    Ok(connection.query_row(
        "SELECT reservation,target,issuer,authority_epoch,client_key,wave,
                expected_version,request_record_id,request_object_sha256,
                composition_source_record_id,composition_source_object_sha256,
                input_manifest_object_sha256,trigger_sha256,project_policy,
                objective_policy,price_profile,dispatch_limits,reserved_envelope,
                reservation_attempt_limit,deadline_ms,created_ms
         FROM local_resolution_reservation_proofs WHERE reservation=?1",
        [reservation.as_str()],
        |row| {
            Ok(ReservationProofRow {
                reservation: row.get(0)?,
                target: row.get(1)?,
                issuer: row.get(2)?,
                authority_epoch: row.get(3)?,
                client_key: row.get(4)?,
                wave: row.get(5)?,
                expected_version: row.get(6)?,
                request_record_id: row.get(7)?,
                request_object_sha256: row.get(8)?,
                source_record_id: row.get(9)?,
                source_object_sha256: row.get(10)?,
                input_manifest_sha256: row.get(11)?,
                trigger_sha256: row.get(12)?,
                project_policy: row.get(13)?,
                objective_policy: row.get(14)?,
                price_profile: row.get(15)?,
                dispatch_limits: row.get(16)?,
                reserved_envelope: row.get(17)?,
                reservation_attempt_limit: row.get(18)?,
                deadline_ms: row.get(19)?,
                created_ms: row.get(20)?,
            })
        },
    )?)
}

fn retained_status(
    store: &mut CollaborationStore,
    repo: &Path,
    owner: &ControllerOwner,
    command: &ManagedResolutionCommand,
    proof: &ReservationProofRow,
) -> Result<ManagedResolutionStatus, BudgetError> {
    validate_digest(&proof.request_object_sha256)?;
    validate_digest(&proof.source_object_sha256)?;
    validate_digest(&proof.input_manifest_sha256)?;
    validate_digest(&proof.trigger_sha256)?;
    if proof.target != owner.target()
        || proof.issuer != owner.issuer()
        || proof.client_key != command.client_key.as_str()
        || proof.reservation_attempt_limit != 1
        || proof.deadline_ms < 0
        || proof.created_ms < 0
    {
        return Err(BudgetError::Stale);
    }
    RecordId::parse(&proof.authority_epoch).map_err(|_| BudgetError::Stale)?;
    let reservation_id = RecordId::parse(&proof.reservation).map_err(|_| BudgetError::Stale)?;
    let request_ref = resolution::RecordRef {
        id: RecordId::parse(&proof.request_record_id).map_err(|_| BudgetError::Stale)?,
        object: archive::parse_digest(&proof.request_object_sha256).ok_or(BudgetError::Stale)?,
    };
    let source_ref = resolution::RecordRef {
        id: RecordId::parse(&proof.source_record_id).map_err(|_| BudgetError::Stale)?,
        object: archive::parse_digest(&proof.source_object_sha256).ok_or(BudgetError::Stale)?,
    };
    RecordId::parse(&proof.project_policy).map_err(|_| BudgetError::Stale)?;
    RecordId::parse(&proof.objective_policy).map_err(|_| BudgetError::Stale)?;
    RecordId::parse(&proof.price_profile).map_err(|_| BudgetError::Stale)?;
    let limits = resolution::DispatchLimits::from_canonical_ledger_value(&proof.dispatch_limits)?;
    let envelope: Units = serde_json::from_slice(&proof.reserved_envelope)?;
    envelope.validate()?;
    if limits != command.limits || envelope != command.envelope {
        return Err(BudgetError::Stale);
    }

    let binding =
        local_wave::read_retained_conflict_request_binding(store, repo, &request_ref, &source_ref)?;
    if binding.request != request_ref
        || binding.source != source_ref
        || binding.plan_payload.wave != proof.wave
    {
        return Err(BudgetError::Stale);
    }
    let revision =
        RecordId::parse(&binding.plan_payload.revision).map_err(|_| BudgetError::Stale)?;
    if revision.as_str() != binding.plan_payload.revision {
        return Err(BudgetError::Stale);
    }
    let wave = local_wave::load(
        store,
        &RecordId::parse(&proof.wave).map_err(|_| BudgetError::Stale)?,
    )?;
    if wave.id.as_str() != proof.wave
        || wave.spec.target != proof.target
        || wave.spec.issuer != proof.issuer
        || wave.spec.revision != revision.as_str()
        || wave.spec.selected != binding.plan_payload.selected
        || wave.spec.policy != binding.plan_payload.policy
        || wave.spec.prior_completion != binding.plan_payload.prior_completion
        || wave.resolution_request.as_ref() != Some(&request_ref)
        || wave.composition_source.as_ref() != Some(&source_ref)
    {
        return Err(BudgetError::Stale);
    }
    let roots = selected_roots(&binding.plan_payload.order)?;
    let mut root_statement = store.read_connection().prepare(
        "SELECT lineage_record_id FROM local_resolution_reservation_roots
         WHERE reservation=?1 ORDER BY lineage_record_id",
    )?;
    let root_rows = root_statement
        .query_map([reservation_id.as_str()], |row| row.get::<_, String>(0))?
        .collect::<Result<BTreeSet<_>, _>>()?;
    drop(root_statement);
    let stored_roots = root_rows
        .iter()
        .map(|root| RecordId::parse(root).map_err(|_| BudgetError::Stale))
        .collect::<Result<BTreeSet<_>, _>>()?;
    if roots.is_empty() || stored_roots != roots {
        return Err(BudgetError::Stale);
    }

    let request = resolution::load_request(store, repo, &request_ref)?;
    if request.record() != &request_ref
        || !matches!(request.trigger(), Trigger::Conflict(conflicts) if !conflicts.is_empty())
        || conflict_trigger_digest(request.trigger())? != proof.trigger_sha256
    {
        return Err(BudgetError::Stale);
    }
    let (manifest_bytes, manifest) = resolution::build_input_manifest(store, &request, limits)?;
    if manifest.sha256 != proof.input_manifest_sha256
        || ObjectDigest::of(&manifest_bytes).hex() != proof.input_manifest_sha256
    {
        return Err(BudgetError::Stale);
    }
    let manifest_digest =
        archive::parse_digest(&proof.input_manifest_sha256).ok_or(BudgetError::Stale)?;
    let stored_manifest = archive::read_object_bounded(
        store,
        &manifest_digest,
        usize::try_from(limits.max_manifest_bytes).map_err(|_| BudgetError::Stale)?,
    )
    .map_err(|error| BudgetError::Invalid(format!("read retained input manifest: {error}")))?;
    if stored_manifest != manifest_bytes {
        return Err(BudgetError::Stale);
    }

    let reservation = load_retained_reservation(
        store.read_connection(),
        &reservation_id,
        proof,
        command,
        &revision,
    )?;
    let binding = load_retained_attempt_binding(
        store.read_connection(),
        &reservation_id,
        proof,
        command,
        &request,
    )?;
    let state = reservation.state;
    if !matches!(state, State::CompletionPending | State::ConsumptionUnknown)
        || reservation.attempts != 1
        || reservation.version < binding.reservation_version_at_dispatch
        || reservation.objective != wave.spec.objective_authorization
    {
        return Err(BudgetError::Stale);
    }
    Ok(ManagedResolutionStatus {
        reservation: reservation_id.as_str().into(),
        phase: state,
        reservation_attempt: 1,
        reservation_version: reservation.version,
        shared_attempt: binding.shared_attempt,
        async_state: binding.async_state,
        outcome: None,
    })
}

struct RetainedAttemptBinding {
    binding_id: String,
    attempt_id: i64,
    authority_epoch: String,
    reservation_version_at_dispatch: i64,
    shared_attempt: u32,
    async_token: String,
    async_state: String,
}

struct RetainedReservation {
    objective: String,
    state: State,
    attempts: u32,
    version: i64,
}

type RetainedReservationRow = (
    Vec<u8>,
    String,
    u32,
    i64,
    String,
    Option<String>,
    String,
    String,
    String,
    String,
    Vec<u8>,
    Option<String>,
);

type CurrentConflictWaveRow = (
    String,
    String,
    i64,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn load_retained_reservation(
    connection: &rusqlite::Connection,
    reservation: &RecordId,
    proof: &ReservationProofRow,
    command: &ManagedResolutionCommand,
    revision: &RecordId,
) -> Result<RetainedReservation, BudgetError> {
    let row: RetainedReservationRow = connection.query_row(
        "SELECT body,phase,attempts,version,root,parent,target,revision,wave,nonce,spent,receipt
         FROM local_budget_reservations WHERE reservation=?1",
        [reservation.as_str()],
        |row| {
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
            ))
        },
    )?;
    let spec: ReservationSpec = decode(reservation.as_str(), &row.0)?;
    let encoded = super::record(&spec)?;
    let spent: Units = serde_json::from_slice(&row.10)?;
    spent.validate()?;
    let state = parse_state(&row.1)?;
    let expected_nonce = reservation_nonce_for(&proof.target, &proof.issuer, &command.client_key);
    RecordId::parse(&spec.objective).map_err(|_| BudgetError::Stale)?;
    if &encoded.id() != reservation
        || spec.issuer != proof.issuer
        || spec.target != proof.target
        || spec.revision != revision.as_str()
        || spec.wave != command.wave.as_str()
        || spec.project_policy != proof.project_policy
        || spec.objective_policy != proof.objective_policy
        || spec.parent.is_some()
        || spec.request.kind != WorkKind::Resolution
        || spec.request.nonce != expected_nonce
        || spec.request.recipe != command.request.id.as_str()
        || spec.request.units != command.envelope
        || spec.request.max_attempts != 1
        || spec.request.deadline_ms != command.deadline_ms
        || row.2 != 1
        || row.3 < 1
        || row.4 != reservation.as_str()
        || row.5.is_some()
        || row.6 != proof.target
        || row.7 != revision.as_str()
        || row.8 != command.wave.as_str()
        || row.9 != expected_nonce
        || !spent.zero()
        || row.11.is_some()
    {
        return Err(BudgetError::Stale);
    }
    Ok(RetainedReservation {
        objective: spec.objective,
        state,
        attempts: row.2,
        version: row.3,
    })
}

fn load_retained_attempt_binding(
    connection: &rusqlite::Connection,
    reservation: &RecordId,
    proof: &ReservationProofRow,
    command: &ManagedResolutionCommand,
    request: &ResolutionRequest,
) -> Result<RetainedAttemptBinding, BudgetError> {
    let row: (String, i64, String, i64, u32, u32, String, i64) = connection.query_row(
        "SELECT binding_id,attempt_id,authority_epoch,reservation_version,
                shared_attempt_number,reservation_attempt_number,route,created_ms
         FROM local_managed_resolution_attempt_bindings WHERE reservation=?1",
        [reservation.as_str()],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
            ))
        },
    )?;
    let expected_binding = binding_id(reservation, row.1);
    let (decision_key, decision_kind, decision_members, decision_paths) =
        request.managed_decision_fields();
    let attempt_matches: bool = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM resolution_attempts
             WHERE attempt_id=?1 AND decision_key=?2 AND kind=?3
               AND members=?4 AND paths=?5 AND request_record_id=?6
               AND resolver='local-managed-resolution' AND outcome IS NULL
               AND request_sha256=?7 AND input_sha256=?8
               AND input_manifest_sha256=?9 AND client_key=?10
               AND dispatch_limits=?11 AND async_state IN ('dispatching','outcome_unknown')
               AND session_label IS NULL AND response_sha256 IS NULL
               AND attempt_number=?12
         )",
        params![
            row.1,
            decision_key,
            decision_kind,
            decision_members,
            decision_paths,
            command.request.id.as_str(),
            command.request.object.hex(),
            resolution::resolution_input_digest(request, &command.request.object),
            proof.input_manifest_sha256,
            command.client_key.as_str(),
            command.limits.canonical_ledger_value(),
            row.4,
        ],
        |row| row.get(0),
    )?;
    if !attempt_matches {
        return Err(BudgetError::Stale);
    }
    let async_state: String = connection.query_row(
        "SELECT async_state FROM resolution_attempts WHERE attempt_id=?1",
        [row.1],
        |row| row.get(0),
    )?;
    let async_token: String = connection.query_row(
        "SELECT async_token FROM resolution_attempts WHERE attempt_id=?1",
        [row.1],
        |row| row.get(0),
    )?;
    OperationId::parse(&async_token).map_err(|_| BudgetError::Stale)?;
    if row.0 != expected_binding.as_str()
        || row.2 != proof.authority_epoch
        || row.3 != 1
        || row.4 == 0
        || row.5 != 1
        || row.6 != MANAGED_ROUTE
        || row.7 != proof.created_ms
    {
        return Err(BudgetError::Stale);
    }
    Ok(RetainedAttemptBinding {
        binding_id: row.0,
        attempt_id: row.1,
        authority_epoch: row.2,
        reservation_version_at_dispatch: row.3,
        shared_attempt: row.4,
        async_token,
        async_state,
    })
}

fn selected_roots(
    order: &[crate::local_wave::ConflictOrderPayload],
) -> Result<BTreeSet<RecordId>, BudgetError> {
    let roots = order
        .iter()
        .map(|entry| RecordId::parse(&entry.lineage).map_err(|_| BudgetError::Stale))
        .collect::<Result<BTreeSet<_>, _>>()?;
    if roots.len() != order.len() {
        return Err(BudgetError::Stale);
    }
    Ok(roots)
}

fn conflict_trigger_digest(trigger: &Trigger) -> Result<String, BudgetError> {
    let conflicts = match trigger {
        Trigger::Conflict(conflicts) if !conflicts.is_empty() => conflicts,
        _ => return Err(BudgetError::Stale),
    };
    let body = serde_json::to_vec(&serde_json::json!({
        "schema": "aethyme.local-managed-resolution-trigger/experimental-v0",
        "version": 1,
        "kind": "conflict",
        "conflicts": conflicts.iter().map(|conflict| serde_json::json!({
            "path": conflict.path,
            "input": conflict.input.as_str(),
            "reason": conflict.reason,
        })).collect::<Vec<_>>(),
    }))?;
    Ok(ObjectDigest::of(&body).hex())
}

fn parse_state(value: &str) -> Result<State, BudgetError> {
    serde_json::from_value(serde_json::Value::String(value.to_owned()))
        .map_err(|_| BudgetError::Stale)
}

fn wall_clock_ms() -> Result<i64, BudgetError> {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| BudgetError::Stale)?
        .as_millis();
    i64::try_from(millis).map_err(|_| BudgetError::Stale)
}

// Keep each independently checked dispatch identity explicit at this boundary.
#[allow(clippy::too_many_arguments)]
fn dispatch_acknowledgement(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    authority_guard: &crate::local_authority::Guard,
    command: &ManagedResolutionCommand,
    wave: &Wave,
    request: &ResolutionRequest,
    trigger_sha256: &str,
    roots: &BTreeSet<RecordId>,
    permit: ResolutionDispatchPermit,
    adapter: AdapterAcknowledge,
) -> Result<(), BudgetError> {
    let now_ms = wall_clock_ms()?;
    if now_ms >= permit.deadline_ms
        || !authority_guard.covers_active(store, &permit.authority_epoch)
        || permit.owner_target != owner.target()
        || permit.owner_issuer != owner.issuer()
        || permit.authority_epoch != *owner.authority_epoch()
        || permit.wave != command.wave
        || permit.expected_version != command.expected_version
        || permit.request != command.request
        || permit.source != command.source
        || permit.manifest
            != ObjectDigest::of(
                &resolution::build_input_manifest(store, request, command.limits)?.0,
            )
        || permit.limits != command.limits
        || permit.deadline_ms != command.deadline_ms
        || &permit.roots != roots
    {
        return Err(BudgetError::Stale);
    }
    let transaction = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    guard(
        &transaction,
        owner,
        &wave.spec.revision,
        command.expected_version,
    )?;
    let validated = ValidatedConflict {
        command,
        wave,
        request,
        trigger_sha256,
        selected_roots: roots,
        input_manifest: permit.manifest,
        now_ms,
    };
    verify_current_wave(&transaction, owner, &validated)?;
    let proof = proof_for_reservation(&transaction, &permit.reservation)?;
    if proof.reservation != permit.reservation.as_str()
        || proof.target != owner.target()
        || proof.issuer != owner.issuer()
        || proof.authority_epoch != permit.authority_epoch.as_str()
        || proof.client_key != command.client_key.as_str()
        || proof.wave != command.wave.as_str()
        || proof.expected_version != command.expected_version
        || proof.request_record_id != command.request.id.as_str()
        || proof.request_object_sha256 != command.request.object.hex()
        || proof.source_record_id != command.source.id.as_str()
        || proof.source_object_sha256 != command.source.object.hex()
        || proof.input_manifest_sha256 != permit.manifest.hex()
        || proof.trigger_sha256 != trigger_sha256
        || proof.dispatch_limits != command.limits.canonical_ledger_value()
        || proof.reserved_envelope != serde_json::to_vec(&command.envelope)?
        || proof.deadline_ms != command.deadline_ms
    {
        return Err(BudgetError::Stale);
    }
    let reservation_id = permit.reservation.clone();
    let revision = RecordId::parse(&wave.spec.revision).map_err(|_| BudgetError::Stale)?;
    let reservation =
        load_retained_reservation(&transaction, &reservation_id, &proof, command, &revision)?;
    if reservation.state != State::CompletionPending
        || reservation.attempts != 1
        || reservation.version != permit.reservation_version
    {
        return Err(BudgetError::Stale);
    }
    let retained_binding =
        load_retained_attempt_binding(&transaction, &reservation_id, &proof, command, request)?;
    if retained_binding.binding_id != permit.binding_id.as_str()
        || retained_binding.attempt_id != permit.attempt_id
        || retained_binding.authority_epoch != permit.authority_epoch.as_str()
        || retained_binding.reservation_version_at_dispatch != permit.reservation_version
        || retained_binding.shared_attempt != permit.shared_attempt
        || retained_binding.async_token != permit.async_token.as_str()
        || retained_binding.async_state != "dispatching"
    {
        return Err(BudgetError::Stale);
    }
    let row_matches: bool = transaction.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM local_resolution_reservation_proofs p
             JOIN local_budget_reservations r ON r.reservation=p.reservation
             JOIN local_managed_resolution_attempt_bindings b
               ON b.reservation=p.reservation
             JOIN resolution_attempts a ON a.attempt_id=b.attempt_id
             WHERE p.reservation=?1 AND p.target=?2 AND p.issuer=?3
               AND p.authority_epoch=?4 AND p.client_key=?5
               AND p.wave=?6 AND p.expected_version=?7
               AND p.request_record_id=?8 AND p.request_object_sha256=?9
               AND p.composition_source_record_id=?10
               AND p.composition_source_object_sha256=?11
               AND p.input_manifest_object_sha256=?12
               AND p.trigger_sha256=?13 AND p.dispatch_limits=?14
               AND p.reserved_envelope=?15 AND p.deadline_ms=?16
               AND r.phase='completion_pending' AND r.attempts=1
               AND r.version=?17 AND b.binding_id=?18
               AND b.attempt_id=?19 AND b.authority_epoch=?4
               AND b.reservation_version=?17
               AND b.reservation_attempt_number=1
               AND b.shared_attempt_number=?20 AND b.route=?21
               AND a.resolver='local-managed-resolution'
               AND a.session_label IS NULL AND a.async_token=?22
               AND a.async_state='dispatching' AND a.outcome IS NULL
         )",
        params![
            permit.reservation.as_str(),
            owner.target(),
            owner.issuer(),
            permit.authority_epoch.as_str(),
            command.client_key.as_str(),
            command.wave.as_str(),
            command.expected_version,
            command.request.id.as_str(),
            command.request.object.hex(),
            command.source.id.as_str(),
            command.source.object.hex(),
            permit.manifest.hex(),
            trigger_sha256,
            command.limits.canonical_ledger_value(),
            serde_json::to_vec(&command.envelope)?,
            command.deadline_ms,
            permit.reservation_version,
            permit.binding_id.as_str(),
            permit.attempt_id,
            permit.shared_attempt,
            MANAGED_ROUTE,
            permit.async_token.as_str(),
        ],
        |row| row.get(0),
    )?;
    let mut roots_statement = transaction.prepare(
        "SELECT lineage_record_id FROM local_resolution_reservation_roots
             WHERE reservation=?1 ORDER BY lineage_record_id",
    )?;
    let stored_roots: BTreeSet<String> = roots_statement
        .query_map([permit.reservation.as_str()], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    let expected_roots: BTreeSet<String> =
        roots.iter().map(|root| root.as_str().to_owned()).collect();
    if !row_matches || stored_roots != expected_roots {
        return Err(BudgetError::Stale);
    }
    drop(roots_statement);
    revalidate_current_budget(&transaction, owner, wave, &proof, &reservation_id)?;
    if wall_clock_ms()? >= permit.deadline_ms {
        return Err(BudgetError::Stale);
    }
    adapter(&permit)?;
    transaction.commit()?;
    Ok(())
}

fn revalidate_current_budget(
    transaction: &rusqlite::Transaction<'_>,
    owner: &ControllerOwner,
    wave: &Wave,
    proof: &ReservationProofRow,
    reservation: &RecordId,
) -> Result<(), BudgetError> {
    reject_overrun(transaction)?;
    let (project_id, project_policy) = project(transaction, owner.issuer())?;
    let (objective_id, objective_cap) = objective(
        transaction,
        owner.issuer(),
        &wave.spec.objective_authorization,
        &project_id,
    )?;
    if project_id.as_str() != proof.project_policy
        || objective_id.as_str() != proof.objective_policy
        || project_policy.price_profile != proof.price_profile
    {
        return Err(BudgetError::Stale);
    }

    let roots = rows(
        transaction,
        "SELECT reservation FROM local_budget_reservations WHERE parent IS NULL",
        &[],
    )?;
    if roots.len() > project_policy.max_root_reservations {
        return Err(BudgetError::Capacity("ledger bound"));
    }
    if !sum(roots.iter().map(charge))?.within(project_policy.cap) {
        return Err(BudgetError::Capacity("project"));
    }
    if !sum(roots
        .iter()
        .filter(|root| root.spec.objective == wave.spec.objective_authorization)
        .map(charge))?
    .within(objective_cap)
    {
        return Err(BudgetError::Capacity("objective"));
    }
    if !sum(roots
        .iter()
        .filter(|root| !root.spec.request.kind.recovery())
        .map(charge))?
    .within(
        project_policy
            .cap
            .subtract(project_policy.recovery_headroom)?,
    ) {
        return Err(BudgetError::Capacity("recovery headroom"));
    }
    if children(transaction, reservation)?
        .iter()
        .any(|child| child.state.held())
    {
        return Err(BudgetError::Pending);
    }
    Ok(())
}

/// Reserve and charge a new conflict attempt in one transaction. The caller
/// holds archive, response, and active-authority guards in that order and has
/// checked the exact current conflict binding before entering this function.
/// A caller must not turn a nonce collision without a binding into a replay.
fn admit_new(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    validated: ValidatedConflict<'_>,
) -> Result<ResolutionDispatchPermit, BudgetError> {
    let command = validated.command;
    command.envelope.validate()?;
    command.limits.validate()?;
    validate_digest(validated.trigger_sha256)?;
    if validated.now_ms < 0
        || validated.now_ms >= command.deadline_ms
        || command.deadline_ms.saturating_sub(validated.now_ms) > 86_400_000
        || command.envelope.zero()
        || validated.selected_roots.is_empty()
    {
        return Err(BudgetError::Invalid("managed resolution bounds".into()));
    }
    if validated.request.record() != &command.request
        || validated.wave.id != command.wave
        || validated.wave.resolution_request.as_ref() != Some(&command.request)
        || validated.wave.composition_source.as_ref() != Some(&command.source)
        || validated.wave.spec.target != owner.target()
        || validated.wave.spec.issuer != owner.issuer()
        || validated.wave.spec.prior_completion.is_some()
        || validated.wave.phase != WavePhase::Blocked
        || !matches!(validated.request.trigger(), Trigger::Conflict(conflicts) if !conflicts.is_empty())
    {
        return Err(BudgetError::Stale);
    }

    let async_token = crate::collaboration_capture::OperationId::mint()
        .map_err(|error| BudgetError::Invalid(error.to_string()))?;
    let nonce = reservation_nonce(owner, &command.client_key);
    let request = Request {
        nonce: nonce.clone(),
        recipe: command.request.id.to_string(),
        kind: WorkKind::Resolution,
        units: command.envelope,
        max_attempts: 1,
        deadline_ms: command.deadline_ms,
    };
    request.validate()?;
    let spec = ReservationSpec {
        issuer: owner.issuer().into(),
        target: owner.target().into(),
        revision: validated.wave.spec.revision.clone(),
        wave: command.wave.to_string(),
        objective: validated.wave.spec.objective_authorization.clone(),
        project_policy: String::new(),
        objective_policy: String::new(),
        request,
        parent: None,
    };

    let transaction = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let duplicate_key: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM local_resolution_reservation_proofs
         WHERE target=?1 AND issuer=?2 AND client_key=?3)",
        params![owner.target(), owner.issuer(), command.client_key.as_str()],
        |row| row.get(0),
    )?;
    if duplicate_key || super::nonce_key(&transaction, &nonce)?.is_some() {
        return Err(BudgetError::Stale);
    }
    reject_overrun(&transaction)?;
    let (revision, version, target_phase, resume_phase): (
        Option<String>,
        i64,
        String,
        Option<String>,
    ) = transaction.query_row(
        "SELECT active_revision,version,phase,resume_phase FROM local_targets
         WHERE target=?1 AND issuer=?2",
        params![owner.target(), owner.issuer()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    if version != command.expected_version
        || revision.as_deref() != Some(spec.revision.as_str())
        || target_phase != "idle"
        || resume_phase.is_some()
    {
        return Err(BudgetError::Stale);
    }
    guard(
        &transaction,
        owner,
        &spec.revision,
        command.expected_version,
    )?;
    verify_current_wave(&transaction, owner, &validated)?;

    resolution::initialize_ledger_in_transaction(&transaction)?;
    let unresolved_managed = resolution::has_unresolved_managed_comparable_in_transaction(
        &transaction,
        validated.request,
    )?;
    if unresolved_managed {
        return Err(BudgetError::Pending);
    }

    let (project_id, policy) = project(&transaction, owner.issuer())?;
    let (objective_id, objective_cap) =
        objective(&transaction, owner.issuer(), &spec.objective, &project_id)?;
    let spec = ReservationSpec {
        project_policy: project_id.to_string(),
        objective_policy: objective_id.to_string(),
        ..spec
    };
    let reservation_record = super::record(&spec)?;

    let root_rows = rows(
        &transaction,
        "SELECT reservation FROM local_budget_reservations WHERE parent IS NULL",
        &[],
    )?;
    if root_rows.len() >= policy.max_root_reservations {
        return Err(BudgetError::Capacity("ledger bound"));
    }
    if !sum(root_rows.iter().map(charge))?
        .add(command.envelope)?
        .within(policy.cap)
    {
        return Err(BudgetError::Capacity("project"));
    }
    if !sum(root_rows
        .iter()
        .filter(|reservation| reservation.spec.objective == spec.objective)
        .map(charge))?
    .add(command.envelope)?
    .within(objective_cap)
    {
        return Err(BudgetError::Capacity("objective"));
    }
    let non_recovery = sum(root_rows
        .iter()
        .filter(|reservation| !reservation.spec.request.kind.recovery())
        .map(charge))?
    .add(command.envelope)?;
    if !non_recovery.within(policy.cap.subtract(policy.recovery_headroom)?) {
        return Err(BudgetError::Capacity("recovery headroom"));
    }

    let input_sha256 =
        resolution::resolution_input_digest(validated.request, &command.request.object);
    let Some((shared_attempt, attempt_id)) = resolution::charge_managed_in_transaction(
        &transaction,
        validated.request,
        resolution::ManagedChargeContext {
            input_manifest: validated.input_manifest,
            client_key: &command.client_key,
            async_token: &async_token,
            input_sha256: &input_sha256,
            limits: command.limits,
        },
    )?
    else {
        return Err(BudgetError::AttemptsExhausted);
    };

    insert(
        &transaction,
        &reservation_record,
        &spec,
        &reservation_record.id(),
    )?;
    if transaction.execute(
        "UPDATE local_budget_reservations
         SET phase='completion_pending',attempts=1,version=1
         WHERE reservation=?1 AND phase='reserved' AND attempts=0 AND version=0",
        [reservation_record.id().as_str()],
    )? != 1
    {
        return Err(BudgetError::Stale);
    }
    transaction.execute(
        "INSERT INTO local_resolution_reservation_proofs
         (reservation,target,issuer,authority_epoch,client_key,wave,expected_version,
          request_record_id,request_object_sha256,composition_source_record_id,
          composition_source_object_sha256,input_manifest_object_sha256,trigger_sha256,
          project_policy,objective_policy,price_profile,dispatch_limits,reserved_envelope,
          reservation_attempt_limit,deadline_ms,created_ms)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,1,?19,?20)",
        params![
            reservation_record.id().as_str(),
            owner.target(),
            owner.issuer(),
            owner.authority_epoch().as_str(),
            command.client_key.as_str(),
            command.wave.as_str(),
            command.expected_version,
            command.request.id.as_str(),
            command.request.object.hex(),
            command.source.id.as_str(),
            command.source.object.hex(),
            validated.input_manifest.hex(),
            validated.trigger_sha256,
            project_id.as_str(),
            objective_id.as_str(),
            policy.price_profile,
            command.limits.canonical_ledger_value(),
            serde_json::to_vec(&command.envelope)?,
            command.deadline_ms,
            validated.now_ms,
        ],
    )?;

    let holder = format!("resolution:{}", async_token.as_str());
    resolution::retain_attempt_inputs(
        &transaction,
        async_token.as_str(),
        &command.request,
        validated.request,
    )?;
    transaction.execute(
        "INSERT INTO object_pins(class,object_sha256,holder,until_ms,created_ms)
         VALUES('cited_evidence',?1,?2,NULL,?3)",
        params![validated.input_manifest.hex(), holder, validated.now_ms],
    )?;
    for root in validated.selected_roots {
        let retained: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM retained_contributions
             WHERE lineage_record_id=?1
               AND lineage_record_id NOT IN
                   (SELECT lineage_record_id FROM reclaimed_contributions))",
            [root.as_str()],
            |row| row.get(0),
        )?;
        if !retained {
            return Err(BudgetError::Stale);
        }
        transaction.execute(
            "INSERT INTO local_resolution_reservation_roots(reservation,lineage_record_id)
             VALUES (?1,?2)",
            params![reservation_record.id().as_str(), root.as_str()],
        )?;
        transaction.execute(
            "INSERT INTO reader_leases(target_kind,target,holder,until_ms,created_ms)
             SELECT 'contribution',?1,?2,?3,?4
             WHERE NOT EXISTS (
                 SELECT 1 FROM reader_leases WHERE target_kind='contribution'
                   AND target=?1 AND holder=?2 AND released_ms IS NULL
             )",
            params![root.as_str(), holder, i64::MAX, validated.now_ms],
        )?;
    }
    let binding_id = binding_id(&reservation_record.id(), attempt_id);
    transaction.execute(
        "INSERT INTO local_managed_resolution_attempt_bindings
         (binding_id,reservation,attempt_id,authority_epoch,reservation_version,
          shared_attempt_number,reservation_attempt_number,route,created_ms)
         VALUES (?1,?2,?3,?4,1,?5,1,?6,?7)",
        params![
            binding_id.as_str(),
            reservation_record.id().as_str(),
            attempt_id,
            owner.authority_epoch().as_str(),
            shared_attempt,
            MANAGED_ROUTE,
            validated.now_ms,
        ],
    )?;
    // Schema 27 requires a managed charge to be inserted in exported state
    // before dispatch. After writing the reservation and proof bindings, this
    // admission transaction advances it to dispatching before commit.
    if transaction.execute(
        "UPDATE resolution_attempts SET async_state='dispatching'
         WHERE attempt_id=?1 AND async_token=?2
           AND resolver='local-managed-resolution' AND session_label IS NULL
           AND async_state='exported'",
        params![attempt_id, async_token.as_str()],
    )? != 1
    {
        return Err(BudgetError::Stale);
    }
    transaction.commit()?;

    Ok(ResolutionDispatchPermit {
        reservation: reservation_record.id(),
        binding_id,
        attempt_id,
        async_token,
        reservation_version: 1,
        shared_attempt,
        owner_target: owner.target().to_owned(),
        owner_issuer: owner.issuer().to_owned(),
        authority_epoch: owner.authority_epoch().clone(),
        wave: command.wave.clone(),
        expected_version: command.expected_version,
        request: command.request.clone(),
        source: command.source.clone(),
        manifest: validated.input_manifest,
        limits: command.limits,
        deadline_ms: command.deadline_ms,
        roots: validated.selected_roots.clone(),
    })
}

fn verify_current_wave(
    transaction: &rusqlite::Transaction<'_>,
    owner: &ControllerOwner,
    validated: &ValidatedConflict<'_>,
) -> Result<(), BudgetError> {
    let command = validated.command;
    let target: Option<(String, i64, String, Option<String>)> = transaction
        .query_row(
            "SELECT active_revision,version,phase,resume_phase FROM local_targets
             WHERE target=?1 AND issuer=?2",
            params![owner.target(), owner.issuer()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((active_revision, version, target_phase, resume_phase)) = target else {
        return Err(BudgetError::Stale);
    };
    let row: Option<CurrentConflictWaveRow> = transaction
        .query_row(
            "SELECT target,revision,sequence,phase,resolution_request_record_id,
                    resolution_request_object,composition_source_record_id,
                    composition_source_object
             FROM local_waves WHERE wave=?1",
            [command.wave.as_str()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )
        .optional()?;
    let Some((
        target,
        revision,
        sequence,
        phase,
        request_id,
        request_object,
        source_id,
        source_object,
    )) = row
    else {
        return Err(BudgetError::Stale);
    };
    let latest_sequence: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(sequence),0) FROM local_waves WHERE target=?1 AND revision=?2",
        params![owner.target(), revision],
        |row| row.get(0),
    )?;
    if active_revision != validated.wave.spec.revision
        || version != command.expected_version
        || target_phase != "idle"
        || resume_phase.is_some()
        || target != owner.target()
        || revision != validated.wave.spec.revision
        || sequence != validated.wave.spec.sequence
        || sequence != latest_sequence
        || phase != "blocked"
        || request_id.as_deref() != Some(command.request.id.as_str())
        || request_object.as_deref() != Some(command.request.object.hex().as_str())
        || source_id.as_deref() != Some(command.source.id.as_str())
        || source_object.as_deref() != Some(command.source.object.hex().as_str())
    {
        return Err(BudgetError::Stale);
    }
    Ok(())
}

fn reservation_nonce(
    owner: &ControllerOwner,
    key: &crate::collaboration_capture::OperationId,
) -> String {
    reservation_nonce_for(owner.target(), owner.issuer(), key)
}

fn reservation_nonce_for(target: &str, issuer: &str, key: &OperationId) -> String {
    let digest = ObjectDigest::of(
        format!(
            "aethyme local resolution reservation nonce v1\0{}\0{}\0{}",
            target,
            issuer,
            key.as_str()
        )
        .as_bytes(),
    );
    format!("sha256:{}", digest.hex())
}

fn binding_id(reservation: &RecordId, attempt_id: i64) -> RecordId {
    let digest = ObjectDigest::of(
        format!(
            "aethyme local managed resolution binding v1\0{}\0{attempt_id}",
            reservation.as_str()
        )
        .as_bytes(),
    );
    RecordId::parse(&format!("sha256:{}", digest.hex())).expect("SHA-256 record ID")
}

fn validate_digest(value: &str) -> Result<(), BudgetError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(BudgetError::Invalid("managed resolution digest".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collaboration_archive::{CommitOid, RetainedContribution};
    use crate::local_budget::{authorize_objective, configure_project};
    use crate::local_target::{self, RequestedContribution};
    use std::cell::Cell;

    thread_local! {
        static ACKNOWLEDGEMENTS: Cell<u32> = const { Cell::new(0) };
    }

    fn acknowledge(permit: &ResolutionDispatchPermit) -> Result<(), BudgetError> {
        assert!((1..=2).contains(&permit.shared_attempt));
        ACKNOWLEDGEMENTS.with(|count| count.set(count.get() + 1));
        Ok(())
    }

    fn mark_overrun_after_admission(store: &mut CollaborationStore) {
        let updated = store
            .connection()
            .execute(
                "UPDATE local_budget_reservations
                 SET phase='consumption_unknown',attempts=1,version=version+1,
                     receipt=?1,detail='injected overrun between admission and acknowledgement',
                     observed_overrun=?2
                 WHERE target='target' AND nonce=?3 AND phase='reserved'",
                params![
                    reference('c'),
                    serde_json::to_vec(&units(11)).unwrap(),
                    reference('a')
                ],
            )
            .unwrap();
        assert_eq!(updated, 1);
    }

    fn change_project_profile_after_admission(store: &mut CollaborationStore) {
        let (project_key, project_body): (String, Vec<u8>) = store
            .read_connection()
            .query_row(
                "SELECT policy,body FROM local_budget_project WHERE singleton=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let mut project: super::super::ProjectRecord =
            super::super::decode(&project_key, &project_body).unwrap();
        project.policy.price_profile = reference('9');
        let project_record = super::super::record(&project).unwrap();
        let project_id = project_record.id();
        assert_eq!(
            store
                .connection()
                .execute(
                    "UPDATE local_budget_project SET policy=?1,body=?2 WHERE singleton=1",
                    params![project_id.as_str(), project_record.canonical_bytes()],
                )
                .unwrap(),
            1
        );

        let (objective_id, objective_key, objective_body): (String, String, Vec<u8>) = store
            .read_connection()
            .query_row(
                "SELECT objective,policy,body FROM local_budget_objectives LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let mut objective: super::super::ObjectiveRecord =
            super::super::decode(&objective_key, &objective_body).unwrap();
        objective.project_policy = project_id.as_str().to_owned();
        let objective_record = super::super::record(&objective).unwrap();
        assert_eq!(
            store
                .connection()
                .execute(
                    "UPDATE local_budget_objectives SET policy=?1,body=?2 WHERE objective=?3",
                    params![
                        objective_record.id().as_str(),
                        objective_record.canonical_bytes(),
                        objective_id
                    ],
                )
                .unwrap(),
            1
        );
    }

    fn reset_acknowledgements() {
        ACKNOWLEDGEMENTS.with(|count| count.set(0));
    }

    fn acknowledgements() -> u32 {
        ACKNOWLEDGEMENTS.with(Cell::get)
    }

    fn units(value: i64) -> Units {
        Units {
            time_ms: value,
            memory_bytes: value,
            disk_bytes: value,
            model_requests: value,
            input_tokens: value,
            output_tokens: value,
            spend_microunits: value,
            slots: value,
            previews: value,
        }
    }

    fn reference(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn limits() -> resolution::DispatchLimits {
        resolution::DispatchLimits {
            max_manifest_bytes: 1 << 20,
            max_manifest_entries: 64,
            max_file_bytes: 1 << 20,
            max_total_input_bytes: 4 << 20,
            max_response_bytes: 1 << 20,
        }
    }

    fn project_policy() -> super::super::ProjectPolicy {
        super::super::ProjectPolicy {
            authorization: reference('7'),
            price_profile: reference('8'),
            cap: units(10_000),
            recovery_headroom: units(100),
            mandatory_headroom: units(100),
            max_root_reservations: 32,
        }
    }

    fn conflict_fixture() -> (local_target::tests::Fixture, ControllerOwner, Wave) {
        let mut fixture = local_target::tests::Fixture::new();
        let base = fixture.a.base.commit.clone();
        local_target::tests::git(
            fixture._repo.path(),
            &["switch", "-qc", "managed-conflict", base.as_str()],
        );
        std::fs::write(fixture._repo.path().join("one.txt"), "incompatible\n").unwrap();
        local_target::tests::git(
            fixture._repo.path(),
            &["commit", "-qam", "conflicting edit"],
        );
        let tip = CommitOid::parse(&local_target::tests::git(
            fixture._repo.path(),
            &["rev-parse", "HEAD"],
        ))
        .unwrap();
        let conflict: RetainedContribution =
            archive::retain_contribution(&mut fixture.store, fixture._repo.path(), &base, &tip)
                .unwrap();

        let mut spec = fixture.spec();
        spec.contributions.push(RequestedContribution {
            id: "C".into(),
            lineage: conflict.lineage_record_id.to_string(),
            requires: Vec::new(),
            atomic_group: None,
            revision_of: None,
            derived_from: Vec::new(),
        });
        spec.selected = vec!["A".into(), "C".into()];
        let owner =
            local_target::acquire_controller(&mut fixture.store, "target", "appointed").unwrap();
        let revision = local_target::propose_revision(&mut fixture.store, &owner, spec).unwrap();
        local_target::select_revision(&mut fixture.store, &owner, &revision.id).unwrap();
        configure_project(&mut fixture.store, &owner, project_policy()).unwrap();
        authorize_objective(&mut fixture.store, &owner, units(5_000)).unwrap();
        local_wave::available(&mut fixture.store, &owner, "A", 0).unwrap();
        local_wave::available(&mut fixture.store, &owner, "C", 1).unwrap();
        let admission = local_wave::admit(
            &mut fixture.store,
            &owner,
            local_wave::WavePolicy {
                debounce_ms: 0,
                ..local_wave::WavePolicy::default()
            },
            1,
        )
        .unwrap();
        let local_wave::Admission::Frozen(wave) = admission else {
            panic!("expected frozen conflict wave, got {admission:?}");
        };
        let wave = local_wave::compose_with_source(
            &mut fixture.store,
            &owner,
            fixture._repo.path(),
            &wave.id,
        )
        .unwrap()
        .wave;
        assert_eq!(wave.phase, WavePhase::Blocked);
        assert!(wave.resolution_request.is_some());
        assert!(wave.composition_source.is_some());
        (fixture, owner, wave)
    }

    fn command(store: &CollaborationStore, wave: &Wave) -> ManagedResolutionCommand {
        ManagedResolutionCommand {
            wave: wave.id.clone(),
            expected_version: local_target::status(store, &wave.spec.target)
                .unwrap()
                .version,
            request: wave.resolution_request.clone().unwrap(),
            source: wave.composition_source.clone().unwrap(),
            client_key: OperationId::mint().unwrap(),
            envelope: units(10),
            limits: limits(),
            deadline_ms: wall_clock_ms().unwrap() + 60_000,
        }
    }

    fn resolution_attempt_snapshot(
        store: &CollaborationStore,
        client_key: &str,
    ) -> Vec<rusqlite::types::Value> {
        let connection = store.read_connection();
        let mut statement = connection
            .prepare("SELECT * FROM resolution_attempts WHERE client_key=?1")
            .unwrap();
        let column_count = statement.column_count();
        statement
            .query_row([client_key], |row| {
                let mut values = Vec::with_capacity(column_count);
                for index in 0..column_count {
                    values.push(row.get::<_, rusqlite::types::Value>(index)?);
                }
                Ok(values)
            })
            .unwrap()
    }

    type WriteSnapshot = (i64, i64, i64, i64, i64, i64, i64, i64);

    fn write_snapshot(store: &CollaborationStore, target: &str) -> WriteSnapshot {
        store
            .read_connection()
            .query_row(
                "SELECT
                    (SELECT count(*) FROM local_budget_reservations),
                    (SELECT count(*) FROM resolution_attempts),
                    (SELECT count(*) FROM local_resolution_reservation_proofs),
                    (SELECT count(*) FROM local_resolution_reservation_roots),
                    (SELECT count(*) FROM local_managed_resolution_attempt_bindings),
                    (SELECT count(*) FROM object_pins WHERE holder LIKE 'resolution:%'),
                    (SELECT count(*) FROM reader_leases WHERE holder LIKE 'resolution:%'),
                    (SELECT version FROM local_targets WHERE target=?1)",
                [target],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                    ))
                },
            )
            .unwrap()
    }

    #[test]
    fn empty_production_registry_refuses_before_any_managed_write() {
        let (mut fixture, owner, wave) = conflict_fixture();
        let command = command(&fixture.store, &wave);
        let request =
            resolution::load_request(&mut fixture.store, fixture._repo.path(), &command.request)
                .unwrap();
        let (manifest, _) =
            resolution::build_input_manifest(&fixture.store, &request, command.limits).unwrap();
        let manifest_digest = ObjectDigest::of(&manifest);
        assert!(archive::read_object(&fixture.store, &manifest_digest).is_err());
        let before = write_snapshot(&fixture.store, owner.target());

        let outcome = begin(&mut fixture.store, fixture._repo.path(), &owner, &command).unwrap();

        assert_eq!(outcome, ManagedResolutionOutcome::AdapterUnavailable);
        assert_eq!(write_snapshot(&fixture.store, owner.target()), before);
        assert!(archive::read_object(&fixture.store, &manifest_digest).is_err());
    }

    #[test]
    fn registered_test_adapter_charges_once_and_retries_are_status_only() {
        let (mut fixture, owner, wave) = conflict_fixture();
        let command = command(&fixture.store, &wave);
        let before = write_snapshot(&fixture.store, owner.target());
        reset_acknowledgements();
        let _adapter = register_test_adapter(acknowledge);

        let first = begin(&mut fixture.store, fixture._repo.path(), &owner, &command).unwrap();
        let ManagedResolutionOutcome::DispatchAcknowledged(status) = first else {
            panic!("expected private fake acknowledgement, got {first:?}");
        };
        assert_eq!(status.phase, State::CompletionPending);
        assert_eq!(status.reservation_attempt, 1);
        assert_eq!(status.shared_attempt, 1);
        assert_eq!(status.async_state, "dispatching");
        assert_eq!(acknowledgements(), 1);
        let admitted = write_snapshot(&fixture.store, owner.target());
        assert_eq!(admitted.0, before.0 + 1);
        assert_eq!(admitted.1, before.1 + 1);
        assert_eq!(admitted.2, before.2 + 1);
        assert_eq!(admitted.3, before.3 + 2);
        assert_eq!(admitted.4, before.4 + 1);

        let retry = begin(&mut fixture.store, fixture._repo.path(), &owner, &command).unwrap();
        assert_eq!(retry, ManagedResolutionOutcome::StatusOnly(status.clone()));
        assert_eq!(acknowledgements(), 1);
        assert_eq!(write_snapshot(&fixture.store, owner.target()), admitted);

        let mut distinct_key = command.clone();
        distinct_key.client_key = OperationId::mint().unwrap();
        let held = begin(
            &mut fixture.store,
            fixture._repo.path(),
            &owner,
            &distinct_key,
        );
        assert!(matches!(held, Err(BudgetError::Pending)));
        assert_eq!(acknowledgements(), 1);
        assert_eq!(write_snapshot(&fixture.store, owner.target()), admitted);

        let root =
            crate::collaboration_state::CollaborationRoot::under_host_state(fixture._host.path());
        let project_key =
            crate::collaboration_state::ProjectKey::parse("local-target-test").unwrap();
        drop(fixture.store);
        fixture.store = CollaborationStore::open(&root, &project_key, &[]).unwrap();
        let after_reopen = begin(
            &mut fixture.store,
            fixture._repo.path(),
            &owner,
            &distinct_key,
        );
        assert!(matches!(after_reopen, Err(BudgetError::Pending)));
        assert_eq!(acknowledgements(), 1);
        assert_eq!(write_snapshot(&fixture.store, owner.target()), admitted);

        let target = owner.target().to_owned();
        let issuer = owner.issuer().to_owned();
        let previous_epoch = owner.authority_epoch().clone();
        let frozen = crate::local_authority::freeze(
            &mut fixture.store,
            &previous_epoch,
            "verify retained managed key after authority rotation",
        )
        .unwrap();
        crate::local_authority::resume(&mut fixture.store, &frozen.epoch).unwrap();
        drop(owner);
        let current_owner =
            local_target::acquire_controller(&mut fixture.store, &target, &issuer).unwrap();
        assert_ne!(current_owner.authority_epoch(), &previous_epoch);
        let after_epoch_rotation = write_snapshot(&fixture.store, &target);

        let retained_after_epoch = begin(
            &mut fixture.store,
            fixture._repo.path(),
            &current_owner,
            &command,
        )
        .unwrap();
        let ManagedResolutionOutcome::StatusOnly(retained_after_epoch) = retained_after_epoch
        else {
            panic!(
                "expected retained status after authority rotation, got {retained_after_epoch:?}"
            );
        };
        assert_eq!(retained_after_epoch.phase, State::ConsumptionUnknown);
        assert_eq!(retained_after_epoch.async_state, "dispatching");
        assert_eq!(acknowledgements(), 1);
        assert_eq!(
            write_snapshot(&fixture.store, &target),
            after_epoch_rotation
        );

        let mut changed = command.clone();
        changed.envelope = units(11);
        assert!(matches!(
            begin(
                &mut fixture.store,
                fixture._repo.path(),
                &current_owner,
                &changed
            ),
            Err(BudgetError::Immutable)
        ));
        assert_eq!(acknowledgements(), 1);
        assert_eq!(
            write_snapshot(&fixture.store, &target),
            after_epoch_rotation
        );
    }

    #[test]
    fn unresolved_managed_superset_blocks_new_admission_without_managed_ledger_writes() {
        let (mut fixture, subset_owner, subset_wave) = conflict_fixture();
        let base = fixture.a.base.commit.clone();
        local_target::tests::git(
            fixture._repo.path(),
            &["switch", "-qc", "managed-superset", base.as_str()],
        );
        std::fs::write(
            fixture._repo.path().join("one.txt"),
            "another incompatible edit\n",
        )
        .unwrap();
        local_target::tests::git(
            fixture._repo.path(),
            &["commit", "-qam", "another conflict"],
        );
        let tip = CommitOid::parse(&local_target::tests::git(
            fixture._repo.path(),
            &["rev-parse", "HEAD"],
        ))
        .unwrap();
        let additional: RetainedContribution =
            archive::retain_contribution(&mut fixture.store, fixture._repo.path(), &base, &tip)
                .unwrap();

        let subset_revision = local_target::status(&fixture.store, subset_owner.target())
            .unwrap()
            .revision
            .unwrap();
        let mut superset_spec = local_target::load_revision(&fixture.store, &subset_revision)
            .unwrap()
            .spec;
        superset_spec.contributions.push(RequestedContribution {
            id: "D".into(),
            lineage: additional.lineage_record_id.to_string(),
            requires: Vec::new(),
            atomic_group: None,
            revision_of: None,
            derived_from: Vec::new(),
        });
        superset_spec.selected = vec!["A".into(), "C".into(), "D".into()];

        let superset_owner = local_target::acquire_controller(
            &mut fixture.store,
            "managed-superset-target",
            subset_owner.issuer(),
        )
        .unwrap();
        let revision =
            local_target::propose_revision(&mut fixture.store, &superset_owner, superset_spec)
                .unwrap();
        local_target::select_revision(&mut fixture.store, &superset_owner, &revision.id).unwrap();
        local_wave::available(&mut fixture.store, &superset_owner, "A", 0).unwrap();
        local_wave::available(&mut fixture.store, &superset_owner, "C", 1).unwrap();
        local_wave::available(&mut fixture.store, &superset_owner, "D", 2).unwrap();
        let admission = local_wave::admit(
            &mut fixture.store,
            &superset_owner,
            local_wave::WavePolicy {
                debounce_ms: 0,
                ..local_wave::WavePolicy::default()
            },
            2,
        )
        .unwrap();
        let local_wave::Admission::Frozen(wave) = admission else {
            panic!("expected frozen superset conflict wave, got {admission:?}");
        };
        let superset_wave = local_wave::compose_with_source(
            &mut fixture.store,
            &superset_owner,
            fixture._repo.path(),
            &wave.id,
        )
        .unwrap()
        .wave;
        assert_eq!(superset_wave.phase, WavePhase::Blocked);
        assert!(superset_wave.resolution_request.is_some());

        let superset_command = command(&fixture.store, &superset_wave);
        let subset_command = command(&fixture.store, &subset_wave);
        let superset_request = resolution::load_request(
            &mut fixture.store,
            fixture._repo.path(),
            &superset_command.request,
        )
        .unwrap();
        let subset_request = resolution::load_request(
            &mut fixture.store,
            fixture._repo.path(),
            &subset_command.request,
        )
        .unwrap();
        let (_, superset_kind, superset_members, superset_paths) =
            superset_request.managed_decision_fields();
        let (_, subset_kind, subset_members, subset_paths) =
            subset_request.managed_decision_fields();
        assert_eq!(subset_kind, superset_kind);
        let decision_set = |raw: &str| {
            raw.split('\n')
                .map(str::to_owned)
                .collect::<std::collections::BTreeSet<_>>()
        };
        let subset_members = decision_set(&subset_members);
        let superset_members = decision_set(&superset_members);
        assert!(subset_members.is_subset(&superset_members));
        assert!(subset_members.len() < superset_members.len());
        assert_eq!(decision_set(&subset_paths), decision_set(&superset_paths));

        reset_acknowledgements();
        let _adapter = register_test_adapter(acknowledge);
        let first = begin(
            &mut fixture.store,
            fixture._repo.path(),
            &superset_owner,
            &superset_command,
        )
        .unwrap();
        let ManagedResolutionOutcome::DispatchAcknowledged(status) = first else {
            panic!("expected first superset acknowledgement, got {first:?}");
        };
        assert_eq!(status.async_state, "dispatching");
        assert_eq!(acknowledgements(), 1);
        let attempt_before =
            resolution_attempt_snapshot(&fixture.store, superset_command.client_key.as_str());

        // `begin` durably stores the input manifest before checking the pending
        // decision. Prime the exact object so the oracle below measures managed
        // ledger writes; also verify the manifest bytes survive the pending refusal.
        let (subset_manifest_bytes, subset_manifest) = resolution::build_input_manifest(
            &fixture.store,
            &subset_request,
            subset_command.limits,
        )
        .unwrap();
        let subset_manifest_object =
            archive::put_object(&fixture.store, &subset_manifest_bytes).unwrap();
        assert_eq!(subset_manifest_object.hex(), subset_manifest.sha256);
        assert_eq!(
            archive::read_object(&fixture.store, &subset_manifest_object).unwrap(),
            subset_manifest_bytes
        );
        let before = write_snapshot(&fixture.store, subset_owner.target());
        let mut distinct = subset_command;
        distinct.client_key = OperationId::mint().unwrap();
        let outcome = begin(
            &mut fixture.store,
            fixture._repo.path(),
            &subset_owner,
            &distinct,
        );
        assert!(matches!(outcome, Err(BudgetError::Pending)), "{outcome:?}");
        assert_eq!(acknowledgements(), 1);
        assert_eq!(
            write_snapshot(&fixture.store, subset_owner.target()),
            before
        );
        assert_eq!(
            resolution_attempt_snapshot(&fixture.store, superset_command.client_key.as_str()),
            attempt_before
        );
        assert_eq!(
            archive::read_object(&fixture.store, &subset_manifest_object).unwrap(),
            subset_manifest_bytes
        );
    }

    #[test]
    fn managed_charge_uses_manual_comparable_attempt_count() {
        let (mut fixture, owner, wave) = conflict_fixture();
        let command = command(&fixture.store, &wave);
        let manual_key = OperationId::mint().unwrap();
        resolution::begin_attempt(
            &mut fixture.store,
            fixture._repo.path(),
            &command.request,
            "manual-comparable-attempt",
            manual_key.as_str(),
            command.limits,
        )
        .unwrap();
        reset_acknowledgements();
        let _adapter = register_test_adapter(acknowledge);

        let outcome = begin(&mut fixture.store, fixture._repo.path(), &owner, &command).unwrap();

        let ManagedResolutionOutcome::DispatchAcknowledged(status) = outcome else {
            panic!("expected second shared comparable attempt, got {outcome:?}");
        };
        assert_eq!(status.shared_attempt, 2);
        assert_eq!(status.reservation_attempt, 1);
        assert_eq!(acknowledgements(), 1);
        let managed_rows: i64 = fixture
            .store
            .read_connection()
            .query_row(
                "SELECT count(*) FROM resolution_attempts
                 WHERE resolver='local-managed-resolution' AND client_key=?1",
                [command.client_key.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(managed_rows, 1);
    }

    #[test]
    fn exhausted_manual_shared_cap_does_not_create_local_reservation() {
        let (mut fixture, owner, wave) = conflict_fixture();
        let command = command(&fixture.store, &wave);
        for session in ["manual-first", "manual-second"] {
            let key = OperationId::mint().unwrap();
            resolution::begin_attempt(
                &mut fixture.store,
                fixture._repo.path(),
                &command.request,
                session,
                key.as_str(),
                command.limits,
            )
            .unwrap();
        }
        let before = write_snapshot(&fixture.store, owner.target());
        reset_acknowledgements();
        let _adapter = register_test_adapter(acknowledge);

        let result = begin(&mut fixture.store, fixture._repo.path(), &owner, &command);

        assert!(matches!(result, Err(BudgetError::AttemptsExhausted)));
        assert_eq!(acknowledgements(), 0);
        assert_eq!(write_snapshot(&fixture.store, owner.target()), before);
    }

    #[test]
    fn binding_insert_failure_rolls_back_charge_reservation_and_retention_rows() {
        let (mut fixture, owner, wave) = conflict_fixture();
        let command = command(&fixture.store, &wave);
        let before = write_snapshot(&fixture.store, owner.target());
        fixture
            .store
            .connection()
            .execute_batch(
                "CREATE TEMP TRIGGER reject_managed_binding
                 BEFORE INSERT ON main.local_managed_resolution_attempt_bindings
                 BEGIN SELECT RAISE(ABORT, 'injected binding failure'); END;",
            )
            .unwrap();
        reset_acknowledgements();
        let _adapter = register_test_adapter(acknowledge);

        let result = begin(&mut fixture.store, fixture._repo.path(), &owner, &command);

        assert!(result.is_err());
        assert_eq!(acknowledgements(), 0);
        assert_eq!(write_snapshot(&fixture.store, owner.target()), before);
        fixture
            .store
            .connection()
            .execute_batch("DROP TRIGGER reject_managed_binding")
            .unwrap();
    }

    #[test]
    fn acknowledgement_rechecks_overrun_after_admission_commit() {
        let (mut fixture, owner, wave) = conflict_fixture();
        let command = command(&fixture.store, &wave);
        let now_ms = wall_clock_ms().unwrap();
        super::super::reserve(
            &mut fixture.store,
            &owner,
            &wave.id,
            Request {
                nonce: reference('a'),
                recipe: command.request.id.as_str().to_owned(),
                kind: WorkKind::Recovery,
                units: units(10),
                max_attempts: 1,
                deadline_ms: now_ms + 60_000,
            },
            now_ms,
        )
        .unwrap();
        let before = write_snapshot(&fixture.store, owner.target());
        reset_acknowledgements();
        let _adapter = register_test_adapter(acknowledge);
        let _mutation = register_after_admission(mark_overrun_after_admission);

        let result = begin(&mut fixture.store, fixture._repo.path(), &owner, &command);

        assert!(matches!(
            result,
            Err(BudgetError::Capacity("unreconciled overrun"))
        ));
        assert_eq!(acknowledgements(), 0);
        let after = write_snapshot(&fixture.store, owner.target());
        assert_eq!(after.0, before.0 + 1);
        assert_eq!(after.1, before.1 + 1);
        assert_eq!(after.2, before.2 + 1);
        assert_eq!(after.4, before.4 + 1);
    }

    #[test]
    fn acknowledgement_rechecks_exact_project_policy_after_admission_commit() {
        let (mut fixture, owner, wave) = conflict_fixture();
        let command = command(&fixture.store, &wave);
        let before = write_snapshot(&fixture.store, owner.target());
        reset_acknowledgements();
        let _adapter = register_test_adapter(acknowledge);
        let _mutation = register_after_admission(change_project_profile_after_admission);

        let result = begin(&mut fixture.store, fixture._repo.path(), &owner, &command);

        assert!(matches!(result, Err(BudgetError::Stale)));
        assert_eq!(acknowledgements(), 0);
        let after = write_snapshot(&fixture.store, owner.target());
        assert_eq!(after.0, before.0 + 1);
        assert_eq!(after.1, before.1 + 1);
        assert_eq!(after.2, before.2 + 1);
        assert_eq!(after.4, before.4 + 1);
    }
}
