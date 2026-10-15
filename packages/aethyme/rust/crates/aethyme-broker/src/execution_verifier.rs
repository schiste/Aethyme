//! Independent offline CLI assertions (#672), checked outside candidate authority.
//! The first profile observes sealed executable artifacts through fresh sandbox
//! invocations. Uploaded descriptors/result files never create TrustedEvidence.
use crate::collaboration_archive::{self as archive, CommitOid, ObjectDigest};
use crate::collaboration_capture::{self as capture, CaptureReceipt, OperationId};
use crate::collaboration_state::CollaborationStore;
use crate::execution_snapshot::{self as snapshot, CommandSpec, ExecutionSnapshot};
use crate::execution_supervisor::{DockerExecutor, SealedArtifact, SupervisorError};
use crate::local_target::{self as target, ControllerOwner};
use aethyme_contracts::experimental_v0::source_snapshot::{EntryKind, SourceSnapshotId};
use aethyme_contracts::experimental_v0::{FieldKind, FieldSpec, Record, RecordId, RecordSchema};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub const PROFILE: &str = "aethyme.verify.offline-cli/provisional-v0";
pub const INVENTORY_SCHEMA: &str = "aethyme.assertion-inventory/provisional-v0";
pub const POLICY_SCHEMA: &str = "aethyme.verifier-policy/provisional-v0";
pub const EVIDENCE_SCHEMA: &str = "aethyme.verification-evidence/provisional-v0";
const NONZERO_EXIT_UNSUPPORTED: &str =
    "nonzero exit assertions require a separate stream-completion witness";
static INVENTORY: RecordSchema = RecordSchema {
    name: INVENTORY_SCHEMA,
    fields: &[FieldSpec {
        name: "cases",
        required: true,
        kind: FieldKind::Opaque,
        capability: None,
    }],
    capabilities: &[],
};
static POLICY: RecordSchema = RecordSchema {
    name: POLICY_SCHEMA,
    fields: &[FieldSpec {
        name: "policy",
        required: true,
        kind: FieldKind::Opaque,
        capability: None,
    }],
    capabilities: &[],
};
static EVIDENCE: RecordSchema = RecordSchema {
    name: EVIDENCE_SCHEMA,
    fields: &[FieldSpec {
        name: "observation",
        required: true,
        kind: FieldKind::Opaque,
        capability: None,
    }],
    capabilities: &[],
};
#[derive(Debug, thiserror::Error)]
pub enum VerificationError {
    #[error("unsupported verification profile: {0}")]
    Unsupported(String),
    #[error("invalid assertion inventory: {0}")]
    Inventory(String),
    #[error("verification subject or policy does not match")]
    Mismatch,
    #[error("verification producer is missing, revoked or stale")]
    Producer,
    #[error("verification evidence is missing, expired or not a complete pass")]
    Unfulfilled,
    #[error("verification catalogue changed")]
    Stale,
    #[error("verifier harness capture is incomplete ({code}): {detail}")]
    CaptureIncomplete { code: String, detail: String },
    #[error(transparent)]
    Supervisor(#[from] SupervisorError),
    #[error(transparent)]
    Resources(#[from] crate::execution_resources::ResourceError),
    #[error(transparent)]
    Budget(#[from] crate::local_budget::BudgetError),
    #[error(transparent)]
    Snapshot(#[from] snapshot::SnapshotError),
    #[error(transparent)]
    Target(#[from] target::TargetError),
    #[error(transparent)]
    Archive(#[from] archive::ArchiveError),
    #[error(transparent)]
    Capture(#[from] capture::CaptureError),
    #[error(transparent)]
    Sensitive(#[from] crate::collaboration_sensitive::SensitiveError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
/// Controlled inventory: stdin/arguments are trials, not candidate reports.
/// Expected digests are checked by this native supervisor, not by the app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssertionCase {
    pub id: String,
    pub arguments: Vec<String>,
    pub stdin_hex: String,
    pub exit_code: u8,
    pub stdout: String,
    pub stderr: String,
}
#[derive(Debug, Clone)]
pub struct AssertionInventory {
    record: Record,
    cases: Vec<AssertionCase>,
}
impl PartialEq for AssertionInventory {
    fn eq(&self, other: &Self) -> bool {
        self.record.id() == other.record.id() && self.cases == other.cases
    }
}
impl Eq for AssertionInventory {}
fn decode_record(bytes: &[u8], schema: &'static RecordSchema) -> Result<Record, VerificationError> {
    Record::decode(bytes, &[schema]).map_err(|e| VerificationError::Inventory(e.to_string()))
}
fn digest(value: &str) -> Result<ObjectDigest, VerificationError> {
    let hex = value
        .strip_prefix("sha256:")
        .ok_or(VerificationError::Mismatch)?;
    let id = aethyme_contracts::experimental_v0::SourceSnapshotId::parse(&format!("sha256:{hex}"))
        .map_err(|_| VerificationError::Mismatch)?;
    Ok(ObjectDigest::of_snapshot(&id))
}
fn hex_bytes(value: &str) -> Result<Vec<u8>, VerificationError> {
    if value.len() > 128 * 1024 || !value.len().is_multiple_of(2) {
        return Err(VerificationError::Inventory("stdin bounds".into()));
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let n = |c: u8| match c {
                b'0'..=b'9' => Some(c - b'0'),
                b'a'..=b'f' => Some(c - b'a' + 10),
                _ => None,
            };
            Ok(
                (n(pair[0]).ok_or_else(|| VerificationError::Inventory("stdin hex".into()))? << 4)
                    | n(pair[1]).ok_or_else(|| VerificationError::Inventory("stdin hex".into()))?,
            )
        })
        .collect()
}
impl AssertionInventory {
    pub fn new(cases: Vec<AssertionCase>) -> Result<Self, VerificationError> {
        if cases.is_empty() || cases.len() > 32 {
            return Err(VerificationError::Inventory(
                "require 1..32 mandatory cases".into(),
            ));
        }
        let mut names = BTreeSet::new();
        for case in &cases {
            if case.id.is_empty()
                || case.id.len() > 128
                || !case
                    .id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
                || !names.insert(&case.id)
            {
                return Err(VerificationError::Inventory("assertion identity".into()));
            }
            if case.arguments.len() > 32
                || case
                    .arguments
                    .iter()
                    .any(|v| v.len() > 4096 || v.contains('\0'))
            {
                return Err(VerificationError::Inventory("argument bounds".into()));
            }
            hex_bytes(&case.stdin_hex)?;
            digest(&case.stdout)?;
            digest(&case.stderr)?;
        }
        let record = decode_record(
            &serde_json::to_vec(&serde_json::json!({"schema":INVENTORY_SCHEMA,"cases":cases}))?,
            &INVENTORY,
        )?;
        if record.canonical_bytes().len() > 4 * 1024 * 1024 {
            return Err(VerificationError::Inventory("inventory bytes".into()));
        }
        Ok(Self { record, cases })
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, VerificationError> {
        if bytes.len() > 4 * 1024 * 1024 {
            return Err(VerificationError::Inventory("inventory bytes".into()));
        }
        let record = decode_record(bytes, &INVENTORY)?;
        if record.canonical_bytes() != bytes {
            return Err(VerificationError::Inventory(
                "noncanonical inventory".into(),
            ));
        }
        let value: serde_json::Value = serde_json::from_slice(bytes)?;
        let inventory = Self::new(serde_json::from_value(value["cases"].clone())?)?;
        if inventory.record.id() != record.id() {
            return Err(VerificationError::Mismatch);
        }
        Ok(inventory)
    }
    pub fn id(&self) -> RecordId {
        self.record.id()
    }
    pub fn canonical_bytes(&self) -> Vec<u8> {
        self.record.canonical_bytes()
    }
    pub fn cases(&self) -> &[AssertionCase] {
        &self.cases
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifierPolicy {
    pub profile: String,
    pub runtime: String,
    pub image: String,
    pub harness: String,
    pub inventory: String,
    pub command: CommandSpec,
    pub max_age_ms: u64,
}

/// Inputs that can be chosen before the retained harness and canonical
/// inventory identities are known. The runtime profile is derived from the
/// explicitly probed Docker endpoint; the profile name is fixed by this
/// adapter rather than accepted from an operator file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifierPlan {
    pub image: String,
    pub command: CommandSpec,
    pub max_age_ms: u64,
}

/// Durable result of preparing a verifier policy. The capture receipt roots
/// the exact harness tree until the selected target revision takes its own
/// root during configure/freeze.
#[derive(Debug, Clone)]
pub struct PreparedVerifier {
    pub policy: VerifierPolicy,
    pub policy_id: RecordId,
    pub runtime: crate::execution_supervisor::RuntimeDescription,
    pub capture: CaptureReceipt,
    pub inventory_cases: usize,
    pub inventory_sha256: String,
}

/// Retain and bind an exact offline verifier input set before target proposal.
/// The caller supplies a full Git commit and the canonical literal inventory
/// bytes. This command creates a durable capture root, but grants no producer
/// authority and starts no workload.
pub fn prepare(
    store: &mut CollaborationStore,
    operation_id: OperationId,
    harness_repository: &Path,
    harness_commit: &str,
    inventory_bytes: &[u8],
    plan: VerifierPlan,
    docker_cli: PathBuf,
    endpoint: &str,
) -> Result<PreparedVerifier, VerificationError> {
    let _response = crate::collaboration_sensitive::response_use(store)?;
    let inventory = AssertionInventory::decode(inventory_bytes)?;
    ensure_supported_inventory(&inventory)?;
    let commit = CommitOid::parse(harness_commit).map_err(|_| VerificationError::Mismatch)?;
    let pinned = archive::pin_commit(harness_repository, commit.as_str())?;
    if pinned != commit {
        return Err(VerificationError::Mismatch);
    }
    let source = archive::snapshot_of_commit(harness_repository, &commit)?;
    let inventory_entry = source
        .entries()
        .iter()
        .find(|entry| entry.path() == b"harness/inventory.json")
        .ok_or(VerificationError::Mismatch)?;
    if inventory_entry.kind() != EntryKind::Regular
        || inventory_entry.content_sha256() != &ObjectDigest::of(inventory_bytes).raw()
    {
        return Err(VerificationError::Mismatch);
    }

    let executor = DockerExecutor::probe_at(docker_cli, endpoint)?;
    let runtime = executor.runtime().clone();
    let harness_id = source.id();
    let policy = VerifierPolicy {
        profile: PROFILE.into(),
        runtime: runtime.id(),
        image: plan.image,
        harness: harness_id.as_str().into(),
        inventory: inventory.id().as_str().into(),
        command: plan.command,
        max_age_ms: plan.max_age_ms,
    };
    let policy_record = policy.record()?;

    let request = capture::CaptureRequest {
        operation_id,
        repository: harness_repository.to_path_buf(),
        base: commit.clone(),
        result: commit,
        policy: capture::CapturePolicy::Required,
        retention: capture::RetentionBoundary::UntilReleased,
    };
    let receipt = match capture::capture(store, &request)? {
        capture::CaptureOutcome::Acknowledged(receipt) => receipt,
        capture::CaptureOutcome::Incomplete { code, detail, .. } => {
            return Err(VerificationError::CaptureIncomplete { code, detail });
        }
    };
    if receipt.status != "retained_local"
        || receipt.retention != capture::RetentionBoundary::UntilReleased
        || receipt.base != harness_id
        || receipt.result != harness_id
    {
        return Err(VerificationError::Mismatch);
    }

    Ok(PreparedVerifier {
        policy,
        policy_id: policy_record.id(),
        runtime,
        capture: receipt,
        inventory_cases: inventory.cases().len(),
        inventory_sha256: ObjectDigest::of(inventory_bytes).hex(),
    })
}

fn ensure_supported_inventory(inventory: &AssertionInventory) -> Result<(), VerificationError> {
    if inventory.cases().iter().any(|case| case.exit_code != 0) {
        return Err(VerificationError::Unsupported(
            NONZERO_EXIT_UNSUPPORTED.into(),
        ));
    }
    Ok(())
}

impl VerifierPolicy {
    pub fn record(&self) -> Result<Record, VerificationError> {
        if self.profile != PROFILE
            || self.max_age_ms == 0
            || self.max_age_ms > 3_600_000
            || !self.command.program.starts_with('/')
            || self.command.program.contains('\0')
            || self.command.program.len() > 4096
            || self
                .command
                .program
                .split('/')
                .any(|p| p == "." || p == "..")
            || self.command.arguments.len() > 128
            || self
                .command
                .arguments
                .iter()
                .any(|s| s.len() > 4096 || s.contains('\0'))
            || self.command.environment.len() > 64
            || self.command.environment.iter().any(|(k, v)| {
                k.is_empty()
                    || k.len() > 128
                    || !k
                        .bytes()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
                    || v.len() > 4096
                    || v.contains('\0')
            })
        {
            return Err(VerificationError::Unsupported(
                "policy bounds/profile".into(),
            ));
        }
        digest(&self.runtime)?;
        digest(&self.image)?;
        digest(&self.harness)?;
        digest(&self.inventory)?;
        decode_record(
            &serde_json::to_vec(&serde_json::json!({"schema":POLICY_SCHEMA,"policy":self}))?,
            &POLICY,
        )
    }
}
/// Appointed local producer permission, never deserializable or cloneable.
#[derive(Debug)]
pub struct ProducerPermit {
    producer: String,
    generation: i64,
    policy: VerifierPolicy,
    epoch: crate::local_authority::Epoch,
    target: String,
}
impl ProducerPermit {
    pub fn generation(&self) -> i64 {
        self.generation
    }
    pub fn producer(&self) -> &str {
        &self.producer
    }
    pub fn policy(&self) -> &VerifierPolicy {
        &self.policy
    }
}
fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
}

/// Re-open the exact retained verifier source and its canonical literal
/// inventory. A syntactically valid policy cannot appoint an unretained or
/// mismatched harness.
fn retained_harness_inventory(
    store: &CollaborationStore,
    policy: &VerifierPolicy,
) -> Result<SourceSnapshotId, VerificationError> {
    let harness =
        SourceSnapshotId::parse(&policy.harness).map_err(|_| VerificationError::Mismatch)?;
    if archive::retained(store, &harness)?.is_none() {
        return Err(VerificationError::Mismatch);
    }
    let manifest_bytes = archive::read_object(store, &ObjectDigest::of_snapshot(&harness))?;
    let source = archive::parse_manifest(&manifest_bytes).ok_or(VerificationError::Mismatch)?;
    if source.id() != harness || source.manifest_bytes() != manifest_bytes {
        return Err(VerificationError::Mismatch);
    }
    let entry = source
        .entries()
        .iter()
        .find(|entry| entry.path() == b"harness/inventory.json")
        .ok_or(VerificationError::Mismatch)?;
    if entry.kind() != EntryKind::Regular {
        return Err(VerificationError::Mismatch);
    }
    let bytes = archive::read_object(store, &ObjectDigest::from_bytes(*entry.content_sha256()))?;
    let inventory = AssertionInventory::decode(&bytes)?;
    if inventory.id().as_str() != policy.inventory {
        return Err(VerificationError::Mismatch);
    }
    Ok(harness)
}

/// Trusted local operator entry. Compare-and-swap rotation prevents a lost
/// response or stale configurator from silently reviving revoked authority.
pub fn configure(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    producer: &str,
    expected: Option<i64>,
    policy: VerifierPolicy,
) -> Result<ProducerPermit, VerificationError> {
    let archive_guard = crate::collaboration_gc::archive_use(store)?;
    let _response = crate::collaboration_sensitive::response_use(store)?;
    let authority_guard = target::authorize(store, owner)?;
    if !valid_name(producer) {
        return Err(VerificationError::Producer);
    }
    let file = producer_lock(store, owner.target(), producer)?;
    let _producer = crate::file_lock::ExclusiveFileLock::acquire(file)?;
    let record = policy.record()?;
    let state = target::status(store, owner.target())?;
    let revision = state.revision.ok_or(VerificationError::Mismatch)?;
    let target_revision = target::load_revision_under_response(store, &revision, &_response)?;
    if target_revision.spec.acceptance_policy != record.id().as_str() {
        return Err(VerificationError::Mismatch);
    }
    let harness = retained_harness_inventory(store, &policy)?;
    let physical = archive::put_object(store, &record.canonical_bytes())?;
    target::retain_artifacts_under_guards(
        store,
        owner,
        &revision,
        &[harness],
        &[physical],
        &archive_guard,
        &_response,
        &authority_guard,
    )?;
    let tx = store
        .connection()
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let selected: Option<String> = tx.query_row(
        "SELECT active_revision FROM local_targets WHERE target=?1",
        [owner.target()],
        |r| r.get(0),
    )?;
    if selected.as_deref() != Some(revision.as_str()) {
        return Err(VerificationError::Stale);
    }
    let old: Option<i64> = tx
        .query_row(
            "SELECT generation FROM local_verifier_producers WHERE target=?1 AND producer=?2",
            params![owner.target(), producer],
            |r| r.get(0),
        )
        .optional()?;
    if old != expected {
        return Err(VerificationError::Stale);
    }
    let generation = old
        .unwrap_or(0)
        .checked_add(1)
        .ok_or(VerificationError::Stale)?;
    tx.execute("INSERT INTO local_verifier_producers(target,producer,generation,policy,body,epoch,revoked) VALUES(?1,?2,?3,?4,?5,?6,0) ON CONFLICT(target,producer) DO UPDATE SET generation=excluded.generation,policy=excluded.policy,body=excluded.body,epoch=excluded.epoch,revoked=0",params![owner.target(),producer,generation,record.id().as_str(),record.canonical_bytes(),owner.authority_epoch().as_str()])?;
    tx.commit()?;
    Ok(ProducerPermit {
        producer: producer.into(),
        generation,
        policy,
        epoch: owner.authority_epoch().clone(),
        target: owner.target().into(),
    })
}
pub fn revoke(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    producer: &str,
    expected: i64,
) -> Result<(), VerificationError> {
    let _authority = target::authorize(store, owner)?;
    if expected < 1 || !valid_name(producer) {
        return Err(VerificationError::Producer);
    }
    let file = producer_lock(store, owner.target(), producer)?;
    let _producer = crate::file_lock::ExclusiveFileLock::acquire(file)?;
    let changed = store.connection().execute("UPDATE local_verifier_producers SET revoked=1,generation=generation+1 WHERE target=?1 AND producer=?2 AND generation=?3",params![owner.target(),producer,expected])?;
    if changed != 1 {
        return Err(VerificationError::Stale);
    }
    Ok(())
}
/// Recover an already configured native producer at an explicit generation.
/// This never configures, rotates or revives producer authority. Dispatch
/// rechecks the catalogue under its existing shared producer guard.
pub fn existing_permit(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    producer: &str,
    expected: i64,
) -> Result<ProducerPermit, VerificationError> {
    let _authority = target::authorize(store, owner)?;
    if !valid_name(producer) || expected < 1 {
        return Err(VerificationError::Producer);
    }
    let file = producer_lock(store, owner.target(), producer)?;
    file.lock_shared()?;
    let row:Option<(i64,String,Vec<u8>,String,bool)>=store.read_connection().query_row("SELECT generation,policy,body,epoch,revoked FROM local_verifier_producers WHERE target=?1 AND producer=?2",params![owner.target(),producer],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    let (generation, identity, body, epoch, revoked) = row.ok_or(VerificationError::Producer)?;
    if generation != expected || revoked || epoch != owner.authority_epoch().as_str() {
        return Err(VerificationError::Stale);
    }
    let record = decode_record(&body, &POLICY)?;
    let policy: VerifierPolicy = serde_json::from_value(
        serde_json::from_slice::<serde_json::Value>(&body)?["policy"].clone(),
    )?;
    let revision = target::status(store, owner.target())?
        .revision
        .ok_or(VerificationError::Mismatch)?;
    if record.canonical_bytes() != body
        || record.id().as_str() != identity
        || policy.record()?.id() != record.id()
        || target::load_revision(store, &revision)?
            .spec
            .acceptance_policy
            != identity
    {
        return Err(VerificationError::Mismatch);
    }
    Ok(ProducerPermit {
        producer: producer.into(),
        generation,
        policy,
        epoch: owner.authority_epoch().clone(),
        target: owner.target().into(),
    })
}

/// Shared producer lock spans dispatch acknowledgement; catalogue changes take
/// it exclusively. External epoch authority is acquired first everywhere.
pub(crate) struct VerificationGuard {
    _epoch: crate::local_authority::Guard,
    _producer: std::fs::File,
}
fn producer_lock(
    store: &CollaborationStore,
    target: &str,
    producer: &str,
) -> Result<std::fs::File, VerificationError> {
    let directory = store.project_dir().join("verifier-authority");
    std::fs::create_dir_all(&directory)?;
    crate::host_state::protect_host_state_path(&directory, true)?;
    let identity = ObjectDigest::of(&serde_json::to_vec(&(target, producer))?).hex();
    let path = directory.join(format!("{identity}.lock"));
    let file = crate::file_lock::open_lock_file(&path)?;
    crate::host_state::protect_host_state_path(&path, false)?;
    Ok(file)
}
fn producer_guard(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    permit: &ProducerPermit,
) -> Result<VerificationGuard, VerificationError> {
    let guard = target::authorize(store, owner)?;
    let lock = producer_lock(store, owner.target(), &permit.producer)?;
    lock.lock_shared()?;
    if permit.target != owner.target() || &permit.epoch != owner.authority_epoch() {
        return Err(VerificationError::Producer);
    }
    let record = permit.policy.record()?;
    let row:Option<(i64,String,Vec<u8>,String,i64)>=store.read_connection().query_row("SELECT generation,policy,body,epoch,revoked FROM local_verifier_producers WHERE target=?1 AND producer=?2",params![owner.target(),permit.producer],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    if row
        != Some((
            permit.generation,
            record.id().to_string(),
            record.canonical_bytes(),
            permit.epoch.as_str().into(),
            0,
        ))
    {
        return Err(VerificationError::Producer);
    }
    Ok(VerificationGuard {
        _epoch: guard,
        _producer: lock,
    })
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationOutcome {
    Passed,
    AssertionFailed,
    InfrastructureDeferred,
    Cancelled,
    TimedOut,
    Inconclusive,
    TamperingDetected,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AssertionObservation {
    id: String,
    outcome: VerificationOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stderr: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidenceBody {
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
    result: VerificationOutcome,
    assertions: Vec<AssertionObservation>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceRef {
    pub id: RecordId,
    pub object: ObjectDigest,
}
/// Constructor is confined to observed verification or the local authority's
/// retained evidence journal. A signed-looking uploaded JSON file is just data.
#[derive(Debug)]
pub struct TrustedEvidence {
    reference: EvidenceRef,
    body: EvidenceBody,
    issued_ms: i64,
    expires_ms: i64,
}
impl TrustedEvidence {
    pub fn reference(&self) -> &EvidenceRef {
        &self.reference
    }
    pub fn outcome(&self) -> VerificationOutcome {
        self.body.result
    }
    pub fn collected_assertions(&self) -> usize {
        self.body.assertions.len()
    }
    pub fn expires_ms(&self) -> i64 {
        self.expires_ms
    }
}
fn now() -> Result<i64, VerificationError> {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| VerificationError::Unfulfilled)?
        .as_millis();
    i64::try_from(ms).map_err(|_| VerificationError::Unfulfilled)
}
fn subject_guard(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    execution: &ExecutionSnapshot,
    artifact: &SealedArtifact,
    permit: &ProducerPermit,
) -> Result<VerificationGuard, VerificationError> {
    let guard = producer_guard(store, owner, permit)?;
    let state = target::status(store, owner.target())?;
    let policy_record = permit.policy.record()?;
    if state.phase != target::Phase::Verifying
        || state.revision.as_ref() != Some(execution.target_revision())
        || artifact.execution() != execution.id()
        || artifact.authority_epoch() != owner.authority_epoch()
        || artifact.runtime() != permit.policy.runtime
    {
        return Err(VerificationError::Mismatch);
    }
    let revision = target::load_revision(store, execution.target_revision())?;
    let spec = execution.spec();
    if revision.spec.acceptance_policy != policy_record.id().as_str()
        || permit.policy.harness != spec.harness
        || permit.policy.inventory != spec.assertion_inventory
        || permit.policy.runtime != spec.profile.runtime_profile
        || permit.policy.image != spec.profile.verifier_image
        || permit.policy.command != spec.verify
    {
        return Err(VerificationError::Mismatch);
    }
    if snapshot::load(store, execution.reference())? != *execution {
        return Err(VerificationError::Mismatch);
    }
    let _archive = crate::collaboration_gc::archive_use(store)?;
    let root:bool=store.read_connection().query_row("SELECT EXISTS(SELECT 1 FROM local_target_roots WHERE revision=?1 AND kind='object' AND reference=?2)",params![execution.target_revision().as_str(),artifact.record_object().hex()],|r|r.get(0))?;
    if !root {
        return Err(VerificationError::Mismatch);
    }
    artifact.verify_record(store, execution)?;
    let manifest = archive::read_object(store, &ObjectDigest::of_snapshot(&artifact.output_id()))?;
    if manifest != artifact.output.manifest.manifest_bytes() {
        return Err(VerificationError::Mismatch);
    }
    for entry in artifact.output.manifest.entries() {
        let bytes =
            archive::read_object(store, &ObjectDigest::from_bytes(*entry.content_sha256()))?;
        if artifact.output.files.get(entry.path()) != Some(&bytes) {
            return Err(VerificationError::Mismatch);
        }
    }
    Ok(guard)
}
fn inventory_from_inputs(
    inputs: &snapshot::MaterializedInputs,
    policy: &VerifierPolicy,
) -> Result<AssertionInventory, VerificationError> {
    let path = inputs.root().join("harness/inventory.json");
    let mut file = std::fs::File::open(path)?;
    use std::io::Read;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    let inventory = AssertionInventory::decode(&bytes)?;
    if inventory.id().as_str() != policy.inventory {
        return Err(VerificationError::Mismatch);
    }
    Ok(inventory)
}
fn complete_pass(body: &EvidenceBody, inventory: &AssertionInventory) -> bool {
    inventory.cases.iter().all(|case| case.exit_code == 0)
        && body.result == VerificationOutcome::Passed
        && body.assertions.len() == inventory.cases.len()
        && body
            .assertions
            .iter()
            .zip(&inventory.cases)
            .all(|(observed, expected)| {
                observed.id == expected.id
                    && observed.outcome == VerificationOutcome::Passed
                    && observed.exit_code == Some(i32::from(expected.exit_code))
                    && observed.stdout.as_deref() == Some(&expected.stdout)
                    && observed.stderr.as_deref() == Some(&expected.stderr)
            })
}
fn expected_body(
    owner: &ControllerOwner,
    execution: &ExecutionSnapshot,
    artifact: &SealedArtifact,
    permit: &ProducerPermit,
    verification: String,
    assertions: Vec<AssertionObservation>,
    result: VerificationOutcome,
) -> Result<EvidenceBody, VerificationError> {
    Ok(EvidenceBody {
        verification,
        producer: permit.producer.clone(),
        generation: permit.generation.to_string(),
        epoch: owner.authority_epoch().as_str().into(),
        revision: execution.target_revision().to_string(),
        execution: execution.id().to_string(),
        candidate: execution.spec().candidate_source.clone(),
        artifact: artifact.id().to_string(),
        output: artifact.output_id().to_string(),
        policy: permit.policy.record()?.id().to_string(),
        runtime: permit.policy.runtime.clone(),
        image: permit.policy.image.clone(),
        harness: permit.policy.harness.clone(),
        inventory: permit.policy.inventory.clone(),
        profile: PROFILE.into(),
        result,
        assertions,
    })
}
fn retain_evidence(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    body: EvidenceBody,
    max_age_ms: u64,
    cancel: &AtomicBool,
    before_issue: impl FnOnce(),
) -> Result<TrustedEvidence, VerificationError> {
    let record = decode_record(
        &serde_json::to_vec(&serde_json::json!({"schema":EVIDENCE_SCHEMA,"observation":body}))?,
        &EVIDENCE,
    )?;
    let issued_ms = now()?;
    let expires_ms = issued_ms
        .checked_add(i64::try_from(max_age_ms).map_err(|_| VerificationError::Unfulfilled)?)
        .ok_or(VerificationError::Unfulfilled)?;
    let revision = RecordId::parse(&body.revision).map_err(|_| VerificationError::Mismatch)?;
    let archive_guard = crate::collaboration_gc::archive_use(store)?;
    let response_guard = crate::collaboration_sensitive::response_use(store)?;
    let authority_guard = target::authorize(store, owner)?;
    let physical = archive::put_object(store, &record.canonical_bytes())?;
    target::retain_artifacts_under_guards(
        store,
        owner,
        &revision,
        &[],
        &[physical],
        &archive_guard,
        &response_guard,
        &authority_guard,
    )?;
    let tx = store
        .connection()
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    before_issue();
    if body.result == VerificationOutcome::Passed && cancel.load(Ordering::SeqCst) {
        return Err(SupervisorError::Cancelled.into());
    }
    tx.execute("INSERT INTO local_verification_evidence(id,object,revision,producer,generation,issued_ms,expires_ms,body) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![record.id().as_str(),physical.hex(),body.revision,body.producer,body.generation,issued_ms,expires_ms,record.canonical_bytes()])?;
    if body.result == VerificationOutcome::Passed && cancel.load(Ordering::SeqCst) {
        return Err(SupervisorError::Cancelled.into());
    }
    tx.commit()?;
    Ok(TrustedEvidence {
        reference: EvidenceRef {
            id: record.id(),
            object: physical,
        },
        body,
        issued_ms,
        expires_ms,
    })
}
#[allow(clippy::too_many_arguments)]
fn applicable_guard_at(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    execution: &ExecutionSnapshot,
    artifact: &SealedArtifact,
    permit: &ProducerPermit,
    evidence: &TrustedEvidence,
    inventory: &AssertionInventory,
    at: i64,
) -> Result<VerificationGuard, VerificationError> {
    let authority = subject_guard(store, owner, execution, artifact, permit)?;
    if inventory.id().as_str() != permit.policy.inventory
        || evidence.body.verification.len() != 64
        || !evidence
            .body
            .verification
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        || at < evidence.issued_ms
        || at >= evidence.expires_ms
        || evidence.expires_ms.checked_sub(evidence.issued_ms)
            != Some(permit.policy.max_age_ms as i64)
        || !complete_pass(&evidence.body, inventory)
    {
        return Err(VerificationError::Unfulfilled);
    }
    let expected = expected_body(
        owner,
        execution,
        artifact,
        permit,
        evidence.body.verification.clone(),
        evidence.body.assertions.clone(),
        VerificationOutcome::Passed,
    )?;
    if expected != evidence.body {
        return Err(VerificationError::Mismatch);
    }
    Ok(authority)
}
#[allow(clippy::too_many_arguments)]
fn applicable_at(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    execution: &ExecutionSnapshot,
    artifact: &SealedArtifact,
    permit: &ProducerPermit,
    evidence: &TrustedEvidence,
    inventory: &AssertionInventory,
    at: i64,
) -> Result<(), VerificationError> {
    applicable_guard_at(
        store, owner, execution, artifact, permit, evidence, inventory, at,
    )
    .map(|_| ())
}
/// Keep the producer/epoch locks through the caller's acceptance commit.
pub(crate) fn acceptance_guard(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    execution: &ExecutionSnapshot,
    artifact: &SealedArtifact,
    permit: &ProducerPermit,
    evidence: &TrustedEvidence,
) -> Result<VerificationGuard, VerificationError> {
    let inputs = snapshot::materialize(store, owner, execution)?;
    let inventory = inventory_from_inputs(&inputs, &permit.policy)?;
    applicable_guard_at(
        store,
        owner,
        execution,
        artifact,
        permit,
        evidence,
        &inventory,
        now()?,
    )
}

/// Recheck present producer trust and policy; historical evidence stays data
/// after revoke/expiry/subject drift. This never advances an acceptance state.
pub fn applicable(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    execution: &ExecutionSnapshot,
    artifact: &SealedArtifact,
    permit: &ProducerPermit,
    evidence: &TrustedEvidence,
) -> Result<(), VerificationError> {
    let inputs = snapshot::materialize(store, owner, execution)?;
    let inventory = inventory_from_inputs(&inputs, &permit.policy)?;
    applicable_at(
        store,
        owner,
        execution,
        artifact,
        permit,
        evidence,
        &inventory,
        now()?,
    )
}
/// Reload only the authority-controlled journal. An arbitrary CAS object with
/// identical-looking result fields is never treated as a producer receipt.
pub fn load_evidence(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    execution: &ExecutionSnapshot,
    artifact: &SealedArtifact,
    permit: &ProducerPermit,
    reference: &EvidenceRef,
) -> Result<TrustedEvidence, VerificationError> {
    let _authority = subject_guard(store, owner, execution, artifact, permit)?;
    type JournalRow = (String, String, String, i64, i64, i64, Vec<u8>);
    let row:Option<JournalRow>=store.read_connection().query_row("SELECT object,revision,producer,generation,issued_ms,expires_ms,body FROM local_verification_evidence WHERE id=?1",[reference.id.as_str()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).optional()?;
    let Some((object, revision, producer, generation, issued_ms, expires_ms, bytes)) = row else {
        return Err(VerificationError::Unfulfilled);
    };
    if object != reference.object.hex()
        || revision != execution.target_revision().as_str()
        || producer != permit.producer
        || generation != permit.generation
        || bytes.len() > 1024 * 1024
    {
        return Err(VerificationError::Mismatch);
    }
    let rooted:bool=store.read_connection().query_row("SELECT EXISTS(SELECT 1 FROM local_target_roots WHERE revision=?1 AND kind='object' AND reference=?2)",params![revision,object],|r|r.get(0))?;
    if !rooted {
        return Err(VerificationError::Mismatch);
    }
    let retained = archive::read_object(store, &reference.object)?;
    if retained != bytes {
        return Err(VerificationError::Mismatch);
    }
    let record = decode_record(&bytes, &EVIDENCE)?;
    if record.id() != reference.id || record.canonical_bytes() != bytes {
        return Err(VerificationError::Mismatch);
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    let evidence = TrustedEvidence {
        reference: reference.clone(),
        body: serde_json::from_value(value["observation"].clone())?,
        issued_ms,
        expires_ms,
    };
    applicable(store, owner, execution, artifact, permit, &evidence)?;
    Ok(evidence)
}
fn classify(error: &SupervisorError) -> VerificationOutcome {
    match error {
        SupervisorError::Cancelled => VerificationOutcome::Cancelled,
        SupervisorError::TimedOut => VerificationOutcome::TimedOut,
        SupervisorError::Tampered(_) => VerificationOutcome::TamperingDetected,
        SupervisorError::Inconclusive(_) => VerificationOutcome::Inconclusive,
        _ => VerificationOutcome::InfrastructureDeferred,
    }
}
/// Native verifier for the offline CLI profile. Every mandatory trial runs in
/// a new sandbox, with only its arguments/stdin and the sealed artifact. The
/// inventory and expected observations stay in the trusted native supervisor.
#[allow(clippy::too_many_arguments)]
fn verify_body(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    execution: &ExecutionSnapshot,
    artifact: &SealedArtifact,
    permit: &ProducerPermit,
    executor: &DockerExecutor,
    cancel: &AtomicBool,
    verification: String,
    mut admission: crate::execution_resources::Admission<'_>,
    attempt: Option<&crate::local_budget::Attempt>,
) -> Result<EvidenceBody, VerificationError> {
    subject_guard(store, owner, execution, artifact, permit)?;
    if execution.spec().preparation.is_some() || execution.spec().dataset.is_some() {
        return Err(VerificationError::Unsupported(
            "offline CLI preparation/dataset".into(),
        ));
    }
    if executor.runtime().id() != permit.policy.runtime {
        return Err(VerificationError::Mismatch);
    }
    let inputs =
        snapshot::materialize_in(store, owner, execution, executor.optional_temporary_root())?;
    let inventory = inventory_from_inputs(&inputs, &permit.policy)?;
    ensure_supported_inventory(&inventory)?;
    let mut assertions = Vec::new();
    let mut result = VerificationOutcome::Passed;
    for (index, case) in inventory.cases.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            result = VerificationOutcome::Cancelled;
            break;
        }
        let generation = format!("{verification}-{index}");
        let trial = executor.observe_cli(
            store,
            execution,
            artifact,
            &generation,
            &case.arguments,
            &hex_bytes(&case.stdin_hex)?,
            admission.reborrow(),
            cancel,
            || {
                let producer = subject_guard(store, owner, execution, artifact, permit)
                    .map_err(|_| SupervisorError::Cancelled)?;
                let budget = if let Some(attempt) = attempt {
                    Some(crate::local_budget::execution_guard(
                        store,
                        owner,
                        attempt,
                        execution,
                        crate::execution_resources::verify_envelope(execution)?.units()?,
                    )?)
                } else {
                    None
                };
                Ok((producer, budget))
            },
        );
        let observation = match trial {
            Ok(observed) => {
                let stdout = format!("sha256:{}", ObjectDigest::of(&observed.stdout).hex());
                let stderr = format!("sha256:{}", ObjectDigest::of(&observed.stderr).hex());
                let outcome = if observed.exit == i32::from(case.exit_code)
                    && stdout == case.stdout
                    && stderr == case.stderr
                {
                    VerificationOutcome::Passed
                } else {
                    VerificationOutcome::AssertionFailed
                };
                AssertionObservation {
                    id: case.id.clone(),
                    outcome,
                    exit_code: Some(observed.exit),
                    stdout: Some(stdout),
                    stderr: Some(stderr),
                }
            }
            Err(error) => AssertionObservation {
                id: case.id.clone(),
                outcome: classify(&error),
                exit_code: None,
                stdout: None,
                stderr: None,
            },
        };
        result = observation.outcome;
        assertions.push(observation);
        if result != VerificationOutcome::Passed {
            break;
        }
    }
    let _authority = subject_guard(store, owner, execution, artifact, permit)?;
    if cancel.load(Ordering::SeqCst) {
        result = VerificationOutcome::Cancelled;
    }
    let body = expected_body(
        owner,
        execution,
        artifact,
        permit,
        verification,
        assertions,
        result,
    )?;
    if body.result == VerificationOutcome::Passed && !complete_pass(&body, &inventory) {
        return Err(VerificationError::Unfulfilled);
    }
    Ok(body)
}
/// Native verification consumes one current exact execution budget attempt.
#[allow(clippy::too_many_arguments)]
pub fn verify(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    execution: &ExecutionSnapshot,
    artifact: &SealedArtifact,
    permit: &ProducerPermit,
    executor: &DockerExecutor,
    attempt: &crate::local_budget::Attempt,
    cancel: &AtomicBool,
) -> Result<TrustedEvidence, VerificationError> {
    subject_guard(store, owner, execution, artifact, permit)?;
    let policy = executor.resource_policy()?;
    let generation: String =
        store
            .read_connection()
            .query_row("SELECT lower(hex(randomblob(32)))", [], |r| r.get(0))?;
    let mut run = crate::execution_resources::Run::admit(
        policy,
        store,
        owner,
        execution,
        attempt,
        generation.clone(),
        executor.endpoint().into(),
        crate::execution_resources::verify_envelope(execution)?,
    )?;
    let result = verify_body(
        store,
        owner,
        execution,
        artifact,
        permit,
        executor,
        cancel,
        generation,
        crate::execution_resources::Admission::Bound(&mut run),
        Some(attempt),
    );
    if run
        .journal
        .objects
        .iter()
        .any(|o| !o.removed || o.id.is_none())
    {
        crate::local_budget::execution_unknown(
            store,
            owner,
            attempt,
            "unconfirmed independent verifier teardown",
        )?;
        return Err(VerificationError::Unfulfilled);
    }
    run.cleaned()?;
    run.finish(
        store,
        owner,
        attempt,
        &format!("{:?}", result.as_ref().map(|body| body.result)),
    )?;
    let body = result?;
    let _current = subject_guard(store, owner, execution, artifact, permit)?;
    retain_evidence(store, owner, body, permit.policy.max_age_ms, cancel, || {})
}
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn test_verify(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    execution: &ExecutionSnapshot,
    artifact: &SealedArtifact,
    permit: &ProducerPermit,
    executor: &DockerExecutor,
    cancel: &AtomicBool,
) -> Result<TrustedEvidence, VerificationError> {
    let generation: String =
        store
            .read_connection()
            .query_row("SELECT lower(hex(randomblob(32)))", [], |r| r.get(0))?;
    let body = verify_body(
        store,
        owner,
        execution,
        artifact,
        permit,
        executor,
        cancel,
        generation,
        crate::execution_resources::Admission::ProfileOnly,
        None,
    )?;
    retain_evidence(store, owner, body, permit.policy.max_age_ms, cancel, || {})
}
#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
mod prepare_preflight_tests {
    use super::*;

    #[test]
    fn prepare_rejects_nonzero_exit_before_runtime_or_capture_side_effects() {
        let mut fixture = crate::local_target::tests::Fixture::new();
        let inventory = AssertionInventory::new(vec![AssertionCase {
            id: "expected-failure".into(),
            arguments: vec![],
            stdin_hex: String::new(),
            exit_code: 2,
            stdout: format!("sha256:{}", ObjectDigest::of(b"").hex()),
            stderr: format!("sha256:{}", ObjectDigest::of(b"").hex()),
        }])
        .unwrap();
        let before = {
            let connection = fixture.store.connection();
            (
                connection
                    .query_row("SELECT count(*) FROM capture_operations", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .unwrap(),
                connection
                    .query_row("SELECT count(*) FROM retention_roots", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .unwrap(),
                connection
                    .query_row("SELECT count(*) FROM outbox", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .unwrap(),
            )
        };

        let result = prepare(
            &mut fixture.store,
            OperationId::mint().unwrap(),
            Path::new("/no-such-harness"),
            "not-a-commit",
            &inventory.canonical_bytes(),
            VerifierPlan {
                image: "not-an-image".into(),
                command: CommandSpec {
                    program: "/bin/sh".into(),
                    arguments: vec![],
                    environment: Default::default(),
                },
                max_age_ms: 1000,
            },
            PathBuf::from("/no-such-docker"),
            "unix:///no-such-endpoint",
        );
        assert!(matches!(
            result,
            Err(VerificationError::Unsupported(detail))
                if detail == NONZERO_EXIT_UNSUPPORTED
        ));

        let after = {
            let connection = fixture.store.connection();
            (
                connection
                    .query_row("SELECT count(*) FROM capture_operations", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .unwrap(),
                connection
                    .query_row("SELECT count(*) FROM retention_roots", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .unwrap(),
                connection
                    .query_row("SELECT count(*) FROM outbox", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .unwrap(),
            )
        };
        assert_eq!(after, before);
    }
}
