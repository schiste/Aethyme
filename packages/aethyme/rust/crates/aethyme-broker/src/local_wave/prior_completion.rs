//! Exact private-completion bindings used when a later Local wave builds on
//! an accepted source. This module never makes an unverified synthesis an
//! accepted baseline; it only reconstructs the prior request and provenance
//! bindings that callers must validate before dispatch or gate verification.

use std::collections::BTreeSet;
use std::path::Path;

use aethyme_contracts::experimental_v0::{Record, RecordId, SourceSnapshotId};
use rusqlite::OptionalExtension;
use serde_json::Value;

use crate::collaboration_archive::{self as archive, CommitOid};
use crate::collaboration_state::CollaborationStore;
use crate::composer::{self, CompositionBudget, CompositionRequest};
use crate::composition::CompositionMode;
use crate::local_acceptance::{HistoricalCompletionIdentity, verify_historical_completion};
use crate::local_controller::CompletionSpec;
use crate::local_target::{self, ControllerOwner, TargetRevision};
use crate::resolution::{RequirementRef, ResolutionRequest};

use super::{Wave, WaveError, WaveSpec, accepted_completion, id, snapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RevisionRelation {
    Same,
    Cross,
}

/// The accepted source that a frozen wave is allowed to build on. The
/// completion's accepted commit is the request baseline; its source snapshot
/// is the retained identity used by the Local wave and resolver request.
#[derive(Debug, Clone)]
pub(crate) struct Binding {
    key: RecordId,
    completion: CompletionSpec,
    previous: WaveSpec,
    relation: RevisionRelation,
    source: SourceSnapshotId,
    accepted_commit: CommitOid,
}

pub(crate) fn load(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    wave: &Wave,
    pinned: &TargetRevision,
) -> Result<Option<Binding>, WaveError> {
    if wave.spec.target != owner.target() || wave.spec.issuer != owner.issuer() {
        return Err(WaveError::Stale);
    }
    load_for_identity(store, wave, pinned, owner.target(), owner.issuer())
}

/// Validate a retained wave binding without claiming that its historical
/// completion confers current authority. Callers must separately authorize
/// any present owner action.
pub(crate) fn load_retained(
    store: &CollaborationStore,
    wave: &Wave,
    pinned: &TargetRevision,
) -> Result<Option<Binding>, WaveError> {
    load_for_identity(store, wave, pinned, &wave.spec.target, &wave.spec.issuer)
}

fn load_for_identity(
    store: &CollaborationStore,
    wave: &Wave,
    pinned: &TargetRevision,
    target: &str,
    issuer: &str,
) -> Result<Option<Binding>, WaveError> {
    let Some(raw_key) = wave.spec.prior_completion.as_deref() else {
        return Ok(None);
    };
    if wave.spec.target != target
        || wave.spec.issuer != issuer
        || pinned.target != target
        || pinned.issuer != issuer
        || pinned.id.as_str() != wave.spec.revision
    {
        return Err(WaveError::Stale);
    }

    let key = id(raw_key)?;
    let (completion, previous) = accepted_completion(store, &key, wave.spec.sequence)?;
    if completion.target != target
        || previous.target != wave.spec.target
        || previous.issuer != issuer
        || previous.revision != completion.revision
    {
        return Err(WaveError::Stale);
    }
    let historical = historical_identity(&key, &completion, issuer);
    verify_historical_completion(store, &historical).map_err(|_| WaveError::Stale)?;
    verify_historical_source(store, &key, &completion, &previous)?;

    let relation = classify_revision_relation(
        &completion.revision,
        &wave.spec.revision,
        &completion.source,
        &wave.spec.baseline,
        &pinned.spec.baseline,
        &previous.selected,
        &wave.spec.selected,
    )?;
    let source = snapshot(&completion.source)?;
    let accepted_commit = CommitOid::parse(&completion.accepted_commit)?;
    if completion.accepted_tree != completion.candidate_tree {
        return Err(WaveError::Invalid(
            "prior completion accepted tree differs from its candidate tree".into(),
        ));
    }

    Ok(Some(Binding {
        key,
        completion,
        previous,
        relation,
        source,
        accepted_commit,
    }))
}

fn historical_identity(
    key: &RecordId,
    completion: &CompletionSpec,
    issuer: &str,
) -> HistoricalCompletionIdentity {
    HistoricalCompletionIdentity {
        completion: key.as_str().to_owned(),
        target: completion.target.clone(),
        issuer: issuer.to_owned(),
        revision: completion.revision.clone(),
        epoch: completion.epoch.clone(),
        wave: completion.wave.clone(),
        source: completion.source.clone(),
        candidate_commit: completion.candidate_commit.clone(),
        candidate_tree: completion.candidate_tree.clone(),
        qualification: completion.qualification.clone(),
        receipt: completion.acceptance_receipt.clone(),
        accepted_commit: completion.accepted_commit.clone(),
        accepted_tree: completion.accepted_tree.clone(),
    }
}

/// Select the newest proof-verified completion for this exact target issuer.
/// Same-revision history is preferred; cross-revision reuse is considered
/// only when its accepted source is the new revision's explicit baseline.
/// A matching but damaged newest proof is an error, never a reason to fall
/// back to an older completion.
pub(super) fn latest_for_admission(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    pinned: &TargetRevision,
) -> Result<Option<RecordId>, WaveError> {
    if pinned.target != owner.target() || pinned.issuer != owner.issuer() {
        return Err(WaveError::Stale);
    }
    let find = |revision: Option<&str>| -> Result<Option<RecordId>, WaveError> {
        let connection = store.read_connection();
        let sql = if revision.is_some() {
            "SELECT c.id FROM local_controller_completions c
             JOIN local_waves w ON w.wave=c.wave
             JOIN local_targets t ON t.target=c.target
             WHERE c.target=?1 AND w.target=?1 AND w.phase='accepted'
               AND w.revision=?3
               AND t.issuer=?2
             ORDER BY w.sequence DESC,c.id DESC LIMIT 1"
        } else {
            "SELECT c.id FROM local_controller_completions c
             JOIN local_waves w ON w.wave=c.wave
             JOIN local_targets t ON t.target=c.target
             WHERE c.target=?1 AND w.target=?1 AND w.phase='accepted'
               AND w.revision<>?4 AND w.candidate_snapshot=?3
               AND t.issuer=?2
             ORDER BY w.sequence DESC,c.id DESC LIMIT 1"
        };
        let raw: Option<String> = if let Some(revision) = revision {
            connection
                .query_row(
                    sql,
                    rusqlite::params![owner.target(), owner.issuer(), revision],
                    |row| row.get(0),
                )
                .optional()?
        } else {
            connection
                .query_row(
                    sql,
                    rusqlite::params![
                        owner.target(),
                        owner.issuer(),
                        pinned.spec.baseline,
                        pinned.id.as_str(),
                    ],
                    |row| row.get(0),
                )
                .optional()?
        };
        raw.map(|raw| id(&raw)).transpose()
    };

    // Same-revision cumulative history wins. Only if none exists do we
    // consider a prior revision whose accepted source is this revision's
    // explicit baseline. Each query selects one newest candidate; a damaged
    // newest proof fails closed instead of falling back to an older row.
    let key = match find(Some(pinned.id.as_str()))? {
        Some(key) => Some(key),
        None => find(None)?,
    };
    let Some(key) = key else {
        return Ok(None);
    };
    let (completion, previous) = accepted_completion(store, &key, i64::MAX)?;
    if completion.target != owner.target()
        || previous.target != owner.target()
        || previous.issuer != owner.issuer()
        || (completion.revision != pinned.id.as_str() && completion.source != pinned.spec.baseline)
    {
        return Err(WaveError::Stale);
    }
    verify_historical_completion(
        store,
        &historical_identity(&key, &completion, owner.issuer()),
    )
    .map_err(|_| WaveError::Stale)?;
    verify_historical_source(store, &key, &completion, &previous)?;
    Ok(Some(key))
}

const MAX_HISTORICAL_COMPLETION_DEPTH: usize = 32;

/// Validate the immutable composition source for an accepted completion.
/// This is provenance only: it never authorizes a current controller action.
fn verify_historical_source(
    store: &CollaborationStore,
    key: &RecordId,
    completion: &CompletionSpec,
    wave_spec: &WaveSpec,
) -> Result<(), WaveError> {
    let _archive = crate::collaboration_gc::archive_use(store)
        .map_err(|error| WaveError::Invalid(format!("archive retention: {error}")))?;
    let response = crate::collaboration_sensitive::response_use(store)?;
    let mut seen = BTreeSet::new();
    verify_historical_source_inner(store, &response, key, completion, wave_spec, &mut seen, 0)
}

fn verify_historical_source_inner(
    store: &CollaborationStore,
    response: &crate::collaboration_sensitive::ResponseUse,
    key: &RecordId,
    completion: &CompletionSpec,
    wave_spec: &WaveSpec,
    seen: &mut BTreeSet<String>,
    depth: usize,
) -> Result<(), WaveError> {
    if depth >= MAX_HISTORICAL_COMPLETION_DEPTH || !seen.insert(key.to_string()) {
        return Err(WaveError::Invalid(
            "historical completion source chain is cyclic or too deep".into(),
        ));
    }
    let result = (|| {
        let row: (
            Vec<u8>,
            String,
            Option<String>,
            i64,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        ) = store.read_connection().query_row(
            "SELECT body,phase,detail,sequence,target,revision,candidate_snapshot,
                    candidate_commit,recipe_object,composition_source_record_id,
                    composition_source_object,resolution_request_record_id,
                    resolution_request_object,resolution_result_record_id,
                    resolution_result_object
             FROM local_waves WHERE wave=?1",
            [&completion.wave],
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
                    row.get(12)?,
                    row.get(13)?,
                    row.get(14)?,
                ))
            },
        )?;
        let record = Record::decode(&row.0, &[&super::SCHEMA])
            .map_err(|error| WaveError::Invalid(format!("historical wave record: {error}")))?;
        let body: Value = serde_json::from_slice(&row.0)?;
        let stored_spec: WaveSpec = serde_json::from_value(body["spec"].clone())?;
        if record.id().as_str() != completion.wave
            || record.canonical_bytes() != row.0
            || row.1 != "accepted"
            || row.2.as_deref() != Some(key.as_str())
            || row.3 != wave_spec.sequence
            || row.4 != completion.target
            || row.5 != completion.revision
            || stored_spec != *wave_spec
            || row.6.as_deref() != Some(completion.source.as_str())
            || row.7.as_deref() != Some(completion.candidate_commit.as_str())
        {
            return Err(WaveError::Invalid(
                "historical accepted wave source projection".into(),
            ));
        }
        let source_ref = super::record_ref(row.9.as_deref(), row.10.as_deref())?
            .ok_or_else(|| WaveError::Invalid("accepted wave has no composition source".into()))?;
        let request_ref = super::record_ref(row.11.as_deref(), row.12.as_deref())?;
        let result_ref = super::record_ref(row.13.as_deref(), row.14.as_deref())?;
        let historical_synthesis = match (request_ref.as_ref(), result_ref.as_ref()) {
            (None, None) => None,
            (Some(request), Some(result)) => {
                let proof = crate::resolution::read_retained_synthesized_result_for_refs(
                    store, request, result,
                )?;
                if proof.request.baseline().snapshot_id.as_str() != wave_spec.baseline
                    || proof.group_record != *result
                {
                    return Err(WaveError::Invalid(
                        "historical synthesized baseline or result differs from its wave".into(),
                    ));
                }
                for reference in [request.object.hex(), result.object.hex()] {
                    require_completion_root(store, key, "object", &reference)?;
                }
                require_completion_root(store, key, "object", &proof.input_manifest.sha256)?;
                require_completion_root(store, key, "snapshot", proof.candidate_subject.as_str())?;
                for digest in proof
                    .input_manifest
                    .entries
                    .iter()
                    .filter_map(|entry| entry.sha256.as_deref())
                {
                    require_completion_root(store, key, "object", digest)?;
                }
                Some(proof)
            }
            _ => {
                return Err(WaveError::Invalid(
                    "historical wave has an incomplete synthesized request/result pair".into(),
                ));
            }
        };

        let recipe_object = row
            .8
            .as_deref()
            .map(super::object)
            .transpose()?
            .ok_or_else(|| {
                WaveError::Invalid("accepted wave has no composition recipe object".into())
            })?;
        let source_bytes = archive::read_object_bounded(
            store,
            &source_ref.object,
            super::COMPOSITION_SOURCE_MAX_BYTES,
        )?;
        let source_record = Record::decode(&source_bytes, &[&super::COMPOSITION_SOURCE_SCHEMA])
            .map_err(|error| WaveError::Invalid(format!("composition source record: {error}")))?;
        if source_record.id() != source_ref.id || source_record.canonical_bytes() != source_bytes {
            return Err(WaveError::Invalid(
                "historical composition source digest".into(),
            ));
        }
        let payload: super::CompositionSourcePayload = serde_json::from_slice(&source_bytes)?;
        let source = super::source_from_payload(&payload, &source_ref)?;
        let candidate = source
            .composition
            .outcome
            .candidate()
            .ok_or_else(|| WaveError::Invalid("historical source has no candidate".into()))?;
        let recipe = source
            .composition
            .recipe
            .as_ref()
            .ok_or_else(|| WaveError::Invalid("historical source has no recipe".into()))?;
        if payload.wave != completion.wave
            || payload.revision != completion.revision
            || payload.profile != composer::PROVISIONAL_TEXT_PROFILE
            || source.record != source_ref
            || recipe.object != recipe_object
            || candidate.baseline_source != composer::COMPOSER_BASELINE_SOURCE
            || candidate.baseline != source.request.baseline
        {
            return Err(WaveError::Invalid(
                "historical composition source candidate binding".into(),
            ));
        }

        let revision_id = super::id(&completion.revision)?;
        let revision =
            local_target::load_revision_history_under_response(store, &revision_id, response)?;
        if revision.target != completion.target
            || revision.issuer != wave_spec.issuer
            || wave_spec.target != completion.target
            || wave_spec.revision != completion.revision
            || wave_spec.selected.is_empty()
            || wave_spec.selected.iter().collect::<BTreeSet<_>>().len() != wave_spec.selected.len()
            || wave_spec
                .selected
                .iter()
                .any(|member| !revision.spec.selected.contains(member))
        {
            return Err(WaveError::Invalid(
                "historical source revision binding".into(),
            ));
        }
        let (released, revision_body): (bool, Vec<u8>) = store.read_connection().query_row(
            "SELECT released,body FROM local_target_revisions WHERE revision=?1",
            [revision_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let revision_json: Value = serde_json::from_slice(&revision_body)?;
        if released && revision_json.get("revision_bindings").is_some() {
            return Err(WaveError::ResolutionUnsupported);
        }
        let revision_ids = if released {
            None
        } else {
            local_target::load_revision_under_response(store, &revision_id, response)?
                .revision_bindings
                .map(|bindings| bindings.revision_ids())
                .transpose()?
        };

        let revision_baseline = revision.spec.baseline.as_str();
        let baseline_snapshot = super::snapshot(&wave_spec.baseline)?;
        let baseline = archive::retained(store, &baseline_snapshot)?
            .ok_or(crate::local_target::TargetError::Missing)?;
        let (expected_commit, expected_accepted, expected_profile, expected_prior) =
            if let Some(prior_text) = wave_spec.prior_completion.as_deref() {
                let prior_key = super::id(prior_text)?;
                if prior_key == *key {
                    return Err(WaveError::Invalid("historical completion cycle".into()));
                }
                let (prior_completion, prior_wave) =
                    super::accepted_completion(store, &prior_key, wave_spec.sequence)?;
                if prior_completion.target != completion.target
                    || prior_wave.issuer != wave_spec.issuer
                {
                    return Err(WaveError::Stale);
                }
                verify_historical_completion(
                    store,
                    &historical_identity(&prior_key, &prior_completion, &prior_wave.issuer),
                )
                .map_err(|_| WaveError::Stale)?;
                verify_historical_source_inner(
                    store,
                    response,
                    &prior_key,
                    &prior_completion,
                    &prior_wave,
                    seen,
                    depth + 1,
                )?;
                let relation = classify_revision_relation(
                    &prior_completion.revision,
                    &wave_spec.revision,
                    &prior_completion.source,
                    &wave_spec.baseline,
                    revision_baseline,
                    &prior_wave.selected,
                    &wave_spec.selected,
                )?;
                let accepted = if relation == RevisionRelation::Same {
                    let original_baseline_snapshot = snapshot(revision_baseline)?;
                    require_completion_root(
                        store,
                        key,
                        "snapshot",
                        original_baseline_snapshot.as_str(),
                    )?;
                    let original_baseline = archive::retained(store, &original_baseline_snapshot)?
                        .ok_or(crate::local_target::TargetError::Missing)?;
                    vec![original_baseline.commit]
                } else {
                    Vec::new()
                };
                let members = if relation == RevisionRelation::Same {
                    prior_wave.selected.clone()
                } else {
                    Vec::new()
                };
                (
                    CommitOid::parse(&prior_completion.accepted_commit)?,
                    accepted,
                    super::ACCEPTED_BASELINE_TEXT_PROFILE,
                    Some(serde_json::json!({
                        "completion": prior_key.as_str(),
                        "members": members,
                    })),
                )
            } else {
                if wave_spec.baseline != revision_baseline {
                    return Err(WaveError::Invalid(
                        "historical initial wave differs from its revision baseline".into(),
                    ));
                }
                (
                    baseline.commit.clone(),
                    Vec::new(),
                    composer::PROVISIONAL_TEXT_PROFILE,
                    None,
                )
            };

        let expected_catalog = revision.spec.catalog()?;
        let expected_request = CompositionRequest {
            baseline: expected_commit.clone(),
            accepted: expected_accepted.clone(),
            catalog: expected_catalog,
            deliveries: wave_spec.selected.clone(),
            budget: CompositionBudget {
                max_contributions: wave_spec.policy.max_group,
                ..CompositionBudget::default()
            },
        };
        if source.request.baseline != expected_request.baseline
            || source.request.accepted != expected_request.accepted
            || source.request.catalog != expected_request.catalog
            || source.request.deliveries != expected_request.deliveries
            || source.request.budget != expected_request.budget
            || source.requirements
                != revision
                    .spec
                    .requirements
                    .iter()
                    .map(|id| RequirementRef {
                        id: id.clone(),
                        path: None,
                    })
                    .collect::<Vec<_>>()
            || payload.requirements != super::requirement_payloads(&source.requirements)
            || candidate.producer.profile() != expected_profile
            || recipe_json_acceptance_baseline(&recipe.object, store)? != expected_prior
        {
            return Err(WaveError::Invalid(
                "historical source request, requirements, or prior binding".into(),
            ));
        }

        let recipe_bytes = archive::read_object_bounded(
            store,
            &recipe.object,
            super::COMPOSITION_SOURCE_MAX_BYTES,
        )?;
        let recipe_record = Record::decode(&recipe_bytes, &[&composer::COMPOSITION_RECIPE_SCHEMA])
            .map_err(|error| WaveError::Invalid(format!("composition recipe: {error}")))?;
        if recipe_record.id() != recipe.id || recipe_record.canonical_bytes() != recipe_bytes {
            return Err(WaveError::Invalid(
                "historical composition recipe digest".into(),
            ));
        }
        let recipe_json: Value = serde_json::from_slice(&recipe_bytes)?;
        let seeded_recipe = recipe_json.get("seeded_conflict").is_some();
        match (historical_synthesis.as_ref(), seeded_recipe) {
            (None, false) => {
                if candidate.mode != CompositionMode::Text
                    || candidate.subject.as_str() != completion.source
                    || candidate.commit.as_str() != completion.candidate_commit
                    || candidate.tree != completion.candidate_tree
                {
                    return Err(WaveError::Invalid(
                        "historical direct composition candidate binding".into(),
                    ));
                }
            }
            (Some(proof), false) => {
                // Direct synthesis leaves the original Text source and recipe
                // intact. The accepted candidate is the settled resolver
                // output, not the candidate in that original composition.
                if candidate.mode != CompositionMode::Text
                    || proof.candidate_subject.as_str() != completion.source
                    || proof.candidate_commit.as_str() != completion.candidate_commit
                {
                    return Err(WaveError::Invalid(
                        "historical direct synthesized candidate binding".into(),
                    ));
                }
            }
            (Some(_), true) => {
                if candidate.mode != CompositionMode::Hybrid
                    || candidate.subject.as_str() != completion.source
                    || candidate.commit.as_str() != completion.candidate_commit
                    || candidate.tree != completion.candidate_tree
                {
                    return Err(WaveError::Invalid(
                        "historical seeded hybrid candidate binding".into(),
                    ));
                }
            }
            (None, true) => {
                return Err(WaveError::Invalid(
                    "historical seeded recipe has no synthesized result".into(),
                ));
            }
        }
        if recipe_json["candidate"].as_str() != Some(candidate.subject.as_str())
            || recipe_json["candidate_commit"].as_str() != Some(candidate.commit.as_str())
            || recipe_json["baseline"].as_str() != Some(wave_spec.baseline.as_str())
            || recipe_json["baseline_commit"].as_str() != Some(expected_commit.as_str())
            || recipe_json["profile"].as_str() != Some(expected_profile)
            || (seeded_recipe && historical_synthesis.is_none())
        {
            return Err(WaveError::Invalid(
                "historical composition recipe binding".into(),
            ));
        }
        if let (Some(seeded), Some(proof), Some(request), Some(result)) = (
            recipe_json.get("seeded_conflict"),
            historical_synthesis.as_ref(),
            request_ref.as_ref(),
            result_ref.as_ref(),
        ) {
            let seeded_request = super::seeded_recipe_record_ref(seeded, "resolution_request")?;
            let seeded_source = super::seeded_recipe_record_ref(seeded, "composition_source")?;
            let seeded_plan = super::seeded_recipe_record_ref(seeded, "composition_plan")?;
            let seeded_result = super::seeded_recipe_record_ref(seeded, "resolution_result")?;
            let prefix_len = proof
                .request
                .contents()
                .len()
                .checked_add(1)
                .ok_or_else(|| {
                    WaveError::Invalid("historical seed prefix length overflow".into())
                })?;
            let order = seeded["order"]
                .as_array()
                .ok_or_else(|| WaveError::Invalid("historical seeded order".into()))?;
            let suffix = seeded["suffix"]
                .as_array()
                .ok_or_else(|| WaveError::Invalid("historical seeded suffix".into()))?;
            let expected_suffix = order
                .iter()
                .skip(prefix_len)
                .map(|entry| {
                    entry["id"]
                        .as_str()
                        .map(Value::from)
                        .ok_or_else(|| WaveError::Invalid("historical seeded order id".into()))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let seed_candidate = &seeded["seed_candidate"];
            let seed_tree = seed_candidate["tree"]
                .as_str()
                .ok_or_else(|| WaveError::Invalid("historical seed tree".into()))?;
            if seeded_request != *request
                || seeded_result != *result
                || seeded["schema"].as_str()
                    != Some(composer::SEEDED_CONFLICT_RECIPE_SCHEMA_NAME)
                || seeded["version"].as_u64() != Some(1)
                || seeded["trust"].as_str() != Some("unverified")
                || seeded["input_manifest_sha256"].as_str()
                    != Some(proof.input_manifest.sha256.as_str())
                || seed_candidate["subject"].as_str() != Some(proof.candidate_subject.as_str())
                || seed_candidate["commit"].as_str() != Some(proof.candidate_commit.as_str())
                || seed_tree.is_empty()
                || seeded["prefix_len"].as_u64() != Some(prefix_len as u64)
                || seeded["selected"] != serde_json::to_value(&wave_spec.selected)?
                || suffix != &expected_suffix
                || order.len() != wave_spec.selected.len()
                || order
                    .iter()
                    .zip(&wave_spec.selected)
                    .any(|(entry, selected)| entry["id"].as_str() != Some(selected.as_str()))
            {
                return Err(WaveError::Invalid(
                    "historical seeded recipe does not match its settled prefix and suffix".into(),
                ));
            }
            for reference in [seeded_source.object.hex(), seeded_plan.object.hex()] {
                require_completion_root(store, key, "object", &reference)?;
            }
        }
        let usage = source.composition.usage;
        let budget = source.request.budget;
        if usage.contributions > budget.max_contributions
            || usage.paths > budget.max_paths
            || usage.bytes > budget.max_bytes
            || usage.merges > budget.max_merges
            || recipe_json["deliveries"].as_u64() != Some(source.request.deliveries.len() as u64)
            || recipe_json["budget"]
                != serde_json::json!({
                    "limits": {
                        "contributions": budget.max_contributions,
                        "paths": budget.max_paths,
                        "bytes": budget.max_bytes.to_string(),
                        "merges": budget.max_merges,
                    },
                    "used": {
                        "contributions": usage.contributions,
                        "paths": usage.paths,
                        "bytes": usage.bytes.to_string(),
                        "merges": usage.merges,
                    },
                })
        {
            return Err(WaveError::Invalid(
                "historical composition budget binding".into(),
            ));
        }

        let order = recipe_json["order"]
            .as_array()
            .ok_or_else(|| WaveError::Invalid("historical recipe order".into()))?;
        let selected: BTreeSet<&str> = wave_spec.selected.iter().map(String::as_str).collect();
        let ordered: BTreeSet<&str> = source
            .composition
            .order
            .iter()
            .map(String::as_str)
            .collect();
        if source.composition.order.len() != selected.len()
            || ordered.len() != source.composition.order.len()
            || ordered != selected
            || order.len() != source.composition.order.len()
            || candidate.inputs.len() != order.len()
        {
            return Err(WaveError::Invalid(
                "historical recipe does not cover the full selected order".into(),
            ));
        }
        for ((entry, contribution_id), input) in order
            .iter()
            .zip(&source.composition.order)
            .zip(&candidate.inputs)
        {
            let contribution = source
                .request
                .catalog
                .iter()
                .find(|item| item.id == *contribution_id)
                .ok_or_else(|| WaveError::Invalid("historical recipe contribution".into()))?;
            let lineage = contribution
                .lineage
                .as_ref()
                .ok_or_else(|| WaveError::Invalid("historical recipe lineage".into()))?;
            let retained = archive::retained_contribution(store, lineage)?
                .ok_or(crate::local_target::TargetError::Missing)?;
            let mut expected_entry = serde_json::json!({
                "id": contribution_id,
                "lineage": lineage.as_str(),
                "base": retained.base.snapshot_id.as_str(),
                "result": retained.result.snapshot_id.as_str(),
            });
            if let Some(revision_id) = revision_ids
                .as_ref()
                .and_then(|ids| ids.get(contribution_id))
            {
                expected_entry["revision"] = Value::String(revision_id.as_str().to_owned());
            }
            if entry != &expected_entry
                || input.base != retained.base.commit
                || input.result != retained.result.commit
                || input.base_snapshot.as_ref() != Some(&retained.base.snapshot_id)
                || input.result_snapshot.as_ref() != Some(&retained.result.snapshot_id)
            {
                return Err(WaveError::Invalid(
                    "historical recipe input differs from its retained lineage".into(),
                ));
            }
            require_completion_root(store, key, "contribution", lineage.as_str())?;
            require_completion_root(store, key, "snapshot", retained.base.snapshot_id.as_str())?;
            require_completion_root(store, key, "snapshot", retained.result.snapshot_id.as_str())?;
        }
        require_completion_root(store, key, "object", source_ref.object.hex().as_str())?;
        require_completion_root(store, key, "object", recipe.object.hex().as_str())?;
        require_completion_root(store, key, "snapshot", candidate.subject.as_str())?;
        require_completion_root(store, key, "snapshot", baseline_snapshot.as_str())?;
        archive::retained(store, &baseline_snapshot)?
            .ok_or(crate::local_target::TargetError::Missing)?;
        archive::retained(store, &candidate.subject)?
            .ok_or(crate::local_target::TargetError::Missing)?;
        Ok(())
    })();
    seen.remove(key.as_str());
    result
}

fn recipe_json_acceptance_baseline(
    recipe_object: &archive::ObjectDigest,
    store: &CollaborationStore,
) -> Result<Option<Value>, WaveError> {
    let bytes =
        archive::read_object_bounded(store, recipe_object, super::COMPOSITION_SOURCE_MAX_BYTES)?;
    let record = Record::decode(&bytes, &[&composer::COMPOSITION_RECIPE_SCHEMA])
        .map_err(|error| WaveError::Invalid(format!("composition recipe: {error}")))?;
    if record.canonical_bytes() != bytes {
        return Err(WaveError::Invalid(
            "historical composition recipe digest".into(),
        ));
    }
    let value: Value = serde_json::from_slice(&bytes)?;
    Ok(value.get("accepted_baseline").cloned())
}

fn require_completion_root(
    store: &CollaborationStore,
    key: &RecordId,
    kind: &str,
    reference: &str,
) -> Result<(), WaveError> {
    let rooted: bool = store.read_connection().query_row(
        "SELECT EXISTS(SELECT 1 FROM local_controller_completion_roots
         WHERE completion=?1 AND kind=?2 AND reference=?3)",
        rusqlite::params![key.as_str(), kind, reference],
        |row| row.get(0),
    )?;
    if !rooted {
        return Err(WaveError::Invalid(format!(
            "historical completion does not retain {kind} {reference}"
        )));
    }
    Ok(())
}

fn classify_revision_relation(
    prior_revision: &str,
    current_revision: &str,
    prior_source: &str,
    current_wave_baseline: &str,
    current_revision_baseline: &str,
    prior_selected: &[String],
    current_selected: &[String],
) -> Result<RevisionRelation, WaveError> {
    if prior_revision == current_revision {
        let previous: BTreeSet<_> = prior_selected.iter().collect();
        let selected: BTreeSet<_> = current_selected.iter().collect();
        if previous.len() != prior_selected.len()
            || selected.len() != current_selected.len()
            || current_selected.get(..prior_selected.len()) != Some(prior_selected)
            || prior_source != current_wave_baseline
        {
            return Err(WaveError::Invalid(
                "same-revision completion baseline or cumulative selection".into(),
            ));
        }
        Ok(RevisionRelation::Same)
    } else if prior_source == current_revision_baseline
        && current_wave_baseline == current_revision_baseline
    {
        // A new revision inherits the exact source snapshot, not the previous
        // revision's contribution identifiers or request accepted-list.
        Ok(RevisionRelation::Cross)
    } else {
        Err(WaveError::Invalid(
            "cross-revision completion does not match the explicit revision baseline".into(),
        ))
    }
}

fn verify_historical_seed_binding_git(
    store: &mut CollaborationStore,
    repo: &Path,
    key: &RecordId,
    completion: &CompletionSpec,
    wave_spec: &WaveSpec,
) -> Result<(), WaveError> {
    let wave_id = id(&completion.wave)?;
    let wave = super::load(store, &wave_id)?;
    if wave.phase != super::WavePhase::Accepted
        || wave.spec != *wave_spec
        || wave
            .candidate_snapshot
            .as_ref()
            .map(SourceSnapshotId::as_str)
            != Some(completion.source.as_str())
        || wave.candidate_commit.as_ref().map(CommitOid::as_str)
            != Some(completion.candidate_commit.as_str())
    {
        return Err(WaveError::Invalid(
            "historical seeded wave no longer matches its accepted completion".into(),
        ));
    }
    let Some(request_ref) = wave.resolution_request.as_ref() else {
        if wave.resolution_result.is_none() {
            return Ok(());
        }
        return Err(WaveError::Invalid(
            "historical synthesized request/result pair is incomplete".into(),
        ));
    };
    let result_ref = wave
        .resolution_result
        .as_ref()
        .ok_or_else(|| WaveError::Invalid("historical synthesized result is missing".into()))?;
    let Some(recipe) = super::recipe_json_for_wave(store, &wave)? else {
        return Err(WaveError::Invalid(
            "historical seeded recipe is missing".into(),
        ));
    };
    let proof = crate::resolution::read_retained_synthesized_result_for_refs(
        store,
        request_ref,
        result_ref,
    )?;
    let synthesis = crate::resolution::read_synthesized_attempt_result_for_refs(
        store,
        repo,
        request_ref,
        result_ref,
    )?;
    if proof.request.record() != synthesis.request.record()
        || proof.group_record != synthesis.group_record
        || proof.candidate_subject != synthesis.candidate.subject
        || proof.candidate_commit != synthesis.candidate.commit
        || proof.input_manifest.sha256 != synthesis.input_manifest.sha256
    {
        return Err(WaveError::Invalid(
            "historical retained synthesis proof differs from its Git-bound result".into(),
        ));
    }
    let result_tree = archive::raw_commit_tree(repo, &proof.candidate_commit)?;
    if archive::snapshot_of_commit(repo, &proof.candidate_commit)?.id() != proof.candidate_subject
        || synthesis.candidate.tree != result_tree
        || proof.request.baseline().snapshot_id.as_str() != wave.spec.baseline
        || proof.candidate_subject.as_str() != completion.source
        || proof.candidate_commit.as_str() != completion.candidate_commit
        || synthesis.candidate.subject.as_str() != completion.source
        || synthesis.candidate.commit.as_str() != completion.candidate_commit
        || result_tree != completion.candidate_tree
    {
        return Err(WaveError::Invalid(
            "historical synthesized candidate differs from its accepted Git result".into(),
        ));
    }
    let Some(seeded) = recipe.get("seeded_conflict") else {
        // Direct attachment preserves the original Text composition source;
        // the accepted completion is instead bound to the settled resolver
        // result checked above.
        return Ok(());
    };
    let conflict_request = super::seeded_recipe_record_ref(seeded, "resolution_request")?;
    let conflict_source = super::seeded_recipe_record_ref(seeded, "composition_source")?;
    if &conflict_request != request_ref {
        return Err(WaveError::Invalid(
            "historical seeded request differs from its accepted wave".into(),
        ));
    }
    let binding = super::read_retained_conflict_request_binding(
        store,
        repo,
        &conflict_request,
        &conflict_source,
    )?;
    if binding.plan_payload.wave != wave.id.as_str()
        || binding.plan_payload.revision != wave.spec.revision
        || binding.plan_payload.selected != wave.spec.selected
    {
        return Err(WaveError::Invalid(
            "historical conflict plan differs from its accepted wave".into(),
        ));
    }
    if seeded["seed_candidate"]["tree"].as_str() != Some(result_tree.as_str()) {
        return Err(WaveError::Invalid(
            "historical seeded candidate tree differs from its retained commit".into(),
        ));
    }
    let expected = composer::expected_seeded_conflict_metadata(
        &binding,
        result_ref,
        &synthesis.request,
        &synthesis.candidate,
        &synthesis.input_manifest.sha256,
    );
    let expected: Value = serde_json::to_value(expected)?;
    if seeded != &expected {
        return Err(WaveError::Invalid(
            "historical seeded recipe differs from its exact conflict, result, or suffix".into(),
        ));
    }
    for reference in [
        request_ref.object.hex(),
        result_ref.object.hex(),
        proof.input_manifest.sha256,
        binding.request.object.hex(),
        binding.source.object.hex(),
        binding.plan.object.hex(),
    ] {
        require_completion_root(store, key, "object", &reference)?;
    }
    require_completion_root(store, key, "snapshot", proof.candidate_subject.as_str())?;
    for digest in proof
        .input_manifest
        .entries
        .iter()
        .filter_map(|entry| entry.sha256.as_deref())
    {
        require_completion_root(store, key, "object", digest)?;
    }
    Ok(())
}

impl Binding {
    pub(crate) fn key(&self) -> &RecordId {
        &self.key
    }

    pub(crate) fn source(&self) -> &SourceSnapshotId {
        &self.source
    }

    /// The actual request baseline is the accepted commit. WaveSpec::baseline
    /// is the source snapshot ID and must not be substituted for this commit.
    pub(crate) fn request_baseline_commit(&self) -> &CommitOid {
        &self.accepted_commit
    }

    pub(crate) fn previous_members(&self) -> &[String] {
        match self.relation {
            RevisionRelation::Same => &self.previous.selected,
            RevisionRelation::Cross => &[],
        }
    }

    pub(crate) fn same_revision(&self) -> bool {
        self.relation == RevisionRelation::Same
    }

    /// The original pinned revision baseline is carried as accepted history
    /// only within the same revision. Across revisions, the accepted source
    /// itself is the explicit baseline and prior contribution IDs are not
    /// inherited.
    pub(crate) fn accepted_history(
        &self,
        store: &CollaborationStore,
        pinned: &TargetRevision,
    ) -> Result<Vec<CommitOid>, WaveError> {
        if !self.same_revision() {
            return Ok(Vec::new());
        }
        let baseline = snapshot(&pinned.spec.baseline)?;
        let retained = archive::retained(store, &baseline)?
            .ok_or(crate::local_target::TargetError::Missing)?;
        Ok(vec![retained.commit])
    }

    pub(crate) fn recipe_binding(&self) -> Value {
        serde_json::json!({
            "completion": self.key.as_str(),
            "members": self.previous_members(),
        })
    }

    pub(crate) fn producer_profile(&self) -> &'static str {
        super::ACCEPTED_BASELINE_TEXT_PROFILE
    }

    /// A resolver request must name both halves of this binding exactly:
    /// the accepted commit and the snapshot retained by this wave.
    pub(crate) fn validate_request_baseline(
        &self,
        request: &ResolutionRequest,
        wave: &Wave,
    ) -> Result<(), WaveError> {
        if request.baseline().commit != self.accepted_commit
            || request.baseline().snapshot_id != self.source
            || self.source.as_str() != wave.spec.baseline
        {
            return Err(WaveError::Invalid(
                "resolver request baseline differs from the prior completion".into(),
            ));
        }
        Ok(())
    }

    /// Verify that the accepted commit still materializes the exact source
    /// snapshot and tree stated by the private completion.
    pub(crate) fn validate_git_binding(
        &self,
        store: &mut CollaborationStore,
        repo: &Path,
    ) -> Result<(), WaveError> {
        if archive::raw_commit_tree(repo, &self.accepted_commit)? != self.completion.accepted_tree
            || archive::snapshot_of_commit(repo, &self.accepted_commit)?.id() != self.source
        {
            return Err(WaveError::Invalid(
                "prior completion accepted commit no longer matches its source".into(),
            ));
        }
        verify_historical_seed_binding_git(store, repo, &self.key, &self.completion, &self.previous)
    }

    /// Completion artifacts are rooted independently of a later revision.
    /// Check the completion record and accepted source before relying on them.
    pub(crate) fn validate_retained_completion(
        &self,
        store: &CollaborationStore,
    ) -> Result<(), WaveError> {
        let body: Vec<u8> = store.read_connection().query_row(
            "SELECT body FROM local_controller_completions WHERE id=?1",
            [self.key.as_str()],
            |row| row.get(0),
        )?;
        let object = archive::ObjectDigest::of(&body).hex();
        for (kind, reference) in [
            ("object", object.as_str()),
            ("snapshot", self.source.as_str()),
        ] {
            let rooted: bool = store.read_connection().query_row(
                "SELECT EXISTS(SELECT 1 FROM local_controller_completion_roots WHERE completion=?1 AND kind=?2 AND reference=?3)",
                rusqlite::params![self.key.as_str(), kind, reference],
                |row| row.get(0),
            )?;
            if !rooted {
                return Err(WaveError::Invalid(format!(
                    "prior completion {kind} {reference} is no longer retained"
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classified(
        prior_revision: &str,
        current_revision: &str,
        prior_source: &str,
        current_wave_baseline: &str,
        current_revision_baseline: &str,
        prior_selected: &[&str],
        current_selected: &[&str],
    ) -> Result<RevisionRelation, WaveError> {
        classify_revision_relation(
            prior_revision,
            current_revision,
            prior_source,
            current_wave_baseline,
            current_revision_baseline,
            &prior_selected
                .iter()
                .map(|s| (*s).to_owned())
                .collect::<Vec<_>>(),
            &current_selected
                .iter()
                .map(|s| (*s).to_owned())
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn same_revision_keeps_cumulative_members_and_source_snapshot_binding() {
        assert_eq!(
            classified(
                "rev-a",
                "rev-a",
                "sha256:source-a",
                "sha256:source-a",
                "sha256:original-a",
                &["delivered-a"],
                &["delivered-a", "new-b"],
            )
            .unwrap(),
            RevisionRelation::Same
        );
        assert!(
            classified(
                "rev-a",
                "rev-a",
                "sha256:source-a",
                "sha256:another-source",
                "sha256:original-a",
                &["delivered-a"],
                &["delivered-a", "new-b"],
            )
            .is_err()
        );
        assert!(
            classified(
                "rev-a",
                "rev-a",
                "sha256:source-a",
                "sha256:source-a",
                "sha256:original-a",
                &["delivered-a"],
                &["new-b"],
            )
            .is_err()
        );
    }

    #[test]
    fn cross_revision_reuses_only_the_exact_pinned_source_snapshot() {
        assert_eq!(
            classified(
                "rev-a",
                "rev-b",
                "sha256:accepted-source",
                "sha256:accepted-source",
                "sha256:accepted-source",
                &["old-id"],
                &["new-id"],
            )
            .unwrap(),
            RevisionRelation::Cross
        );
        assert!(
            classified(
                "rev-a",
                "rev-b",
                "sha256:accepted-source",
                "sha256:other-source",
                "sha256:accepted-source",
                &["old-id"],
                &["new-id"],
            )
            .is_err()
        );
        assert!(
            classified(
                "rev-a",
                "rev-b",
                "sha256:accepted-source",
                "sha256:accepted-source",
                "sha256:other-source",
                &["old-id"],
                &["new-id"],
            )
            .is_err()
        );
    }

    #[test]
    fn duplicate_selection_ids_cannot_become_prior_recipe_members() {
        assert!(
            classified(
                "rev-a",
                "rev-a",
                "sha256:source-a",
                "sha256:source-a",
                "sha256:original-a",
                &["same", "same"],
                &["same", "new"],
            )
            .is_err()
        );
        assert!(
            classified(
                "rev-a",
                "rev-a",
                "sha256:source-a",
                "sha256:source-a",
                "sha256:original-a",
                &["same"],
                &["same", "same"],
            )
            .is_err()
        );
    }
}
