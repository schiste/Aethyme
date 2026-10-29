//! Exact Local source acceptance (#675). Native provider reads are observations;
//! canonical receipts are historical data. Current permission also requires the
//! appointed verifier's private evidence. No merge or policy change is performed.
use crate::collaboration_archive::{self as archive, CommitOid, ObjectDigest};
use crate::collaboration_state::CollaborationStore;
use crate::execution_snapshot::ExecutionSnapshot;
use crate::execution_supervisor::SealedArtifact;
use crate::execution_verifier::{self as verifier, ProducerPermit, TrustedEvidence};
use crate::local_source_transport::{self as transport, GitCarrier, Intent};
use crate::local_target::{self as target, ControllerOwner};
use crate::{Broker, QueueWait};
use aethyme_contracts::experimental_v0::{FieldKind, FieldSpec, Record, RecordId, RecordSchema};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

pub const PROFILE: &str = "aethyme.acceptance.exact-source/provisional-v0";
static BINDING: RecordSchema = schema("aethyme.acceptance-binding/provisional-v0");
static RECEIPT: RecordSchema = schema("aethyme.acceptance-receipt/provisional-v0");
static QUALIFICATION: RecordSchema = schema("aethyme.acceptance-qualification/provisional-v0");
static HISTORICAL_EVIDENCE: RecordSchema = RecordSchema {
    name: "aethyme.verification-evidence/provisional-v0",
    fields: &[FieldSpec {
        name: "observation",
        required: true,
        kind: FieldKind::Opaque,
        capability: None,
    }],
    capabilities: &[],
};
static HISTORICAL_ARTIFACT: RecordSchema = RecordSchema {
    name: crate::execution_supervisor::ARTIFACT_SCHEMA,
    fields: &[
        FieldSpec {
            name: "execution",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
        FieldSpec {
            name: "target_revision",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
        FieldSpec {
            name: "output",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
        FieldSpec {
            name: "packaging",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
        FieldSpec {
            name: "runtime",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
        FieldSpec {
            name: "generation",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
    ],
    capabilities: &[],
};
const fn schema(name: &'static str) -> RecordSchema {
    RecordSchema {
        name,
        fields: &[FieldSpec {
            name: "spec",
            required: true,
            kind: FieldKind::Opaque,
            capability: None,
        }],
        capabilities: &[],
    }
}
#[derive(Debug, thiserror::Error)]
pub enum AcceptanceError {
    #[error("invalid acceptance binding or provider subject: {0}")]
    Invalid(String),
    #[error("acceptance binding is missing")]
    Missing,
    #[error(
        "historical qualification has no exact {0} object identity; legacy proof cannot be reused"
    )]
    MissingHistoricalObjectIdentity(&'static str),
    #[error("finite provider observation allowance exhausted; explicit new decision required")]
    Exhausted,
    #[error("provider is unavailable or its response is incomplete")]
    Unavailable,
    #[error("provider has not accepted this exact source")]
    NotAccepted,
    #[error(transparent)]
    Transport(#[from] transport::TransportError),
    #[error(transparent)]
    Wave(#[from] crate::local_wave::WaveError),
    #[error(transparent)]
    Target(#[from] target::TargetError),
    #[error(transparent)]
    Archive(#[from] archive::ArchiveError),
    #[error(transparent)]
    Verification(#[from] verifier::VerificationError),
    #[error(transparent)]
    Snapshot(#[from] crate::execution_snapshot::SnapshotError),
    #[error(transparent)]
    Sensitive(#[from] crate::collaboration_sensitive::SensitiveError),
    #[error(transparent)]
    Broker(#[from] crate::BrokerOpError),
    #[error(transparent)]
    Git(#[from] crate::GitError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
fn invalid(message: &str) -> AcceptanceError {
    AcceptanceError::Invalid(message.into())
}
fn canonical<T: Serialize>(
    schema: &'static RecordSchema,
    value: &T,
) -> Result<Record, AcceptanceError> {
    Record::decode(
        &serde_json::to_vec(&serde_json::json!({"schema":schema.name,"spec":value}))?,
        &[schema],
    )
    .map_err(|e| AcceptanceError::Invalid(e.to_string()))
}
fn id(value: &str) -> Result<RecordId, AcceptanceError> {
    RecordId::parse(value).map_err(|_| invalid("record ID"))
}
fn now() -> Result<i64, AcceptanceError> {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| invalid("clock"))?
            .as_millis(),
    )
    .map_err(|_| invalid("clock"))
}
fn sha(value: &str) -> Result<CommitOid, AcceptanceError> {
    if value.len() != 40 {
        return Err(invalid("SHA-1 profile requires concrete full commit"));
    }
    Ok(CommitOid::parse(value)?)
}
fn token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.[]".contains(&c))
}

/// Operator-appointed routes. Generic Git is only an explicitly authorized ref
/// route; it cannot represent mandatory review or Mergify semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum ProviderRoute {
    GenericGit,
    Github { pull_request: u64 },
    Mergify { pull_request: u64, merger: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindingSpec {
    pub profile: String,
    pub intent: String,
    pub revision: String,
    pub epoch: String,
    pub route: ProviderRoute,
    pub base_ref: String,
    pub policy_commit: String,
    pub config_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mergify_digest: Option<String>,
    pub verifier_policy: String,
    pub max_observations: u8,
}
/// Constructed by appointment or checked native journal load, never by decoding
/// an incoming canonical record. Loading grants no acceptance permission.
#[derive(Debug)]
pub struct AcceptanceBinding {
    id: RecordId,
    spec: BindingSpec,
}
impl AcceptanceBinding {
    pub fn id(&self) -> &RecordId {
        &self.id
    }
    pub fn spec(&self) -> &BindingSpec {
        &self.spec
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderDecision {
    Waiting,
    Rejected,
    MissingReview,
    TargetDrift,
    Unavailable,
    Accepted,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptSpec {
    pub profile: String,
    pub binding: String,
    pub intent: String,
    pub revision: String,
    pub source: String,
    pub candidate_commit: String,
    pub candidate_tree: String,
    pub policy_commit: String,
    pub config_digest: String,
    pub route: ProviderRoute,
    pub peer: String,
    pub base_ref: String,
    pub decision: ProviderDecision,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted_tree: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_digest: Option<String>,
    pub observed_ms: i64,
    pub sequence: u8,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptanceReceipt {
    pub id: RecordId,
    pub spec: ReceiptSpec,
}

/// The exact historical identities a Local completion froze when it consumed
/// a private acceptance grant. This record validates provenance for baseline
/// reuse only; it is not a current acceptance grant or a producer permit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HistoricalCompletionIdentity {
    pub(crate) completion: String,
    pub(crate) target: String,
    pub(crate) issuer: String,
    pub(crate) revision: String,
    pub(crate) epoch: String,
    pub(crate) wave: String,
    pub(crate) source: String,
    pub(crate) candidate_commit: String,
    pub(crate) candidate_tree: String,
    pub(crate) qualification: String,
    pub(crate) receipt: String,
    pub(crate) accepted_commit: String,
    pub(crate) accepted_tree: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoricalQualificationSpec {
    binding: String,
    receipt: String,
    evidence: String,
    execution: String,
    // `default` keeps old persisted specs readable for diagnostics. Historical
    // verification refuses them when these raw CAS identities are absent; a
    // domain-separated RecordId is not an object digest.
    #[serde(default)]
    execution_object_sha256: Option<String>,
    #[serde(default)]
    inventory_object_sha256: Option<String>,
    #[serde(default)]
    artifact_object_sha256: Option<String>,
    epoch: String,
    expires_ms: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoricalEvidenceBody {
    verification: String,
    producer: String,
    generation: String,
    epoch: String,
    revision: String,
    execution: String,
    candidate: String,
    artifact: String,
    output: String,
    policy: String,
    runtime: String,
    image: String,
    harness: String,
    inventory: String,
    profile: String,
    result: verifier::VerificationOutcome,
    assertions: Vec<HistoricalAssertionObservation>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoricalAssertionObservation {
    id: String,
    outcome: verifier::VerificationOutcome,
    exit_code: Option<i32>,
    stdout: Option<String>,
    stderr: Option<String>,
}

fn historical_object_digest(
    value: Option<&str>,
    kind: &'static str,
) -> Result<ObjectDigest, AcceptanceError> {
    let value = value.ok_or(AcceptanceError::MissingHistoricalObjectIdentity(kind))?;
    archive::parse_digest(value).ok_or_else(|| invalid("historical object digest"))
}

fn inventory_object_for_execution(
    store: &CollaborationStore,
    execution: &ExecutionSnapshot,
) -> Result<ObjectDigest, AcceptanceError> {
    let harness_id =
        aethyme_contracts::experimental_v0::SourceSnapshotId::parse(&execution.spec().harness)
            .map_err(|_| invalid("historical verifier harness"))?;
    let manifest_object = ObjectDigest::of_snapshot(&harness_id);
    let manifest_bytes = archive::read_object(store, &manifest_object)?;
    let manifest = archive::parse_manifest(&manifest_bytes).ok_or(AcceptanceError::Missing)?;
    if manifest.id() != harness_id || manifest.manifest_bytes() != manifest_bytes {
        return Err(invalid("historical verifier harness manifest"));
    }
    let entry = manifest
        .entries()
        .iter()
        .find(|entry| entry.path() == b"harness/inventory.json")
        .ok_or_else(|| invalid("historical verifier inventory file"))?;
    if entry.kind() != aethyme_contracts::experimental_v0::source_snapshot::EntryKind::Regular {
        return Err(invalid("historical verifier inventory file kind"));
    }
    Ok(ObjectDigest::from_bytes(*entry.content_sha256()))
}

fn lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn completion_rooted(
    store: &CollaborationStore,
    completion: &RecordId,
    kind: &str,
    reference: &str,
) -> Result<bool, AcceptanceError> {
    Ok(store.read_connection().query_row(
        "SELECT EXISTS(SELECT 1 FROM local_controller_completion_roots
         WHERE completion=?1 AND kind=?2 AND reference=?3)",
        params![completion.as_str(), kind, reference],
        |row| row.get(0),
    )?)
}

/// Revalidate an accepted completion's historical qualification without
/// querying provider state. A qualification row can be written only by
/// qualify_receipt, after its private QualifiedAcceptance capability has
/// passed the current verifier guard and its exact receipt is accepted. We
/// verify that sealed-row identity and successful evidence chain here, but
/// deliberately do not turn it into a present grant, permit, or settlement.
pub(crate) fn verify_historical_completion(
    store: &CollaborationStore,
    completion: &HistoricalCompletionIdentity,
) -> Result<(), AcceptanceError> {
    let _archive = crate::collaboration_gc::archive_use(store).map_err(AcceptanceError::Io)?;
    let _response = crate::collaboration_sensitive::response_use(store)?;
    let completion_id = id(&completion.completion)?;
    let qualification_id = id(&completion.qualification)?;
    let expected_receipt_id = id(&completion.receipt)?;
    let expected_wave_id = id(&completion.wave)?;
    let expected_revision_id = id(&completion.revision)?;
    let expected_epoch_id = id(&completion.epoch)?;
    let expected_source =
        aethyme_contracts::experimental_v0::SourceSnapshotId::parse(&completion.source)
            .map_err(|_| invalid("historical completion source"))?;
    let candidate_commit = sha(&completion.candidate_commit)?;
    let candidate_tree = sha(&completion.candidate_tree)?;
    let accepted_commit = sha(&completion.accepted_commit)?;
    let accepted_tree = sha(&completion.accepted_tree)?;
    if completion.target.trim().is_empty()
        || completion.issuer.trim().is_empty()
        || candidate_tree != accepted_tree
    {
        return Err(invalid("historical completion identity"));
    }

    let stored_completion = crate::local_controller::load(store, &completion_id)
        .map_err(|_| invalid("historical completion record"))?;
    if stored_completion.target != completion.target
        || stored_completion.revision != completion.revision
        || stored_completion.epoch != completion.epoch
        || stored_completion.wave != completion.wave
        || stored_completion.source != completion.source
        || stored_completion.candidate_commit != completion.candidate_commit
        || stored_completion.candidate_tree != completion.candidate_tree
        || stored_completion.qualification != completion.qualification
        || stored_completion.acceptance_receipt != completion.receipt
        || stored_completion.accepted_commit != completion.accepted_commit
        || stored_completion.accepted_tree != completion.accepted_tree
    {
        return Err(invalid("historical completion identity"));
    }
    let completion_bytes: Vec<u8> = store.read_connection().query_row(
        "SELECT body FROM local_controller_completions WHERE id=?1",
        [completion_id.as_str()],
        |row| row.get(0),
    )?;
    let completion_object = ObjectDigest::of(&completion_bytes);
    if archive::read_object(store, &completion_object)? != completion_bytes
        || !completion_rooted(store, &completion_id, "object", &completion_object.hex())?
    {
        return Err(invalid("historical completion is not retained"));
    }

    let (qualification_binding, qualification_bytes): (String, Vec<u8>) = store
        .read_connection()
        .query_row(
            "SELECT binding,body FROM local_acceptance_qualifications WHERE id=?1",
            [qualification_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or(AcceptanceError::Missing)?;
    let qualification_record = Record::decode(&qualification_bytes, &[&QUALIFICATION])
        .map_err(|_| invalid("historical qualification record"))?;
    let qualification_value: serde_json::Value = serde_json::from_slice(&qualification_bytes)?;
    let qualification: HistoricalQualificationSpec =
        serde_json::from_value(qualification_value["spec"].clone())
            .map_err(|_| invalid("historical qualification payload"))?;
    let binding_id = id(&qualification.binding)?;
    let evidence_id = id(&qualification.evidence)?;
    let execution_id = id(&qualification.execution)?;
    let execution_object = historical_object_digest(
        qualification.execution_object_sha256.as_deref(),
        "execution",
    )?;
    let inventory_object = historical_object_digest(
        qualification.inventory_object_sha256.as_deref(),
        "assertion inventory",
    )?;
    let artifact_object = historical_object_digest(
        qualification.artifact_object_sha256.as_deref(),
        "sealed artifact",
    )?;
    let qualification_object = archive::ObjectDigest::of(&qualification_bytes);
    let qualification_object_retained = archive::read_object(store, &qualification_object)?;
    let qualification_rooted =
        completion_rooted(store, &completion_id, "object", &qualification_object.hex())?;
    if qualification_record.id() != qualification_id
        || qualification_record.canonical_bytes() != qualification_bytes
        || qualification_object_retained != qualification_bytes
        || !qualification_rooted
        || qualification_binding != binding_id.as_str()
        || qualification.receipt != expected_receipt_id.as_str()
        || qualification.epoch != expected_epoch_id.as_str()
        || qualification.expires_ms <= 0
    {
        return Err(invalid("historical qualification identity"));
    }

    let binding = load(store, &binding_id)?;
    if binding.spec().revision != completion.revision
        || binding.spec().epoch != expected_epoch_id.as_str()
        || binding.spec().intent.is_empty()
    {
        return Err(invalid("historical acceptance binding"));
    }
    let intent_id = id(&binding.spec().intent)?;
    let intent = transport::load(store, &intent_id)?;
    if intent.id != intent_id
        || intent.spec.target != completion.target
        || intent.spec.issuer != completion.issuer
        || intent.spec.epoch != expected_epoch_id.as_str()
        || intent.spec.revision != completion.revision
        || intent.spec.wave != expected_wave_id.as_str()
        || intent.spec.source != expected_source.as_str()
        || intent.spec.commit != candidate_commit.as_str()
        || intent.spec.tree != candidate_tree.as_str()
    {
        return Err(invalid("historical acceptance intent"));
    }

    let (receipt_binding, receipt_bytes): (String, Vec<u8>) = store
        .read_connection()
        .query_row(
            "SELECT binding,body FROM local_acceptance_receipts WHERE id=?1",
            [expected_receipt_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or(AcceptanceError::Missing)?;
    let receipt_record = Record::decode(&receipt_bytes, &[&RECEIPT])
        .map_err(|_| invalid("historical receipt record"))?;
    let receipt_value: serde_json::Value = serde_json::from_slice(&receipt_bytes)?;
    let receipt_spec: ReceiptSpec = serde_json::from_value(receipt_value["spec"].clone())
        .map_err(|_| invalid("historical receipt payload"))?;
    if receipt_record.id() != expected_receipt_id
        || receipt_record.canonical_bytes() != receipt_bytes
        || receipt_binding != binding.id().as_str()
        || receipt_spec.profile != PROFILE
        || receipt_spec.binding != binding.id().as_str()
        || receipt_spec.intent != intent.id.as_str()
        || receipt_spec.revision != completion.revision
        || receipt_spec.source != expected_source.as_str()
        || receipt_spec.candidate_commit != candidate_commit.as_str()
        || receipt_spec.candidate_tree != candidate_tree.as_str()
        || receipt_spec.policy_commit != binding.spec().policy_commit
        || receipt_spec.config_digest != binding.spec().config_digest
        || receipt_spec.route != binding.spec().route
        || receipt_spec.peer != intent.spec.peer
        || receipt_spec.base_ref != binding.spec().base_ref
        || receipt_spec.decision != ProviderDecision::Accepted
        || receipt_spec.accepted_commit.as_deref() != Some(accepted_commit.as_str())
        || receipt_spec.accepted_tree.as_deref() != Some(accepted_tree.as_str())
        || receipt_spec.sequence == 0
        || receipt_spec.sequence > binding.spec().max_observations
        || receipt_spec.observed_ms <= 0
    {
        return Err(invalid("historical accepted receipt binding"));
    }

    let execution = crate::execution_snapshot::load_from_completion(
        store,
        &crate::execution_snapshot::SnapshotRef {
            id: execution_id.clone(),
            object: execution_object,
        },
        &completion_id,
    )?;
    if execution.target_revision() != &expected_revision_id
        || execution.spec().candidate_source != expected_source.as_str()
    {
        return Err(invalid("historical acceptance execution"));
    }

    let (
        evidence_object_hex,
        evidence_revision,
        evidence_producer,
        evidence_generation,
        evidence_issued_ms,
        evidence_expires_ms,
        evidence_bytes,
    ): (String, String, String, i64, i64, i64, Vec<u8>) = store
        .read_connection()
        .query_row(
            "SELECT object,revision,producer,generation,issued_ms,expires_ms,body
             FROM local_verification_evidence WHERE id=?1",
            [evidence_id.as_str()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()?
        .ok_or(AcceptanceError::Missing)?;
    if evidence_bytes.len() > 1024 * 1024 {
        return Err(invalid("historical evidence size"));
    }
    let evidence_object =
        archive::parse_digest(&evidence_object_hex).ok_or_else(|| invalid("evidence object"))?;
    let evidence_record = Record::decode(&evidence_bytes, &[&HISTORICAL_EVIDENCE])
        .map_err(|_| invalid("historical evidence record"))?;
    let evidence_value: serde_json::Value = serde_json::from_slice(&evidence_bytes)?;
    let evidence: HistoricalEvidenceBody =
        serde_json::from_value(evidence_value["observation"].clone())
            .map_err(|_| invalid("historical evidence payload"))?;
    let artifact_id = id(&evidence.artifact)?;
    let output_snapshot =
        aethyme_contracts::experimental_v0::SourceSnapshotId::parse(&evidence.output)
            .map_err(|_| invalid("historical evidence output"))?;
    let rooted = completion_rooted(store, &completion_id, "object", &evidence_object_hex)?;
    let retained_evidence = archive::read_object(store, &evidence_object)?;
    let inventory_id = id(&execution.spec().assertion_inventory)?;
    let harness_id =
        aethyme_contracts::experimental_v0::SourceSnapshotId::parse(&execution.spec().harness)
            .map_err(|_| invalid("historical verifier harness"))?;
    let harness_manifest_bytes =
        archive::read_object(store, &ObjectDigest::of_snapshot(&harness_id))?;
    let harness_manifest =
        archive::parse_manifest(&harness_manifest_bytes).ok_or(AcceptanceError::Missing)?;
    let inventory_entry = harness_manifest
        .entries()
        .iter()
        .find(|entry| entry.path() == b"harness/inventory.json")
        .ok_or_else(|| invalid("historical verifier inventory file"))?;
    let inventory_embedded = inventory_entry.kind()
        == aethyme_contracts::experimental_v0::source_snapshot::EntryKind::Regular
        && ObjectDigest::from_bytes(*inventory_entry.content_sha256()) == inventory_object;
    if !inventory_embedded {
        return Err(invalid("historical verifier inventory object identity"));
    }
    let inventory_bytes = archive::read_object_bounded(store, &inventory_object, 4 * 1024 * 1024)?;
    let inventory = verifier::AssertionInventory::decode(&inventory_bytes)?;
    if inventory.id() != inventory_id || inventory.canonical_bytes() != inventory_bytes {
        return Err(invalid("historical verifier inventory record"));
    }
    let artifact_bytes = archive::read_object(store, &artifact_object)?;
    let artifact_record = Record::decode(&artifact_bytes, &[&HISTORICAL_ARTIFACT])
        .map_err(|_| invalid("historical artifact record"))?;
    let artifact_value: serde_json::Value = serde_json::from_slice(&artifact_bytes)?;
    let expected_packaging = format!(
        "sha256:{}",
        ObjectDigest::of(&serde_json::to_vec(&execution.spec().build)?).hex()
    );
    let artifact_rooted =
        completion_rooted(store, &completion_id, "object", &artifact_object.hex())?;
    let output_manifest_object = ObjectDigest::of_snapshot(&output_snapshot);
    let output_manifest_bytes = archive::read_object(store, &output_manifest_object)?;
    let output_manifest =
        archive::parse_manifest(&output_manifest_bytes).ok_or(AcceptanceError::Missing)?;
    let output_manifest_matches = output_manifest.id() == output_snapshot
        && output_manifest.manifest_bytes() == output_manifest_bytes;
    let mut output_closure_retained = completion_rooted(
        store,
        &completion_id,
        "object",
        &output_manifest_object.hex(),
    )?;
    for entry in output_manifest.entries() {
        let digest = ObjectDigest::from_bytes(*entry.content_sha256());
        output_closure_retained &=
            completion_rooted(store, &completion_id, "object", &digest.hex())?;
        archive::read_object(store, &digest)?;
    }
    let artifact_matches = artifact_record.id() == artifact_id
        && artifact_record.canonical_bytes() == artifact_bytes
        && artifact_value["execution"] == execution_id.as_str()
        && artifact_value["target_revision"] == completion.revision
        && artifact_value["output"] == output_snapshot.as_str()
        && artifact_value["packaging"] == expected_packaging
        && artifact_value["runtime"] == evidence.runtime
        && artifact_value["generation"]
            .as_str()
            .is_some_and(|generation| !generation.is_empty());
    let assertions_match = evidence.assertions.len() == inventory.cases().len()
        && inventory_embedded
        && inventory
            .cases()
            .iter()
            .zip(&evidence.assertions)
            .all(|(case, observed)| {
                case.exit_code == 0
                    && observed.id == case.id
                    && observed.outcome == verifier::VerificationOutcome::Passed
                    && observed.exit_code == Some(i32::from(case.exit_code))
                    && observed.stdout.as_deref() == Some(case.stdout.as_str())
                    && observed.stderr.as_deref() == Some(case.stderr.as_str())
            });
    if evidence_record.id() != evidence_id
        || evidence_record.canonical_bytes() != evidence_bytes
        || archive::ObjectDigest::of(&evidence_bytes) != evidence_object
        || retained_evidence != evidence_bytes
        || !rooted
        || !artifact_rooted
        || !output_closure_retained
        || !output_manifest_matches
        || !artifact_matches
        || evidence_revision != completion.revision
        || evidence_generation <= 0
        || evidence_issued_ms <= 0
        || evidence_expires_ms != qualification.expires_ms
        || evidence_expires_ms <= evidence_issued_ms
        || evidence.producer != evidence_producer
        || evidence.generation != evidence_generation.to_string()
        || evidence.epoch != expected_epoch_id.as_str()
        || evidence.revision != completion.revision
        || evidence.execution != execution_id.as_str()
        || evidence.candidate != expected_source.as_str()
        || evidence.policy != binding.spec().verifier_policy
        || evidence.harness != execution.spec().harness
        || evidence.inventory != execution.spec().assertion_inventory
        || evidence.runtime.trim().is_empty()
        || evidence.image.trim().is_empty()
        || evidence.profile != verifier::PROFILE
        || !lower_hex(&evidence.verification, 64)
        || evidence.result != verifier::VerificationOutcome::Passed
        || !assertions_match
    {
        return Err(invalid("historical qualified verifier evidence"));
    }

    Ok(())
}
/// A grant is private, current, epoch-bound, and non-deserializable. Its public
/// representation is a receipt, which cannot be imported as present authority.
pub struct QualifiedAcceptance<'a> {
    id: RecordId,
    receipt: AcceptanceReceipt,
    evidence: &'a TrustedEvidence,
    execution: &'a ExecutionSnapshot,
    artifact: &'a SealedArtifact,
    permit: &'a ProducerPermit,
}
impl QualifiedAcceptance<'_> {
    pub fn id(&self) -> &RecordId {
        &self.id
    }
    pub fn receipt(&self) -> &AcceptanceReceipt {
        &self.receipt
    }
    pub fn expires_ms(&self) -> i64 {
        self.evidence.expires_ms()
    }
}

type CommittedPolicies = (
    Vec<u8>,
    crate::ReviewPolicy,
    crate::ShipPublicationPolicy,
    Option<Vec<u8>>,
);
fn policies(carrier: &GitCarrier, commit: &str) -> Result<CommittedPolicies, AcceptanceError> {
    let commit = sha(commit)?;
    // `.aethyme/config` in either format (#1223), as TOML text.
    let text = aethyme_config::try_read_config_text_with(
        aethyme_config::CONFIG_DIR,
        "config",
        |relpath| {
            archive::raw_file_at_commit(carrier.repo.root(), &commit, relpath.as_bytes())?
                .map(|bytes| String::from_utf8(bytes).map_err(|_| invalid("policy UTF-8")))
                .transpose()
        },
    )?
    .and_then(|text| {
        text.map(|text| text.toml_text().map(|toml| toml.into_owned()))
            .transpose()
    })
    .map_err(|error| invalid(&error.to_string()))?
    .unwrap_or_default()
    .into_bytes();
    let utf = std::str::from_utf8(&text).map_err(|_| invalid("policy UTF-8"))?;
    let review = crate::ReviewPolicy::from_config_text(utf)?;
    let publication = crate::ShipPublicationPolicy::from_config_text(utf)?;
    let a = archive::raw_file_at_commit(carrier.repo.root(), &commit, b".mergify.yml")?;
    let b = archive::raw_file_at_commit(carrier.repo.root(), &commit, b".mergify.yaml")?;
    if a.is_some() && b.is_some() {
        return Err(invalid("ambiguous Mergify policy paths"));
    }
    Ok((text, review, publication, a.or(b)))
}
fn query_ref(carrier: &GitCarrier, reference: &str) -> Result<Option<String>, AcceptanceError> {
    Ok(
        crate::git::with_git_deadline(Duration::from_secs(carrier.timeout), || {
            carrier
                .repo
                .remote_fetch_ref_oids(&carrier.remote, &[reference.into()])
        })?
        .remove(reference)
        .flatten(),
    )
}
fn fetch(
    broker: &mut Broker,
    carrier: &GitCarrier,
    intent: &Intent,
    commit: &str,
) -> Result<(), AcceptanceError> {
    sha(commit)?;
    carrier.check_broker(broker)?;
    carrier.check()?;
    let args = vec![
        "fetch".into(),
        "--no-write-fetch-head".into(),
        "--no-tags".into(),
        "--refmap=".into(),
        "--no-auto-gc".into(),
        "--no-recurse-submodules".into(),
        carrier.remote.clone(),
        commit.into(),
    ];
    let report = crate::git::with_git_deadline(Duration::from_secs(carrier.timeout), || {
        broker.run_coordinated_operation_at_with_hooks(
            carrier.command(intent, args),
            carrier.repo.root(),
            QueueWait::Seconds(carrier.timeout),
            || carrier.check().map_err(|e| e.to_string()),
            |_, _| Ok(None),
        )
    })?;
    if !report.ok() {
        return Err(AcceptanceError::Unavailable);
    }
    Ok(())
}
fn check_owner(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    intent: &Intent,
) -> Result<(), AcceptanceError> {
    if intent.spec.project != store.project().as_str()
        || intent.spec.target != owner.target()
        || intent.spec.issuer != owner.issuer()
        || intent.spec.epoch != owner.authority_epoch().as_str()
    {
        return Err(invalid("owner/project/epoch"));
    }
    Ok(())
}
/// Pin the existing committed policies at the native remote base. Appointment
/// does not publish, merge, unlock review, or grant verifier permission.
#[allow(clippy::too_many_arguments)]
pub fn appoint(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    broker: &mut Broker,
    carrier: &GitCarrier,
    intent_id: &RecordId,
    route: ProviderRoute,
    base_ref: &str,
    max_observations: u8,
) -> Result<AcceptanceBinding, AcceptanceError> {
    let intent = transport::load(store, intent_id)?;
    check_owner(store, owner, &intent)?;
    transport::route(carrier, &intent)?;
    carrier.check_broker(broker)?;
    carrier.repo.validate_push_destination(base_ref)?;
    if !base_ref.starts_with("refs/heads/") || !(1..=64).contains(&max_observations) {
        return Err(invalid("branch and finite observation allowance"));
    }
    let state = target::status(store, owner.target())?;
    if state.phase != target::Phase::Verifying
        || state.revision.as_ref().map(RecordId::as_str) != Some(intent.spec.revision.as_str())
    {
        return Err(invalid("current verifying revision required"));
    }
    match &route {
        ProviderRoute::GenericGit if base_ref != carrier.destination => {
            return Err(invalid(
                "generic Git requires this exact authorized destination ref",
            ));
        }
        ProviderRoute::Github { pull_request } | ProviderRoute::Mergify { pull_request, .. }
            if *pull_request == 0 || carrier.identity.normalized_host != "github.com" =>
        {
            return Err(invalid("GitHub.com route and exact PR required"));
        }
        ProviderRoute::Mergify { merger, .. } if !token(merger) => {
            return Err(invalid("appointed Mergify actor"));
        }
        _ => {}
    }
    let policy_commit = query_ref(carrier, base_ref)?.ok_or(AcceptanceError::Unavailable)?;
    fetch(broker, carrier, &intent, &policy_commit)?;
    let (config, review, publication, mergify) = policies(carrier, &policy_commit)?;
    if route == ProviderRoute::GenericGit
        && (review.enabled || publication.mode == crate::ShipPublicationMode::ReviewGated)
    {
        return Err(invalid("generic Git cannot satisfy mandatory review"));
    }
    if matches!(route, ProviderRoute::Mergify { .. }) && mergify.is_none() {
        return Err(invalid("committed Mergify policy required"));
    }
    let archive_guard = crate::collaboration_gc::archive_use(store)?;
    let response_guard = crate::collaboration_sensitive::response_use(store)?;
    let authority_guard = target::authorize(store, owner)?;
    let retention = target::ArtifactRetentionPermit::new(
        store,
        owner,
        &archive_guard,
        &response_guard,
        &authority_guard,
    )?;
    let intent = transport::load(store, intent_id)?;
    check_owner(store, owner, &intent)?;
    let state = target::status(store, owner.target())?;
    let wave = crate::local_wave::load(store, &id(&intent.spec.wave)?)?;
    if state.phase != target::Phase::Verifying
        || state.revision.as_ref().map(RecordId::as_str) != Some(intent.spec.revision.as_str())
        || wave.phase != crate::local_wave::WavePhase::Verifying
        || wave.spec.revision != intent.spec.revision
        || wave.candidate_snapshot.as_ref().map(ToString::to_string)
            != Some(intent.spec.source.clone())
        || wave.candidate_commit.as_ref().map(CommitOid::as_str)
            != Some(intent.spec.commit.as_str())
    {
        return Err(invalid(
            "target or candidate wave changed during policy discovery",
        ));
    }
    let revision_id = id(&intent.spec.revision)?;
    let revision = target::load_revision_under_response(store, &revision_id, &response_guard)?;
    if revision.target != owner.target()
        || revision.issuer != owner.issuer()
        || revision.id != revision_id
    {
        return Err(invalid("current target revision owner"));
    }
    let spec = BindingSpec {
        profile: PROFILE.into(),
        intent: intent_id.to_string(),
        revision: intent.spec.revision,
        epoch: owner.authority_epoch().as_str().into(),
        route,
        base_ref: base_ref.into(),
        policy_commit,
        config_digest: ObjectDigest::of(&config).hex(),
        mergify_digest: mergify.as_ref().map(|b| ObjectDigest::of(b).hex()),
        verifier_policy: revision.spec.acceptance_policy,
        max_observations,
    };
    let record = canonical(&BINDING, &spec)?;
    let object = archive::put_object(store, &record.canonical_bytes())?;
    let config_object = archive::put_object(store, &config)?;
    let mut objects = vec![object, config_object];
    if let Some(bytes) = mergify {
        objects.push(archive::put_object(store, &bytes)?);
    }
    target::retain_artifacts_under_guards(store, &revision_id, &[], &objects, &retention)?;
    store.connection().execute("INSERT OR IGNORE INTO local_acceptance_bindings(id,revision,body,last_receipt) VALUES(?1,?2,?3,NULL)",params![record.id().as_str(),spec.revision,record.canonical_bytes()])?;
    Ok(AcceptanceBinding {
        id: record.id(),
        spec,
    })
}
pub fn load(
    store: &CollaborationStore,
    binding: &RecordId,
) -> Result<AcceptanceBinding, AcceptanceError> {
    let _response = crate::collaboration_sensitive::response_use(store)?;
    let (revision, body): (String, Vec<u8>) = store
        .read_connection()
        .query_row(
            "SELECT revision,body FROM local_acceptance_bindings WHERE id=?1",
            [binding.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or(AcceptanceError::Missing)?;
    let record = Record::decode(&body, &[&BINDING]).map_err(|_| invalid("binding record"))?;
    let value: serde_json::Value = serde_json::from_slice(&body)?;
    let spec: BindingSpec = serde_json::from_value(value["spec"].clone())?;
    if record.id() != *binding
        || record.canonical_bytes() != body
        || spec.revision != revision
        || spec.profile != PROFILE
        || !(1..=64).contains(&spec.max_observations)
    {
        return Err(invalid("binding digest or index"));
    }
    Ok(AcceptanceBinding {
        id: binding.clone(),
        spec,
    })
}

struct NativeReader {
    deadline: Instant,
}
impl NativeReader {
    fn api(&self, repository: &str, suffix: &str) -> Result<serde_json::Value, AcceptanceError> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(AcceptanceError::Unavailable);
        }
        let mut cmd = std::process::Command::new("gh");
        cmd.args([
            "api",
            "--hostname",
            "github.com",
            "--method",
            "GET",
            &format!("repos/{repository}/{suffix}"),
        ])
        .env_remove("GH_REPO")
        .env_remove("GH_HOST")
        .env_remove("GH_DEBUG");
        let output = crate::bounded_output::output_within(&mut cmd, remaining)?
            .ok_or(AcceptanceError::Unavailable)?;
        if !output.status.success() || output.stdout.len() > 4 * 1024 * 1024 {
            return Err(AcceptanceError::Unavailable);
        }
        Ok(serde_json::from_slice(&output.stdout)?)
    }
    fn pages(
        &self,
        repo: &str,
        suffix: &str,
        key: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, AcceptanceError> {
        let mut all = Vec::new();
        for page in 1..=16 {
            let join = if suffix.contains('?') { '&' } else { '?' };
            let value = self.api(repo, &format!("{suffix}{join}per_page=100&page={page}"))?;
            let array = key
                .map_or(Some(&value), |k| value.get(k))
                .and_then(serde_json::Value::as_array)
                .ok_or(AcceptanceError::Unavailable)?;
            all.extend(array.iter().cloned());
            if array.len() < 100 {
                return Ok(all);
            }
        }
        Err(AcceptanceError::Unavailable)
    }
}
fn str_at<'a>(v: &'a serde_json::Value, path: &str) -> Result<&'a str, AcceptanceError> {
    v.pointer(path)
        .and_then(serde_json::Value::as_str)
        .ok_or(AcceptanceError::Unavailable)
}
fn pr_subject(
    pr: &serde_json::Value,
    repo: &str,
    number: u64,
    head: &str,
    base: &str,
) -> Result<(), AcceptanceError> {
    if pr["number"].as_u64() != Some(number)
        || !str_at(pr, "/base/repo/full_name")?.eq_ignore_ascii_case(repo)
        || str_at(pr, "/base/ref")? != base
        || str_at(pr, "/head/sha")? != head
    {
        return Err(invalid("exact provider repository/PR/head/base"));
    }
    Ok(())
}
fn review_pass(
    policy: &crate::ReviewPolicy,
    head: &str,
    pr_author: &str,
    aggregate: Option<&str>,
    reviews: &[serde_json::Value],
    checks: &[serde_json::Value],
) -> Result<bool, AcceptanceError> {
    if !policy.enabled {
        return Ok(true);
    }
    match policy.evidence_adapter {
        crate::ReviewEvidenceAdapter::GithubApproval => {
            if aggregate != Some("APPROVED") {
                return Ok(false);
            }
            let mut latest = std::collections::BTreeMap::<u64, (u64, &serde_json::Value)>::new();
            let mut ids = std::collections::BTreeSet::new();
            for review in reviews {
                let rid = review["id"].as_u64().ok_or(AcceptanceError::Unavailable)?;
                let user = review["user"]["id"]
                    .as_u64()
                    .ok_or(AcceptanceError::Unavailable)?;
                if !ids.insert(rid) {
                    return Err(AcceptanceError::Unavailable);
                }
                if review["state"] == "COMMENTED" {
                    continue;
                }
                if latest.get(&user).is_none_or(|(old, _)| rid > *old) {
                    latest.insert(user, (rid, review));
                }
            }
            Ok(latest
                .values()
                .filter(|(_, r)| {
                    r["state"] == "APPROVED"
                        && r["commit_id"] == head
                        && r["user"]["login"].as_str().is_some_and(|u| u != pr_author)
                })
                .count()
                >= policy.required_approvals as usize)
        }
        crate::ReviewEvidenceAdapter::GithubCheckRun => {
            let matching: Vec<_> = checks
                .iter()
                .filter(|c| {
                    c["name"].as_str() == policy.evidence_check_name.as_deref()
                        && c["app"]["slug"].as_str() == policy.evidence_app_slug.as_deref()
                })
                .collect();
            Ok(matching.len() == 1
                && matching[0]["head_sha"] == head
                && matching[0]["status"] == "completed"
                && matching[0]["conclusion"] == "success")
        }
    }
}
fn provider_decision(
    pr: &serde_json::Value,
    route: &ProviderRoute,
) -> Result<(ProviderDecision, Option<String>), AcceptanceError> {
    if pr["merged"].as_bool() != Some(true) {
        if pr["merged"].as_bool() != Some(false) {
            return Err(AcceptanceError::Unavailable);
        }
        return Ok((
            if pr["state"] == "closed" {
                ProviderDecision::Rejected
            } else {
                ProviderDecision::Waiting
            },
            None,
        ));
    }
    if pr["state"] != "closed"
        || pr["merged_at"].as_str().is_none_or(str::is_empty)
        || pr["draft"].as_bool() != Some(false)
    {
        return Err(AcceptanceError::Unavailable);
    }
    if let ProviderRoute::Mergify { merger, .. } = route
        && str_at(pr, "/merged_by/login")? != merger
    {
        return Err(invalid("appointed Mergify merger differs"));
    }
    let accepted = str_at(pr, "/merge_commit_sha")?.to_string();
    sha(&accepted)?;
    Ok((ProviderDecision::Accepted, Some(accepted)))
}

/// Read actual provider state once. Never retries a merge/write: publication
/// uncertainty remains in the native write journal, independent of this read.
pub fn observe(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    broker: &mut Broker,
    carrier: &GitCarrier,
    binding: &RecordId,
) -> Result<AcceptanceReceipt, AcceptanceError> {
    observe_with(store, owner, broker, carrier, binding, None)
}
fn observe_with(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    broker: &mut Broker,
    carrier: &GitCarrier,
    binding: &RecordId,
    fixture: Option<&ProviderFixture>,
) -> Result<AcceptanceReceipt, AcceptanceError> {
    let _authority = target::authorize(store, owner)?;
    let binding = load(store, binding)?;
    let spec = &binding.spec;
    let intent = transport::load(store, &id(&spec.intent)?)?;
    check_owner(store, owner, &intent)?;
    transport::route(carrier, &intent)?;
    carrier.check_broker(broker)?;
    if spec.epoch != owner.authority_epoch().as_str() {
        return Err(invalid("binding epoch"));
    }
    // Reserve a sequence before I/O; a killed observer consumes its finite slot.
    let control = rusqlite::Transaction::new_unchecked(
        store.read_connection(),
        TransactionBehavior::Immediate,
    )?;
    let count: i64 = control.query_row(
        "SELECT count(*) FROM local_acceptance_receipts WHERE binding=?1",
        [binding.id.as_str()],
        |r| r.get(0),
    )?;
    // Interrupted slots are records too; the pending row is never an acceptance.
    if count >= i64::from(spec.max_observations) {
        return Err(AcceptanceError::Exhausted);
    }
    let sequence = (count + 1) as u8;
    let pending = receipt_spec(
        &binding,
        &intent,
        sequence,
        ProviderDecision::Unavailable,
        None,
        None,
        None,
        now()?,
    );
    let record = canonical(&RECEIPT, &pending)?;
    control.execute(
        "INSERT INTO local_acceptance_receipts VALUES(?1,?2,?3)",
        params![
            record.id().as_str(),
            binding.id.as_str(),
            record.canonical_bytes()
        ],
    )?;
    control.commit()?;
    let mut receipt = pending.clone();
    let observed = read_provider(store, broker, carrier, &binding, &intent, fixture);
    match observed {
        Ok((decision, commit, tree, digest)) => {
            receipt.decision = decision;
            receipt.accepted_commit = commit;
            receipt.accepted_tree = tree;
            receipt.provider_digest = digest;
        }
        Err(AcceptanceError::Unavailable)
        | Err(AcceptanceError::Io(_))
        | Err(AcceptanceError::Git(_))
        | Err(AcceptanceError::Json(_)) => {}
        Err(error) => return Err(error),
    }
    // Native route configuration may have changed during reads. Historical data
    // is recorded only under the pinned route; a change requires reappointment.
    transport::route(carrier, &intent)?;
    let record = canonical(&RECEIPT, &receipt)?;
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "DELETE FROM local_acceptance_receipts WHERE id=?1",
        [canonical(&RECEIPT, &pending)?.id().as_str()],
    )?;
    tx.execute(
        "INSERT INTO local_acceptance_receipts VALUES(?1,?2,?3)",
        params![
            record.id().as_str(),
            binding.id.as_str(),
            record.canonical_bytes()
        ],
    )?;
    tx.execute(
        "UPDATE local_acceptance_bindings SET last_receipt=?2 WHERE id=?1",
        params![binding.id.as_str(), record.id().as_str()],
    )?;
    tx.commit()?;
    Ok(AcceptanceReceipt {
        id: record.id(),
        spec: receipt,
    })
}
#[allow(clippy::too_many_arguments)]
fn receipt_spec(
    binding: &AcceptanceBinding,
    intent: &Intent,
    sequence: u8,
    decision: ProviderDecision,
    accepted_commit: Option<String>,
    accepted_tree: Option<String>,
    provider_digest: Option<String>,
    observed_ms: i64,
) -> ReceiptSpec {
    ReceiptSpec {
        profile: PROFILE.into(),
        binding: binding.id.to_string(),
        intent: intent.id.to_string(),
        revision: intent.spec.revision.clone(),
        source: intent.spec.source.clone(),
        candidate_commit: intent.spec.commit.clone(),
        candidate_tree: intent.spec.tree.clone(),
        policy_commit: binding.spec.policy_commit.clone(),
        config_digest: binding.spec.config_digest.clone(),
        route: binding.spec.route.clone(),
        peer: intent.spec.peer.clone(),
        base_ref: binding.spec.base_ref.clone(),
        decision,
        accepted_commit,
        accepted_tree,
        provider_digest,
        observed_ms,
        sequence,
    }
}
// This is private. Fixtures can reach it only in this module's tests; incoming
// provider payloads never create a public current-authority constructor.
struct ProviderFixture {
    pr: serde_json::Value,
    reviews: Vec<serde_json::Value>,
    checks: Vec<serde_json::Value>,
    aggregate: Option<String>,
}
type ProviderObservation = (
    ProviderDecision,
    Option<String>,
    Option<String>,
    Option<String>,
);
fn read_provider(
    store: &CollaborationStore,
    broker: &mut Broker,
    carrier: &GitCarrier,
    binding: &AcceptanceBinding,
    intent: &Intent,
    fixture: Option<&ProviderFixture>,
) -> Result<ProviderObservation, AcceptanceError> {
    let s = &binding.spec;
    let physical = |hex: &str| -> Result<ObjectDigest, AcceptanceError> {
        let sid =
            aethyme_contracts::experimental_v0::SourceSnapshotId::parse(&format!("sha256:{hex}"))
                .map_err(|_| invalid("policy digest"))?;
        Ok(ObjectDigest::of_snapshot(&sid))
    };
    let config = archive::read_object(store, &physical(&s.config_digest)?)?;
    let mergify = s
        .mergify_digest
        .as_ref()
        .map(|h| archive::read_object(store, &physical(h)?).map_err(AcceptanceError::from))
        .transpose()?;
    let utf = std::str::from_utf8(&config).map_err(|_| invalid("policy UTF-8"))?;
    let review = crate::ReviewPolicy::from_config_text(utf)?;
    let publication = crate::ShipPublicationPolicy::from_config_text(utf)?;
    if ObjectDigest::of(&config).hex() != s.config_digest
        || mergify.as_ref().map(|b| ObjectDigest::of(b).hex()) != s.mergify_digest
    {
        return Err(invalid("pinned policy content"));
    }
    let (decision, accepted, digest) = match &s.route {
        ProviderRoute::GenericGit => {
            if review.enabled || publication.mode == crate::ShipPublicationMode::ReviewGated {
                return Err(invalid("generic mandatory review"));
            }
            let value = query_ref(carrier, &s.base_ref)?;
            let decision = if value.as_deref() == Some(&intent.spec.commit) {
                ProviderDecision::Accepted
            } else {
                ProviderDecision::Waiting
            };
            (
                decision,
                if decision == ProviderDecision::Accepted {
                    value
                } else {
                    None
                },
                None,
            )
        }
        ProviderRoute::Github { pull_request } | ProviderRoute::Mergify { pull_request, .. } => {
            let repo = &carrier.identity.repository_path;
            let reader = NativeReader {
                deadline: Instant::now() + Duration::from_secs(carrier.timeout),
            };
            let raw = match fixture {
                Some(f) => f.pr.clone(),
                None => reader.api(repo, &format!("pulls/{pull_request}"))?,
            };
            pr_subject(
                &raw,
                repo,
                *pull_request,
                &intent.spec.commit,
                s.base_ref
                    .strip_prefix("refs/heads/")
                    .ok_or_else(|| invalid("base"))?,
            )?;
            let (mut decision, accepted) = provider_decision(&raw, &s.route)?;
            if decision == ProviderDecision::Accepted && review.enabled {
                let (reviews, checks) = if let Some(f) = fixture {
                    (f.reviews.clone(), f.checks.clone())
                } else {
                    match review.evidence_adapter {
                        crate::ReviewEvidenceAdapter::GithubApproval => (
                            reader.pages(repo, &format!("pulls/{pull_request}/reviews"), None)?,
                            vec![],
                        ),
                        crate::ReviewEvidenceAdapter::GithubCheckRun => (
                            vec![],
                            reader.pages(
                                repo,
                                &format!("commits/{}/check-runs?filter=latest", intent.spec.commit),
                                Some("check_runs"),
                            )?,
                        ),
                    }
                };
                let aggregate = if review.evidence_adapter
                    == crate::ReviewEvidenceAdapter::GithubApproval
                {
                    match fixture {
                        Some(f) => f.aggregate.clone(),
                        None => {
                            let remaining =
                                reader.deadline.saturating_duration_since(Instant::now());
                            if remaining.is_zero() {
                                return Err(AcceptanceError::Unavailable);
                            }
                            let snapshot = crate::review::load_review_provider_snapshot_bounded(
                                carrier.repo.root(),
                                repo,
                                i64::try_from(*pull_request).map_err(|_| invalid("PR number"))?,
                                &review,
                                remaining,
                            )
                            .map_err(|_| AcceptanceError::Unavailable)?;
                            if snapshot.pr_number != *pull_request as i64
                                || snapshot.head_sha != intent.spec.commit
                                || snapshot.target_branch
                                    != s.base_ref.strip_prefix("refs/heads/").unwrap_or("")
                                || snapshot.state != "MERGED"
                                || snapshot.is_draft
                            {
                                return Err(invalid("aggregate review subject"));
                            }
                            snapshot.review_decision
                        }
                    }
                } else {
                    None
                };
                if !review_pass(
                    &review,
                    &intent.spec.commit,
                    str_at(&raw, "/user/login")?,
                    aggregate.as_deref(),
                    &reviews,
                    &checks,
                )? {
                    decision = ProviderDecision::MissingReview;
                }
            }
            // A provider event authorizes its source route; review-gated local
            // shipping remains a separate policy. This adapter performs no ship.
            (
                decision,
                accepted,
                Some(ObjectDigest::of(&serde_json::to_vec(&raw)?).hex()),
            )
        }
    };
    if decision != ProviderDecision::Accepted {
        return Ok((decision, None, None, digest));
    }
    let accepted = accepted.ok_or(AcceptanceError::Unavailable)?;
    if query_ref(carrier, &s.base_ref)?.as_deref() != Some(&accepted) {
        return Ok((ProviderDecision::TargetDrift, Some(accepted), None, digest));
    }
    fetch(broker, carrier, intent, &accepted)?;
    let accepted_oid = sha(&accepted)?;
    let snapshot = archive::snapshot_of_commit(carrier.repo.root(), &accepted_oid)?;
    let tree = archive::raw_commit_tree(carrier.repo.root(), &accepted_oid)?;
    let (after_config, _, _, after_mergify) = policies(carrier, &accepted)?;
    if snapshot.id().as_str() != intent.spec.source
        || tree != intent.spec.tree
        || ObjectDigest::of(&after_config).hex() != s.config_digest
        || after_mergify.as_ref().map(|b| ObjectDigest::of(b).hex()) != s.mergify_digest
    {
        return Ok((
            ProviderDecision::TargetDrift,
            Some(accepted),
            Some(tree),
            digest,
        ));
    }
    // Recheck the authoritative ref after content read; movement is an explicit
    // drift result, never a transferred approval for a newer base.
    if query_ref(carrier, &s.base_ref)?.as_deref() != Some(&accepted) {
        return Ok((
            ProviderDecision::TargetDrift,
            Some(accepted),
            Some(tree),
            digest,
        ));
    }
    Ok((
        ProviderDecision::Accepted,
        Some(accepted),
        Some(tree),
        digest,
    ))
}

pub fn last_receipt(
    store: &CollaborationStore,
    binding: &RecordId,
) -> Result<Option<AcceptanceReceipt>, AcceptanceError> {
    let _response = crate::collaboration_sensitive::response_use(store)?;
    let b = load(store, binding)?;
    let row:Option<(String,Vec<u8>)>=store.read_connection().query_row("SELECT r.id,r.body FROM local_acceptance_bindings b JOIN local_acceptance_receipts r ON r.id=b.last_receipt AND r.binding=b.id WHERE b.id=?1",[binding.as_str()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let Some((rid, body)) = row else {
        return Ok(None);
    };
    let rec = Record::decode(&body, &[&RECEIPT]).map_err(|_| invalid("receipt schema"))?;
    let raw: serde_json::Value = serde_json::from_slice(&body)?;
    let spec: ReceiptSpec = serde_json::from_value(raw["spec"].clone())?;
    let intent = transport::load(store, &id(&b.spec.intent)?)?;
    if rec.id().as_str() != rid
        || rec.canonical_bytes() != body
        || spec
            != receipt_spec(
                &b,
                &intent,
                spec.sequence,
                spec.decision,
                spec.accepted_commit.clone(),
                spec.accepted_tree.clone(),
                spec.provider_digest.clone(),
                spec.observed_ms,
            )
        || spec.sequence == 0
        || spec.sequence > b.spec.max_observations
    {
        return Err(invalid("historical receipt subject"));
    }
    Ok(Some(AcceptanceReceipt { id: rec.id(), spec }))
}
/// Grant only after a fresh native read and a current complete verifier pass.
/// No incoming receipt or caller-provided provider response can call this path.
#[allow(clippy::too_many_arguments)]
pub fn qualify<'a>(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    broker: &mut Broker,
    carrier: &GitCarrier,
    binding: &RecordId,
    execution: &'a ExecutionSnapshot,
    artifact: &'a SealedArtifact,
    permit: &'a ProducerPermit,
    evidence: &'a TrustedEvidence,
) -> Result<QualifiedAcceptance<'a>, AcceptanceError> {
    let receipt = observe(store, owner, broker, carrier, binding)?;
    qualify_receipt(
        store, owner, binding, execution, artifact, permit, evidence, receipt,
    )
}
#[allow(clippy::too_many_arguments)]
fn qualify_receipt<'a>(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    binding: &RecordId,
    execution: &'a ExecutionSnapshot,
    artifact: &'a SealedArtifact,
    permit: &'a ProducerPermit,
    evidence: &'a TrustedEvidence,
    receipt: AcceptanceReceipt,
) -> Result<QualifiedAcceptance<'a>, AcceptanceError> {
    let b = load(store, binding)?;
    let _trusted = verifier::acceptance_guard(store, owner, execution, artifact, permit, evidence)?;
    let revision = target::load_revision(store, execution.target_revision())?;
    if receipt.spec.decision != ProviderDecision::Accepted
        || last_receipt(store, binding)?.as_ref() != Some(&receipt)
        || receipt.spec.revision != execution.target_revision().as_str()
        || receipt.spec.source != execution.spec().candidate_source
        || b.spec.verifier_policy != revision.spec.acceptance_policy
        || permit.policy().record()?.id().as_str() != b.spec.verifier_policy
        || receipt.spec.accepted_tree.as_deref() != Some(&receipt.spec.candidate_tree)
    {
        return Err(AcceptanceError::NotAccepted);
    }
    let inventory_id = id(&execution.spec().assertion_inventory)?;
    let inventory_object = inventory_object_for_execution(store, execution)?;
    let inventory_bytes = archive::read_object_bounded(store, &inventory_object, 4 * 1024 * 1024)?;
    let inventory = verifier::AssertionInventory::decode(&inventory_bytes)?;
    if inventory.id() != inventory_id || inventory.canonical_bytes() != inventory_bytes {
        return Err(invalid("verifier inventory object identity"));
    }
    let record = canonical(
        &QUALIFICATION,
        &serde_json::json!({
            "binding":binding.as_str(),
            "receipt":receipt.id.as_str(),
            "evidence":evidence.reference().id.as_str(),
            "execution":execution.id().as_str(),
            "execution_object_sha256":execution.reference().object.hex(),
            "inventory_object_sha256":inventory_object.hex(),
            "artifact_object_sha256":artifact.record_object().hex(),
            "epoch":owner.authority_epoch().as_str(),
            "expires_ms":evidence.expires_ms()
        }),
    )?;
    let _archive = crate::collaboration_gc::archive_use(store)?;
    let object = archive::put_object(store, &record.canonical_bytes())?;
    target::retain_artifacts(store, owner, execution.target_revision(), &[], &[object])?;
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let state: (Option<String>, String) = tx.query_row(
        "SELECT active_revision,phase FROM local_targets WHERE target=?1",
        [owner.target()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if state
        != (
            Some(execution.target_revision().to_string()),
            "verifying".into(),
        )
        || now()? >= evidence.expires_ms()
    {
        return Err(AcceptanceError::NotAccepted);
    }
    tx.execute(
        "INSERT OR IGNORE INTO local_acceptance_qualifications VALUES(?1,?2,?3)",
        params![
            record.id().as_str(),
            binding.as_str(),
            record.canonical_bytes()
        ],
    )?;
    // Qualification is permission for the exact candidate; the controller keeps
    // its explicit disposition transition separate until handoff integration.
    tx.commit()?;
    Ok(QualifiedAcceptance {
        id: record.id(),
        receipt,
        evidence,
        execution,
        artifact,
        permit,
    })
}

/// Native current permission guard. Hold through a bounded generation commit;
/// keeping a serialized receipt never preserves producer or epoch authority.
pub struct CurrentAcceptanceGuard {
    _verification: verifier::VerificationGuard,
    expires_ms: i64,
}
impl CurrentAcceptanceGuard {
    /// Recheck immediately before committing a generation after materialization.
    pub fn check_clock(&self) -> Result<(), AcceptanceError> {
        if now()? >= self.expires_ms {
            return Err(AcceptanceError::NotAccepted);
        }
        Ok(())
    }
}
/// Re-observe the pinned provider representation and verifier trust immediately
/// before consuming a private grant. A receipt alone cannot enter this API.
pub fn current_guard(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    broker: &mut Broker,
    carrier: &GitCarrier,
    grant: &QualifiedAcceptance<'_>,
) -> Result<CurrentAcceptanceGuard, AcceptanceError> {
    let guard = verifier::acceptance_guard(
        store,
        owner,
        grant.execution,
        grant.artifact,
        grant.permit,
        grant.evidence,
    )?;
    let observed = observe(
        store,
        owner,
        broker,
        carrier,
        &id(&grant.receipt.spec.binding)?,
    )?;
    if observed.spec.decision != ProviderDecision::Accepted
        || observed.spec.accepted_commit != grant.receipt.spec.accepted_commit
        || observed.spec.accepted_tree != grant.receipt.spec.accepted_tree
        || now()? >= grant.expires_ms()
    {
        return Err(AcceptanceError::NotAccepted);
    }
    Ok(CurrentAcceptanceGuard {
        _verification: guard,
        expires_ms: grant.expires_ms(),
    })
}

#[cfg(test)]
mod tests;
