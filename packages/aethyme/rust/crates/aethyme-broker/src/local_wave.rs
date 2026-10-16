//! Provisional finite Local candidate waves (#667). Capture notifications only
//! admit already-authorized inputs. Composition runs the real provisional
//! composer; verification and acceptance remain unavailable until their
//! trusted adapters exist. No candidate code or publication is executed here.

use std::collections::BTreeMap;
use std::path::Path;

use aethyme_contracts::experimental_v0::{
    FieldKind, FieldSpec, Record, RecordId, RecordSchema, SourceSnapshotId,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::collaboration_archive::{self as archive, CommitOid, ObjectDigest};
use crate::collaboration_gc::archive_use;
use crate::collaboration_state::CollaborationStore;
use crate::composer::{self, CompositionBudget, CompositionRequest};
use crate::local_target::{self as target, ControllerOwner, Phase, TargetError};

const SCHEMA_NAME: &str = "aethyme.local-wave/provisional-v0";
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

/// Fixed at admission. Aging gives old or large groups a dedicated wave;
/// readiness order never changes when a notification is repeated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WavePolicy {
    pub batch_width: usize,
    pub max_group: usize,
    pub debounce_ms: i64,
    pub age_promotion_ms: i64,
    pub drift_rechecks: u32,
}

impl Default for WavePolicy {
    fn default() -> Self {
        Self {
            batch_width: 3,
            max_group: 1024,
            debounce_ms: 200,
            age_promotion_ms: 5000,
            drift_rechecks: 2,
        }
    }
}

impl WavePolicy {
    fn validate(self) -> Result<(), WaveError> {
        if self.batch_width == 0
            || self.batch_width > 1024
            || self.max_group < self.batch_width
            || self.max_group > 1024
            || self.debounce_ms < 0
            || self.debounce_ms > 60_000
            || self.age_promotion_ms < self.debounce_ms
            || self.age_promotion_ms > 3_600_000
            || self.drift_rechecks > 10
        {
            return Err(WaveError::Invalid("wave policy bounds".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaveSpec {
    pub target: String,
    pub issuer: String,
    pub revision: String,
    pub objective_authorization: String,
    pub sequence: i64,
    pub baseline: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_completion: Option<String>,
    pub selected: Vec<String>,
    pub policy: WavePolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WavePhase {
    Frozen,
    Composing,
    Verifying,
    Accepted,
    Blocked,
    Unsupported,
    NoChange,
    OutcomeUnknown,
    Stopped,
    TargetDrift,
}

impl WavePhase {
    fn parse(value: &str) -> Result<Self, WaveError> {
        serde_json::from_value(serde_json::Value::String(value.into()))
            .map_err(|_| WaveError::Invalid("stored wave phase".into()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wave {
    pub id: RecordId,
    pub spec: WaveSpec,
    pub phase: WavePhase,
    pub candidate_snapshot: Option<SourceSnapshotId>,
    pub candidate_commit: Option<CommitOid>,
    pub recipe_object: Option<ObjectDigest>,
    pub detail: Option<String>,
    pub composition_ms: i64,
    pub drift_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    WaitingForInput { missing: Vec<String> },
    Debouncing { until_ms: i64 },
    Existing(Wave),
    Frozen(Wave),
    Blocked { detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Run {
    Waiting(Admission),
    RequiresVerification(Wave),
    RequiresReconciliation(Wave),
    Blocked(Wave),
}

/// Reference driver for an authorized event or timed tick. A notification is
/// optional: the action is derived from durable state. This invokes only the
/// provisional composer, never a verifier, resolver, model, or provider.
pub fn tick(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    repo: &Path,
    policy: WavePolicy,
    now_ms: i64,
) -> Result<Run, WaveError> {
    let wave = match admit(store, owner, policy, now_ms)? {
        Admission::Frozen(wave) | Admission::Existing(wave) => wave,
        waiting => return Ok(Run::Waiting(waiting)),
    };
    let wave = if wave.phase == WavePhase::Frozen {
        compose(store, owner, repo, &wave.id)?
    } else {
        wave
    };
    Ok(match wave.phase {
        WavePhase::Verifying => Run::RequiresVerification(wave),
        WavePhase::Composing | WavePhase::OutcomeUnknown => Run::RequiresReconciliation(wave),
        _ => Run::Blocked(wave),
    })
}

#[derive(Debug, thiserror::Error)]
pub enum WaveError {
    #[error("invalid wave: {0}")]
    Invalid(String),
    #[error("wave or target is no longer current")]
    Stale,
    #[error("an earlier dispatch has an unknown outcome; reconcile it before retrying")]
    OutcomeUnknown,
    #[error("capture notice admission failed: {0}")]
    CaptureNotice(String),
    #[error(transparent)]
    Target(#[from] TargetError),
    #[error(transparent)]
    Sensitive(#[from] crate::collaboration_sensitive::SensitiveError),
    #[error(transparent)]
    Archive(#[from] archive::ArchiveError),
    #[error(transparent)]
    Compose(#[from] composer::ComposeError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

fn id(value: &str) -> Result<RecordId, WaveError> {
    RecordId::parse(value).map_err(|e| WaveError::Invalid(e.to_string()))
}
fn snapshot(value: &str) -> Result<SourceSnapshotId, WaveError> {
    SourceSnapshotId::parse(value).map_err(|e| WaveError::Invalid(e.to_string()))
}
fn object(value: &str) -> Result<ObjectDigest, WaveError> {
    Ok(ObjectDigest::of_snapshot(&snapshot(&format!(
        "sha256:{value}"
    ))?))
}

/// Idempotent availability for an exact selected input. This is not execution
/// authorization and cannot add a new input to the target. Production arrivals
/// use the retained capture-notice ingress and owner-tick path below.
#[cfg(test)]
pub(crate) fn available(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    contribution: &str,
    now_ms: i64,
) -> Result<(), WaveError> {
    let _response = crate::collaboration_sensitive::response_use(store)?;
    let authority = target::authorize(store, owner)?;
    available_under_owner_tick(store, owner, contribution, now_ms, &authority)
}

/// Record an arrival while an enclosing owner tick already holds the response
/// and authority guards. This avoids reacquiring authority below ingress.
pub(crate) fn available_under_owner_tick(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    contribution: &str,
    now_ms: i64,
    _authority: &crate::local_authority::Guard,
) -> Result<(), WaveError> {
    if now_ms < 0 {
        return Err(WaveError::Invalid("negative clock".into()));
    }
    let revision = target::status(store, owner.target())?
        .revision
        .ok_or(TargetError::Missing)?;
    let pinned = target::load_revision(store, &revision)?;
    if !pinned.spec.selected.iter().any(|c| c == contribution) {
        return Err(WaveError::Invalid(
            "availability is outside the authorized selection".into(),
        ));
    }
    store.connection().execute("INSERT OR IGNORE INTO local_wave_arrivals (revision, contribution, first_seen_ms) VALUES (?1, ?2, ?3)", params![revision.as_str(), contribution, now_ms])?;
    Ok(())
}

pub fn load(store: &CollaborationStore, wave: &RecordId) -> Result<Wave, WaveError> {
    let _response = crate::collaboration_sensitive::response_use(store)?;
    type Row = (
        Vec<u8>,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
        u32,
        String,
        String,
        i64,
    );
    let row: Row = store.read_connection().query_row(
        "SELECT body, phase, candidate_snapshot, candidate_commit, recipe_object, detail, composition_ms, drift_count, target, revision, sequence FROM local_waves WHERE wave = ?1", [wave.as_str()],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?,r.get(10)?)),
    ).optional()?.ok_or(TargetError::Missing)?;
    let record =
        Record::decode(&row.0, &[&SCHEMA]).map_err(|e| WaveError::Invalid(e.to_string()))?;
    if record.id() != *wave || record.canonical_bytes() != row.0 {
        return Err(WaveError::Invalid("wave digest".into()));
    }
    let body: serde_json::Value = serde_json::from_slice(&row.0)?;
    let spec: WaveSpec = serde_json::from_value(body["spec"].clone())?;
    spec.policy.validate()?;
    let pinned = target::load_revision(store, &id(&spec.revision)?)?;
    let unique: std::collections::BTreeSet<_> = spec.selected.iter().collect();
    if spec.target != row.8
        || spec.revision != row.9
        || spec.sequence != row.10
        || spec.sequence <= 0
        || spec.target != pinned.target
        || spec.issuer != pinned.issuer
        || (spec.prior_completion.is_none() && spec.baseline != pinned.spec.baseline)
        || spec.objective_authorization != pinned.spec.objective_authorization
        || spec.selected.is_empty()
        || unique.len() != spec.selected.len()
        || spec.selected.len() > spec.policy.max_group
        || spec
            .selected
            .iter()
            .any(|c| !pinned.spec.selected.contains(c))
        || pinned.spec.composer_profile != composer::PROVISIONAL_TEXT_PROFILE
    {
        return Err(WaveError::Invalid("wave authorization or scope".into()));
    }
    if let Some(key) = &spec.prior_completion {
        let (completion, previous) = accepted_completion(store, &id(key)?, spec.sequence)?;
        let same_revision = completion.revision == spec.revision;
        let baseline_matches = if same_revision {
            completion.source == spec.baseline
        } else {
            completion.source == pinned.spec.baseline && spec.baseline == pinned.spec.baseline
        };
        if completion.target != spec.target
            || !baseline_matches
            || (same_revision
                && previous
                    .selected
                    .iter()
                    .any(|member| !spec.selected.contains(member)))
        {
            return Err(WaveError::Invalid(
                "prior acceptance baseline or cumulative selection".into(),
            ));
        }
    }
    Ok(Wave {
        id: wave.clone(),
        spec,
        phase: WavePhase::parse(&row.1)?,
        candidate_snapshot: row.2.as_deref().map(snapshot).transpose()?,
        candidate_commit: row.3.as_deref().map(CommitOid::parse).transpose()?,
        recipe_object: row.4.as_deref().map(object).transpose()?,
        detail: row.5,
        composition_ms: row.6,
        drift_count: row.7,
    })
}

struct AcceptedCompletionRow {
    body: Vec<u8>,
    phase: String,
    detail: Option<String>,
    sequence: i64,
    target: String,
    revision: String,
    candidate: Option<String>,
    commit: Option<String>,
}

/// Load a canonical private completion and its accepted wave. Cross-revision
/// callers must additionally bind the completion's source to the new
/// revision's explicitly selected baseline before using its commit as history.
pub(crate) fn accepted_completion(
    store: &CollaborationStore,
    key: &RecordId,
    sequence: i64,
) -> Result<(crate::local_controller::CompletionSpec, WaveSpec), WaveError> {
    let completion =
        crate::local_controller::load(store, key).map_err(|e| WaveError::Invalid(e.to_string()))?;
    let row: AcceptedCompletionRow = store.read_connection().query_row(
        "SELECT body,phase,detail,sequence,target,revision,candidate_snapshot,candidate_commit FROM local_waves WHERE wave=?1",
        [completion.wave.as_str()],
        |r| {
            Ok(AcceptedCompletionRow {
                body: r.get(0)?,
                phase: r.get(1)?,
                detail: r.get(2)?,
                sequence: r.get(3)?,
                target: r.get(4)?,
                revision: r.get(5)?,
                candidate: r.get(6)?,
                commit: r.get(7)?,
            })
        },
    )?;
    let record =
        Record::decode(&row.body, &[&SCHEMA]).map_err(|e| WaveError::Invalid(e.to_string()))?;
    let spec: WaveSpec = serde_json::from_value(
        serde_json::from_slice::<serde_json::Value>(&row.body)?["spec"].clone(),
    )?;
    if record.id().as_str() != completion.wave
        || record.canonical_bytes() != row.body
        || row.phase != "accepted"
        || row.detail.as_deref() != Some(key.as_str())
        || row.sequence != spec.sequence
        || row.sequence <= 0
        || row.sequence >= sequence
        || row.target != completion.target
        || row.revision != completion.revision
        || spec.target != completion.target
        || spec.revision != completion.revision
        || row.candidate.as_deref() != Some(completion.source.as_str())
        || row.commit.as_deref() != Some(completion.candidate_commit.as_str())
        || completion.accepted_tree != completion.candidate_tree
    {
        return Err(WaveError::Invalid("prior proof-bound accepted wave".into()));
    }
    Ok((completion, spec))
}

/// Find the newest canonical accepted completion for this target and exact
/// epoch. The sequence and record ID ordering make repeated source snapshots
/// resolve deterministically; the source comparison is still exact.
pub(crate) fn latest_completion_for(
    store: &CollaborationStore,
    target: &str,
    epoch: &str,
    revision: Option<&str>,
    source: Option<&str>,
) -> Result<Option<RecordId>, WaveError> {
    if revision.is_some() == source.is_some() {
        return Err(WaveError::Invalid(
            "completion lookup requires exactly one revision or source binding".into(),
        ));
    }
    let ids: Vec<String> = store
        .read_connection()
        .prepare(
            "SELECT c.id FROM local_controller_completions c JOIN local_waves w ON w.wave=c.wave WHERE c.target=?1 ORDER BY w.sequence DESC,c.id DESC",
        )?
        .query_map([target], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    for raw_id in ids {
        let key = id(&raw_id)?;
        let (completion, _) = accepted_completion(store, &key, i64::MAX)?;
        if completion.target != target || completion.epoch != epoch {
            continue;
        }
        let matches = match (revision, source) {
            (Some(revision), None) => completion.revision == revision,
            (None, Some(source)) => completion.source == source,
            _ => unreachable!("lookup mode validated above"),
        };
        if matches {
            return Ok(Some(key));
        }
    }
    Ok(None)
}

fn admission_phase(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    observed: &target::ControllerStatus,
    waiting: bool,
) -> Result<(), WaveError> {
    let phase = if waiting {
        Phase::WaitingForInput
    } else {
        Phase::Admitting
    };
    if observed.phase == phase {
        return Ok(());
    }
    let revision = observed.revision.as_ref().ok_or(TargetError::Missing)?;
    let changed = store.connection().execute(
        "UPDATE local_targets SET phase=?4, resume_phase=?5, version=version+1 WHERE target=?1 AND active_revision=?2 AND version=?3",
        params![owner.target(),revision.as_str(),observed.version,if waiting { "waiting_for_input" } else { "admitting" },if waiting { Some("admitting") } else { None }],
    )?;
    if changed != 1 {
        return Err(WaveError::Stale);
    }
    Ok(())
}

fn block_admission(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    observed: &target::ControllerStatus,
    members: &[String],
    unsupported: bool,
    detail: &str,
) -> Result<Admission, WaveError> {
    let revision = observed.revision.as_ref().ok_or(TargetError::Missing)?;
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    if tx.execute("UPDATE local_targets SET phase='blocked_requirement', resume_phase=NULL, version=version+1 WHERE target=?1 AND active_revision=?2 AND version=?3", params![owner.target(),revision.as_str(),observed.version])? != 1 {
        return Err(WaveError::Stale);
    }
    for member in members {
        tx.execute("UPDATE local_target_dispositions SET status=?3, detail=?4 WHERE revision=?1 AND contribution=?2 AND status='pending'", params![revision.as_str(),member,if unsupported { "unsupported" } else { "blocked" },detail])?;
    }
    tx.commit()?;
    Ok(Admission::Blocked {
        detail: detail.into(),
    })
}

/// Freeze oldest-ready dependency/atomic closures. New arrivals are excluded
/// from an existing wave. A timed call recovers without a notification.
pub fn admit(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    policy: WavePolicy,
    now_ms: i64,
) -> Result<Admission, WaveError> {
    crate::local_capture_notice::admit_for_owner(store, owner, policy, now_ms).map_err(|error| {
        match error {
            crate::local_capture_notice::NoticeError::Wave(wave) => wave,
            other => WaveError::CaptureNotice(other.to_string()),
        }
    })
}

/// Admission primitive for the owner tick after capture notices have been
/// drained while the ingress lock remains held.
pub(crate) fn admit_after_notices(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    policy: WavePolicy,
    now_ms: i64,
    _authority: &crate::local_authority::Guard,
) -> Result<Admission, WaveError> {
    policy.validate()?;
    if now_ms < 0 {
        return Err(WaveError::Invalid("negative clock".into()));
    }
    let observed = target::status(store, owner.target())?;
    let revision = observed.revision.as_ref().ok_or(TargetError::Missing)?;
    let existing: Option<String> = store.read_connection().query_row(
        "SELECT wave FROM local_waves WHERE target = ?1 AND phase IN ('frozen','composing','verifying','outcome_unknown')", [owner.target()], |r| r.get(0)).optional()?;
    if let Some(existing) = existing {
        let wave = load(store, &id(&existing)?)?;
        if wave.spec.revision != revision.as_str() {
            return Err(WaveError::Stale);
        }
        return Ok(Admission::Existing(wave));
    }
    if !matches!(
        observed.phase,
        Phase::Idle | Phase::Admitting | Phase::WaitingForInput | Phase::PublicationConfirmed
    ) {
        return Err(TargetError::Frozen.into());
    }
    let pinned = target::load_revision(store, revision)?;
    if pinned.spec.composer_profile != composer::PROVISIONAL_TEXT_PROFILE {
        return block_admission(
            store,
            owner,
            &observed,
            &pinned.spec.selected,
            true,
            "unsupported composer profile",
        );
    }
    // `admit_for_owner` holds the archive shared guard through this call.
    // Reopening the same flock below the ingress lock can wait behind a
    // queued reclamation writer while this tick already holds a shared guard.
    let baseline =
        archive::retained(store, &snapshot(&pinned.spec.baseline)?)?.ok_or(TargetError::Missing)?;
    let groups = composer::validate_retained_selection(
        store,
        baseline,
        pinned.spec.catalog()?,
        pinned.spec.selected.clone(),
    )?
    .map_err(|(reason, detail)| TargetError::SelectionRefused { reason, detail })?;
    let arrivals: BTreeMap<String, (i64,i64)> = store.read_connection().prepare(
        "SELECT contribution, sequence, first_seen_ms FROM local_wave_arrivals WHERE revision = ?1")?
        .query_map([revision.as_str()], |r| Ok((r.get(0)?, (r.get(1)?,r.get(2)?))))?.collect::<Result<_,_>>()?;
    let dispositions: BTreeMap<_, _> = target::dispositions(store, revision)?
        .into_iter()
        .map(|d| (d.contribution, d.status))
        .collect();
    let mut ready = Vec::new();
    let mut missing = Vec::new();
    for group in groups {
        if group.iter().any(|c| {
            !matches!(
                dispositions.get(c),
                Some(target::Disposition::Pending | target::Disposition::Delivered)
            )
        }) {
            continue;
        }
        let group: Vec<_> = group
            .into_iter()
            .filter(|c| dispositions.get(c) == Some(&target::Disposition::Pending))
            .collect();
        if group.is_empty() {
            continue;
        }
        if group.iter().all(|c| arrivals.contains_key(c)) {
            let sequence = group
                .iter()
                .map(|c| arrivals[c].0)
                .max()
                .expect("nonempty group");
            let ready_at = group
                .iter()
                .map(|c| arrivals[c].1)
                .max()
                .expect("nonempty group");
            ready.push((sequence, ready_at, group));
        } else {
            missing.extend(group.into_iter().filter(|c| !arrivals.contains_key(c)));
        }
    }
    ready.sort_by_key(|(sequence, _, _)| *sequence);
    let Some((_, ready_at, oldest)) = ready.first() else {
        return if missing.is_empty() && observed.phase == Phase::PublicationConfirmed {
            Ok(Admission::WaitingForInput { missing })
        } else if missing.is_empty() {
            block_admission(
                store,
                owner,
                &observed,
                &[],
                false,
                "no pending supported contribution group",
            )
        } else {
            admission_phase(store, owner, &observed, true)?;
            Ok(Admission::WaitingForInput { missing })
        };
    };
    let until_ms = ready_at.saturating_add(policy.debounce_ms);
    if now_ms < until_ms {
        admission_phase(store, owner, &observed, false)?;
        return Ok(Admission::Debouncing { until_ms });
    }
    if oldest.len() > policy.max_group {
        return block_admission(
            store,
            owner,
            &observed,
            oldest,
            false,
            "oldest ready group exceeds the authorized group allowance",
        );
    }
    let dedicated = oldest.len() > policy.batch_width
        || now_ms.saturating_sub(*ready_at) >= policy.age_promotion_ms;
    let mut selected = oldest.clone();
    if !dedicated {
        for (_, _, group) in ready.iter().skip(1) {
            if selected.len() + group.len() <= policy.batch_width {
                selected.extend(group.iter().cloned());
            }
        }
    }
    let same_revision_completion = latest_completion_for(
        store,
        owner.target(),
        owner.authority_epoch().as_str(),
        Some(revision.as_str()),
        None,
    )?;
    let prior_completion = match same_revision_completion {
        Some(key) => Some(key),
        None => latest_completion_for(
            store,
            owner.target(),
            owner.authority_epoch().as_str(),
            None,
            Some(&pinned.spec.baseline),
        )?,
    };
    if let Some(key) = &prior_completion {
        let (completion, previous) = accepted_completion(store, key, i64::MAX)?;
        let same_revision = completion.revision == revision.as_str();
        if completion.target != owner.target()
            || completion.epoch != owner.authority_epoch().as_str()
            || (!same_revision && completion.source != pinned.spec.baseline)
        {
            return Err(WaveError::Stale);
        }
        let delivered: Vec<_> = dispositions
            .iter()
            .filter(|(_, d)| **d == target::Disposition::Delivered)
            .map(|(c, _)| c.clone())
            .collect();
        if same_revision {
            if delivered.iter().any(|c| !previous.selected.contains(c))
                || previous.selected.iter().any(|c| !delivered.contains(c))
            {
                return Err(WaveError::Invalid(
                    "delivered members do not match prior accepted wave".into(),
                ));
            }
            selected.extend(delivered);
        } else if !delivered.is_empty() {
            return Err(WaveError::Invalid(
                "new revision cannot inherit delivered contribution identities".into(),
            ));
        }
    } else if dispositions
        .values()
        .any(|d| *d == target::Disposition::Delivered)
    {
        return Err(WaveError::Invalid(
            "delivered members have no private completion".into(),
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    selected.retain(|member| seen.insert(member.clone()));
    if selected.len() > policy.max_group {
        return Err(WaveError::Invalid(
            "cumulative selection exceeds finite allowance".into(),
        ));
    }
    let prior_completion = prior_completion.map(|key| key.to_string());
    let next_baseline = prior_completion
        .as_ref()
        .map(|key| {
            crate::local_controller::load(store, &id(key)?)
                .map(|c| c.source)
                .map_err(|e| WaveError::Invalid(e.to_string()))
        })
        .transpose()?
        .unwrap_or_else(|| pinned.spec.baseline.clone());
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let sequence: i64 = tx.query_row(
        "SELECT coalesce(max(sequence), 0) + 1 FROM local_waves WHERE target = ?1",
        [owner.target()],
        |r| r.get(0),
    )?;
    let spec = WaveSpec {
        target: owner.target().into(),
        issuer: owner.issuer().into(),
        revision: revision.to_string(),
        objective_authorization: pinned.spec.objective_authorization,
        sequence,
        baseline: next_baseline,
        prior_completion,
        selected,
        policy,
    };
    let bytes = serde_json::to_vec(&serde_json::json!({ "schema": SCHEMA_NAME, "spec": spec }))?;
    let record =
        Record::decode(&bytes, &[&SCHEMA]).map_err(|e| WaveError::Invalid(e.to_string()))?;
    let wave = record.id();
    let changed = tx.execute("UPDATE local_targets SET phase = 'wave_frozen', resume_phase = NULL, version = version + 1 WHERE target = ?1 AND active_revision = ?2 AND version = ?3", params![owner.target(), revision.as_str(), observed.version])?;
    if changed != 1 {
        return Err(WaveError::Stale);
    }
    tx.execute("INSERT INTO local_waves (wave,target,revision,sequence,body,phase) VALUES (?1,?2,?3,?4,?5,'frozen')", params![wave.as_str(),owner.target(),revision.as_str(),sequence,record.canonical_bytes()])?;
    for contribution in &spec.selected {
        tx.execute("UPDATE local_target_dispositions SET status = 'in_wave' WHERE revision = ?1 AND contribution = ?2 AND status = 'pending'", params![revision.as_str(),contribution])?;
    }
    tx.commit()?;
    Ok(Admission::Frozen(load(store, &wave)?))
}

fn current(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    wave: &Wave,
) -> Result<target::ControllerStatus, WaveError> {
    target::authorize(store, owner)?;
    let state = target::status(store, owner.target())?;
    if wave.spec.target != owner.target()
        || wave.spec.issuer != owner.issuer()
        || state.revision.as_ref().map(RecordId::as_str) != Some(wave.spec.revision.as_str())
        || !(matches!(
            state.phase,
            Phase::WaveFrozen | Phase::Composing | Phase::Verifying
        ) || (state.phase == Phase::WaitingForInput
            && state.resume_phase == Some(Phase::Verifying))
            || (wave.phase == WavePhase::OutcomeUnknown
                && matches!(
                    state.phase,
                    Phase::OutcomeUnknown | Phase::BlockedTargetDrift
                )))
    {
        return Err(WaveError::Stale);
    }
    if !active_identity(store.read_connection(), owner, wave)? {
        return Err(WaveError::Stale);
    }
    Ok(state)
}

fn active_identity(
    conn: &rusqlite::Connection,
    owner: &ControllerOwner,
    wave: &Wave,
) -> Result<bool, WaveError> {
    Ok(conn.query_row("SELECT EXISTS(SELECT 1 FROM local_waves WHERE wave=?1 AND target=?2 AND revision=?3 AND phase IN ('frozen','composing','verifying','outcome_unknown'))", params![wave.id.as_str(),owner.target(),wave.spec.revision], |r| r.get(0))?)
}

// Recheck the exact wave and observed controller generation under the write
// transaction. A stop based on a pre-dispatch read must not terminalize work
// that became composing while the caller was waiting for the database lock.
fn guard_current(
    conn: &rusqlite::Connection,
    owner: &ControllerOwner,
    wave: &Wave,
    version: i64,
) -> Result<(), WaveError> {
    let phase = serde_json::to_value(wave.phase)?;
    let valid: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM local_targets t JOIN local_waves w ON w.target=t.target WHERE t.target=?1 AND t.issuer=?2 AND t.active_revision=?3 AND t.version=?4 AND w.wave=?5 AND w.phase=?6 AND w.phase IN ('frozen','composing','verifying','outcome_unknown'))", params![owner.target(),owner.issuer(),wave.spec.revision,version,wave.id.as_str(),phase.as_str()], |r| r.get(0))?;
    if !valid {
        return Err(WaveError::Stale);
    }
    Ok(())
}

/// One durable dispatch. Replaying a completed composition returns its stored
/// candidate; a crash during dispatch is explicit, never silently retried.
pub fn compose(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    repo: &Path,
    wave_id: &RecordId,
) -> Result<Wave, WaveError> {
    let (wave, request, already_delivered, revision_bindings, namespace_conflict) = {
        let _archive_guard = archive_use(store)?;
        let response_guard = crate::collaboration_sensitive::response_use(store)?;
        let _authority_guard = target::authorize(store, owner)?;
        let wave = load(store, wave_id)?;
        let state = current(store, owner, &wave)?;
        if wave.phase == WavePhase::Verifying {
            return Ok(wave);
        }
        if wave.phase != WavePhase::Frozen {
            return Err(WaveError::OutcomeUnknown);
        }
        let pinned = target::load_revision_under_response(
            store,
            &id(&wave.spec.revision)?,
            &response_guard,
        )?;
        // Validate the complete issuer-selected cohort before composing this
        // scheduling batch. A narrow wave must not hide conflicts with a
        // selected revision that will be admitted in a later wave.
        let namespace_conflict =
            match target::validate_d10_file_namespace(store, &pinned, &pinned.spec.selected) {
                Ok(()) => None,
                Err(TargetError::FileNamespaceConflict(detail)) => Some(detail),
                Err(error) => return Err(error.into()),
            };
        let revision_bindings = pinned
            .revision_bindings
            .as_ref()
            .map(target::D10RevisionBindings::revision_ids)
            .transpose()?;
        let baseline = archive::retained(store, &snapshot(&wave.spec.baseline)?)?
            .ok_or(TargetError::Missing)?;
        let (base_commit, accepted, already_delivered) = if let Some(key) =
            &wave.spec.prior_completion
        {
            let (completion, prior) = accepted_completion(store, &id(key)?, wave.spec.sequence)?;
            if completion.target != owner.target()
                || completion.epoch != owner.authority_epoch().as_str()
            {
                return Err(WaveError::Stale);
            }
            let original = archive::retained(store, &snapshot(&pinned.spec.baseline)?)?
                .ok_or(TargetError::Missing)?;
            let members = if completion.revision == wave.spec.revision {
                prior.selected
            } else {
                Vec::new()
            };
            let accepted = if completion.revision == wave.spec.revision {
                vec![original.commit]
            } else {
                Vec::new()
            };
            (
                CommitOid::parse(&completion.accepted_commit)?,
                accepted,
                Some((id(key)?, members)),
            )
        } else {
            (baseline.commit, vec![], None)
        };
        let tx = store
            .connection()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        guard_current(&tx, owner, &wave, state.version)?;
        let changed = tx.execute(
            "UPDATE local_waves SET phase = 'composing' WHERE wave = ?1 AND phase = 'frozen'",
            [wave_id.as_str()],
        )?;
        if changed != 1 {
            return Err(WaveError::Stale);
        }
        if tx.execute("UPDATE local_targets SET phase = 'composing', version = version + 1 WHERE target = ?1 AND active_revision = ?2 AND phase = 'wave_frozen'", params![owner.target(),wave.spec.revision])? != 1 {
            return Err(WaveError::Stale);
        }
        tx.commit()?;
        let request = CompositionRequest {
            baseline: base_commit,
            accepted,
            catalog: pinned.spec.catalog()?,
            deliveries: wave.spec.selected.clone(),
            budget: CompositionBudget {
                max_contributions: wave.spec.policy.max_group,
                ..CompositionBudget::default()
            },
        };
        (
            wave,
            request,
            already_delivered,
            revision_bindings,
            namespace_conflict,
        )
    };
    let started = std::time::Instant::now();
    let revision_id = id(&wave.spec.revision)?;
    let mut retain_candidate =
        |store: &mut CollaborationStore,
         candidate: &crate::composition::Candidate,
         recipe: &composer::RecipeRef,
         archive_guard: &crate::collaboration_gc::ArchiveUse| {
            #[cfg(test)]
            if crate::collaboration_gc::exclusive_available_for_test(store)
                .map_err(|error| composer::ComposeError::CandidateRetention(error.to_string()))?
            {
                return Err(composer::ComposeError::CandidateRetention(
                    "archive became exclusively available before candidate retention".into(),
                ));
            }
            let response_guard = crate::collaboration_sensitive::response_use(store)
                .map_err(|error| composer::ComposeError::CandidateRetention(error.to_string()))?;
            let authority_guard = target::authorize(store, owner)
                .map_err(|error| composer::ComposeError::CandidateRetention(error.to_string()))?;
            let retention = target::ArtifactRetentionPermit::new(
                store,
                owner,
                archive_guard,
                &response_guard,
                &authority_guard,
            )
            .map_err(|error| composer::ComposeError::CandidateRetention(error.to_string()))?;
            target::retain_artifacts_under_guards(
                store,
                &revision_id,
                std::slice::from_ref(&candidate.subject),
                std::slice::from_ref(&recipe.object),
                &retention,
            )
            .map_err(|error| composer::ComposeError::CandidateRetention(error.to_string()))
        };
    let result = if let Some(detail) = namespace_conflict {
        Ok(composer::Composition {
            outcome: crate::composition::CompositionOutcome::Refused {
                reason: crate::composition::Refusal::FileNamespaceConflict,
                detail,
            },
            recipe: None,
            order: Vec::new(),
            usage: composer::BudgetUsage::default(),
        })
    } else {
        match (already_delivered, revision_bindings.as_ref()) {
            (Some((completion, members)), Some(revisions)) => {
                composer::compose_after_accepted_with_candidate_retainer_and_revisions(
                    store,
                    repo,
                    &request,
                    completion,
                    members,
                    revisions,
                    &mut retain_candidate,
                )
            }
            (Some((completion, members)), None) => {
                composer::compose_after_accepted_with_candidate_retainer(
                    store,
                    repo,
                    &request,
                    completion,
                    members,
                    &mut retain_candidate,
                )
            }
            (None, Some(revisions)) => composer::compose_with_candidate_retainer_and_revisions(
                store,
                repo,
                &request,
                revisions,
                &mut retain_candidate,
            ),
            (None, None) => composer::compose_with_candidate_retainer(
                store,
                repo,
                &request,
                &mut retain_candidate,
            ),
        }
    };
    let elapsed = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
    // State may have been stopped while the external Git tool ran. Retain the
    // bytes as historical work, but reject changing the current generation.
    let (phase, detail, candidate, commit, recipe) = match result {
        Ok(composition) => match composition.outcome {
            crate::composition::CompositionOutcome::Candidate(candidate) => {
                let recipe = composition
                    .recipe
                    .ok_or_else(|| WaveError::Invalid("candidate has no recipe".into()))?;
                (
                    "verifying",
                    "trusted verification adapter unavailable".into(),
                    Some(candidate.subject.to_string()),
                    Some(candidate.commit.as_str().to_string()),
                    Some(recipe.object.hex()),
                )
            }
            outcome => {
                let phase = if matches!(
                    outcome,
                    crate::composition::CompositionOutcome::Unsupported { .. }
                ) {
                    "unsupported"
                } else if matches!(
                    outcome,
                    crate::composition::CompositionOutcome::NoChange { .. }
                ) {
                    "no_change"
                } else {
                    "blocked"
                };
                (phase, outcome.code().to_string(), None, None, None)
            }
        },
        Err(error) => ("outcome_unknown", error.to_string(), None, None, None),
    };
    let _archive_guard = archive_use(store)?;
    let _response_guard = crate::collaboration_sensitive::response_use(store)?;
    let _authority_guard = target::authorize(store, owner)?;
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let state: (String, Option<String>) = tx.query_row(
        "SELECT phase,active_revision FROM local_targets WHERE target=?1",
        [owner.target()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if state.0 != "composing" || state.1.as_deref() != Some(wave.spec.revision.as_str()) {
        return Err(WaveError::Stale);
    }
    if tx.execute("UPDATE local_waves SET phase=?2, detail=?3, candidate_snapshot=?4, candidate_commit=?5, recipe_object=?6, composition_ms=?7 WHERE wave=?1 AND phase='composing'", params![wave_id.as_str(),phase,detail,candidate,commit,recipe,elapsed])? != 1 {
        return Err(WaveError::Stale);
    }
    let next = if phase == "verifying" {
        "waiting_for_input"
    } else if phase == "outcome_unknown" {
        "outcome_unknown"
    } else {
        "idle"
    };
    tx.execute("UPDATE local_targets SET phase=?3, resume_phase=?4, version=version+1 WHERE target=?1 AND active_revision=?2 AND phase='composing'", params![owner.target(),wave.spec.revision,next,if phase == "verifying" { Some("verifying") } else { None }])?;
    if !matches!(phase, "verifying" | "outcome_unknown") {
        for contribution in &wave.spec.selected {
            tx.execute("UPDATE local_target_dispositions SET status=?3, detail=?4 WHERE revision=?1 AND contribution=?2 AND status='in_wave'", params![wave.spec.revision,contribution,if phase == "unsupported" { "unsupported" } else { "blocked" },detail])?;
        }
    }
    tx.commit()?;
    load(store, wave_id)
}

/// Observe external movement without changing the frozen baseline or bypassing
/// provider policy. Duplicate observations consume no extra recheck allowance.
/// Repeated distinct movement reaches an explicit blocked target.
pub fn observe_target(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    wave_id: &RecordId,
    observed: &CommitOid,
) -> Result<Wave, WaveError> {
    let _authority = target::authorize(store, owner)?;
    let wave = load(store, wave_id)?;
    let state = current(store, owner, &wave)?;
    let baseline =
        archive::retained(store, &snapshot(&wave.spec.baseline)?)?.ok_or(TargetError::Missing)?;
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    guard_current(&tx, owner, &wave, state.version)?;
    tx.execute("UPDATE local_waves SET drift_count = drift_count + ?3, last_observed_commit = ?2 WHERE wave = ?1 AND (last_observed_commit IS NULL OR last_observed_commit != ?2)", params![wave_id.as_str(), observed.as_str(), u32::from(baseline.commit != *observed)])?;
    let count: u32 = tx.query_row(
        "SELECT drift_count FROM local_waves WHERE wave = ?1",
        [wave_id.as_str()],
        |r| r.get(0),
    )?;
    if count > wave.spec.policy.drift_rechecks {
        let live = matches!(wave.phase, WavePhase::Composing | WavePhase::OutcomeUnknown);
        tx.execute("UPDATE local_waves SET phase=?2, detail='external target movement exceeded the frozen recheck allowance' WHERE wave=?1", params![wave_id.as_str(),if live { "outcome_unknown" } else { "target_drift" }])?;
        tx.execute("UPDATE local_targets SET phase='blocked_target_drift', resume_phase=NULL, version=version+1 WHERE target=?1 AND active_revision=?2", params![owner.target(),wave.spec.revision])?;
        for contribution in &wave.spec.selected {
            tx.execute("UPDATE local_target_dispositions SET status='blocked', detail='external target drift' WHERE revision=?1 AND contribution=?2", params![wave.spec.revision,contribution])?;
        }
    }
    tx.commit()?;
    load(store, wave_id)
}

/// Recover an interrupted compose dispatch conservatively. Git object writes
/// may have happened before the process died. This grants no fresh retry.
pub fn recover_dispatch(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    wave_id: &RecordId,
) -> Result<Wave, WaveError> {
    let _authority = target::authorize(store, owner)?;
    let wave = load(store, wave_id)?;
    target::authorize(store, owner)?;
    if wave.spec.target != owner.target() || wave.spec.issuer != owner.issuer() {
        return Err(WaveError::Stale);
    }
    if wave.phase != WavePhase::Composing {
        return Ok(wave);
    }
    let state = target::status(store, owner.target())?;
    let next = match state.phase {
        Phase::StopRequested | Phase::Stopped => "stop_requested",
        Phase::BlockedTargetDrift => "blocked_target_drift",
        _ => "outcome_unknown",
    };
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    guard_current(&tx, owner, &wave, state.version)?;
    tx.execute("UPDATE local_waves SET phase='outcome_unknown', detail='compose dispatch interrupted; reconcile retained output before retry' WHERE wave=?1 AND phase='composing'", [wave_id.as_str()])?;
    tx.execute("UPDATE local_targets SET phase=?3, resume_phase=NULL, version=version+1 WHERE target=?1 AND active_revision=?2", params![owner.target(),wave.spec.revision,next])?;
    tx.commit()?;
    load(store, wave_id)
}

/// Stop scheduling. A compose dispatch still in flight stays unknown and
/// active, retaining its sources. Stopped means no composer dispatch was live;
/// it makes no claim about an external verifier that this package never starts.
pub fn stop(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    wave_id: &RecordId,
) -> Result<Wave, WaveError> {
    let _authority = target::authorize(store, owner)?;
    let wave = load(store, wave_id)?;
    target::authorize(store, owner)?;
    let state = target::status(store, owner.target())?;
    if wave.spec.target != owner.target()
        || wave.spec.issuer != owner.issuer()
        || state.revision.as_ref().map(RecordId::as_str) != Some(wave.spec.revision.as_str())
    {
        return Err(WaveError::Stale);
    }
    commit_stop(store, owner, &wave, &state)
}

fn commit_stop(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    wave: &Wave,
    state: &target::ControllerStatus,
) -> Result<Wave, WaveError> {
    let live = matches!(wave.phase, WavePhase::Composing | WavePhase::OutcomeUnknown);
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    guard_current(&tx, owner, wave, state.version)?;
    tx.execute(
        "UPDATE local_waves SET phase=?2, detail=?3 WHERE wave=?1",
        params![
            wave.id.as_str(),
            if live { "outcome_unknown" } else { "stopped" },
            if live {
                "stop requested; compose liveness or output unknown"
            } else {
                "scheduling stopped before further dispatch"
            }
        ],
    )?;
    tx.execute("UPDATE local_targets SET phase=?3, version=version+1 WHERE target=?1 AND active_revision=?2", params![owner.target(),wave.spec.revision,if live { "stop_requested" } else { "stopped" }])?;
    tx.commit()?;
    load(store, &wave.id)
}

#[cfg(test)]
mod tests;
