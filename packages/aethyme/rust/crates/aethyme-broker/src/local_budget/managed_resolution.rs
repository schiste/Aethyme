//! The managed Local resolution route. Only code that has already validated
//! the current conflict source may construct `ValidatedConflict`; that value
//! is consumed by one Immediate transaction which writes the shared charge,
//! root reservation, retained inputs, proof, roots, and attempt binding.

use std::collections::BTreeSet;

use aethyme_contracts::experimental_v0::RecordId;
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{
    BudgetError, ManagedResolutionCommand, Request, ReservationSpec, Units, WorkKind, charge,
    guard, insert, objective, project, reject_overrun, rows, sum,
};
use crate::collaboration_archive::ObjectDigest;
use crate::collaboration_state::CollaborationStore;
use crate::local_target::ControllerOwner;
use crate::local_wave::{Wave, WavePhase};
use crate::resolution::{self, ResolutionRequest, Trigger};

const MANAGED_ROUTE: &str = "local_managed_resolution_v1";

/// Data assembled only after the typed current conflict reader validates the
/// wave, request, source wrapper, and frozen plan. In particular, selected
/// roots and the price profile are derived from that archived plan, never the
/// command supplied by the caller.
pub(super) struct ValidatedConflict<'a> {
    pub command: &'a ManagedResolutionCommand,
    pub wave: &'a Wave,
    pub request: &'a ResolutionRequest,
    pub trigger_sha256: &'a str,
    pub selected_roots: &'a BTreeSet<RecordId>,
    pub price_profile: &'a str,
    pub budget_ceiling: Units,
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

/// Reserve and charge a new conflict attempt in one transaction. The caller
/// holds archive, response, and active-authority guards in that order and has
/// checked the exact current conflict binding before entering this function.
/// A caller must not turn a nonce collision without a binding into a replay.
pub(super) fn admit_new(
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
        || !command.envelope.within(validated.budget_ceiling)
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

    let (project_id, policy) = project(&transaction, owner.issuer())?;
    if policy.price_profile != validated.price_profile {
        return Err(BudgetError::Stale);
    }
    let (objective_id, objective_cap) =
        objective(&transaction, owner.issuer(), &spec.objective, &project_id)?;
    let spec = ReservationSpec {
        project_policy: project_id.to_string(),
        objective_policy: objective_id.to_string(),
        ..spec
    };
    let reservation_record = super::record(&spec)?;

    let duplicate_key: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM local_resolution_reservation_proofs
         WHERE target=?1 AND issuer=?2 AND client_key=?3)",
        params![owner.target(), owner.issuer(), command.client_key.as_str()],
        |row| row.get(0),
    )?;
    if duplicate_key || super::nonce_key(&transaction, &nonce)?.is_some() {
        return Err(BudgetError::Stale);
    }

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

    resolution::initialize_ledger_in_transaction(&transaction)?;
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
            validated.price_profile,
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
    let row: Option<(
        String,
        String,
        i64,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = transaction
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
    if target != owner.target()
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
    let digest = ObjectDigest::of(
        format!(
            "aethyme local resolution reservation nonce v1\0{}\0{}\0{}",
            owner.target(),
            owner.issuer(),
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
