//! Appointed Local controller integration. Only a private, freshly rechecked
//! acceptance grant can finish a wave. Receipts remain historical data.
use aethyme_contracts::experimental_v0::{FieldKind, FieldSpec, Record, RecordId, RecordSchema};
use rusqlite::{TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::Broker;
use crate::collaboration_archive as archive;
use crate::collaboration_gc::archive_use;
use crate::collaboration_state::CollaborationStore;
use crate::local_acceptance::{self as acceptance, QualifiedAcceptance};
use crate::local_source_transport::GitCarrier;
use crate::local_target::{self as target, ControllerOwner, Phase};
use crate::local_wave::{self as wave, WavePhase};

pub mod drive;
pub use drive::{DrivePolicy, DriveResult, drive};

const SCHEMA: RecordSchema = RecordSchema {
    name: "aethyme.local-controller-completion/experimental-v1",
    fields: &[FieldSpec {
        name: "spec",
        required: true,
        kind: FieldKind::Opaque,
        capability: None,
    }],
    capabilities: &[],
};

#[derive(Debug, thiserror::Error)]
pub enum ControllerError {
    #[error("controller completion does not match the exact current wave")]
    Mismatch,
    #[error("controller dispatch is held: {0}; explicit reconciliation is required")]
    DispatchHeld(String),
    #[error(transparent)]
    Snapshot(#[from] crate::execution_snapshot::SnapshotError),
    #[error(transparent)]
    Budget(#[from] crate::local_budget::BudgetError),
    #[error(transparent)]
    Supervisor(#[from] crate::execution_supervisor::SupervisorError),
    #[error(transparent)]
    Verification(#[from] crate::execution_verifier::VerificationError),
    #[error(transparent)]
    Resources(#[from] crate::execution_resources::ResourceError),
    #[error(transparent)]
    Native(#[from] crate::native_gates::NativeGateError),
    #[error("controller record is not canonical: {0}")]
    Canonical(String),
    #[error(transparent)]
    Target(#[from] target::TargetError),
    #[error(transparent)]
    Wave(#[from] wave::WaveError),
    #[error(transparent)]
    Acceptance(#[from] acceptance::AcceptanceError),
    #[error(transparent)]
    Transport(#[from] crate::local_source_transport::TransportError),
    #[error(transparent)]
    Archive(#[from] archive::ArchiveError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Historical statement of the private grant actually consumed. This value
/// confers no provider, verification, dispatch or future generation authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletionSpec {
    pub target: String,
    pub revision: String,
    pub epoch: String,
    pub wave: String,
    pub source: String,
    pub candidate_commit: String,
    pub candidate_tree: String,
    pub qualification: String,
    pub acceptance_receipt: String,
    pub accepted_commit: String,
    pub accepted_tree: String,
}
impl CompletionSpec {
    fn record(&self) -> Result<Record, ControllerError> {
        Record::decode(
            &serde_json::to_vec(&json!({"schema": SCHEMA.name,"spec":self}))?,
            &[&SCHEMA],
        )
        .map_err(|error| ControllerError::Canonical(error.to_string()))
    }
}

/// Finish an exact wave after the existing external verifier and provider APIs
/// supply a private grant. This re-observes present acceptance and producer
/// trust, and holds their guards through one atomic durable state commit.
/// Public target transitions still refuse to manufacture acceptance states.
/// Invoke after any requested generation preparation that also consumes the
/// grant: those APIs currently require the verifying phase.
pub fn complete(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    broker: &mut Broker,
    carrier: &GitCarrier,
    wave_id: &RecordId,
    grant: &QualifiedAcceptance<'_>,
) -> Result<CompletionSpec, ControllerError> {
    let _authority = target::authorize(store, owner)?;
    let _archive = archive_use(store)?;
    let current = acceptance::current_guard(store, owner, broker, carrier, grant)?;
    let state = target::status(store, owner.target())?;
    let active = wave::load(store, wave_id)?;
    let receipt = grant.receipt();
    let intent_id = RecordId::parse(&receipt.spec.intent).map_err(|_| ControllerError::Mismatch)?;
    let intent = crate::local_source_transport::load(store, &intent_id)?;
    let source = active
        .candidate_snapshot
        .as_ref()
        .ok_or(ControllerError::Mismatch)?;
    let commit = active
        .candidate_commit
        .as_ref()
        .ok_or(ControllerError::Mismatch)?;
    let _retained = archive::retained(store, source)?.ok_or(ControllerError::Mismatch)?;
    if state.phase != Phase::Verifying
        || state.revision.as_ref().map(RecordId::as_str) != Some(active.spec.revision.as_str())
        || active.phase != WavePhase::Verifying
        || active.spec.target != owner.target()
        || receipt.spec.decision != acceptance::ProviderDecision::Accepted
        || intent.spec.wave != wave_id.as_str()
        || receipt.spec.revision != active.spec.revision
        || receipt.spec.source != source.as_str()
        || receipt.spec.candidate_commit != commit.as_str()
        || receipt.spec.accepted_tree.as_deref() != Some(receipt.spec.candidate_tree.as_str())
    {
        return Err(ControllerError::Mismatch);
    }
    let spec = CompletionSpec {
        target: owner.target().into(),
        revision: active.spec.revision.clone(),
        epoch: owner.authority_epoch().as_str().into(),
        wave: wave_id.to_string(),
        source: source.to_string(),
        candidate_commit: commit.as_str().into(),
        candidate_tree: receipt.spec.candidate_tree.clone(),
        qualification: grant.id().to_string(),
        acceptance_receipt: receipt.id.to_string(),
        accepted_commit: receipt
            .spec
            .accepted_commit
            .clone()
            .ok_or(ControllerError::Mismatch)?,
        accepted_tree: receipt
            .spec
            .accepted_tree
            .clone()
            .ok_or(ControllerError::Mismatch)?,
    };
    let record = spec.record()?;
    let object = archive::put_object(store, &record.canonical_bytes())?;
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = tx.execute(
        "UPDATE local_targets SET phase='publication_confirmed',resume_phase=NULL,version=version+1 WHERE target=?1 AND active_revision=?2 AND phase='verifying' AND version=?3",
        params![owner.target(),active.spec.revision,state.version],
    )?;
    if changed != 1 {
        return Err(ControllerError::Mismatch);
    }
    if tx.execute(
        "UPDATE local_waves SET phase='accepted',detail=?2 WHERE wave=?1 AND phase='verifying'",
        params![wave_id.as_str(), record.id().as_str()],
    )? != 1
    {
        return Err(ControllerError::Mismatch);
    }
    for member in &active.spec.selected {
        if tx.execute("UPDATE local_target_dispositions SET status='delivered',detail=?3 WHERE revision=?1 AND contribution=?2 AND status='in_wave'", params![active.spec.revision,member,record.id().as_str()])? != 1 {
            return Err(ControllerError::Mismatch);
        }
    }
    tx.execute("INSERT INTO local_controller_completions(id,wave,revision,target,body) VALUES(?1,?2,?3,?4,?5)", params![record.id().as_str(),wave_id.as_str(),active.spec.revision,owner.target(),record.canonical_bytes()])?;
    // Keep complete provenance independently of later revision release.
    tx.execute("INSERT INTO local_controller_completion_roots(completion,kind,reference) SELECT ?1,kind,reference FROM local_target_roots WHERE revision=?2", params![record.id().as_str(),active.spec.revision])?;
    for (kind, reference) in [
        ("snapshot", source.as_str()),
        ("object", object.hex().as_str()),
    ] {
        tx.execute(
            "INSERT OR IGNORE INTO local_controller_completion_roots VALUES(?1,?2,?3)",
            params![record.id().as_str(), kind, reference],
        )?;
    }
    current.check_clock()?;
    tx.commit()?;
    Ok(spec)
}

/// Read a historical completion. Canonical records and their SQL projections
/// must agree; this function cannot return a current private acceptance grant.
pub fn load(store: &CollaborationStore, key: &RecordId) -> Result<CompletionSpec, ControllerError> {
    let row: (Vec<u8>, String, String, String) = store.read_connection().query_row(
        "SELECT body,wave,revision,target FROM local_controller_completions WHERE id=?1",
        [key.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let record = Record::decode(&row.0, &[&SCHEMA])
        .map_err(|error| ControllerError::Canonical(error.to_string()))?;
    let spec: CompletionSpec = serde_json::from_value(
        serde_json::from_slice::<serde_json::Value>(&row.0)?["spec"].clone(),
    )?;
    if record.id() != *key
        || record.canonical_bytes() != row.0
        || (
            spec.wave.as_str(),
            spec.revision.as_str(),
            spec.target.as_str(),
        ) != (row.1.as_str(), row.2.as_str(), row.3.as_str())
    {
        return Err(ControllerError::Mismatch);
    }
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn historical_completion_encoding_binds_every_identity() {
        let id = |label: &str| {
            format!(
                "sha256:{}",
                archive::ObjectDigest::of(label.as_bytes()).hex()
            )
        };
        let spec = CompletionSpec {
            target: "playground".into(),
            revision: id("revision"),
            epoch: "e".repeat(64),
            wave: id("wave"),
            source: id("source"),
            candidate_commit: "c".repeat(40),
            candidate_tree: "d".repeat(40),
            qualification: id("private qualification"),
            acceptance_receipt: id("receipt"),
            accepted_commit: "c".repeat(40),
            accepted_tree: "d".repeat(40),
        };
        let record = spec.record().unwrap();
        assert_eq!(
            Record::decode(&record.canonical_bytes(), &[&SCHEMA])
                .unwrap()
                .id(),
            record.id()
        );
        for field in [
            "target",
            "revision",
            "epoch",
            "wave",
            "source",
            "candidate_commit",
            "candidate_tree",
            "qualification",
            "acceptance_receipt",
            "accepted_commit",
            "accepted_tree",
        ] {
            let mut changed = serde_json::to_value(&spec).unwrap();
            changed[field] = json!(format!("{}changed", changed[field].as_str().unwrap()));
            let changed: CompletionSpec = serde_json::from_value(changed).unwrap();
            assert_ne!(
                record.id(),
                changed.record().unwrap().id(),
                "unbound historical completion field {field}"
            );
        }
    }
    #[test]
    fn public_transition_cannot_manufacture_delivery() {
        let mut f = target::tests::Fixture::new();
        let owner = target::acquire_controller(&mut f.store, "completion", "appointed").unwrap();
        let spec = f.spec();
        let revision = target::propose_revision(&mut f.store, &owner, spec).unwrap();
        target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
        for next in [
            Phase::Admitting,
            Phase::WaveFrozen,
            Phase::Composing,
            Phase::Verifying,
        ] {
            let state = target::status(&f.store, owner.target()).unwrap();
            target::transition(&mut f.store, &owner, state.version, next).unwrap();
        }
        let state = target::status(&f.store, owner.target()).unwrap();
        assert!(matches!(
            target::transition(
                &mut f.store,
                &owner,
                state.version,
                Phase::ReadyForAcceptance
            ),
            Err(target::TargetError::VerificationUnavailable)
        ));
        assert_eq!(target::status(&f.store, owner.target()).unwrap(), state);
        assert!(
            target::dispositions(&f.store, &revision.id)
                .unwrap()
                .iter()
                .all(|entry| entry.status != target::Disposition::Delivered)
        );
        let count: i64 = f
            .store
            .read_connection()
            .query_row(
                "SELECT COUNT(*) FROM local_controller_completions",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }
}
