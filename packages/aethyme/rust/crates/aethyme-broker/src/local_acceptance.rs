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
    let text = archive::raw_file_at_commit(carrier.repo.root(), &commit, b".aethyme/config.toml")?
        .unwrap_or_default();
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
    let record = canonical(
        &QUALIFICATION,
        &serde_json::json!({"binding":binding.as_str(),"receipt":receipt.id.as_str(),"evidence":evidence.reference().id.as_str(),"execution":execution.id().as_str(),"epoch":owner.authority_epoch().as_str(),"expires_ms":evidence.expires_ms()}),
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
