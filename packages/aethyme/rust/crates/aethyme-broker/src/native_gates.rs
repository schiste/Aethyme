//! Actual native gate execution for an exact Local composed candidate.
//! Jobs are outside the legacy session promotion queue. Observations confer no
//! TrustedEvidence, provider acceptance or authority to move an integration ref.
use crate::collaboration_archive::{self as archive, CommitOid, ObjectDigest};
use crate::collaboration_state::CollaborationStore;
use crate::local_target::{self as target, ControllerOwner};
use crate::local_wave::{self as wave, WavePhase};
use crate::{Broker, BrokerOpError, GitRepo};
use aethyme_contracts::experimental_v0::{
    FieldKind, FieldSpec, Record, RecordId, RecordSchema, SourceEntry, SourceSnapshot,
    SourceSnapshotId, source_snapshot::EntryKind,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;
use std::time::{Duration, Instant};

pub const PROFILE: &str = "aethyme.native-local-gates/experimental-v1";
const SCHEMA_NAME: &str = "aethyme.native-gate-job/experimental-v1";
static SCHEMA: RecordSchema = RecordSchema {
    name: SCHEMA_NAME,
    fields: &[FieldSpec {
        name: "spec",
        required: true,
        kind: FieldKind::Opaque,
        capability: None,
    }],
    capabilities: &[],
};
#[derive(Debug, thiserror::Error)]
pub enum NativeGateError {
    #[error("native gate job does not match the retained current wave")]
    Mismatch,
    #[error("native gate binding refused: {0}")]
    Binding(&'static str),
    #[error("native gate canonical record refused: {0}")]
    Canonical(String),
    #[error("native gate profile is unsupported: {0}")]
    Unsupported(String),
    #[error("native gate outcome is unknown; reconcile before further dispatch")]
    Unknown,
    #[error("native gate roots were released; this history cannot be dispatched")]
    RootsReleased,
    #[error(transparent)]
    Archive(#[from] archive::ArchiveError),
    #[error(transparent)]
    Target(#[from] target::TargetError),
    #[error(transparent)]
    Wave(#[from] wave::WaveError),
    #[error(transparent)]
    Broker(#[from] BrokerOpError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Git(#[from] crate::git::GitError),
    #[error(transparent)]
    Gates(#[from] crate::gates::GateConfigError),
    #[error(transparent)]
    Preparation(#[from] crate::preparation::PreparationError),
    #[error(transparent)]
    Sensitive(#[from] crate::collaboration_sensitive::SensitiveError),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub profile: String,
    pub repository_root: String,
    pub repository_common_dir: String,
    pub target: String,
    pub revision: String,
    pub epoch: String,
    pub wave: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthesized_group: Option<wave::SynthesizedGroupProvenance>,
    pub baseline: String,
    pub subject: String,
    pub base_commit: String,
    pub tree: String,
    pub commit: String,
    pub inputs: Vec<String>,
    pub policy_sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate_config_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graph_config_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preparation_sha256: Option<String>,
    pub max_dispatches: u32,
    pub execution_allowance_seconds: u64,
}
#[derive(Debug, Clone)]
pub struct Job {
    pub id: RecordId,
    pub spec: Spec,
    pub state: String,
    pub observation: Option<Value>,
    /// An irreversible retention-release witness. The job row remains
    /// readable, while its source edges no longer participate in GC.
    pub roots_released_ms: Option<i64>,
}

const RETENTION_LIST_LIMIT: i64 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RootReleaseStatus {
    Released,
    AlreadyReleased,
    UnknownHeld,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetentionStatus {
    pub id: String,
    pub target: String,
    pub state: String,
    pub roots_released_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetentionListing {
    pub jobs: Vec<RetentionStatus>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RootRelease {
    pub status: RootReleaseStatus,
    pub job: RetentionStatus,
}
fn id(text: &str) -> Result<RecordId, NativeGateError> {
    RecordId::parse(text).map_err(|_| NativeGateError::Mismatch)
}
fn commit(text: &str) -> Result<CommitOid, NativeGateError> {
    Ok(CommitOid::parse(text)?)
}
fn snapshot(text: &str) -> Result<SourceSnapshotId, NativeGateError> {
    SourceSnapshotId::parse(text).map_err(|_| NativeGateError::Mismatch)
}
fn digest(text: &str) -> Result<ObjectDigest, NativeGateError> {
    Ok(ObjectDigest::of_snapshot(&snapshot(&format!(
        "sha256:{text}"
    ))?))
}
fn record(spec: &Spec) -> Result<Record, NativeGateError> {
    Record::decode(
        &serde_json::to_vec(&json!({"schema":SCHEMA_NAME,"spec":spec}))?,
        &[&SCHEMA],
    )
    .map_err(|error| NativeGateError::Canonical(format!("job spec: {error}")))
}
fn repository_binding(broker: &Broker, spec: &Spec) -> Result<(), NativeGateError> {
    if broker.main_root_path().canonicalize()?.to_str() != Some(spec.repository_root.as_str())
        || broker
            .repo_handle()
            .git_common_dir()?
            .canonicalize()?
            .to_str()
            != Some(spec.repository_common_dir.as_str())
    {
        return Err(NativeGateError::Binding(
            "broker repository trust namespace",
        ));
    }
    Ok(())
}
struct StoredJobRow {
    body: Vec<u8>,
    state: String,
    observation: Option<Vec<u8>>,
    target: String,
    revision: String,
    epoch: String,
    wave: String,
    roots_released_ms: Option<i64>,
}

/// Durable historical read. Dispatch separately requires current authority.
pub fn load(store: &CollaborationStore, key: &RecordId) -> Result<Job, NativeGateError> {
    let _response = crate::collaboration_sensitive::response_use(store)?;
    let row = store
        .read_connection()
        .query_row(
            "SELECT body,state,observation,target,revision,epoch,wave,roots_released_ms FROM local_native_gate_jobs WHERE id=?1",
            [key.as_str()],
            |row| {
                Ok(StoredJobRow {
                    body: row.get(0)?,
                    state: row.get(1)?,
                    observation: row.get(2)?,
                    target: row.get(3)?,
                    revision: row.get(4)?,
                    epoch: row.get(5)?,
                    wave: row.get(6)?,
                    roots_released_ms: row.get(7)?,
                })
            },
        )
        .optional()?
        .ok_or(NativeGateError::Mismatch)?;
    let rec = Record::decode(&row.body, &[&SCHEMA]).map_err(|_| NativeGateError::Mismatch)?;
    let body: Value = serde_json::from_slice(&row.body)?;
    let spec: Spec = serde_json::from_value(body["spec"].clone())?;
    if rec.id() != *key
        || rec.canonical_bytes() != row.body
        || spec.profile != PROFILE
        || spec.max_dispatches != 1
        || spec.execution_allowance_seconds != 300
        || spec.target != row.target
        || spec.revision != row.revision
        || spec.epoch != row.epoch
        || spec.wave != row.wave
    {
        return Err(NativeGateError::Mismatch);
    }
    Ok(Job {
        id: key.clone(),
        spec,
        state: row.state,
        observation: row
            .observation
            .map(|bytes| serde_json::from_slice(&bytes))
            .transpose()?,
        roots_released_ms: row.roots_released_ms,
    })
}

fn load_exact_spec(
    store: &CollaborationStore,
    spec: &Spec,
) -> Result<Option<Job>, NativeGateError> {
    let key = record(spec)?.id();
    let exists: bool = store.read_connection().query_row(
        "SELECT EXISTS(SELECT 1 FROM local_native_gate_jobs WHERE id=?1)",
        [key.as_str()],
        |row| row.get(0),
    )?;
    if !exists {
        return Ok(None);
    }
    let job = load(store, &key)?;
    if job.spec != *spec {
        return Err(NativeGateError::Mismatch);
    }
    Ok(Some(job))
}

fn retention_status(
    id: String,
    target: String,
    state: String,
    roots_released_ms: Option<i64>,
) -> RetentionStatus {
    RetentionStatus {
        id,
        target,
        state,
        roots_released_ms,
    }
}

/// List a bounded page of job-retention records for the currently appointed
/// target owner. This is an operator view only; it does not inspect or
/// reconstruct archived source.
pub fn list_retention(
    store: &CollaborationStore,
    owner: &ControllerOwner,
) -> Result<RetentionListing, NativeGateError> {
    let _authority = target::authorize(store, owner)?;
    let mut statement = store.read_connection().prepare(
        "SELECT id,target,state,roots_released_ms FROM local_native_gate_jobs
         WHERE target=?1 ORDER BY rowid DESC LIMIT ?2",
    )?;
    let rows = statement
        .query_map(params![owner.target(), RETENTION_LIST_LIMIT + 1], |row| {
            Ok(retention_status(
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let truncated = rows.len() as i64 > RETENTION_LIST_LIMIT;
    Ok(RetentionListing {
        jobs: rows
            .into_iter()
            .take(RETENTION_LIST_LIMIT as usize)
            .collect(),
        truncated,
    })
}

/// Read one retention record without relying on its archived source object.
pub fn retention_status_for(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    key: &RecordId,
) -> Result<RetentionStatus, NativeGateError> {
    let _authority = target::authorize(store, owner)?;
    store
        .read_connection()
        .query_row(
            "SELECT id,target,state,roots_released_ms FROM local_native_gate_jobs
             WHERE id=?1 AND target=?2",
            params![key.as_str(), owner.target()],
            |row| {
                Ok(retention_status(
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                ))
            },
        )
        .optional()?
        .ok_or(NativeGateError::Mismatch)
}

/// Explicitly abandon a never-dispatched prepared job or release the roots of
/// a terminal completed job. Dispatching/unknown jobs stay rooted: this
/// runtime has no job-correlated process-tree proof that would make releasing
/// them safe.
pub fn release_roots(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    key: &RecordId,
) -> Result<RootRelease, NativeGateError> {
    let _authority = target::authorize(store, owner)?;
    let _archive = crate::collaboration_gc::archive_use(store)?;
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let row: Option<(String, String, Option<i64>)> = tx
        .query_row(
            "SELECT target,state,roots_released_ms FROM local_native_gate_jobs WHERE id=?1",
            [key.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((job_target, state, released_ms)) = row else {
        return Err(NativeGateError::Mismatch);
    };
    if owner.target() != job_target {
        return Err(NativeGateError::Binding("appointed target owner"));
    }
    let status = if released_ms.is_some() {
        RootReleaseStatus::AlreadyReleased
    } else {
        match state.as_str() {
            "prepared" | "completed" => {
                let released_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .min(i64::MAX as u128) as i64;
                if tx.execute(
                    "UPDATE local_native_gate_jobs SET roots_released_ms=?2
                     WHERE id=?1 AND state=?3 AND roots_released_ms IS NULL",
                    params![key.as_str(), released_ms, state.as_str()],
                )? != 1
                {
                    return Err(NativeGateError::Unknown);
                }
                RootReleaseStatus::Released
            }
            "dispatching" | "unknown" => RootReleaseStatus::UnknownHeld,
            _ => return Err(NativeGateError::Mismatch),
        }
    };
    let released_ms: Option<i64> = tx.query_row(
        "SELECT roots_released_ms FROM local_native_gate_jobs WHERE id=?1",
        [key.as_str()],
        |row| row.get(0),
    )?;
    tx.commit()?;
    Ok(RootRelease {
        status,
        job: retention_status(key.to_string(), job_target, state, released_ms),
    })
}

struct Policy {
    gates: Vec<crate::gates::Gate>,
    present: bool,
    graph: aethyme_graph_storage::GraphIntegrityPolicy,
    trust: crate::broker::gate_trust::GatePolicy,
    gate_sha: Option<String>,
    graph_sha: Option<String>,
    prepare_sha: Option<String>,
}
fn raw_text(repo: &Path, base: &CommitOid, path: &str) -> Result<Option<String>, NativeGateError> {
    archive::raw_file_at_commit(repo, base, path.as_bytes())?
        .map(|bytes| String::from_utf8(bytes).map_err(|_| NativeGateError::Mismatch))
        .transpose()
}
fn optional_sha(text: &Option<String>) -> Option<String> {
    text.as_ref().map(|s| ObjectDigest::of(s.as_bytes()).hex())
}
/// A committed `.aethyme/<stem>` in either format (#1223), as TOML text.
fn config_text(
    repo: &Path,
    base: &CommitOid,
    stem: &str,
) -> Result<Option<String>, NativeGateError> {
    aethyme_config::try_read_config_text_with(aethyme_config::CONFIG_DIR, stem, |relpath| {
        raw_text(repo, base, relpath)
    })?
    .and_then(|text| {
        text.map(|text| text.toml_text().map(|toml| toml.into_owned()))
            .transpose()
    })
    .map_err(|error| NativeGateError::Unsupported(error.to_string()))
}
fn policy(repo: &Path, base: &CommitOid) -> Result<Policy, NativeGateError> {
    let gate = config_text(repo, base, "gates")?;
    let graph = config_text(repo, base, "config")?;
    let prepare = config_text(repo, base, "prepare")?;
    let gates = gate
        .as_ref()
        .map(|s| crate::gates::parse_gates(s))
        .transpose()?
        .unwrap_or_default();
    // No unbounded legacy gate definitions are admitted by this finite profile.
    if gates.len() > 32
        || gates.iter().any(|g| {
            g.timeout_seconds.is_none_or(|t| t == 0 || t > 60)
                || g.resource_wait_seconds > 60
                || g.managed_cache.is_some()
        })
    {
        return Err(NativeGateError::Unsupported(
            "require at most 32 gates, each with a 1..60 second timeout, at most 60 seconds admission wait, and no managed cache".into(),
        ));
    }
    let preparation = prepare
        .as_ref()
        .map(|s| crate::preparation::parse_config(s))
        .transpose()?;
    let graph_policy = graph
        .as_ref()
        .map(|s| aethyme_graph_storage::GraphIntegrityPolicy::parse(s))
        .transpose()
        .map_err(|_| NativeGateError::Mismatch)?
        .unwrap_or_default();
    Ok(Policy {
        trust: crate::broker::gate_trust::GatePolicy::from_parts(&gates, preparation.as_ref()),
        present: gate.is_some(),
        gates,
        graph: graph_policy,
        gate_sha: optional_sha(&gate),
        graph_sha: optional_sha(&graph),
        prepare_sha: optional_sha(&prepare),
    })
}
fn current(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    spec: &Spec,
) -> Result<(), NativeGateError> {
    target::authorize(store, owner)?;
    let status = target::status(store, owner.target())?;
    if owner.target() != spec.target
        || owner.authority_epoch().as_str() != spec.epoch
        || status.revision.as_ref().map(RecordId::as_str) != Some(spec.revision.as_str())
        || !(status.phase == target::Phase::Verifying
            || (status.phase == target::Phase::WaitingForInput
                && status.resume_phase == Some(target::Phase::Verifying)))
    {
        return Err(NativeGateError::Binding(
            "current target revision, epoch or dispatch phase",
        ));
    }
    let w = wave::load(store, &id(&spec.wave)?)?;
    if w.phase != WavePhase::Verifying
        || w.spec.target != spec.target
        || w.spec.revision != spec.revision
        || w.spec.baseline != spec.baseline
        || w.candidate_commit.as_ref().map(CommitOid::as_str) != Some(spec.commit.as_str())
        || w.candidate_snapshot.as_ref().map(SourceSnapshotId::as_str)
            != Some(spec.subject.as_str())
        || w.recipe_object.map(|d| d.hex()) != spec.recipe
        || w.resolution_result.is_some() != spec.synthesized_group.is_some()
    {
        return Err(NativeGateError::Binding(
            "current wave candidate, recipe or baseline",
        ));
    }
    Ok(())
}
pub(crate) fn validate_source(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    repo: &Path,
    spec: &Spec,
) -> Result<crate::types::VerificationCandidate, NativeGateError> {
    let _archive_guard = crate::collaboration_gc::archive_use(store)?;
    let _response_guard = crate::collaboration_sensitive::response_use(store)?;
    let candidate = commit(&spec.commit)?;
    let baseline = commit(&spec.base_commit)?;
    if archive::raw_commit_tree(repo, &candidate)? != spec.tree
        || archive::snapshot_of_commit(repo, &candidate)?.id() != snapshot(&spec.subject)?
        || archive::snapshot_of_commit(repo, &baseline)?.id() != snapshot(&spec.baseline)?
    {
        return Err(NativeGateError::Binding(
            "raw Git candidate or baseline source",
        ));
    }
    let w = wave::load(store, &id(&spec.wave)?)?;
    if let Some(expected_provenance) = &spec.synthesized_group {
        let (synthesis, provenance) = wave::read_synthesized_wave_result(store, repo, &w)?
            .ok_or(NativeGateError::Mismatch)?;
        if &provenance != expected_provenance || w.spec.baseline != spec.baseline {
            return Err(NativeGateError::Binding(
                "synthesized result or provenance differs from the current wave",
            ));
        }
        wave::validate_synthesized_group_roots(store, &w, &synthesis, &provenance)?;
        let (source_wave, source) =
            wave::load_composition_source(store, owner, repo, &id(&spec.wave)?)?;
        if source_wave != w {
            return Err(NativeGateError::Binding("current synthesized wave"));
        }
        match (&spec.recipe, source.as_ref()) {
            (Some(recipe), Some(source))
                if source
                    .composition
                    .recipe
                    .as_ref()
                    .is_some_and(|reference| reference.object.hex() == *recipe) =>
            {
                let recipe_ref = source
                    .composition
                    .recipe
                    .as_ref()
                    .ok_or(NativeGateError::Mismatch)?;
                let recipe_bytes = archive::read_object(store, &recipe_ref.object)?;
                let recipe_body: Value = serde_json::from_slice(&recipe_bytes)?;
                if recipe_body.get("seeded_conflict").is_some() {
                    let candidate = source
                        .composition
                        .outcome
                        .candidate()
                        .ok_or(NativeGateError::Mismatch)?;
                    let inputs = candidate
                        .inputs
                        .iter()
                        .map(|input| input.result.as_str().to_owned())
                        .collect::<Vec<_>>();
                    if candidate.mode != crate::composition::CompositionMode::Hybrid
                        || candidate.subject.as_str() != spec.subject
                        || candidate.tree != spec.tree
                        || candidate.commit.as_str() != spec.commit
                        || candidate.baseline.as_str() != spec.base_commit
                        || inputs != spec.inputs
                        || Some(&candidate.subject) != w.candidate_snapshot.as_ref()
                        || Some(&candidate.commit) != w.candidate_commit.as_ref()
                        || synthesis.candidate.baseline.as_str() != spec.base_commit
                    {
                        return Err(NativeGateError::Binding(
                            "native gate spec does not use the complete seeded suffix candidate",
                        ));
                    }
                } else if synthesis.candidate.subject.as_str() != spec.subject
                    || synthesis.candidate.tree != spec.tree
                    || synthesis.candidate.commit.as_str() != spec.commit
                    || synthesis.candidate.baseline.as_str() != spec.base_commit
                    || synthesis
                        .candidate
                        .inputs
                        .iter()
                        .map(|input| input.result.as_str().to_owned())
                        .collect::<Vec<_>>()
                        != spec.inputs
                {
                    return Err(NativeGateError::Binding(
                        "native gate spec differs from its exact settled synthesized candidate",
                    ));
                }
            }
            (None, None) if w.recipe_object.is_none() && w.composition_source.is_none() => {
                if synthesis.candidate.subject.as_str() != spec.subject
                    || synthesis.candidate.tree != spec.tree
                    || synthesis.candidate.commit.as_str() != spec.commit
                    || synthesis.candidate.baseline.as_str() != spec.base_commit
                    || synthesis
                        .candidate
                        .inputs
                        .iter()
                        .map(|input| input.result.as_str().to_owned())
                        .collect::<Vec<_>>()
                        != spec.inputs
                {
                    return Err(NativeGateError::Binding(
                        "legacy synthesized spec differs from its exact result",
                    ));
                }
            }
            _ => return Err(NativeGateError::Binding("original composition source")),
        }
        return Ok(crate::types::VerificationCandidate {
            base: spec.base_commit.clone(),
            inputs: spec.inputs.clone(),
            tree: spec.tree.clone(),
            commit: spec.commit.clone(),
        });
    }
    if w.resolution_result.is_some() {
        return Err(NativeGateError::Binding(
            "synthesized result is absent from the native gate Spec",
        ));
    }
    let recipe_hex = spec.recipe.as_deref().ok_or(NativeGateError::Mismatch)?;
    let bytes = archive::read_object(store, &digest(recipe_hex)?)?;
    let recipe = Record::decode(&bytes, &[&crate::composer::COMPOSITION_RECIPE_SCHEMA])
        .map_err(|_| NativeGateError::Mismatch)?;
    if recipe.canonical_bytes() != bytes {
        return Err(NativeGateError::Binding("canonical composition recipe"));
    }
    let body: Value = serde_json::from_slice(&bytes)?;
    if body["candidate"] != spec.subject
        || body["candidate_commit"] != spec.commit
        || body["baseline"] != spec.baseline
        || body["baseline_commit"] != spec.base_commit
        || recipe_inputs(store, &body)? != spec.inputs
    {
        return Err(NativeGateError::Binding(
            "composition recipe candidate, baseline or inputs",
        ));
    }
    match &w.spec.prior_completion {
        Some(_) => {
            let revision = target::load_revision(store, &id(&spec.revision)?)?;
            let prior = wave::prior_completion::load(store, owner, &w, &revision)?
                .ok_or(NativeGateError::Mismatch)?;
            prior.validate_retained_completion(store)?;
            prior.validate_git_binding(store, repo)?;
            if body["accepted_baseline"] != prior.recipe_binding()
                || prior.request_baseline_commit().as_str() != spec.base_commit
                || body["profile"] != prior.producer_profile()
            {
                return Err(NativeGateError::Binding(
                    "accepted-baseline recipe differs from prior private completion",
                ));
            }
        }
        None if !body["accepted_baseline"].is_null() => {
            return Err(NativeGateError::Binding(
                "unexpected accepted-baseline recipe",
            ));
        }
        None => (),
    }
    let revision = target::load_revision(store, &id(&spec.revision)?)?;
    let mut ordered = Vec::new();
    for row in body["order"].as_array().ok_or(NativeGateError::Mismatch)? {
        let alias = row["id"].as_str().ok_or(NativeGateError::Mismatch)?;
        let authorized = revision
            .spec
            .contributions
            .iter()
            .find(|c| c.id == alias)
            .ok_or(NativeGateError::Mismatch)?;
        if row["lineage"] != authorized.lineage {
            return Err(NativeGateError::Mismatch);
        }
        ordered.push(alias.to_string());
    }
    ordered.sort();
    let mut selected = w.spec.selected.clone();
    selected.sort();
    if ordered != selected {
        return Err(NativeGateError::Binding(
            "wave selection differs from applied recipe",
        ));
    }
    Ok(crate::types::VerificationCandidate {
        base: spec.base_commit.clone(),
        inputs: spec.inputs.clone(),
        tree: spec.tree.clone(),
        commit: spec.commit.clone(),
    })
}
fn recipe_inputs(store: &CollaborationStore, body: &Value) -> Result<Vec<String>, NativeGateError> {
    body["order"]
        .as_array()
        .ok_or(NativeGateError::Mismatch)?
        .iter()
        .map(|row| {
            let lineage = archive::retained_contribution(
                store,
                &id(row["lineage"].as_str().ok_or(NativeGateError::Mismatch)?)?,
            )?
            .ok_or(NativeGateError::Mismatch)?;
            if row["base"] != lineage.base.snapshot_id.as_str()
                || row["result"] != lineage.result.snapshot_id.as_str()
            {
                return Err(NativeGateError::Mismatch);
            }
            Ok(lineage.result.commit.as_str().to_owned())
        })
        .collect()
}
/// Freeze a single-dispatch job before any native code executes.
pub fn prepare(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    broker: &mut Broker,
    wave_id: &RecordId,
) -> Result<Job, NativeGateError> {
    let _archive_guard = crate::collaboration_gc::archive_use(store)?;
    let _response_guard = crate::collaboration_sensitive::response_use(store)?;
    let (w, source) =
        wave::load_composition_source(store, owner, broker.repo_handle().root(), wave_id)?;
    let synthesized = wave::read_synthesized_wave_result(store, broker.repo_handle().root(), &w)?;
    let (base, subject, tree, candidate_commit, inputs, recipe, synthesized_group, synthesis) =
        if let Some((synthesis, provenance)) = synthesized {
            let seeded = if let Some(source) = source.as_ref() {
                if let Some(recipe) = source.composition.recipe.as_ref() {
                    let bytes = archive::read_object(store, &recipe.object)?;
                    let body: Value = serde_json::from_slice(&bytes)?;
                    body.get("seeded_conflict").is_some()
                } else {
                    false
                }
            } else {
                false
            };
            if seeded {
                let candidate = source
                    .as_ref()
                    .and_then(|source| source.composition.outcome.candidate())
                    .ok_or(NativeGateError::Mismatch)?;
                if candidate.mode != crate::composition::CompositionMode::Hybrid
                    || Some(&candidate.subject) != w.candidate_snapshot.as_ref()
                    || Some(&candidate.commit) != w.candidate_commit.as_ref()
                {
                    return Err(NativeGateError::Binding(
                        "seeded suffix source is not the current hybrid wave candidate",
                    ));
                }
                (
                    candidate.baseline.as_str().to_owned(),
                    candidate.subject.to_string(),
                    candidate.tree.clone(),
                    candidate.commit.as_str().to_owned(),
                    candidate
                        .inputs
                        .iter()
                        .map(|input| input.result.as_str().to_owned())
                        .collect(),
                    w.recipe_object.map(|object| object.hex()),
                    Some(provenance),
                    Some(synthesis),
                )
            } else {
                (
                    synthesis.candidate.baseline.as_str().to_owned(),
                    synthesis.candidate.subject.to_string(),
                    synthesis.candidate.tree.clone(),
                    synthesis.candidate.commit.as_str().to_owned(),
                    synthesis
                        .candidate
                        .inputs
                        .iter()
                        .map(|input| input.result.as_str().to_owned())
                        .collect(),
                    w.recipe_object.map(|object| object.hex()),
                    Some(provenance),
                    Some(synthesis),
                )
            }
        } else {
            let recipe = w.recipe_object.ok_or(NativeGateError::Mismatch)?;
            let bytes = archive::read_object(store, &recipe)?;
            let rec = Record::decode(&bytes, &[&crate::composer::COMPOSITION_RECIPE_SCHEMA])
                .map_err(|error| NativeGateError::Canonical(format!("prepare recipe: {error}")))?;
            if rec.canonical_bytes() != bytes {
                return Err(NativeGateError::Mismatch);
            }
            let body: Value = serde_json::from_slice(&bytes)?;
            let base = body["baseline_commit"]
                .as_str()
                .ok_or(NativeGateError::Mismatch)?
                .to_owned();
            let subject = w
                .candidate_snapshot
                .as_ref()
                .ok_or(NativeGateError::Mismatch)?
                .to_string();
            let candidate_commit = w
                .candidate_commit
                .as_ref()
                .ok_or(NativeGateError::Mismatch)?
                .as_str()
                .to_owned();
            let tree =
                archive::raw_commit_tree(broker.repo_handle().root(), &commit(&candidate_commit)?)?;
            let inputs = recipe_inputs(store, &body)?;
            (
                base,
                subject,
                tree,
                candidate_commit,
                inputs,
                Some(recipe.hex()),
                None,
                None,
            )
        };
    let p = policy(broker.repo_handle().root(), &commit(&base)?)?;
    require_explicit_trust(broker, &p.trust)?;
    let spec = Spec {
        profile: PROFILE.into(),
        repository_root: broker
            .main_root_path()
            .canonicalize()?
            .to_str()
            .ok_or(NativeGateError::Mismatch)?
            .into(),
        repository_common_dir: broker
            .repo_handle()
            .git_common_dir()?
            .canonicalize()?
            .to_str()
            .ok_or(NativeGateError::Mismatch)?
            .into(),
        target: owner.target().into(),
        revision: w.spec.revision.clone(),
        epoch: owner.authority_epoch().as_str().into(),
        wave: wave_id.to_string(),
        recipe,
        synthesized_group,
        baseline: w.spec.baseline.clone(),
        subject,
        base_commit: base,
        tree,
        commit: candidate_commit,
        inputs,
        policy_sha256: p.trust.policy_sha256,
        gate_config_sha256: p.gate_sha,
        graph_config_sha256: p.graph_sha,
        preparation_sha256: p.prepare_sha,
        max_dispatches: 1,
        execution_allowance_seconds: 300,
    };
    if let Some(job) = load_exact_spec(store, &spec)? {
        repository_binding(broker, &job.spec)?;
        if job.roots_released_ms.is_some() {
            return Err(NativeGateError::RootsReleased);
        }
        current(store, owner, &job.spec)?;
        validate_source(store, owner, broker.repo_handle().root(), &job.spec)?;
        return Ok(job);
    }
    current(store, owner, &spec)?;
    validate_source(store, owner, broker.repo_handle().root(), &spec)?;
    let rec = record(&spec)?;
    let object = archive::put_object(store, &rec.canonical_bytes())?;
    let recipe_bytes = if let Some(recipe) = spec.recipe.as_deref() {
        let recipe_digest = digest(recipe)?;
        Some(archive::read_object(store, &recipe_digest)?)
    } else {
        None
    };
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute("INSERT OR IGNORE INTO local_native_gate_jobs(id,wave,target,revision,epoch,body,state) VALUES(?1,?2,?3,?4,?5,?6,'prepared')",
        params![rec.id().as_str(),spec.wave,spec.target,spec.revision,spec.epoch,rec.canonical_bytes()])?;
    let mut roots = std::collections::BTreeSet::new();
    roots.insert(("snapshot", spec.subject.clone()));
    roots.insert(("snapshot", spec.baseline.clone()));
    roots.insert(("object", object.hex()));
    if let Some(recipe) = &spec.recipe {
        roots.insert(("object", recipe.clone()));
    }
    if let Some(provenance) = &spec.synthesized_group {
        roots.insert(("object", provenance.request_object.clone()));
        roots.insert(("object", provenance.group_object.clone()));
        roots.insert(("object", provenance.input_manifest_sha256.clone()));
        for input in &provenance.inputs {
            roots.insert(("snapshot", input.base_snapshot.clone()));
            roots.insert(("snapshot", input.result_snapshot.clone()));
            roots.insert(("contribution", input.lineage.clone()));
        }
    }
    if let Some(synthesis) = &synthesis {
        for entry in &synthesis.input_manifest.entries {
            if let Some(content) = &entry.sha256 {
                roots.insert(("object", content.clone()));
            }
        }
    }
    for (kind, reference) in roots {
        tx.execute(
            "INSERT OR IGNORE INTO local_native_gate_roots(job,kind,reference) VALUES(?1,?2,?3)",
            params![rec.id().as_str(), kind, reference],
        )?;
    }
    if let Some(bytes) = recipe_bytes.as_deref() {
        let body: Value = serde_json::from_slice(bytes)?;
        for member in body["order"].as_array().ok_or(NativeGateError::Mismatch)? {
            for field in ["base", "result"] {
                tx.execute("INSERT OR IGNORE INTO local_native_gate_roots(job,kind,reference) VALUES(?1,'snapshot',?2)",params![rec.id().as_str(),member[field].as_str().ok_or(NativeGateError::Mismatch)?])?;
            }
            tx.execute("INSERT OR IGNORE INTO local_native_gate_roots(job,kind,reference) VALUES(?1,'contribution',?2)",params![rec.id().as_str(),member["lineage"].as_str().ok_or(NativeGateError::Mismatch)?])?;
        }
    }
    tx.commit()?;
    load(store, &rec.id())
}
fn refuse_git_redirects() -> Result<(), NativeGateError> {
    if std::env::vars_os().any(|(k, value)| {
        let key = k.to_string_lossy();
        key.starts_with("GIT_")
            && !(key == "GIT_PAGER" && matches!(value.to_str(), Some("" | "cat" | "/bin/cat")))
    }) {
        return Err(NativeGateError::Unsupported(
            "inherited Git environment is outside this profile".into(),
        ));
    }
    Ok(())
}
fn require_explicit_trust(
    broker: &Broker,
    policy: &crate::broker::gate_trust::GatePolicy,
) -> Result<(), NativeGateError> {
    use crate::broker::gate_trust::{TrustDecision, assess_trusted_for_host_state};
    let main_root = broker.main_root_path();
    let host_state_dir = broker.storage_context().host_state_dir.clone();
    match assess_trusted_for_host_state(&main_root, host_state_dir.as_deref(), policy, None)? {
        TrustDecision::Trusted | TrustDecision::NoCommands => Ok(()),
        TrustDecision::Grandfathered | TrustDecision::TestEscape => {
            Err(NativeGateError::Unsupported(
                "native jobs require an explicit current policy approval".into(),
            ))
        }
    }
}
fn checked_checkout_placement(root: &Path, repository: &Path) -> Result<(), NativeGateError> {
    let root = root.canonicalize()?;
    let repository = repository.canonicalize()?;
    if root.starts_with(repository)
        || root.ancestors().any(|ancestor| {
            ancestor.join(".git").exists()
                || ancestor.join("Cargo.toml").exists()
                || ancestor.join("package.json").exists()
                || ancestor.join("pyproject.toml").exists()
                || ancestor.join("go.mod").exists()
        })
    {
        return Err(NativeGateError::Unsupported(
            "native checkout must be outside enclosing repositories and project manifests".into(),
        ));
    }
    Ok(())
}
fn bounded_git(repo: &Path, args: &[&str]) -> Result<(), NativeGateError> {
    let mut command = archive::git(repo);
    command.args(args).stdin(std::process::Stdio::null());
    let output =
        crate::bounded_output::output_limited(&mut command, Duration::from_secs(10), 1024 * 1024)?;
    if !matches!(output,crate::bounded_output::LimitedOutput::Completed(ref out) if out.status.success())
    {
        return Err(NativeGateError::Unsupported(
            "exact private Git metadata unavailable".into(),
        ));
    }
    Ok(())
}
struct Checkout {
    _root: tempfile::TempDir,
    repo: GitRepo,
}

fn reconstruct_checkout_source(
    store: &CollaborationStore,
    id: &SourceSnapshotId,
    dest: &Path,
    limits: archive::ReconstructionLimits,
) -> Result<SourceSnapshot, NativeGateError> {
    archive::reconstruct_bounded(store, id, dest, limits).map_err(Into::into)
}

impl Checkout {
    fn new(store: &CollaborationStore, repo: &Path, spec: &Spec) -> Result<Self, NativeGateError> {
        refuse_git_redirects()?;
        let root = tempfile::tempdir()?;
        checked_checkout_placement(root.path(), repo)?;
        let source = root.path().join("source");
        let retained = archive::retained(store, &snapshot(&spec.subject)?)?
            .ok_or(NativeGateError::Mismatch)?;
        if retained.entry_count > 8192 || retained.content_bytes > 256 * 1024 * 1024 {
            return Err(NativeGateError::Unsupported("retained source bound".into()));
        }
        let manifest = reconstruct_checkout_source(
            store,
            &snapshot(&spec.subject)?,
            &source,
            crate::execution_snapshot::RECONSTRUCTION_LIMITS,
        )?;
        if manifest.entries().len() > 8192 {
            return Err(NativeGateError::Unsupported("source path bound".into()));
        }
        if manifest.entries().iter().any(|entry| {
            entry.path().rsplit(|byte| *byte == b'/').next() == Some(b".gitattributes".as_slice())
        }) {
            return Err(NativeGateError::Unsupported(
                "attribute/filter execution requires a separately appointed hermetic Git profile"
                    .into(),
            ));
        }
        let metadata = root.path().join("metadata");
        bounded_git(
            repo,
            &[
                "clone",
                "--shared",
                "--bare",
                "--no-tags",
                "--config",
                "core.hooksPath=/dev/null",
                "--config",
                "core.attributesFile=/dev/null",
                "--config",
                "core.autocrlf=false",
                "--config",
                "core.symlinks=true",
                "--config",
                "core.fileMode=true",
                "--config",
                "core.fsmonitor=false",
                "--template=",
                "--",
                repo.to_str().ok_or(NativeGateError::Mismatch)?,
                metadata.to_str().ok_or(NativeGateError::Mismatch)?,
            ],
        )?;
        let gitdir = metadata.to_str().ok_or(NativeGateError::Mismatch)?;
        bounded_git(repo, &["--git-dir", gitdir, "config", "core.bare", "false"])?;
        bounded_git(
            repo,
            &[
                "--git-dir",
                gitdir,
                "config",
                "core.worktree",
                source.to_str().ok_or(NativeGateError::Mismatch)?,
            ],
        )?;
        std::fs::write(source.join(".git"), format!("gitdir: {gitdir}\n"))?;
        bounded_git(&source, &["update-ref", "--no-deref", "HEAD", &spec.commit])?;
        bounded_git(&source, &["read-tree", &spec.commit])?;
        let discovered = GitRepo::discover(&source)?;
        if discovered.head_commit()? != spec.commit
            || physical_snapshot(&source)?.id() != manifest.id()
            || discovered.working_tree_hash()? != spec.tree
        {
            return Err(NativeGateError::Mismatch);
        }
        Ok(Self {
            _root: root,
            repo: discovered,
        })
    }
}
#[cfg(unix)]
fn physical_snapshot(root: &Path) -> Result<SourceSnapshot, NativeGateError> {
    physical_snapshot_with_limits(root, crate::execution_snapshot::RECONSTRUCTION_LIMITS)
}

#[cfg(unix)]
fn read_source_bytes(reader: impl std::io::Read, limit: usize) -> Result<Vec<u8>, NativeGateError> {
    use std::io::Read;
    let read_limit = limit
        .checked_add(1)
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| NativeGateError::Unsupported("source byte bound".into()))?;
    let mut bytes = Vec::new();
    reader.take(read_limit).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(NativeGateError::Unsupported("source byte bound".into()));
    }
    Ok(bytes)
}

#[cfg(unix)]
fn read_source_file(path: &Path, limit: usize) -> Result<(EntryKind, Vec<u8>), NativeGateError> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let opened = file.metadata()?;
    if !opened.is_file() {
        return Err(NativeGateError::Mismatch);
    }
    if opened.len() > u64::try_from(limit).unwrap_or(u64::MAX) {
        return Err(NativeGateError::Unsupported("source byte bound".into()));
    }
    let kind = if opened.permissions().mode() & 0o111 != 0 {
        EntryKind::Executable
    } else {
        EntryKind::Regular
    };
    Ok((kind, read_source_bytes(file, limit)?))
}

#[cfg(unix)]
fn physical_snapshot_with_limits(
    root: &Path,
    limits: archive::ReconstructionLimits,
) -> Result<SourceSnapshot, NativeGateError> {
    use std::os::unix::ffi::OsStrExt;
    struct Walk<'a> {
        root: &'a Path,
        limits: archive::ReconstructionLimits,
        entries: Vec<SourceEntry>,
        nodes: usize,
        total: usize,
    }
    impl Walk<'_> {
        fn visit(&mut self, path: &Path, depth: usize) -> Result<(), NativeGateError> {
            for leaf in std::fs::read_dir(path)? {
                let leaf = leaf?;
                let path = leaf.path();
                if path == self.root.join(".git") {
                    continue;
                }
                if depth >= self.limits.max_path_depth {
                    return Err(NativeGateError::Unsupported("source depth bound".into()));
                }
                if self.nodes >= self.limits.max_nodes {
                    return Err(NativeGateError::Unsupported("source node bound".into()));
                }
                self.nodes += 1;
                if leaf.file_name().as_bytes().len() > self.limits.max_component_bytes {
                    return Err(NativeGateError::Unsupported(
                        "source component bound".into(),
                    ));
                }
                let meta = std::fs::symlink_metadata(&path)?;
                if meta.is_dir() {
                    self.visit(&path, depth + 1)?;
                    continue;
                }
                if self.entries.len() >= self.limits.max_entries {
                    return Err(NativeGateError::Unsupported("source path bound".into()));
                }
                let remaining = self.limits.max_output_bytes - self.total;
                let limit = self.limits.max_file_bytes.min(remaining);
                let (kind, bytes) = if meta.file_type().is_symlink() {
                    let bytes = std::fs::read_link(&path)?.as_os_str().as_bytes().to_vec();
                    if bytes.len() > limit {
                        return Err(NativeGateError::Unsupported("source byte bound".into()));
                    }
                    (EntryKind::Symlink, bytes)
                } else if meta.is_file() {
                    read_source_file(&path, limit)?
                } else {
                    return Err(NativeGateError::Mismatch);
                };
                self.total += bytes.len();
                self.entries.push(SourceEntry::from_content(
                    path.strip_prefix(self.root)
                        .map_err(|_| NativeGateError::Mismatch)?
                        .as_os_str()
                        .as_bytes(),
                    kind,
                    &bytes,
                ));
            }
            Ok(())
        }
    }
    let mut walk = Walk {
        root,
        limits,
        entries: vec![],
        nodes: 0,
        total: 0,
    };
    walk.visit(root, 0)?;
    SourceSnapshot::new(walk.entries).map_err(|_| NativeGateError::Mismatch)
}
#[cfg(not(unix))]
fn physical_snapshot(_root: &Path) -> Result<SourceSnapshot, NativeGateError> {
    Err(NativeGateError::Unsupported(
        "appointed Unix physical-source profile required".into(),
    ))
}

/// Execute the existing native submit gate engine, with the exact candidate
/// commit and raw baseline policy. Reopening a dispatched job never retries.
pub fn run(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    broker: &mut Broker,
    key: &RecordId,
) -> Result<Job, NativeGateError> {
    run_inner(store, owner, broker, key, false)
}

/// Dispatch only a prepared job. Historical completion never becomes fresh
/// gate authority; the existing prepared-to-dispatching CAS closes races.
pub fn run_fresh(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    broker: &mut Broker,
    key: &RecordId,
) -> Result<Job, NativeGateError> {
    run_inner(store, owner, broker, key, true)
}

fn run_inner(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    broker: &mut Broker,
    key: &RecordId,
    fresh: bool,
) -> Result<Job, NativeGateError> {
    let _archive_guard = crate::collaboration_gc::archive_use(store)?;
    let _response_guard = crate::collaboration_sensitive::response_use(store)?;
    let _authority = target::authorize(store, owner)?;
    let job = load(store, key)?;
    repository_binding(broker, &job.spec)?;
    if job.state == "prepared" && job.roots_released_ms.is_some() {
        return Err(NativeGateError::RootsReleased);
    }
    current(store, owner, &job.spec)?;
    match job.state.as_str() {
        "completed" if !fresh => return Ok(job),
        "completed" => return Err(NativeGateError::Unknown),
        "dispatching" | "unknown" => return Err(NativeGateError::Unknown),
        "prepared" => (),
        _ => return Err(NativeGateError::Mismatch),
    }
    let candidate = validate_source(store, owner, broker.repo_handle().root(), &job.spec)?;
    let p = policy(broker.repo_handle().root(), &commit(&candidate.base)?)?;
    if p.trust.policy_sha256 != job.spec.policy_sha256
        || p.gate_sha != job.spec.gate_config_sha256
        || p.graph_sha != job.spec.graph_config_sha256
        || p.prepare_sha != job.spec.preparation_sha256
    {
        return Err(NativeGateError::Mismatch);
    }
    require_explicit_trust(broker, &p.trust)?;
    broker.sync_gate_definitions(&p.gates)?;
    let checkout = Checkout::new(store, broker.repo_handle().root(), &job.spec)?;
    let changed = checkout
        .repo
        .gate_scope_changed_between(&candidate.base, &candidate.commit)?;
    current(store, owner, &job.spec)?;
    // One SQL predicate observes current target/wave and dispatches atomically.
    if store.connection().execute(
        "UPDATE local_native_gate_jobs SET state='dispatching' WHERE id=?1 AND state='prepared' AND roots_released_ms IS NULL AND EXISTS (SELECT 1 FROM local_targets WHERE target=?2 AND active_revision=?3 AND (phase='verifying' OR (phase='waiting_for_input' AND resume_phase='verifying'))) AND EXISTS (SELECT 1 FROM local_waves WHERE wave=?4 AND phase='verifying')",
        params![key.as_str(),job.spec.target,job.spec.revision,job.spec.wave],
    )? != 1
    {
        let released: bool = store.connection().query_row(
            "SELECT roots_released_ms IS NOT NULL FROM local_native_gate_jobs WHERE id=?1",
            [key.as_str()],
            |row| row.get(0),
        )?;
        if released {
            return Err(NativeGateError::RootsReleased);
        }
        return Err(NativeGateError::Unknown);
    }
    let started = Instant::now();
    let outcome = (|| -> Result<Value, NativeGateError> {
        let graph_checkout = Checkout::new(store, broker.repo_handle().root(), &job.spec)?;
        let graph =
            crate::graph_integrity::verify_disposable_checkout(&graph_checkout.repo, &p.graph);
        // Same gate engine used by merge.rs. No separate gate definitions,
        // runner, cache implementation, queue replay or promotion path.
        let main = broker.main_root_path();
        let storage = broker.storage_context().gate_storage_locations();
        let outcomes = crate::gates::run_affected_bounded_in(
            broker.store(),
            &main,
            &checkout.repo,
            &p.gates,
            &changed,
            Duration::from_secs(job.spec.execution_allowance_seconds),
            storage,
        )?;
        let status = if !p.present {
            "no_configuration"
        } else if outcomes.is_empty() {
            "no_gates_triggered"
        } else if outcomes.iter().all(|o| o.status == crate::GateStatus::Pass) {
            "passed"
        } else if outcomes
            .iter()
            .filter(|o| o.status != crate::GateStatus::Pass)
            .all(crate::gates::GateRunOutcome::is_host_fault)
        {
            "deferred"
        } else {
            "failed"
        };
        // A gate modifying source or metadata invalidates its observations.
        if checkout.repo.head_commit()? != candidate.commit
            || physical_snapshot(checkout.repo.root())?.id() != snapshot(&job.spec.subject)?
            || checkout.repo.working_tree_hash()? != candidate.tree
        {
            return Err(NativeGateError::Mismatch);
        }
        Ok(
            json!({"profile":PROFILE,"status":status,"candidate":job.spec,"policy_sha256":p.trust.policy_sha256,
            "configured_gates":p.gates.len(),"selected_gates":outcomes.len(),"graph":graph,"gates":outcomes,
            "wall_ms":started.elapsed().as_millis(),"trusted_evidence":false,"provider_acceptance":false}),
        )
    })();
    match outcome {
        Ok(mut value) => {
            value["current_authority"] = json!(current(store, owner, &job.spec).is_ok());
            if store.connection().execute("UPDATE local_native_gate_jobs SET state='completed',observation=?2 WHERE id=?1 AND state='dispatching'",
                params![key.as_str(),serde_json::to_vec(&value)?])?!=1{return Err(NativeGateError::Unknown);}
            load(store, key)
        }
        Err(error) => {
            store.connection().execute("UPDATE local_native_gate_jobs SET state='unknown',observation=?2 WHERE id=?1 AND state='dispatching'",
                params![key.as_str(),serde_json::to_vec(&json!({"error":error.to_string(),"wall_ms":started.elapsed().as_millis()}))?])?;
            Err(error)
        }
    }
}

#[cfg(all(test, unix))]
pub(crate) fn run_in_clean_git_child(test_name: &str) -> bool {
    const CHILD_TEST_ENV: &str = "AETHYME_NATIVE_GATES_CLEAN_CHILD_TEST";
    const CHILD_ROOT_ENV: &str = "AETHYME_NATIVE_GATES_CLEAN_CHILD_ROOT";

    match (
        std::env::var_os(CHILD_TEST_ENV),
        std::env::var_os(CHILD_ROOT_ENV),
    ) {
        (Some(child_test), Some(child_root)) => {
            assert_eq!(
                child_test,
                std::ffi::OsString::from(test_name),
                "native-gate child marker must bind this exact test"
            );
            let child_root = std::path::PathBuf::from(child_root);
            let child_xdg = child_root.join("xdg-config");
            assert!(child_root.is_dir() && child_xdg.is_dir());
            for key in ["HOME", "TMPDIR", "TMP", "TEMP"] {
                assert_eq!(
                    std::env::var_os(key).as_deref(),
                    Some(child_root.as_os_str()),
                    "isolated native-gate child must use its private {key}"
                );
            }
            assert_eq!(
                std::env::var_os("XDG_CONFIG_HOME").as_deref(),
                Some(child_xdg.as_os_str()),
                "isolated native-gate child must use its private XDG config"
            );
            assert!(
                !std::env::vars_os().any(|(key, _)| key.to_string_lossy().starts_with("GIT_")),
                "isolated native-gate child must not inherit any GIT_* variable"
            );
            return false;
        }
        (None, None) => {}
        _ => panic!("native-gate child markers must be set together by the wrapper"),
    }

    let temporary = tempfile::tempdir().expect("create private native-gate test environment");
    let xdg_config = temporary.path().join("xdg-config");
    std::fs::create_dir(&xdg_config).expect("create private XDG config directory");

    let mut command = std::process::Command::new(
        std::env::current_exe().expect("locate current Rust test binary"),
    );
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let output = command
        .args([
            "--exact",
            test_name,
            "--test-threads=1",
            "--format=pretty",
            "--color=never",
        ])
        .env(CHILD_TEST_ENV, test_name)
        .env(CHILD_ROOT_ENV, temporary.path())
        .env("HOME", temporary.path())
        .env("XDG_CONFIG_HOME", &xdg_config)
        .env("TMPDIR", temporary.path())
        .env("TMP", temporary.path())
        .env("TEMP", temporary.path())
        .output()
        .expect("run exact native-gate test in a clean Git environment");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let expected_test_line = format!("test {test_name} ... ok");
    assert!(
        output.status.success()
            && stdout
                .lines()
                .any(|line| line == expected_test_line.as_str())
            && stdout
                .lines()
                .any(|line| line.starts_with("test result: ok. 1 passed; 0 failed;")),
        "isolated child did not run exactly one passing test ({test_name}); \
         status={:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );
    true
}

#[cfg(all(test, unix))]
#[path = "native_gates/tests.rs"]
mod tests;
