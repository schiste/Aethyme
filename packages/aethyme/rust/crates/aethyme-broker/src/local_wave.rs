//! Provisional finite Local candidate waves (#667). Capture notifications only
//! admit already-authorized inputs. Composition runs the real provisional
//! composer; verification and acceptance remain unavailable until their
//! trusted adapters exist. No candidate code or publication is executed here.

#[cfg(test)]
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use aethyme_contracts::experimental_v0::{
    FieldKind, FieldSpec, Record, RecordId, RecordSchema, SourceSnapshotId,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::collaboration_archive::{self as archive, CommitOid, ObjectDigest};
use crate::collaboration_gc::archive_use;
use crate::collaboration_state::CollaborationStore;
use crate::composer::{self, Composition, CompositionBudget, CompositionRequest};
use crate::composition::{
    Candidate, CandidateInput, CompositionMode, CompositionOutcome, Producer,
};
use crate::local_target::{self as target, ControllerOwner, Phase, TargetError};
use crate::resolution::{self, FailedCheck, RequirementRef};

pub(crate) mod prior_completion;

#[cfg(test)]
struct SynthesizedAttachGuardCallback(Box<dyn FnMut(&CollaborationStore)>);

#[cfg(test)]
thread_local! {
    static SYNTHESIZED_ATTACH_GUARD_PROBE:
        RefCell<Option<SynthesizedAttachGuardCallback>> = const { RefCell::new(None) };
}

#[cfg(test)]
impl SynthesizedAttachGuardCallback {
    fn call(&mut self, store: &CollaborationStore) {
        (self.0)(store);
    }
}

#[cfg(test)]
pub(crate) struct SynthesizedAttachGuardProbe;

#[cfg(test)]
impl Drop for SynthesizedAttachGuardProbe {
    fn drop(&mut self) {
        SYNTHESIZED_ATTACH_GUARD_PROBE.with(|probe| {
            probe.borrow_mut().take();
        });
    }
}

#[cfg(test)]
pub(crate) fn observe_synthesized_attach_guards(
    callback: impl FnMut(&CollaborationStore) + 'static,
) -> SynthesizedAttachGuardProbe {
    SYNTHESIZED_ATTACH_GUARD_PROBE.with(|probe| {
        assert!(
            probe
                .borrow_mut()
                .replace(SynthesizedAttachGuardCallback(Box::new(callback)))
                .is_none()
        );
    });
    SynthesizedAttachGuardProbe
}

#[cfg(test)]
fn check_synthesized_attach_guards(store: &CollaborationStore) {
    SYNTHESIZED_ATTACH_GUARD_PROBE.with(|probe| {
        if let Some(callback) = probe.borrow_mut().as_mut() {
            callback.call(store);
        }
    });
}

const COMPOSITION_SOURCE_SCHEMA_NAME: &str = "aethyme.local-composition-source/experimental-v0";
const COMPOSITION_SOURCE_MAX_BYTES: usize = 1 << 20;
const ACCEPTED_BASELINE_TEXT_PROFILE: &str = "aethyme.compose.git-merge-file/accepted-baseline-v0";
const CONFLICT_PLAN_SCHEMA_NAME: &str = "aethyme.local-conflict-plan/experimental-v0";
const CONFLICT_SOURCE_SCHEMA_NAME: &str = "aethyme.local-conflict-source/experimental-v0";
const CONFLICT_SOURCE_MAX_BYTES: usize = 4 << 20;

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

static COMPOSITION_SOURCE_SCHEMA: RecordSchema = RecordSchema {
    name: COMPOSITION_SOURCE_SCHEMA_NAME,
    fields: &[
        FieldSpec {
            name: "version",
            required: true,
            kind: FieldKind::Integer,
            capability: None,
        },
        FieldSpec {
            name: "wave",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
        FieldSpec {
            name: "revision",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
        FieldSpec {
            name: "profile",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
        FieldSpec {
            name: "request",
            required: true,
            kind: FieldKind::Opaque,
            capability: None,
        },
        FieldSpec {
            name: "composition",
            required: true,
            kind: FieldKind::Opaque,
            capability: None,
        },
        FieldSpec {
            name: "requirements",
            required: true,
            kind: FieldKind::Opaque,
            capability: None,
        },
    ],
    capabilities: &[],
};

static CONFLICT_PLAN_SCHEMA: RecordSchema = RecordSchema {
    name: CONFLICT_PLAN_SCHEMA_NAME,
    fields: &[
        FieldSpec {
            name: "version",
            required: true,
            kind: FieldKind::Integer,
            capability: None,
        },
        FieldSpec {
            name: "wave",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
        FieldSpec {
            name: "revision",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
        FieldSpec {
            name: "profile",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
        FieldSpec {
            name: "engine",
            required: true,
            kind: FieldKind::Opaque,
            capability: None,
        },
        FieldSpec {
            name: "policy",
            required: true,
            kind: FieldKind::Opaque,
            capability: None,
        },
        FieldSpec {
            name: "request",
            required: true,
            kind: FieldKind::Opaque,
            capability: None,
        },
        FieldSpec {
            name: "selected",
            required: true,
            kind: FieldKind::Opaque,
            capability: None,
        },
        FieldSpec {
            name: "prior_completion",
            required: false,
            kind: FieldKind::String,
            capability: None,
        },
        FieldSpec {
            name: "order",
            required: true,
            kind: FieldKind::Opaque,
            capability: None,
        },
        FieldSpec {
            name: "conflicts",
            required: true,
            kind: FieldKind::Opaque,
            capability: None,
        },
        FieldSpec {
            name: "requirements",
            required: true,
            kind: FieldKind::Opaque,
            capability: None,
        },
        FieldSpec {
            name: "usage",
            required: true,
            kind: FieldKind::Opaque,
            capability: None,
        },
    ],
    capabilities: &[],
};

static CONFLICT_SOURCE_SCHEMA: RecordSchema = RecordSchema {
    name: CONFLICT_SOURCE_SCHEMA_NAME,
    fields: &[
        FieldSpec {
            name: "version",
            required: true,
            kind: FieldKind::Integer,
            capability: None,
        },
        FieldSpec {
            name: "wave",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
        FieldSpec {
            name: "revision",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
        FieldSpec {
            name: "request",
            required: true,
            kind: FieldKind::Opaque,
            capability: None,
        },
        FieldSpec {
            name: "plan",
            required: true,
            kind: FieldKind::Opaque,
            capability: None,
        },
    ],
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
    /// The exact request created for a real composer conflict or trusted
    /// failed check. The row stores both pieces because record IDs do not
    /// locate their archive bytes on their own.
    pub resolution_request: Option<resolution::RecordRef>,
    /// Exact inputs and result of the original composition, retained so a
    /// later controller invocation can build a request without recomposing.
    pub composition_source: Option<resolution::RecordRef>,
    /// An exact settled synthesized result attached after Local verification
    /// was requested. This remains unverified until fresh trusted gates run.
    pub resolution_result: Option<resolution::RecordRef>,
    pub detail: Option<String>,
    pub composition_ms: i64,
    pub drift_count: u32,
}

/// Exact immutable records that make a conflict request eligible for local
/// synthesized resolution. `source` is the wave-linked wrapper, and `plan`
/// is the canonical full-order composition source it binds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CurrentConflictRequestBinding {
    pub(crate) request: resolution::RecordRef,
    pub(crate) source: resolution::RecordRef,
    pub(crate) plan: resolution::RecordRef,
    pub(crate) plan_payload: ConflictPlanPayload,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConflictPlanPayload {
    #[serde(rename = "schema")]
    pub(crate) schema_name: String,
    pub(crate) version: u32,
    pub(crate) wave: String,
    pub(crate) revision: String,
    pub(crate) profile: String,
    pub(crate) engine: ConflictEnginePayload,
    pub(crate) policy: WavePolicy,
    pub(crate) request: CompositionRequestPayload,
    pub(crate) selected: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) prior_completion: Option<String>,
    pub(crate) order: Vec<ConflictOrderPayload>,
    pub(crate) conflicts: Vec<ConflictPayload>,
    pub(crate) requirements: Vec<RequirementPayload>,
    pub(crate) usage: BudgetUsagePayload,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConflictEnginePayload {
    pub(crate) engine: String,
    pub(crate) version: String,
    pub(crate) diff: String,
    pub(crate) move_window: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConflictOrderPayload {
    pub(crate) id: String,
    pub(crate) lineage: String,
    pub(crate) base_commit: String,
    pub(crate) result_commit: String,
    pub(crate) base_snapshot: String,
    pub(crate) result_snapshot: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConflictPayload {
    pub(crate) path: String,
    pub(crate) input: String,
    pub(crate) reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConflictSourcePayload {
    #[serde(rename = "schema")]
    schema_name: String,
    version: u32,
    wave: String,
    revision: String,
    request: RecordRefPayload,
    plan: RecordRefPayload,
}

/// Exact lineage and candidate provenance carried into each synthesized
/// native-gate Spec. These are source bindings only and confer no trust.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SynthesizedGroupInput {
    pub id: String,
    pub lineage: String,
    pub base_snapshot: String,
    pub result_snapshot: String,
    pub base_commit: String,
    pub result_commit: String,
}

/// Versioned binding from a Local wave to one strict async-ledger result.
/// The resolver's group is still unverified; native gates must run against
/// this exact candidate before it can advance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SynthesizedGroupProvenance {
    pub schema: String,
    pub version: u32,
    pub trust: String,
    pub request_record_id: String,
    pub request_object: String,
    pub group_record_id: String,
    pub group_object: String,
    pub input_manifest_sha256: String,
    pub baseline_snapshot: String,
    pub baseline_commit: String,
    pub candidate_subject: String,
    pub candidate_tree: String,
    pub candidate_commit: String,
    pub inputs: Vec<SynthesizedGroupInput>,
}

const SYNTHESIZED_GROUP_PROVENANCE_SCHEMA: &str =
    "aethyme.synthesized-group-provenance/experimental-v1";

/// Re-read an attached result through the exact settled ledger binding and
/// prove that it contains every selected lineage exactly once. Conflict
/// requests that only contain an applied prefix remain unattached until a
/// separate full-tail recomposition exists.
pub(crate) fn read_synthesized_wave_result(
    store: &mut CollaborationStore,
    repo: &Path,
    wave: &Wave,
) -> Result<Option<(resolution::UnverifiedSynthesis, SynthesizedGroupProvenance)>, WaveError> {
    let _archive_guard = archive_use(store)?;
    let _response_guard = crate::collaboration_sensitive::response_use(store)?;
    let (Some(request_ref), Some(result_ref)) = (&wave.resolution_request, &wave.resolution_result)
    else {
        if wave.resolution_request.is_none() && wave.resolution_result.is_none() {
            return Ok(None);
        }
        return Err(WaveError::Invalid(
            "synthesized result requires an exact request/result pair".into(),
        ));
    };
    if wave.phase != WavePhase::Verifying {
        return Err(WaveError::ResolutionUnsupported);
    }
    let pinned = target::load_revision(store, &id(&wave.spec.revision)?)?;
    let prior = prior_completion::load_retained(store, wave, &pinned)?;
    let synthesis =
        resolution::read_synthesized_attempt_result_for_refs(store, repo, request_ref, result_ref)?;
    if synthesis.status.request != *request_ref
        || synthesis.group_record != *result_ref
        || synthesis.request.record() != request_ref
        || synthesis.candidate.producer
            != (Producer::Resolver {
                profile: resolution::SYNTHESIS_PROFILE.into(),
            })
        || synthesis.candidate.mode != CompositionMode::Synthesized
        || synthesis.candidate.baseline_source != composer::COMPOSER_BASELINE_SOURCE
        || synthesis.candidate.baseline != synthesis.request.baseline().commit
        || synthesis.request.baseline().snapshot_id.as_str() != wave.spec.baseline
        || prior.as_ref().is_some_and(|binding| {
            binding
                .validate_request_baseline(&synthesis.request, wave)
                .is_err()
        })
        || wave.candidate_snapshot.as_ref() != Some(&synthesis.candidate.subject)
        || wave.candidate_commit.as_ref() != Some(&synthesis.candidate.commit)
    {
        return Err(WaveError::Invalid(
            "synthesized group differs from the current request or candidate contract".into(),
        ));
    }

    let revision = pinned;
    if revision.target != wave.spec.target || revision.issuer != wave.spec.issuer {
        return Err(WaveError::Stale);
    }
    let mut selected = BTreeMap::new();
    let mut selected_lineages = std::collections::BTreeSet::new();
    for selected_id in &wave.spec.selected {
        let contribution = revision
            .spec
            .contributions
            .iter()
            .find(|contribution| contribution.id == *selected_id)
            .ok_or(WaveError::Stale)?;
        let lineage = contribution.lineage.as_str();
        if !selected_lineages.insert(lineage.to_owned())
            || selected
                .insert(selected_id.clone(), lineage.to_owned())
                .is_some()
        {
            return Err(WaveError::Invalid(
                "selected lineages and contribution ids must be unique".into(),
            ));
        }
    }

    let mut request_members = BTreeMap::new();
    for member in synthesis
        .request
        .contents()
        .iter()
        .chain(synthesis.request.members())
    {
        let lineage = member.lineage.as_str().to_owned();
        if request_members
            .get(&lineage)
            .is_some_and(|previous| *previous != member)
        {
            return Err(WaveError::Invalid(
                "request contains conflicting inputs for one lineage".into(),
            ));
        }
        request_members.insert(lineage, member);
    }
    if request_members
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        != selected_lineages
    {
        return Err(WaveError::Invalid(
            "synthesized inputs do not cover the complete selected wave".into(),
        ));
    }

    let expected_inputs = selected
        .iter()
        .map(|(selected_id, lineage)| {
            let member = request_members.get(lineage).ok_or_else(|| {
                WaveError::Invalid("selected input is absent from request".into())
            })?;
            if member.id != *selected_id {
                return Err(WaveError::Invalid(
                    "request input id does not match the selected contribution".into(),
                ));
            }
            let retained = archive::retained_contribution(store, &member.lineage)?
                .ok_or(TargetError::Missing)?;
            if retained.base.snapshot_id != member.base
                || retained.result.snapshot_id != member.result
                || retained.base.commit != member.base_commit
                || retained.result.commit != member.result_commit
                || archive::snapshot_of_commit(repo, &member.base_commit)?.id() != member.base
                || archive::snapshot_of_commit(repo, &member.result_commit)?.id() != member.result
            {
                return Err(WaveError::Invalid(
                    "selected input differs from its retained lineage".into(),
                ));
            }
            Ok((
                member.base.as_str().to_owned(),
                member.result.as_str().to_owned(),
                member.base_commit.as_str().to_owned(),
                member.result_commit.as_str().to_owned(),
            ))
        })
        .collect::<Result<Vec<_>, WaveError>>()?;
    let mut actual_inputs = synthesis
        .candidate
        .inputs
        .iter()
        .map(|input| {
            Ok((
                input
                    .base_snapshot
                    .as_ref()
                    .ok_or_else(|| {
                        WaveError::Invalid("synthesized base snapshot is missing".into())
                    })?
                    .as_str()
                    .to_owned(),
                input
                    .result_snapshot
                    .as_ref()
                    .ok_or_else(|| {
                        WaveError::Invalid("synthesized result snapshot is missing".into())
                    })?
                    .as_str()
                    .to_owned(),
                input.base.as_str().to_owned(),
                input.result.as_str().to_owned(),
            ))
        })
        .collect::<Result<Vec<_>, WaveError>>()?;
    let mut expected_inputs = expected_inputs;
    actual_inputs.sort();
    expected_inputs.sort();
    if actual_inputs != expected_inputs {
        return Err(WaveError::Invalid(
            "synthesized candidate inputs differ from the selected lineages".into(),
        ));
    }

    let inputs = selected
        .iter()
        .map(|(selected_id, lineage)| {
            let member = request_members[lineage];
            SynthesizedGroupInput {
                id: selected_id.clone(),
                lineage: lineage.clone(),
                base_snapshot: member.base.to_string(),
                result_snapshot: member.result.to_string(),
                base_commit: member.base_commit.as_str().to_owned(),
                result_commit: member.result_commit.as_str().to_owned(),
            }
        })
        .collect();
    let provenance = SynthesizedGroupProvenance {
        schema: SYNTHESIZED_GROUP_PROVENANCE_SCHEMA.into(),
        version: 1,
        trust: "unverified_local".into(),
        request_record_id: request_ref.id.to_string(),
        request_object: request_ref.object.hex(),
        group_record_id: result_ref.id.to_string(),
        group_object: result_ref.object.hex(),
        input_manifest_sha256: synthesis.input_manifest.sha256.clone(),
        baseline_snapshot: synthesis.request.baseline().snapshot_id.to_string(),
        baseline_commit: synthesis.request.baseline().commit.as_str().to_owned(),
        candidate_subject: synthesis.candidate.subject.to_string(),
        candidate_tree: synthesis.candidate.tree.clone(),
        candidate_commit: synthesis.candidate.commit.as_str().to_owned(),
        inputs,
    };
    Ok(Some((synthesis, provenance)))
}

pub(crate) fn validate_synthesized_group_roots(
    store: &CollaborationStore,
    wave: &Wave,
    synthesis: &resolution::UnverifiedSynthesis,
    provenance: &SynthesizedGroupProvenance,
) -> Result<(), WaveError> {
    let revision = wave.spec.revision.as_str();
    let require_root = |kind: &str, reference: &str| -> Result<(), WaveError> {
        let rooted: bool = store.read_connection().query_row(
            "SELECT EXISTS(SELECT 1 FROM local_target_roots WHERE revision=?1 AND kind=?2 AND reference=?3)",
            params![revision, kind, reference],
            |row| row.get(0),
        )?;
        if !rooted {
            return Err(WaveError::Invalid(format!(
                "synthesized provenance {kind} {reference} is not retained by its revision"
            )));
        }
        Ok(())
    };
    for object_ref in [
        provenance.request_object.as_str(),
        provenance.group_object.as_str(),
        provenance.input_manifest_sha256.as_str(),
    ] {
        require_root("object", object_ref)?;
    }
    for content in synthesis
        .input_manifest
        .entries
        .iter()
        .filter_map(|entry| entry.sha256.as_deref())
    {
        require_root("object", content)?;
    }
    for snapshot_ref in std::iter::once(provenance.baseline_snapshot.as_str())
        .chain(std::iter::once(provenance.candidate_subject.as_str()))
        .chain(
            provenance
                .inputs
                .iter()
                .flat_map(|input| [input.base_snapshot.as_str(), input.result_snapshot.as_str()]),
        )
    {
        require_root("snapshot", snapshot_ref)?;
    }
    for input in &provenance.inputs {
        require_root("contribution", &input.lineage)?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct CompositionSource {
    pub(crate) record: resolution::RecordRef,
    pub(crate) request: CompositionRequest,
    pub(crate) composition: Composition,
    pub(crate) requirements: Vec<RequirementRef>,
}

#[derive(Debug)]
pub(crate) struct ComposedWave {
    pub(crate) wave: Wave,
    pub(crate) source: Option<CompositionSource>,
}

// These DTOs are the durable, strict representation of the source passed to
// request_resolution. Keeping every request and candidate field here lets a
// restarted controller resume from the original composition without running
// the composer again or deriving a replacement from the workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompositionSourcePayload {
    #[serde(rename = "schema")]
    schema_name: String,
    version: u32,
    wave: String,
    revision: String,
    profile: String,
    request: CompositionRequestPayload,
    composition: CompositionPayload,
    requirements: Vec<RequirementPayload>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompositionRequestPayload {
    pub(crate) baseline: String,
    pub(crate) accepted: Vec<String>,
    pub(crate) catalog: Vec<ContributionPayload>,
    pub(crate) deliveries: Vec<String>,
    pub(crate) budget: CompositionBudgetPayload,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContributionPayload {
    pub(crate) id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) lineage: Option<String>,
    pub(crate) requires: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) atomic_group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) revision_of: Option<String>,
    pub(crate) derived_from: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CompositionBudgetPayload {
    pub(crate) max_contributions: usize,
    pub(crate) max_paths: usize,
    pub(crate) max_bytes: String,
    pub(crate) max_merges: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompositionPayload {
    outcome: String,
    candidate: CandidatePayload,
    recipe: RecordRefPayload,
    order: Vec<String>,
    usage: BudgetUsagePayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidatePayload {
    subject: String,
    tree: String,
    commit: String,
    baseline: String,
    baseline_source: String,
    inputs: Vec<CandidateInputPayload>,
    producer_kind: String,
    producer_profile: String,
    mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CandidateInputPayload {
    base: String,
    result: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base_snapshot: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result_snapshot: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordRefPayload {
    id: String,
    object: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BudgetUsagePayload {
    pub(crate) contributions: usize,
    pub(crate) paths: usize,
    pub(crate) bytes: String,
    pub(crate) merges: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RequirementPayload {
    pub(crate) id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) path: Option<String>,
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
    #[error("resolution requests for accepted-baseline incremental compositions are unsupported")]
    ResolutionUnsupported,
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
    Resolution(Box<resolution::ResolutionError>),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl From<resolution::ResolutionError> for WaveError {
    fn from(error: resolution::ResolutionError) -> Self {
        Self::Resolution(Box::new(error))
    }
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
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
        String,
        i64,
    );
    let row: Row = store.read_connection().query_row(
        "SELECT body, phase, candidate_snapshot, candidate_commit, recipe_object, detail, composition_ms, drift_count, resolution_request_record_id, resolution_request_object, composition_source_record_id, composition_source_object, resolution_result_record_id, resolution_result_object, target, revision, sequence FROM local_waves WHERE wave = ?1", [wave.as_str()],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?,r.get(10)?,r.get(11)?,r.get(12)?,r.get(13)?,r.get(14)?,r.get(15)?,r.get(16)?)),
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
    if spec.target != row.14
        || spec.revision != row.15
        || spec.sequence != row.16
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
        resolution_request: record_ref(row.8.as_deref(), row.9.as_deref())?,
        composition_source: record_ref(row.10.as_deref(), row.11.as_deref())?,
        resolution_result: record_ref(row.12.as_deref(), row.13.as_deref())?,
        detail: row.5,
        composition_ms: row.6,
        drift_count: row.7,
    })
}

fn record_ref(
    id_text: Option<&str>,
    object_text: Option<&str>,
) -> Result<Option<resolution::RecordRef>, WaveError> {
    match (id_text, object_text) {
        (None, None) => Ok(None),
        (Some(id_text), Some(object_text)) => Ok(Some(resolution::RecordRef {
            id: id(id_text)?,
            object: object(object_text)?,
        })),
        _ => Err(WaveError::Invalid(
            "stored wave record reference pair".into(),
        )),
    }
}

fn source_payload(
    wave: &Wave,
    request: &CompositionRequest,
    composition: &Composition,
    requirements: &[RequirementRef],
) -> Result<CompositionSourcePayload, WaveError> {
    let candidate = composition
        .outcome
        .candidate()
        .ok_or_else(|| WaveError::Invalid("composition source is not a candidate".into()))?;
    let recipe = composition
        .recipe
        .as_ref()
        .ok_or_else(|| WaveError::Invalid("candidate source has no recipe".into()))?;
    let (producer_kind, producer_profile) = match &candidate.producer {
        Producer::Composer { profile } => ("composer", profile.clone()),
        Producer::Resolver { .. } | Producer::LegacyGitReplay => {
            return Err(WaveError::Invalid(
                "local composition source has a non-composer producer".into(),
            ));
        }
    };
    Ok(CompositionSourcePayload {
        schema_name: COMPOSITION_SOURCE_SCHEMA_NAME.into(),
        version: 1,
        wave: wave.id.to_string(),
        revision: wave.spec.revision.clone(),
        profile: composer::PROVISIONAL_TEXT_PROFILE.into(),
        request: CompositionRequestPayload {
            baseline: request.baseline.as_str().to_owned(),
            accepted: request
                .accepted
                .iter()
                .map(|commit| commit.as_str().to_owned())
                .collect(),
            catalog: request
                .catalog
                .iter()
                .map(|item| ContributionPayload {
                    id: item.id.clone(),
                    lineage: item.lineage.as_ref().map(ToString::to_string),
                    requires: item.requires.clone(),
                    atomic_group: item.atomic_group.clone(),
                    revision_of: item.revision_of.clone(),
                    derived_from: item.derived_from.clone(),
                })
                .collect(),
            deliveries: request.deliveries.clone(),
            budget: CompositionBudgetPayload {
                max_contributions: request.budget.max_contributions,
                max_paths: request.budget.max_paths,
                max_bytes: request.budget.max_bytes.to_string(),
                max_merges: request.budget.max_merges,
            },
        },
        composition: CompositionPayload {
            outcome: "candidate".into(),
            candidate: CandidatePayload {
                subject: candidate.subject.to_string(),
                tree: candidate.tree.clone(),
                commit: candidate.commit.as_str().to_owned(),
                baseline: candidate.baseline.as_str().to_owned(),
                baseline_source: candidate.baseline_source.into(),
                inputs: candidate
                    .inputs
                    .iter()
                    .map(|input| CandidateInputPayload {
                        base: input.base.as_str().to_owned(),
                        result: input.result.as_str().to_owned(),
                        base_snapshot: input.base_snapshot.as_ref().map(ToString::to_string),
                        result_snapshot: input.result_snapshot.as_ref().map(ToString::to_string),
                    })
                    .collect(),
                producer_kind: producer_kind.into(),
                producer_profile,
                mode: composition_mode_code(candidate.mode).into(),
            },
            recipe: RecordRefPayload {
                id: recipe.id.to_string(),
                object: recipe.object.hex(),
            },
            order: composition.order.clone(),
            usage: BudgetUsagePayload {
                contributions: composition.usage.contributions,
                paths: composition.usage.paths,
                bytes: composition.usage.bytes.to_string(),
                merges: composition.usage.merges,
            },
        },
        requirements: requirements
            .iter()
            .map(|requirement| RequirementPayload {
                id: requirement.id.clone(),
                path: requirement.path.clone(),
            })
            .collect(),
    })
}

fn recipe_has_accepted_baseline(
    store: &CollaborationStore,
    composition: &Composition,
) -> Result<bool, WaveError> {
    let Some(recipe) = composition.recipe.as_ref() else {
        return Ok(false);
    };
    Ok(composer::read_recipe(store, recipe)?
        .get("accepted_baseline")
        .is_some())
}

fn ensure_resolution_supported(
    store: &CollaborationStore,
    composition: &Composition,
) -> Result<(), WaveError> {
    if recipe_has_accepted_baseline(store, composition)? {
        Err(WaveError::ResolutionUnsupported)
    } else {
        Ok(())
    }
}

fn composition_mode_code(mode: CompositionMode) -> &'static str {
    match mode {
        CompositionMode::Text => "text",
        CompositionMode::Structural => "structural",
        CompositionMode::Hybrid => "hybrid",
        CompositionMode::Synthesized => "synthesized",
    }
}

fn write_source_record(
    store: &CollaborationStore,
    wave: &Wave,
    request: &CompositionRequest,
    composition: &Composition,
    requirements: &[RequirementRef],
) -> Result<resolution::RecordRef, WaveError> {
    let payload = source_payload(wave, request, composition, requirements)?;
    let bytes = serde_json::to_vec(&payload)?;
    if bytes.len() > COMPOSITION_SOURCE_MAX_BYTES {
        return Err(WaveError::Invalid(
            "composition source record exceeds its byte limit".into(),
        ));
    }
    let record = Record::decode(&bytes, &[&COMPOSITION_SOURCE_SCHEMA])
        .map_err(|error| WaveError::Invalid(format!("composition source record: {error}")))?;
    let object = archive::put_object(store, &record.canonical_bytes())?;
    Ok(resolution::RecordRef {
        id: record.id(),
        object,
    })
}

fn composition_request_payload(request: &CompositionRequest) -> CompositionRequestPayload {
    CompositionRequestPayload {
        baseline: request.baseline.as_str().to_owned(),
        accepted: request
            .accepted
            .iter()
            .map(|commit| commit.as_str().to_owned())
            .collect(),
        catalog: request
            .catalog
            .iter()
            .map(|item| ContributionPayload {
                id: item.id.clone(),
                lineage: item.lineage.as_ref().map(ToString::to_string),
                requires: item.requires.clone(),
                atomic_group: item.atomic_group.clone(),
                revision_of: item.revision_of.clone(),
                derived_from: item.derived_from.clone(),
            })
            .collect(),
        deliveries: request.deliveries.clone(),
        budget: CompositionBudgetPayload {
            max_contributions: request.budget.max_contributions,
            max_paths: request.budget.max_paths,
            max_bytes: request.budget.max_bytes.to_string(),
            max_merges: request.budget.max_merges,
        },
    }
}

fn write_conflict_plan_record(
    store: &CollaborationStore,
    wave: &Wave,
    request: &CompositionRequest,
    composition: &Composition,
    requirements: &[RequirementRef],
    revisions: Option<&BTreeMap<String, RecordId>>,
    pins: &composer::ProvisionalTextProfilePins,
) -> Result<resolution::RecordRef, WaveError> {
    let CompositionOutcome::Conflict {
        baseline,
        conflicts,
    } = &composition.outcome
    else {
        return Err(WaveError::Invalid(
            "conflict source requires a conflict composition".into(),
        ));
    };
    if *baseline != request.baseline
        || request.deliveries != wave.spec.selected
        || request.deliveries.len() != composition.order.len()
        || composition.order.len() != wave.spec.selected.len()
        || conflicts.is_empty()
        || pins.profile != composer::PROVISIONAL_TEXT_PROFILE
    {
        return Err(WaveError::Invalid(
            "conflict source does not match the frozen wave".into(),
        ));
    }
    let selected: BTreeSet<&str> = wave.spec.selected.iter().map(String::as_str).collect();
    let ordered: BTreeSet<&str> = composition.order.iter().map(String::as_str).collect();
    if selected.len() != wave.spec.selected.len()
        || ordered.len() != composition.order.len()
        || selected != ordered
    {
        return Err(WaveError::Invalid(
            "conflict source order is not the complete selected set".into(),
        ));
    }
    let mut catalog = BTreeMap::new();
    for item in &request.catalog {
        if catalog.insert(item.id.as_str(), item).is_some() {
            return Err(WaveError::Invalid(
                "conflict source catalog repeats a contribution".into(),
            ));
        }
    }
    let order = composition
        .order
        .iter()
        .map(|contribution_id| {
            let contribution = catalog.get(contribution_id.as_str()).ok_or_else(|| {
                WaveError::Invalid("conflict source order has an unknown contribution".into())
            })?;
            let lineage = contribution
                .lineage
                .as_ref()
                .ok_or_else(|| WaveError::Invalid("conflict source lineage is missing".into()))?;
            let retained =
                archive::retained_contribution(store, lineage)?.ok_or(TargetError::Missing)?;
            Ok(ConflictOrderPayload {
                id: contribution_id.clone(),
                lineage: lineage.to_string(),
                base_commit: retained.base.commit.as_str().to_owned(),
                result_commit: retained.result.commit.as_str().to_owned(),
                base_snapshot: retained.base.snapshot_id.to_string(),
                result_snapshot: retained.result.snapshot_id.to_string(),
                revision: revisions
                    .and_then(|revisions| revisions.get(contribution_id))
                    .map(ToString::to_string),
            })
        })
        .collect::<Result<Vec<_>, WaveError>>()?;
    let payload = ConflictPlanPayload {
        schema_name: CONFLICT_PLAN_SCHEMA_NAME.into(),
        version: 1,
        wave: wave.id.to_string(),
        revision: wave.spec.revision.clone(),
        profile: pins.profile.clone(),
        engine: ConflictEnginePayload {
            engine: pins.engine.clone(),
            version: pins.version.clone(),
            diff: pins.diff.clone(),
            move_window: pins.move_window,
        },
        policy: wave.spec.policy,
        request: composition_request_payload(request),
        selected: wave.spec.selected.clone(),
        prior_completion: wave.spec.prior_completion.clone(),
        order,
        conflicts: conflicts
            .iter()
            .map(|conflict| ConflictPayload {
                path: conflict.path.clone(),
                input: conflict.input.as_str().to_owned(),
                reason: conflict.reason.code().to_owned(),
            })
            .collect(),
        requirements: requirement_payloads(requirements),
        usage: BudgetUsagePayload {
            contributions: composition.usage.contributions,
            paths: composition.usage.paths,
            bytes: composition.usage.bytes.to_string(),
            merges: composition.usage.merges,
        },
    };
    let bytes = serde_json::to_vec(&payload)?;
    if bytes.len() > CONFLICT_SOURCE_MAX_BYTES {
        return Err(WaveError::Invalid(
            "conflict plan record exceeds its byte limit".into(),
        ));
    }
    let record = Record::decode(&bytes, &[&CONFLICT_PLAN_SCHEMA])
        .map_err(|error| WaveError::Invalid(format!("conflict plan record: {error}")))?;
    let object = archive::put_object(store, &record.canonical_bytes())?;
    Ok(resolution::RecordRef {
        id: record.id(),
        object,
    })
}

fn write_conflict_source_record(
    store: &CollaborationStore,
    wave: &Wave,
    request: &resolution::RecordRef,
    plan: &resolution::RecordRef,
) -> Result<resolution::RecordRef, WaveError> {
    let payload = ConflictSourcePayload {
        schema_name: CONFLICT_SOURCE_SCHEMA_NAME.into(),
        version: 1,
        wave: wave.id.to_string(),
        revision: wave.spec.revision.clone(),
        request: RecordRefPayload {
            id: request.id.to_string(),
            object: request.object.hex(),
        },
        plan: RecordRefPayload {
            id: plan.id.to_string(),
            object: plan.object.hex(),
        },
    };
    let bytes = serde_json::to_vec(&payload)?;
    if bytes.len() > CONFLICT_SOURCE_MAX_BYTES {
        return Err(WaveError::Invalid(
            "conflict source record exceeds its byte limit".into(),
        ));
    }
    let record = Record::decode(&bytes, &[&CONFLICT_SOURCE_SCHEMA])
        .map_err(|error| WaveError::Invalid(format!("conflict source record: {error}")))?;
    let object = archive::put_object(store, &record.canonical_bytes())?;
    Ok(resolution::RecordRef {
        id: record.id(),
        object,
    })
}

fn request_from_payload(
    payload: &CompositionRequestPayload,
) -> Result<CompositionRequest, WaveError> {
    let max_bytes = canonical_u64(&payload.budget.max_bytes, "composition source byte budget")?;
    let catalog = payload
        .catalog
        .iter()
        .map(|item| {
            Ok(composer::ContributionSpec {
                id: item.id.clone(),
                lineage: item.lineage.as_deref().map(id).transpose()?,
                requires: item.requires.clone(),
                atomic_group: item.atomic_group.clone(),
                revision_of: item.revision_of.clone(),
                derived_from: item.derived_from.clone(),
            })
        })
        .collect::<Result<Vec<_>, WaveError>>()?;
    Ok(CompositionRequest {
        baseline: CommitOid::parse(&payload.baseline)?,
        accepted: payload
            .accepted
            .iter()
            .map(|commit| CommitOid::parse(commit).map_err(WaveError::from))
            .collect::<Result<Vec<_>, _>>()?,
        catalog,
        deliveries: payload.deliveries.clone(),
        budget: CompositionBudget {
            max_contributions: payload.budget.max_contributions,
            max_paths: payload.budget.max_paths,
            max_bytes,
            max_merges: payload.budget.max_merges,
        },
    })
}

fn canonical_u64(value: &str, field: &str) -> Result<u64, WaveError> {
    let parsed = value
        .parse::<u64>()
        .map_err(|_| WaveError::Invalid(field.into()))?;
    if parsed.to_string() != value {
        return Err(WaveError::Invalid(field.into()));
    }
    Ok(parsed)
}

fn source_from_payload(
    payload: &CompositionSourcePayload,
    reference: &resolution::RecordRef,
) -> Result<CompositionSource, WaveError> {
    if payload.schema_name != COMPOSITION_SOURCE_SCHEMA_NAME
        || payload.version != 1
        || payload.profile != composer::PROVISIONAL_TEXT_PROFILE
        || payload.composition.outcome != "candidate"
        || payload.composition.candidate.producer_kind != "composer"
        || !matches!(
            payload.composition.candidate.producer_profile.as_str(),
            composer::PROVISIONAL_TEXT_PROFILE | ACCEPTED_BASELINE_TEXT_PROFILE
        )
        || payload.composition.candidate.baseline_source != composer::COMPOSER_BASELINE_SOURCE
        || payload.composition.candidate.mode != "text"
    {
        return Err(WaveError::Invalid(
            "composition source record version or profile".into(),
        ));
    }
    let request = request_from_payload(&payload.request)?;
    let candidate = Candidate {
        subject: snapshot(&payload.composition.candidate.subject)?,
        tree: payload.composition.candidate.tree.clone(),
        commit: CommitOid::parse(&payload.composition.candidate.commit)?,
        baseline: CommitOid::parse(&payload.composition.candidate.baseline)?,
        baseline_source: composer::COMPOSER_BASELINE_SOURCE,
        inputs: payload
            .composition
            .candidate
            .inputs
            .iter()
            .map(|input| {
                Ok(CandidateInput {
                    base: CommitOid::parse(&input.base)?,
                    result: CommitOid::parse(&input.result)?,
                    base_snapshot: input.base_snapshot.as_deref().map(snapshot).transpose()?,
                    result_snapshot: input.result_snapshot.as_deref().map(snapshot).transpose()?,
                })
            })
            .collect::<Result<Vec<_>, WaveError>>()?,
        producer: Producer::Composer {
            profile: payload.composition.candidate.producer_profile.clone(),
        },
        mode: CompositionMode::Text,
    };
    let recipe = composer::RecipeRef {
        id: id(&payload.composition.recipe.id)?,
        object: object(&payload.composition.recipe.object)?,
    };
    let usage_bytes = canonical_u64(
        &payload.composition.usage.bytes,
        "composition source usage bytes",
    )?;
    let requirements = payload
        .requirements
        .iter()
        .map(|requirement| RequirementRef {
            id: requirement.id.clone(),
            path: requirement.path.clone(),
        })
        .collect();
    Ok(CompositionSource {
        record: reference.clone(),
        request,
        composition: Composition {
            outcome: CompositionOutcome::Candidate(candidate),
            recipe: Some(recipe),
            order: payload.composition.order.clone(),
            usage: composer::BudgetUsage {
                contributions: payload.composition.usage.contributions,
                paths: payload.composition.usage.paths,
                bytes: usage_bytes,
                merges: payload.composition.usage.merges,
            },
        },
        requirements,
    })
}

fn expected_composition_request(
    store: &CollaborationStore,
    repo: &Path,
    wave: &Wave,
    pinned: &target::TargetRevision,
    prior: Option<&prior_completion::Binding>,
) -> Result<CompositionRequest, WaveError> {
    let baseline =
        archive::retained(store, &snapshot(&wave.spec.baseline)?)?.ok_or(TargetError::Missing)?;
    let (base_commit, accepted) = if let Some(prior) = prior {
        prior.validate_retained_completion(store)?;
        prior.validate_git_binding(repo)?;
        (
            prior.request_baseline_commit().clone(),
            prior.accepted_history(store, pinned)?,
        )
    } else {
        (baseline.commit, Vec::new())
    };
    Ok(CompositionRequest {
        baseline: base_commit,
        accepted,
        catalog: pinned.spec.catalog()?,
        deliveries: wave.spec.selected.clone(),
        budget: CompositionBudget {
            max_contributions: wave.spec.policy.max_group,
            ..CompositionBudget::default()
        },
    })
}

fn expected_source_provenance(
    prior: Option<&prior_completion::Binding>,
) -> Result<(&'static str, Option<serde_json::Value>), WaveError> {
    let Some(prior) = prior else {
        return Ok((composer::PROVISIONAL_TEXT_PROFILE, None));
    };
    Ok((prior.producer_profile(), Some(prior.recipe_binding())))
}

fn prior_completion_recipe_matches(
    recipe: &serde_json::Value,
    expected: Option<&serde_json::Value>,
) -> bool {
    recipe.get("accepted_baseline") == expected
}

fn requirement_payloads(requirements: &[RequirementRef]) -> Vec<RequirementPayload> {
    requirements
        .iter()
        .map(|requirement| RequirementPayload {
            id: requirement.id.clone(),
            path: requirement.path.clone(),
        })
        .collect()
}

fn validate_source_context(
    store: &CollaborationStore,
    repo: &Path,
    owner: &ControllerOwner,
    wave: &Wave,
    pinned: &target::TargetRevision,
    payload: &CompositionSourcePayload,
    source: &CompositionSource,
) -> Result<(), WaveError> {
    if payload.wave != wave.id.as_str()
        || payload.revision != wave.spec.revision
        || payload.profile != pinned.spec.composer_profile
        || payload.profile != composer::PROVISIONAL_TEXT_PROFILE
        || wave.phase != WavePhase::Verifying
        || wave.spec.target != owner.target()
        || wave.spec.issuer != owner.issuer()
    {
        return Err(WaveError::Stale);
    }
    let prior = prior_completion::load(store, owner, wave, pinned)?;
    if let Some(prior) = &prior {
        prior.validate_retained_completion(store)?;
        prior.validate_git_binding(repo)?;
    }
    let expected = expected_composition_request(store, repo, wave, pinned, prior.as_ref())?;
    let (expected_producer_profile, expected_prior_completion) =
        expected_source_provenance(prior.as_ref())?;
    if source.request.baseline != expected.baseline
        || source.request.accepted != expected.accepted
        || source.request.catalog != expected.catalog
        || source.request.deliveries != expected.deliveries
        || source.request.budget != expected.budget
    {
        return Err(WaveError::Invalid(
            "composition source request does not match the pinned wave".into(),
        ));
    }
    let expected_requirements = pinned
        .spec
        .requirements
        .iter()
        .map(|id| RequirementRef {
            id: id.clone(),
            path: None,
        })
        .collect::<Vec<_>>();
    if source.requirements != expected_requirements
        || payload.requirements != requirement_payloads(&expected_requirements)
    {
        return Err(WaveError::Invalid(
            "composition source requirements do not match the pinned revision".into(),
        ));
    }
    let candidate = source
        .composition
        .outcome
        .candidate()
        .ok_or_else(|| WaveError::Invalid("composition source has no candidate".into()))?;
    let recipe = source
        .composition
        .recipe
        .as_ref()
        .ok_or_else(|| WaveError::Invalid("composition source has no recipe".into()))?;
    if (wave.resolution_result.is_none()
        && (wave.candidate_snapshot.as_ref() != Some(&candidate.subject)
            || wave.candidate_commit.as_ref() != Some(&candidate.commit)))
        || wave.recipe_object.as_ref() != Some(&recipe.object)
        || source.record != *wave.composition_source.as_ref().ok_or(WaveError::Stale)?
        || candidate.baseline != source.request.baseline
        || candidate.producer.profile() != expected_producer_profile
        || candidate.mode != CompositionMode::Text
        || candidate.baseline_source != composer::COMPOSER_BASELINE_SOURCE
        || archive::raw_commit_tree(repo, &candidate.commit)? != candidate.tree
    {
        return Err(WaveError::Invalid(
            "composition source candidate does not match the current wave".into(),
        ));
    }
    archive::retained(store, &candidate.subject)?.ok_or(TargetError::Missing)?;
    if archive::snapshot_of_commit(repo, &candidate.commit)?.id() != candidate.subject {
        return Err(WaveError::Invalid(
            "composition source subject and commit disagree".into(),
        ));
    }
    let recipe_record = composer::read_recipe(store, recipe)?;
    let recipe_json: serde_json::Value = serde_json::from_slice(&recipe_record.canonical_bytes())?;
    if !prior_completion_recipe_matches(&recipe_json, expected_prior_completion.as_ref()) {
        return Err(WaveError::Invalid(
            "composition recipe prior completion does not match the wave".into(),
        ));
    }
    if recipe_json["candidate"].as_str() != Some(candidate.subject.as_str())
        || recipe_json["candidate_commit"].as_str() != Some(candidate.commit.as_str())
        || recipe_json["baseline_commit"].as_str() != Some(source.request.baseline.as_str())
        || recipe_json["profile"].as_str() != Some(candidate.producer.profile())
    {
        return Err(WaveError::Invalid(
            "composition source candidate disagrees with its recipe".into(),
        ));
    }
    let baseline_snapshot = recipe_json["baseline"]
        .as_str()
        .ok_or_else(|| WaveError::Invalid("composition recipe baseline".into()))?;
    let baseline_snapshot = snapshot(baseline_snapshot)?;
    archive::retained(store, &baseline_snapshot)?.ok_or(TargetError::Missing)?;
    if archive::snapshot_of_commit(repo, &source.request.baseline)?.id() != baseline_snapshot {
        return Err(WaveError::Invalid(
            "composition recipe baseline snapshot changed".into(),
        ));
    }
    let recipe_order = recipe_json["order"]
        .as_array()
        .ok_or_else(|| WaveError::Invalid("composition recipe order".into()))?;
    if recipe_order.len() != source.composition.order.len()
        || candidate.inputs.len() != recipe_order.len()
        || recipe_order
            .iter()
            .zip(&source.composition.order)
            .any(|(entry, id)| entry["id"].as_str() != Some(id.as_str()))
    {
        return Err(WaveError::Invalid(
            "composition source order does not match its recipe".into(),
        ));
    }
    for (entry, input) in recipe_order.iter().zip(&candidate.inputs) {
        let base_text = entry["base"]
            .as_str()
            .ok_or_else(|| WaveError::Invalid("composition recipe input base".into()))?;
        let result_text = entry["result"]
            .as_str()
            .ok_or_else(|| WaveError::Invalid("composition recipe input result".into()))?;
        let base = snapshot(base_text)?;
        let result = snapshot(result_text)?;
        let contribution_id = entry["id"]
            .as_str()
            .ok_or_else(|| WaveError::Invalid("composition recipe input id".into()))?;
        let lineage_text = entry["lineage"]
            .as_str()
            .ok_or_else(|| WaveError::Invalid("composition recipe input lineage".into()))?;
        let contribution = source
            .request
            .catalog
            .iter()
            .find(|contribution| contribution.id == contribution_id)
            .ok_or_else(|| {
                WaveError::Invalid("composition recipe contribution is not requested".into())
            })?;
        let lineage = contribution.lineage.as_ref().ok_or_else(|| {
            WaveError::Invalid("composition recipe lineage is not retained".into())
        })?;
        if lineage.as_str() != lineage_text {
            return Err(WaveError::Invalid(
                "composition recipe lineage does not match the pinned request".into(),
            ));
        }
        let retained =
            archive::retained_contribution(store, lineage)?.ok_or(TargetError::Missing)?;
        if input.base_snapshot.as_ref() != Some(&base)
            || input.result_snapshot.as_ref() != Some(&result)
            || retained.base.snapshot_id != base
            || retained.result.snapshot_id != result
            || archive::snapshot_of_commit(repo, &input.base)?.id() != base
            || archive::snapshot_of_commit(repo, &input.result)?.id() != result
        {
            return Err(WaveError::Invalid(
                "composition source input does not match its recipe".into(),
            ));
        }
    }
    if source.composition.usage.contributions > source.request.budget.max_contributions
        || source.composition.usage.paths > source.request.budget.max_paths
        || source.composition.usage.bytes > source.request.budget.max_bytes
        || source.composition.usage.merges > source.request.budget.max_merges
    {
        return Err(WaveError::Invalid(
            "composition source usage exceeds its request budget".into(),
        ));
    }
    Ok(())
}

pub(crate) fn load_composition_source(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    repo: &Path,
    wave_id: &RecordId,
) -> Result<(Wave, Option<CompositionSource>), WaveError> {
    let _archive_guard = archive_use(store)?;
    let response_guard = crate::collaboration_sensitive::response_use(store)?;
    let observed = load(store, wave_id)?;
    let synthesized = if observed.resolution_result.is_some() {
        read_synthesized_wave_result(store, repo, &observed)?
    } else {
        None
    };
    let _authority_guard = target::authorize(store, owner)?;
    let wave = load(store, wave_id)?;
    if wave.resolution_request != observed.resolution_request
        || wave.resolution_result != observed.resolution_result
    {
        return Err(WaveError::Stale);
    }
    if let Some((synthesis, provenance)) = &synthesized {
        validate_synthesized_group_roots(store, &wave, synthesis, provenance)?;
    }
    let _state = current(store, owner, &wave)?;
    if wave.phase != WavePhase::Verifying {
        return Ok((wave, None));
    }
    let Some(reference) = &wave.composition_source else {
        // A wave composed by an older version has no durable exact context.
        // Keep it verifiable, but never manufacture a resolution request.
        return Ok((wave, None));
    };
    let bytes =
        archive::read_object_bounded(store, &reference.object, COMPOSITION_SOURCE_MAX_BYTES)?;
    let record = Record::decode(&bytes, &[&COMPOSITION_SOURCE_SCHEMA])
        .map_err(|error| WaveError::Invalid(format!("composition source record: {error}")))?;
    if record.id() != reference.id || record.canonical_bytes() != bytes {
        return Err(WaveError::Invalid(
            "composition source record digest".into(),
        ));
    }
    let payload: CompositionSourcePayload = serde_json::from_slice(&bytes)?;
    let source = source_from_payload(&payload, reference)?;
    let revision = id(&wave.spec.revision)?;
    let pinned = target::load_revision_under_response(store, &revision, &response_guard)?;
    validate_source_context(store, repo, owner, &wave, &pinned, &payload, &source)?;
    validate_composition_source_roots(store, &wave, &pinned, &source)?;
    Ok((wave, Some(source)))
}

struct RetentionGuards<'a> {
    archive: &'a crate::collaboration_gc::ArchiveUse,
    response: &'a crate::collaboration_sensitive::ResponseUse,
    authority: &'a crate::local_authority::Guard,
}

struct ConflictRequestRetention {
    revision: RecordId,
    snapshots: Vec<SourceSnapshotId>,
    contributions: Vec<RecordId>,
    objects: Vec<ObjectDigest>,
}

fn prepare_conflict_request_retention(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    revision: &RecordId,
    request: &resolution::ResolutionRequest,
    composition_request: &CompositionRequest,
    plan: &resolution::RecordRef,
    source: &resolution::RecordRef,
    guards: RetentionGuards<'_>,
) -> Result<ConflictRequestRetention, WaveError> {
    let pinned = target::load_revision_under_response(store, revision, guards.response)?;
    if pinned.target != owner.target() || pinned.issuer != owner.issuer() {
        return Err(WaveError::Stale);
    }
    let _permit = target::ArtifactRetentionPermit::new(
        store,
        owner,
        guards.archive,
        guards.response,
        guards.authority,
    )?;

    let mut snapshots = BTreeMap::new();
    let mut add_snapshot = |snapshot: &SourceSnapshotId| {
        snapshots.insert(snapshot.to_string(), snapshot.clone());
    };
    add_snapshot(&request.baseline().snapshot_id);
    add_snapshot(&request.accumulator().snapshot_id);
    for member in request
        .members()
        .iter()
        .chain(request.decision_members())
        .chain(request.contents())
    {
        add_snapshot(&member.base);
        add_snapshot(&member.result);
    }

    let selected: BTreeSet<&str> = composition_request
        .deliveries
        .iter()
        .map(String::as_str)
        .collect();
    if selected.len() != composition_request.deliveries.len() {
        return Err(WaveError::Invalid(
            "conflict request repeats a selected contribution".into(),
        ));
    }
    let mut catalog = BTreeMap::new();
    for contribution in &composition_request.catalog {
        if catalog
            .insert(contribution.id.as_str(), contribution)
            .is_some()
        {
            return Err(WaveError::Invalid(
                "conflict request catalog repeats a contribution".into(),
            ));
        }
    }
    let mut contributions = BTreeMap::new();
    for contribution_id in selected {
        let contribution = catalog.get(contribution_id).ok_or_else(|| {
            WaveError::Invalid("conflict request selection is missing from its catalog".into())
        })?;
        let lineage = contribution
            .lineage
            .as_ref()
            .ok_or_else(|| WaveError::Invalid("conflict request lineage is missing".into()))?;
        let retained =
            archive::retained_contribution(store, lineage)?.ok_or(TargetError::Missing)?;
        add_snapshot(&retained.base.snapshot_id);
        add_snapshot(&retained.result.snapshot_id);
        contributions.insert(lineage.to_string(), lineage.clone());
    }

    let mut objects = BTreeMap::new();
    for object in [request.record().object, plan.object, source.object] {
        objects.insert(object.hex(), object);
    }
    for snapshot in snapshots.values() {
        if archive::retained(store, snapshot)?.is_none() {
            return Err(TargetError::Missing.into());
        }
    }
    for object in objects.values() {
        archive::read_object(store, object)?;
    }

    Ok(ConflictRequestRetention {
        revision: revision.clone(),
        snapshots: snapshots.into_values().collect(),
        contributions: contributions.into_values().collect(),
        objects: objects.into_values().collect(),
    })
}

fn retain_composition_source(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    revision: &RecordId,
    reference: &resolution::RecordRef,
    source: &CompositionSource,
    pinned: &target::TargetRevision,
    guards: RetentionGuards<'_>,
) -> Result<(), WaveError> {
    let candidate = source
        .composition
        .outcome
        .candidate()
        .ok_or_else(|| WaveError::Invalid("composition source has no candidate".into()))?;
    let recipe = source
        .composition
        .recipe
        .as_ref()
        .ok_or_else(|| WaveError::Invalid("composition source has no recipe".into()))?;
    let recipe_record = composer::read_recipe(store, recipe)?;
    let recipe_json: serde_json::Value = serde_json::from_slice(&recipe_record.canonical_bytes())?;
    let baseline = snapshot(
        recipe_json["baseline"]
            .as_str()
            .ok_or_else(|| WaveError::Invalid("composition recipe baseline".into()))?,
    )?;
    let mut snapshots = std::collections::BTreeMap::new();
    snapshots.insert(candidate.subject.to_string(), candidate.subject.clone());
    snapshots.insert(baseline.to_string(), baseline);
    if !source.request.accepted.is_empty() {
        let accepted_baseline = snapshot(&pinned.spec.baseline)?;
        let retained = archive::retained(store, &accepted_baseline)?.ok_or(TargetError::Missing)?;
        if !source.request.accepted.contains(&retained.commit) {
            return Err(WaveError::Invalid(
                "composition source accepted history is not rooted in this revision".into(),
            ));
        }
        snapshots.insert(accepted_baseline.to_string(), accepted_baseline);
    }
    for input in &candidate.inputs {
        for snapshot in [&input.base_snapshot, &input.result_snapshot]
            .into_iter()
            .flatten()
        {
            snapshots.insert(snapshot.to_string(), snapshot.clone());
        }
    }
    for contribution in source
        .request
        .catalog
        .iter()
        .filter(|item| source.request.deliveries.contains(&item.id))
    {
        let lineage = contribution
            .lineage
            .as_ref()
            .ok_or_else(|| WaveError::Invalid("composition source lineage is missing".into()))?;
        let retained =
            archive::retained_contribution(store, lineage)?.ok_or(TargetError::Missing)?;
        snapshots.insert(
            retained.base.snapshot_id.to_string(),
            retained.base.snapshot_id,
        );
        snapshots.insert(
            retained.result.snapshot_id.to_string(),
            retained.result.snapshot_id,
        );
    }
    let snapshots = snapshots.into_values().collect::<Vec<_>>();
    let objects = [reference.object, recipe.object];
    let retention = target::ArtifactRetentionPermit::new(
        store,
        owner,
        guards.archive,
        guards.response,
        guards.authority,
    )?;
    target::retain_artifacts_under_guards(store, revision, &snapshots, &objects, &retention)?;
    Ok(())
}

fn validate_composition_source_roots(
    store: &CollaborationStore,
    wave: &Wave,
    pinned: &target::TargetRevision,
    source: &CompositionSource,
) -> Result<(), WaveError> {
    let revision = wave.spec.revision.as_str();
    let require_root = |kind: &str, reference: &str| -> Result<(), WaveError> {
        let rooted: bool = store.read_connection().query_row(
            "SELECT EXISTS(SELECT 1 FROM local_target_roots WHERE revision=?1 AND kind=?2 AND reference=?3)",
            params![revision, kind, reference],
            |row| row.get(0),
        )?;
        if !rooted {
            return Err(WaveError::Invalid(format!(
                "composition source {kind} {reference} is not retained by its revision"
            )));
        }
        Ok(())
    };
    let candidate = source
        .composition
        .outcome
        .candidate()
        .ok_or_else(|| WaveError::Invalid("composition source has no candidate".into()))?;
    let recipe = source
        .composition
        .recipe
        .as_ref()
        .ok_or_else(|| WaveError::Invalid("composition source has no recipe".into()))?;
    require_root("object", &source.record.object.hex())?;
    require_root("object", &recipe.object.hex())?;
    require_root("snapshot", candidate.subject.as_str())?;

    let recipe_record = composer::read_recipe(store, recipe)?;
    let recipe_json: serde_json::Value = serde_json::from_slice(&recipe_record.canonical_bytes())?;
    let baseline = snapshot(
        recipe_json["baseline"]
            .as_str()
            .ok_or_else(|| WaveError::Invalid("composition recipe baseline".into()))?,
    )?;
    require_root("snapshot", baseline.as_str())?;
    for input in &candidate.inputs {
        for input_snapshot in [&input.base_snapshot, &input.result_snapshot]
            .into_iter()
            .flatten()
        {
            require_root("snapshot", input_snapshot.as_str())?;
        }
    }
    if !source.request.accepted.is_empty() {
        let accepted_baseline = snapshot(&pinned.spec.baseline)?;
        let retained = archive::retained(store, &accepted_baseline)?.ok_or(TargetError::Missing)?;
        if !source.request.accepted.contains(&retained.commit) {
            return Err(WaveError::Invalid(
                "composition source accepted history is not rooted in this revision".into(),
            ));
        }
        require_root("snapshot", accepted_baseline.as_str())?;
    }
    for contribution in source
        .request
        .catalog
        .iter()
        .filter(|item| source.request.deliveries.contains(&item.id))
    {
        let lineage = contribution
            .lineage
            .as_ref()
            .ok_or_else(|| WaveError::Invalid("composition source lineage is missing".into()))?;
        let retained =
            archive::retained_contribution(store, lineage)?.ok_or(TargetError::Missing)?;
        require_root("snapshot", retained.base.snapshot_id.as_str())?;
        require_root("snapshot", retained.result.snapshot_id.as_str())?;
    }
    Ok(())
}

fn retain_resolution_request(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    revision: &RecordId,
    request: &resolution::ResolutionRequest,
    composition_request: &CompositionRequest,
    guards: RetentionGuards<'_>,
) -> Result<(), WaveError> {
    let mut snapshots = std::collections::BTreeMap::new();
    let mut add = |snapshot: &SourceSnapshotId| {
        snapshots.insert(snapshot.to_string(), snapshot.clone());
    };
    add(&request.baseline().snapshot_id);
    add(&request.accumulator().snapshot_id);
    for member in request.members().iter().chain(request.contents()) {
        add(&member.base);
        add(&member.result);
    }
    for contribution in composition_request
        .catalog
        .iter()
        .filter(|item| composition_request.deliveries.contains(&item.id))
    {
        let lineage = contribution
            .lineage
            .as_ref()
            .ok_or_else(|| WaveError::Invalid("resolution input lineage is missing".into()))?;
        let retained =
            archive::retained_contribution(store, lineage)?.ok_or(TargetError::Missing)?;
        add(&retained.base.snapshot_id);
        add(&retained.result.snapshot_id);
    }
    let snapshots = snapshots.into_values().collect::<Vec<_>>();
    let object = request.record().object;
    let retention = target::ArtifactRetentionPermit::new(
        store,
        owner,
        guards.archive,
        guards.response,
        guards.authority,
    )?;
    target::retain_artifacts_under_guards(store, revision, &snapshots, &[object], &retention)?;
    Ok(())
}

fn attach_resolution_request(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    wave: &Wave,
    source: &CompositionSource,
    request: &resolution::RecordRef,
    version: i64,
    cancel: &AtomicBool,
) -> Result<Wave, WaveError> {
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    guard_current(&tx, owner, wave, version)?;
    // BEGIN IMMEDIATE may wait behind another writer. Cancellation is checked
    // again under that lock, at the request issuance boundary.
    if cancel.load(Ordering::SeqCst) {
        return Err(WaveError::Stale);
    }
    if tx.execute(
        "UPDATE local_waves SET resolution_request_record_id=?2, resolution_request_object=?3 WHERE wave=?1 AND target=?4 AND revision=?5 AND phase='verifying' AND candidate_snapshot=?6 AND candidate_commit=?7 AND recipe_object=?8 AND composition_source_record_id=?9 AND composition_source_object=?10 AND resolution_request_record_id IS NULL AND resolution_request_object IS NULL",
        params![
            wave.id.as_str(),
            request.id.as_str(),
            request.object.hex(),
            owner.target(),
            wave.spec.revision,
            wave.candidate_snapshot.as_ref().map(SourceSnapshotId::as_str),
            wave.candidate_commit.as_ref().map(CommitOid::as_str),
            wave.recipe_object.as_ref().map(ObjectDigest::hex),
            source.record.id.as_str(),
            source.record.object.hex(),
        ],
    )? != 1
    {
        return Err(WaveError::Stale);
    }
    tx.commit()?;
    load(store, &wave.id)
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
#[cfg(test)]
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
    let prior_completion = prior_completion::latest_for_admission(store, owner, &pinned)?;
    if let Some(key) = &prior_completion {
        let (completion, previous) = accepted_completion(store, key, i64::MAX)?;
        let same_revision = completion.revision == revision.as_str();
        if completion.target != owner.target()
            || (!same_revision && completion.source != pinned.spec.baseline)
            || previous.target != owner.target()
            || previous.issuer != owner.issuer()
        {
            return Err(WaveError::Stale);
        }
        let delivered: BTreeSet<_> = dispositions
            .iter()
            .filter(|(_, d)| **d == target::Disposition::Delivered)
            .map(|(c, _)| c.as_str())
            .collect();
        if same_revision {
            let prior_members: BTreeSet<_> = previous.selected.iter().map(String::as_str).collect();
            let catalog_members: BTreeSet<_> = pinned
                .spec
                .contributions
                .iter()
                .map(|contribution| contribution.id.as_str())
                .collect();
            if prior_members.len() != previous.selected.len()
                || prior_members != delivered
                || previous
                    .selected
                    .iter()
                    .any(|member| !catalog_members.contains(member.as_str()))
            {
                return Err(WaveError::Invalid(
                    "delivered members do not match prior accepted wave".into(),
                ));
            }
            let current_members = std::mem::take(&mut selected);
            selected = previous.selected;
            selected.extend(current_members);
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

/// Verify that a retained request still belongs to the latest eligible wave
/// before the controller prepares a charged resolution attempt. This checks
/// the request from its archived record, not from caller-provided trigger data.
///
/// This is public because the CLI controller lives in a separate crate and
/// must perform this check before charging or transitioning a resolver attempt.
pub fn require_current_resolution_request(
    store: &mut CollaborationStore,
    repo: &Path,
    owner: &ControllerOwner,
    wave_id: &RecordId,
    expected_version: i64,
    request_ref: &resolution::RecordRef,
) -> Result<(), WaveError> {
    let _archive_guard = archive_use(store)?;
    let _response_guard = crate::collaboration_sensitive::response_use(store)?;
    let _authority_guard = target::authorize(store, owner)?;
    require_current_resolution_request_under_guards(
        store,
        repo,
        owner,
        wave_id,
        expected_version,
        request_ref,
    )?;
    Ok(())
}

fn require_current_resolution_request_under_guards(
    store: &mut CollaborationStore,
    repo: &Path,
    owner: &ControllerOwner,
    wave_id: &RecordId,
    expected_version: i64,
    request_ref: &resolution::RecordRef,
) -> Result<Wave, WaveError> {
    let wave = load(store, wave_id)?;
    let state = target::status(store, owner.target())?;
    let revision = id(&wave.spec.revision)?;
    if state.version != expected_version
        || state.revision.as_ref() != Some(&revision)
        || wave.spec.target != owner.target()
        || wave.spec.issuer != owner.issuer()
        || wave.resolution_request.as_ref() != Some(request_ref)
    {
        return Err(WaveError::Stale);
    }
    let released: bool = store.read_connection().query_row(
        "SELECT released FROM local_target_revisions WHERE revision=?1",
        [revision.as_str()],
        |row| row.get(0),
    )?;
    if released {
        return Err(WaveError::Stale);
    }

    let latest_sequence: i64 = store.read_connection().query_row(
        "SELECT COALESCE(MAX(sequence), 0) FROM local_waves WHERE target=?1 AND revision=?2",
        params![owner.target(), revision.as_str()],
        |row| row.get(0),
    )?;
    if latest_sequence != wave.spec.sequence {
        return Err(WaveError::Stale);
    }

    let request = resolution::load_request(store, repo, request_ref)?;
    match (&wave.phase, request.trigger()) {
        (WavePhase::Blocked, resolution::Trigger::Conflict(conflicts))
            if !conflicts.is_empty()
                && state.phase == Phase::Idle
                && state.resume_phase.is_none() =>
        {
            let current: bool = store.read_connection().query_row(
                "SELECT EXISTS(
                    SELECT 1
                    FROM local_targets t
                    JOIN local_waves w ON w.target=t.target
                    WHERE t.target=?1 AND t.issuer=?2 AND t.active_revision=?3
                      AND t.version=?4 AND t.phase='idle' AND t.resume_phase IS NULL
                      AND w.wave=?5 AND w.revision=t.active_revision
                      AND w.phase='blocked' AND w.sequence=?6
                )",
                params![
                    owner.target(),
                    owner.issuer(),
                    revision.as_str(),
                    expected_version,
                    wave.id.as_str(),
                    wave.spec.sequence,
                ],
                |row| row.get(0),
            )?;
            if !current {
                return Err(WaveError::Stale);
            }
        }
        (WavePhase::Verifying, resolution::Trigger::FailedChecks { candidate, checks })
            if Some(candidate) == wave.candidate_snapshot.as_ref()
                && !checks.is_empty()
                && (state.phase == Phase::Verifying
                    || (state.phase == Phase::WaitingForInput
                        && state.resume_phase == Some(Phase::Verifying))) =>
        {
            guard_current(store.read_connection(), owner, &wave, expected_version)?;
        }
        _ => return Err(WaveError::Stale),
    }

    Ok(wave)
}

/// Read the exact frozen conflict binding while the caller still owns the
/// current target generation. The full archive → response → authority guard
/// lifetime covers request, wrapper, and plan decoding.
pub(crate) fn current_conflict_request_binding(
    store: &mut CollaborationStore,
    repo: &Path,
    owner: &ControllerOwner,
    wave_id: &RecordId,
    expected_version: i64,
    request_ref: &resolution::RecordRef,
) -> Result<CurrentConflictRequestBinding, WaveError> {
    let _archive_guard = archive_use(store)?;
    let _response_guard = crate::collaboration_sensitive::response_use(store)?;
    let _authority_guard = target::authorize(store, owner)?;
    let wave = require_current_resolution_request_under_guards(
        store,
        repo,
        owner,
        wave_id,
        expected_version,
        request_ref,
    )?;
    if wave.phase != WavePhase::Blocked || wave.spec.prior_completion.is_some() {
        return Err(WaveError::ResolutionUnsupported);
    }
    let source_ref = wave.composition_source.as_ref().ok_or(WaveError::Stale)?;
    validate_conflict_request_binding_records(store, repo, request_ref, source_ref, Some(&wave))
}

/// Revalidate a frozen conflict request from its exact immutable references.
/// This historical reader deliberately does not consult current wave, target,
/// owner epoch, policy, or reservation state; status-only retry identity is
/// supplied by the caller's retained proof.
pub(crate) fn read_retained_conflict_request_binding(
    store: &mut CollaborationStore,
    repo: &Path,
    request_ref: &resolution::RecordRef,
    source_ref: &resolution::RecordRef,
) -> Result<CurrentConflictRequestBinding, WaveError> {
    let _archive_guard = archive_use(store)?;
    let _response_guard = crate::collaboration_sensitive::response_use(store)?;
    validate_conflict_request_binding_records(store, repo, request_ref, source_ref, None)
}

fn validate_conflict_request_binding_records(
    store: &mut CollaborationStore,
    repo: &Path,
    request_ref: &resolution::RecordRef,
    source_ref: &resolution::RecordRef,
    expected_wave: Option<&Wave>,
) -> Result<CurrentConflictRequestBinding, WaveError> {
    let source_bytes =
        archive::read_object_bounded(store, &source_ref.object, CONFLICT_SOURCE_MAX_BYTES)?;
    let source_record = Record::decode(&source_bytes, &[&CONFLICT_SOURCE_SCHEMA])
        .map_err(|error| WaveError::Invalid(format!("conflict source record: {error}")))?;
    if source_record.id() != source_ref.id || source_record.canonical_bytes() != source_bytes {
        return Err(WaveError::Invalid("conflict source record digest".into()));
    }
    let source_payload: ConflictSourcePayload = serde_json::from_slice(&source_bytes)?;
    if source_payload.schema_name != CONFLICT_SOURCE_SCHEMA_NAME || source_payload.version != 1 {
        return Err(WaveError::Invalid("conflict source record version".into()));
    }
    let source_wave = id(&source_payload.wave)?;
    let source_revision = id(&source_payload.revision)?;
    if source_wave.as_str() != source_payload.wave
        || source_revision.as_str() != source_payload.revision
    {
        return Err(WaveError::Invalid(
            "conflict source identifier encoding".into(),
        ));
    }
    let bound_request = conflict_record_ref(&source_payload.request)?;
    let plan_ref = conflict_record_ref(&source_payload.plan)?;
    if bound_request != *request_ref {
        return Err(WaveError::Invalid(
            "conflict source request reference mismatch".into(),
        ));
    }
    if let Some(wave) = expected_wave {
        if source_payload.wave != wave.id.as_str()
            || source_payload.revision != wave.spec.revision
            || wave.resolution_request.as_ref() != Some(request_ref)
            || wave.composition_source.as_ref() != Some(source_ref)
            || wave.spec.selected.is_empty()
        {
            return Err(WaveError::Stale);
        }
    }

    let plan_bytes =
        archive::read_object_bounded(store, &plan_ref.object, CONFLICT_SOURCE_MAX_BYTES)?;
    let plan_record = Record::decode(&plan_bytes, &[&CONFLICT_PLAN_SCHEMA])
        .map_err(|error| WaveError::Invalid(format!("conflict plan record: {error}")))?;
    if plan_record.id() != plan_ref.id || plan_record.canonical_bytes() != plan_bytes {
        return Err(WaveError::Invalid("conflict plan record digest".into()));
    }
    let plan_payload: ConflictPlanPayload = serde_json::from_slice(&plan_bytes)?;
    if plan_payload.schema_name != CONFLICT_PLAN_SCHEMA_NAME || plan_payload.version != 1 {
        return Err(WaveError::Invalid("conflict plan record version".into()));
    }
    if plan_payload.prior_completion.is_some() || !plan_payload.request.accepted.is_empty() {
        return Err(WaveError::ResolutionUnsupported);
    }
    let _frozen_request = request_from_payload(&plan_payload.request)?;
    plan_payload.policy.validate()?;
    if source_payload.wave != plan_payload.wave
        || source_payload.revision != plan_payload.revision
        || plan_payload.profile != composer::PROVISIONAL_TEXT_PROFILE
        || plan_payload.engine.engine != "git merge-file"
        || plan_payload.engine.version.trim().is_empty()
        || plan_payload.engine.version.len() > 256
        || plan_payload.engine.diff.is_empty()
        || plan_payload.engine.diff.len() > 256
        || plan_payload.engine.move_window == 0
    {
        return Err(WaveError::Invalid("conflict plan source binding".into()));
    }
    let plan_wave = id(&plan_payload.wave)?;
    let plan_revision = id(&plan_payload.revision)?;
    if plan_wave.as_str() != plan_payload.wave
        || plan_revision.as_str() != plan_payload.revision
        || source_wave != plan_wave
        || source_revision != plan_revision
    {
        return Err(WaveError::Invalid(
            "conflict plan identifier binding".into(),
        ));
    }
    if let Some(wave) = expected_wave {
        if plan_payload.wave != wave.id.as_str()
            || plan_payload.revision != wave.spec.revision
            || plan_payload.selected != wave.spec.selected
            || plan_payload.policy != wave.spec.policy
        {
            return Err(WaveError::Stale);
        }
    }

    let selected: BTreeSet<&str> = plan_payload.selected.iter().map(String::as_str).collect();
    let ordered: BTreeSet<&str> = plan_payload
        .order
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();
    if plan_payload.selected.is_empty()
        || selected.len() != plan_payload.selected.len()
        || ordered.len() != plan_payload.order.len()
        || selected != ordered
        || plan_payload.request.deliveries != plan_payload.selected
    {
        return Err(WaveError::Invalid(
            "conflict plan is not the complete selected order".into(),
        ));
    }

    let mut catalog = BTreeMap::new();
    for contribution in &plan_payload.request.catalog {
        if catalog
            .insert(contribution.id.as_str(), contribution)
            .is_some()
        {
            return Err(WaveError::Invalid(
                "conflict plan catalog repeats a contribution".into(),
            ));
        }
    }
    let mut order = BTreeMap::new();
    let mut lineages = BTreeSet::new();
    for entry in &plan_payload.order {
        let contribution = catalog
            .get(entry.id.as_str())
            .ok_or_else(|| WaveError::Invalid("conflict plan order has no catalog entry".into()))?;
        if contribution.lineage.as_deref() != Some(entry.lineage.as_str())
            || !lineages.insert(entry.lineage.as_str())
        {
            return Err(WaveError::Invalid("conflict plan lineage binding".into()));
        }
        let lineage = id(&entry.lineage)?;
        let retained =
            archive::retained_contribution(store, &lineage)?.ok_or(TargetError::Missing)?;
        if retained.base.commit.as_str() != entry.base_commit
            || retained.result.commit.as_str() != entry.result_commit
            || retained.base.snapshot_id.as_str() != entry.base_snapshot
            || retained.result.snapshot_id.as_str() != entry.result_snapshot
        {
            return Err(WaveError::Invalid(
                "conflict plan retained lineage changed".into(),
            ));
        }
        order.insert(entry.id.as_str(), entry);
    }

    let request = resolution::load_request(store, repo, request_ref)?;
    let resolution::Trigger::Conflict(conflicts) = request.trigger() else {
        return Err(WaveError::Invalid(
            "frozen conflict source is not bound to a conflict request".into(),
        ));
    };
    if conflicts.is_empty()
        || request.baseline().commit.as_str() != plan_payload.request.baseline
        || request.requirements().len() != plan_payload.requirements.len()
        || conflicts.len() != plan_payload.conflicts.len()
    {
        return Err(WaveError::Invalid("conflict plan request binding".into()));
    }
    for (actual, frozen) in conflicts.iter().zip(&plan_payload.conflicts) {
        if actual.path != frozen.path
            || actual.input.as_str() != frozen.input
            || actual.reason != frozen.reason
            || !plan_payload
                .order
                .iter()
                .any(|entry| entry.result_commit == frozen.input)
        {
            return Err(WaveError::Invalid(
                "conflict plan conflict list changed".into(),
            ));
        }
    }
    for (actual, frozen) in request
        .requirements()
        .iter()
        .zip(&plan_payload.requirements)
    {
        if actual.id != frozen.id || actual.path != frozen.path {
            return Err(WaveError::Invalid(
                "conflict plan requirement list changed".into(),
            ));
        }
    }
    for member in request
        .decision_members()
        .iter()
        .chain(request.members())
        .chain(request.contents())
    {
        let frozen = order.get(member.id.as_str()).ok_or_else(|| {
            WaveError::Invalid("conflict request member is outside frozen order".into())
        })?;
        if member.lineage.as_str() != frozen.lineage
            || member.base_commit.as_str() != frozen.base_commit
            || member.result_commit.as_str() != frozen.result_commit
            || member.base.as_str() != frozen.base_snapshot
            || member.result.as_str() != frozen.result_snapshot
        {
            return Err(WaveError::Invalid(
                "conflict request member source changed".into(),
            ));
        }
    }

    Ok(CurrentConflictRequestBinding {
        request: request_ref.clone(),
        source: source_ref.clone(),
        plan: plan_ref,
        plan_payload,
    })
}

fn conflict_record_ref(payload: &RecordRefPayload) -> Result<resolution::RecordRef, WaveError> {
    let id = id(&payload.id)?;
    if id.as_str() != payload.id {
        return Err(WaveError::Invalid(
            "conflict source record id encoding".into(),
        ));
    }
    let object = archive::parse_digest(&payload.object)
        .ok_or_else(|| WaveError::Invalid("conflict source object digest".into()))?;
    if object.hex() != payload.object {
        return Err(WaveError::Invalid(
            "conflict source object digest encoding".into(),
        ));
    }
    Ok(resolution::RecordRef { id, object })
}

/// Attach the exact settled async result to its still-current wave. This only
/// links unverified source and roots its inputs; it does not settle attempts,
/// change allowance, or authorize acceptance.
// These independent authority bindings stay explicit at the public boundary.
#[allow(clippy::too_many_arguments)]
pub fn attach_synthesized_attempt_result(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    repo: &Path,
    wave_id: &RecordId,
    expected_version: i64,
    request_ref: &resolution::RecordRef,
    token: &str,
    cancel: &AtomicBool,
) -> Result<Wave, WaveError> {
    let archive_guard = archive_use(store)?;
    let response_guard = crate::collaboration_sensitive::response_use(store)?;
    let observed = load(store, wave_id)?;
    if observed.resolution_request.as_ref() != Some(request_ref)
        || observed.spec.prior_completion.is_some()
        || observed.spec.target != owner.target()
        || observed.spec.issuer != owner.issuer()
    {
        return Err(WaveError::Stale);
    }

    let synthesis = resolution::read_synthesized_attempt_result_for_token_request(
        store,
        repo,
        token,
        request_ref,
    )?;
    if synthesis.status.request != *request_ref || synthesis.status.token != token {
        return Err(WaveError::Stale);
    }
    #[cfg(test)]
    check_synthesized_attach_guards(store);
    if let Some(existing) = &observed.resolution_result {
        if existing != &synthesis.group_record || observed.phase != WavePhase::Verifying {
            return Err(WaveError::Stale);
        }
        let (_, provenance) =
            read_synthesized_wave_result(store, repo, &observed)?.ok_or(WaveError::Stale)?;
        validate_synthesized_group_roots(store, &observed, &synthesis, &provenance)?;
        if cancel.load(Ordering::SeqCst) {
            return Err(WaveError::Stale);
        }
        let _authority_guard = target::authorize(store, owner)?;
        let status = target::status(store, owner.target())?;
        let latest_sequence: i64 = store.read_connection().query_row(
            "SELECT COALESCE(MAX(sequence),0) FROM local_waves WHERE target=?1 AND revision=?2",
            params![owner.target(), observed.spec.revision],
            |row| row.get(0),
        )?;
        if Some(status.version) != expected_version.checked_add(1)
            || status.revision.as_ref().map(RecordId::as_str)
                != Some(observed.spec.revision.as_str())
            || latest_sequence != observed.spec.sequence
            || !(status.phase == Phase::Verifying
                || (status.phase == Phase::WaitingForInput
                    && status.resume_phase == Some(Phase::Verifying)))
        {
            return Err(WaveError::Stale);
        }
        return Ok(observed);
    }

    require_current_resolution_request(store, repo, owner, wave_id, expected_version, request_ref)?;
    let mut prospective = observed.clone();
    prospective.phase = WavePhase::Verifying;
    prospective.candidate_snapshot = Some(synthesis.candidate.subject.clone());
    prospective.candidate_commit = Some(synthesis.candidate.commit.clone());
    prospective.resolution_result = Some(synthesis.group_record.clone());
    let (_, provenance) =
        read_synthesized_wave_result(store, repo, &prospective)?.ok_or(WaveError::Stale)?;

    let authority_guard = target::authorize(store, owner)?;
    let pinned = target::load_revision_under_response(
        store,
        &id(&observed.spec.revision)?,
        &response_guard,
    )?;
    if pinned.target != owner.target() || pinned.issuer != owner.issuer() {
        return Err(WaveError::Stale);
    }
    let mut snapshots = BTreeMap::new();
    let mut add_snapshot = |snapshot: &SourceSnapshotId| {
        snapshots.insert(snapshot.to_string(), snapshot.clone());
    };
    add_snapshot(&synthesis.request.baseline().snapshot_id);
    add_snapshot(&synthesis.request.accumulator().snapshot_id);
    add_snapshot(&synthesis.candidate.subject);
    for member in synthesis
        .request
        .contents()
        .iter()
        .chain(synthesis.request.members())
    {
        add_snapshot(&member.base);
        add_snapshot(&member.result);
    }
    for input in &provenance.inputs {
        add_snapshot(&snapshot(&input.base_snapshot)?);
        add_snapshot(&snapshot(&input.result_snapshot)?);
    }
    let snapshots = snapshots.into_values().collect::<Vec<_>>();
    let mut objects = BTreeMap::new();
    for digest in [
        synthesis.status.request.object,
        synthesis.group_record.object,
        object(&synthesis.input_manifest.sha256)?,
    ] {
        objects.insert(digest.hex(), digest);
    }
    for entry in &synthesis.input_manifest.entries {
        if let Some(content) = &entry.sha256 {
            let digest = object(content)?;
            objects.insert(digest.hex(), digest);
        }
    }
    let objects = objects.into_values().collect::<Vec<_>>();
    let retention = target::ArtifactRetentionPermit::new(
        store,
        owner,
        &archive_guard,
        &response_guard,
        &authority_guard,
    )?;
    target::retain_artifacts_under_guards(store, &pinned.id, &snapshots, &objects, &retention)?;
    #[cfg(test)]
    check_synthesized_attach_guards(store);

    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    if cancel.load(Ordering::SeqCst) {
        return Err(WaveError::Stale);
    }
    let (version, target_phase, resume_phase, active_revision): (i64, String, Option<String>, Option<String>) = tx
        .query_row(
            "SELECT version,phase,resume_phase,active_revision FROM local_targets WHERE target=?1 AND issuer=?2",
            params![owner.target(), owner.issuer()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
    struct AttachedWaveState {
        phase: String,
        request_id: Option<String>,
        request_object: Option<String>,
        result_id: Option<String>,
        result_object: Option<String>,
        candidate: Option<String>,
        commit: Option<String>,
    }
    let attached_wave = tx.query_row(
        "SELECT phase,resolution_request_record_id,resolution_request_object,
                resolution_result_record_id,resolution_result_object,candidate_snapshot,candidate_commit
         FROM local_waves WHERE wave=?1 AND target=?2 AND revision=?3 AND sequence=?4",
        params![wave_id.as_str(), owner.target(), observed.spec.revision, observed.spec.sequence],
        |row| {
            Ok(AttachedWaveState {
                phase: row.get(0)?,
                request_id: row.get(1)?,
                request_object: row.get(2)?,
                result_id: row.get(3)?,
                result_object: row.get(4)?,
                candidate: row.get(5)?,
                commit: row.get(6)?,
            })
        },
    )?;
    if version != expected_version
        || active_revision.as_deref() != Some(observed.spec.revision.as_str())
        || attached_wave.request_id.as_deref() != Some(request_ref.id.as_str())
        || attached_wave.request_object.as_deref() != Some(request_ref.object.hex().as_str())
        || attached_wave.result_id.is_some()
        || attached_wave.result_object.is_some()
        || attached_wave.candidate.as_deref()
            != observed
                .candidate_snapshot
                .as_ref()
                .map(SourceSnapshotId::as_str)
        || attached_wave.commit.as_deref()
            != observed.candidate_commit.as_ref().map(CommitOid::as_str)
        || attached_wave.phase
            != serde_json::to_value(observed.phase)?
                .as_str()
                .unwrap_or_default()
    {
        return Err(WaveError::Stale);
    }
    let latest_sequence: i64 = tx.query_row(
        "SELECT COALESCE(MAX(sequence),0) FROM local_waves WHERE target=?1 AND revision=?2",
        params![owner.target(), observed.spec.revision],
        |row| row.get(0),
    )?;
    if latest_sequence != observed.spec.sequence {
        return Err(WaveError::Stale);
    }
    let allowed_target = match observed.phase {
        WavePhase::Blocked => target_phase == "idle" && resume_phase.is_none(),
        WavePhase::Verifying => {
            target_phase == "verifying"
                || (target_phase == "waiting_for_input"
                    && resume_phase.as_deref() == Some("verifying"))
        }
        _ => false,
    };
    if !allowed_target {
        return Err(WaveError::Stale);
    }
    if cancel.load(Ordering::SeqCst) {
        return Err(WaveError::Stale);
    }
    if tx.execute(
        "UPDATE local_waves SET phase='verifying',candidate_snapshot=?2,candidate_commit=?3,
                resolution_result_record_id=?4,resolution_result_object=?5,
                detail='synthesized candidate awaits fresh verification'
         WHERE wave=?1 AND phase=?6 AND resolution_result_record_id IS NULL
           AND resolution_result_object IS NULL AND resolution_request_record_id=?7
           AND resolution_request_object=?8",
        params![
            wave_id.as_str(),
            synthesis.candidate.subject.as_str(),
            synthesis.candidate.commit.as_str(),
            synthesis.group_record.id.as_str(),
            synthesis.group_record.object.hex(),
            attached_wave.phase,
            request_ref.id.as_str(),
            request_ref.object.hex(),
        ],
    )? != 1
    {
        return Err(WaveError::Stale);
    }
    if tx.execute(
        "UPDATE local_targets SET phase='verifying',resume_phase=NULL,version=version+1
         WHERE target=?1 AND issuer=?2 AND active_revision=?3 AND version=?4 AND phase=?5",
        params![
            owner.target(),
            owner.issuer(),
            observed.spec.revision,
            expected_version,
            target_phase,
        ],
    )? != 1
    {
        return Err(WaveError::Stale);
    }
    if observed.phase == WavePhase::Blocked {
        for selected in &observed.spec.selected {
            if tx.execute(
                "UPDATE local_target_dispositions SET status='in_wave',detail=NULL
                 WHERE revision=?1 AND contribution=?2 AND status='blocked'",
                params![observed.spec.revision, selected],
            )? != 1
            {
                return Err(WaveError::Stale);
            }
        }
    }
    if cancel.load(Ordering::SeqCst) {
        return Err(WaveError::Stale);
    }
    tx.commit()?;
    load(store, wave_id)
}

/// One durable dispatch. Replaying a completed composition returns its stored
/// candidate; a crash during dispatch is explicit, never silently retried.
pub fn compose(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    repo: &Path,
    wave_id: &RecordId,
) -> Result<Wave, WaveError> {
    Ok(compose_with_source(store, owner, repo, wave_id)?.wave)
}

/// Compose once and retain the exact immutable source needed for a later
/// failed-check request. Existing Verifying waves load that source from the
/// archive; legacy waves without it remain verifiable but cannot raise a
/// request.
pub(crate) fn compose_with_source(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    repo: &Path,
    wave_id: &RecordId,
) -> Result<ComposedWave, WaveError> {
    let observed = load(store, wave_id)?;
    if observed.phase == WavePhase::Verifying {
        let (wave, source) = load_composition_source(store, owner, repo, wave_id)?;
        return Ok(ComposedWave { wave, source });
    }
    let (wave, request, already_delivered, revision_bindings, namespace_conflict, requirements) = {
        let _archive_guard = archive_use(store)?;
        let response_guard = crate::collaboration_sensitive::response_use(store)?;
        let _authority_guard = target::authorize(store, owner)?;
        let wave = load(store, wave_id)?;
        let state = current(store, owner, &wave)?;
        if wave.phase == WavePhase::Verifying {
            // A concurrent composer completed after the initial observation.
            // Retry through the guarded source loader rather than composing
            // or reconstructing its context here.
            return Err(WaveError::OutcomeUnknown);
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
        let prior = prior_completion::load(store, owner, &wave, &pinned)?;
        let (base_commit, accepted, already_delivered) = if let Some(prior) = prior {
            prior.validate_retained_completion(store)?;
            prior.validate_git_binding(repo)?;
            (
                prior.request_baseline_commit().clone(),
                prior.accepted_history(store, &pinned)?,
                Some((prior.key().clone(), prior.previous_members().to_vec())),
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
        let requirements: Vec<RequirementRef> = pinned
            .spec
            .requirements
            .iter()
            .map(|id| RequirementRef {
                id: id.clone(),
                path: None,
            })
            .collect();
        (
            wave,
            request,
            already_delivered,
            revision_bindings,
            namespace_conflict,
            requirements,
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
    let text_profile_pins = composer::provisional_text_profile_pins();
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
    let _archive_guard = archive_use(store)?;
    let _response_guard = crate::collaboration_sensitive::response_use(store)?;
    let authority_guard = target::authorize(store, owner)?;
    let mut source_data = None;
    let mut resolution_request = None;
    let mut conflict_source = None;
    let mut conflict_retention = None;
    let (mut phase, mut detail, candidate, commit, recipe) = match result {
        Ok(composition) => match &composition.outcome {
            CompositionOutcome::Candidate(candidate) => {
                if let Some(recipe_ref) = composition.recipe.as_ref() {
                    source_data =
                        Some((request.clone(), composition.clone(), requirements.clone()));
                    (
                        "verifying",
                        "trusted verification adapter unavailable".into(),
                        Some(candidate.subject.to_string()),
                        Some(candidate.commit.as_str().to_string()),
                        Some(recipe_ref.object.hex()),
                    )
                } else {
                    (
                        "blocked",
                        "candidate has no recipe".into(),
                        None,
                        None,
                        None,
                    )
                }
            }
            CompositionOutcome::Conflict { .. } => {
                let prepared = (|| {
                    ensure_resolution_supported(store, &composition)?;
                    if wave.spec.prior_completion.is_some() || !request.accepted.is_empty() {
                        return Err(WaveError::ResolutionUnsupported);
                    }
                    let pins = text_profile_pins.as_ref().map_err(|error| {
                        WaveError::Invalid(format!("conflict profile pin capture failed: {error}"))
                    })?;
                    let plan = write_conflict_plan_record(
                        store,
                        &wave,
                        &request,
                        &composition,
                        &requirements,
                        revision_bindings.as_ref(),
                        pins,
                    )?;
                    let request_record = resolution::request_resolution(
                        store,
                        repo,
                        &request,
                        &composition,
                        &[],
                        &requirements,
                        None,
                    )?;
                    let source =
                        write_conflict_source_record(store, &wave, request_record.record(), &plan)?;
                    let mut prospective = wave.clone();
                    prospective.phase = WavePhase::Blocked;
                    prospective.resolution_request = Some(request_record.record().clone());
                    prospective.composition_source = Some(source.clone());
                    validate_conflict_request_binding_records(
                        store,
                        repo,
                        request_record.record(),
                        &source,
                        Some(&prospective),
                    )?;
                    let retention = prepare_conflict_request_retention(
                        store,
                        owner,
                        &id(&wave.spec.revision)?,
                        &request_record,
                        &request,
                        &plan,
                        &source,
                        RetentionGuards {
                            archive: &_archive_guard,
                            response: &_response_guard,
                            authority: &authority_guard,
                        },
                    )?;
                    Ok((request_record, source, retention))
                })();
                match prepared {
                    Ok((request_record, source, retention)) => {
                        resolution_request = Some(request_record.record().clone());
                        conflict_source = Some(source);
                        conflict_retention = Some(retention);
                        (
                            "blocked",
                            composition.outcome.code().into(),
                            None,
                            None,
                            None,
                        )
                    }
                    Err(error) => (
                        "blocked",
                        format!("composer conflict; frozen request binding failed: {error}"),
                        None,
                        None,
                        None,
                    ),
                }
            }
            outcome => {
                let phase = if matches!(outcome, CompositionOutcome::Unsupported { .. }) {
                    "unsupported"
                } else if matches!(outcome, CompositionOutcome::NoChange { .. }) {
                    "no_change"
                } else {
                    "blocked"
                };
                (phase, outcome.code().to_string(), None, None, None)
            }
        },
        Err(error) => ("outcome_unknown", error.to_string(), None, None, None),
    };
    let mut source = None;
    let mut composition_source = conflict_source;
    if let Some((request, composition, requirements)) = source_data {
        match write_source_record(store, &wave, &request, &composition, &requirements) {
            Ok(reference) => {
                let retained_source = CompositionSource {
                    record: reference.clone(),
                    request,
                    composition,
                    requirements,
                };
                let validate = (|| {
                    let mut source_wave = wave.clone();
                    source_wave.phase = WavePhase::Verifying;
                    source_wave.candidate_snapshot = retained_source
                        .composition
                        .outcome
                        .candidate()
                        .map(|candidate| candidate.subject.clone());
                    source_wave.candidate_commit = retained_source
                        .composition
                        .outcome
                        .candidate()
                        .map(|candidate| candidate.commit.clone());
                    source_wave.recipe_object = retained_source
                        .composition
                        .recipe
                        .as_ref()
                        .map(|recipe| recipe.object);
                    source_wave.composition_source = Some(reference.clone());
                    let revision = id(&wave.spec.revision)?;
                    let pinned =
                        target::load_revision_under_response(store, &revision, &_response_guard)?;
                    let payload = source_payload(
                        &source_wave,
                        &retained_source.request,
                        &retained_source.composition,
                        &retained_source.requirements,
                    )?;
                    validate_source_context(
                        store,
                        repo,
                        owner,
                        &source_wave,
                        &pinned,
                        &payload,
                        &retained_source,
                    )?;
                    retain_composition_source(
                        store,
                        owner,
                        &revision,
                        &reference,
                        &retained_source,
                        &pinned,
                        RetentionGuards {
                            archive: &_archive_guard,
                            response: &_response_guard,
                            authority: &authority_guard,
                        },
                    )
                })();
                match validate {
                    Ok(()) => {
                        composition_source = Some(reference);
                        source = Some(retained_source);
                    }
                    Err(error) => {
                        phase = "blocked";
                        detail = format!("candidate composition source retention failed: {error}");
                    }
                }
            }
            Err(error) => {
                phase = "blocked";
                detail = format!("candidate composition source persistence failed: {error}");
            }
        }
    }
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
    if let Some(retention) = conflict_retention {
        if retention.revision.as_str() != wave.spec.revision {
            return Err(WaveError::Stale);
        }
        let released: bool = tx.query_row(
            "SELECT released FROM local_target_revisions WHERE revision=?1",
            [retention.revision.as_str()],
            |row| row.get(0),
        )?;
        if released {
            return Err(WaveError::Stale);
        }
        for snapshot in &retention.snapshots {
            tx.execute(
                "INSERT OR IGNORE INTO local_target_roots VALUES (?1, 'snapshot', ?2)",
                params![retention.revision.as_str(), snapshot.as_str()],
            )?;
        }
        for contribution in &retention.contributions {
            tx.execute(
                "INSERT OR IGNORE INTO local_target_roots VALUES (?1, 'contribution', ?2)",
                params![retention.revision.as_str(), contribution.as_str()],
            )?;
        }
        for object in &retention.objects {
            tx.execute(
                "INSERT OR IGNORE INTO local_target_roots VALUES (?1, 'object', ?2)",
                params![retention.revision.as_str(), object.hex()],
            )?;
        }
    }
    if tx.execute("UPDATE local_waves SET phase=?2, detail=?3, candidate_snapshot=?4, candidate_commit=?5, recipe_object=?6, composition_ms=?7, resolution_request_record_id=?8, resolution_request_object=?9, composition_source_record_id=?10, composition_source_object=?11 WHERE wave=?1 AND phase='composing'", params![wave_id.as_str(),phase,detail,candidate,commit,recipe,elapsed,resolution_request.as_ref().map(|record| record.id.as_str()),resolution_request.as_ref().map(|record| record.object.hex()),composition_source.as_ref().map(|record| record.id.as_str()),composition_source.as_ref().map(|record| record.object.hex())])? != 1 {
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
    let wave = load(store, wave_id)?;
    Ok(ComposedWave { wave, source })
}

/// Attach one request to the exact current candidate wave. The request and
/// every source snapshot are rooted before the guarded row update; a stale
/// update can leave only harmless over-retention, never a discoverable stale
/// request.
pub(crate) fn raise_resolution_for_failed_checks(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    repo: &Path,
    wave_id: &RecordId,
    source: &CompositionSource,
    checks: &[FailedCheck],
    cancel: &AtomicBool,
) -> Result<Wave, WaveError> {
    if checks.is_empty() {
        return Err(WaveError::Invalid(
            "resolution request has no failed checks".into(),
        ));
    }
    let _archive_guard = archive_use(store)?;
    let response_guard = crate::collaboration_sensitive::response_use(store)?;
    let authority_guard = target::authorize(store, owner)?;
    if cancel.load(Ordering::SeqCst) {
        return Err(WaveError::Stale);
    }
    let wave = load(store, wave_id)?;
    let state = current(store, owner, &wave)?;
    if wave.phase != WavePhase::Verifying
        || wave.composition_source.as_ref() != Some(&source.record)
    {
        return Err(WaveError::Stale);
    }
    if wave.resolution_request.is_some() {
        return Ok(wave);
    }
    let revision = id(&wave.spec.revision)?;
    let pinned = target::load_revision_under_response(store, &revision, &response_guard)?;
    let payload = source_payload(
        &wave,
        &source.request,
        &source.composition,
        &source.requirements,
    )?;
    validate_source_context(store, repo, owner, &wave, &pinned, &payload, source)?;
    ensure_resolution_supported(store, &source.composition)?;
    let request = resolution::request_resolution(
        store,
        repo,
        &source.request,
        &source.composition,
        checks,
        &source.requirements,
        None,
    )?;
    match request.trigger() {
        resolution::Trigger::FailedChecks { candidate, checks }
            if Some(candidate) == wave.candidate_snapshot.as_ref() && !checks.is_empty() => {}
        _ => {
            return Err(WaveError::Invalid(
                "request is not bound to failed checks on this candidate".into(),
            ));
        }
    }
    let request_record = request.record();
    let decoded = resolution::read_record(store, request_record)?;
    if decoded.id() != request_record.id {
        return Err(WaveError::Invalid(
            "resolution request record identity".into(),
        ));
    }
    retain_resolution_request(
        store,
        owner,
        &revision,
        &request,
        &source.request,
        RetentionGuards {
            archive: &_archive_guard,
            response: &response_guard,
            authority: &authority_guard,
        },
    )?;
    if cancel.load(Ordering::SeqCst) {
        return Err(WaveError::Stale);
    }
    attach_resolution_request(
        store,
        owner,
        &wave,
        source,
        request_record,
        state.version,
        cancel,
    )
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
