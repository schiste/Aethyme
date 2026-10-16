//! Experimental target bindings to immutable D10 contribution revisions.
//!
//! The wire envelope carries references, never authority. All records are
//! re-read from the archive and joined to the exact selected target aliases,
//! capture lineages, base/result snapshots, and dependency edges. Project
//! enrollment and FileId continuity are resolved only through the trusted
//! project identity store.

use std::collections::{BTreeMap, BTreeSet};

use aethyme_contracts::experimental_v0::{
    RecordId,
    brief::Brief,
    contribution_revision::{ContributionId, ContributionRevision},
    project_identity::{FileIdentityTransition, ProjectDescriptor, ProjectId},
};
use serde::{Deserialize, Serialize};

use crate::{
    collaboration_archive::{self as archive, ObjectDigest},
    collaboration_state::CollaborationStore,
    collaboration_identity_store::{
        resolve_approved_project_binding, resolve_bound_contribution_revisions,
        ContributionRevisionPin,
    },
};

use super::{TargetError, TargetSpec, record_id};

pub const D10_TARGET_PROFILE: &str = "aethyme.target-revision/d10-experimental-v0";
const MAX_D10_TARGET_BYTES: usize = 16 * 1024 * 1024;

/// Exact project and contribution-revision references in an immutable target.
/// The values are untrusted until `validate_bindings` re-reads and joins them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct D10RevisionBindings {
    pub profile: String,
    pub project_id: String,
    pub descriptor_record_id: String,
    pub descriptor_object_sha256: String,
    pub revisions: Vec<D10RevisionPin>,
}

/// One target-local alias mapped to its logical contribution and immutable
/// revision record. The capture record remains the legacy lineage reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct D10RevisionPin {
    pub contribution: String,
    pub logical_contribution_id: String,
    pub revision_record_id: String,
    pub revision_object_sha256: String,
}

#[derive(Debug)]
pub(super) struct LoadedD10Bindings {
    pub bindings: D10RevisionBindings,
    pub revision_ids: BTreeMap<String, RecordId>,
    pub object_digests: BTreeSet<ObjectDigest>,
    pub identity_transitions: BTreeMap<String, FileIdentityTransition>,
}

impl D10RevisionBindings {
    pub fn revision_ids(&self) -> Result<BTreeMap<String, RecordId>, TargetError> {
        let mut ids = BTreeMap::new();
        for pin in &self.revisions {
            if ids
                .insert(
                    pin.contribution.clone(),
                    record_id(&pin.revision_record_id)?,
                )
                .is_some()
            {
                return Err(TargetError::Invalid("duplicate D10 contribution mapping".into()));
            }
        }
        Ok(ids)
    }
}

pub(super) fn validate_bindings(
    store: &CollaborationStore,
    spec: &TargetSpec,
    mut bindings: D10RevisionBindings,
) -> Result<LoadedD10Bindings, TargetError> {
    if bindings.profile != D10_TARGET_PROFILE {
        return Err(TargetError::Invalid("D10 target profile".into()));
    }
    let project_id = ProjectId::parse(&bindings.project_id)
        .map_err(|_| TargetError::Invalid("D10 project ID".into()))?;
    let descriptor_id = record_id(&bindings.descriptor_record_id)?;
    let descriptor_digest = archive::parse_digest(&bindings.descriptor_object_sha256)
        .ok_or_else(|| TargetError::Invalid("D10 descriptor object digest".into()))?;
    let descriptor_bytes =
        archive::read_object_bounded(store, &descriptor_digest, MAX_D10_TARGET_BYTES)?;
    let (descriptor, observed_descriptor_id) = ProjectDescriptor::from_record(&descriptor_bytes)
        .map_err(|error| TargetError::Invalid(format!("D10 descriptor: {error}")))?;
    if observed_descriptor_id != descriptor_id || descriptor.project_id() != &project_id {
        return Err(TargetError::Invalid("D10 descriptor binding".into()));
    }
    if archive::retained(store, descriptor.anchor_snapshot())?.is_none() {
        return Err(TargetError::Missing);
    }

    let approved = resolve_approved_project_binding(store)
        .map_err(|_| TargetError::RevisionBindingUnsupported)?
        .ok_or(TargetError::RevisionBindingUnsupported)?;
    if approved.project_id() != &project_id
        || approved.descriptor_record_id() != &descriptor_id
        || approved.descriptor_object() != descriptor_digest
    {
        return Err(TargetError::Invalid(
            "D10 descriptor does not match the approved project binding".into(),
        ));
    }

    validate_selected_map(spec, &bindings.revisions)?;
    bindings
        .revisions
        .sort_by(|left, right| left.contribution.cmp(&right.contribution));

    let mut total_bytes = descriptor_bytes.len();
    let mut object_digests = BTreeSet::from([descriptor_digest]);
    let mut revision_ids = BTreeMap::new();
    let mut unique_revision_ids = BTreeSet::new();
    let contributions: BTreeMap<_, _> = spec
        .contributions
        .iter()
        .map(|contribution| (contribution.id.as_str(), contribution))
        .collect();
    let mut parsed_pins = Vec::with_capacity(bindings.revisions.len());
    for pin in &bindings.revisions {
        let target_id = &pin.contribution;
        let contribution = contributions
            .get(target_id.as_str())
            .ok_or_else(|| TargetError::Invalid("D10 target contribution".into()))?;
        let logical_id = ContributionId::parse(&pin.logical_contribution_id)
            .map_err(|_| TargetError::Invalid("D10 logical contribution ID".into()))?;
        let revision_id = record_id(&pin.revision_record_id)?;
        let revision_digest = archive::parse_digest(&pin.revision_object_sha256)
            .ok_or_else(|| TargetError::Invalid("D10 revision object digest".into()))?;
        if !unique_revision_ids.insert(revision_id.clone()) {
            return Err(TargetError::Invalid(
                "D10 revision is mapped more than once".into(),
            ));
        }
        let lineage = record_id(&contribution.lineage)?;
        parsed_pins.push((target_id.clone(), logical_id, revision_id, revision_digest, lineage));
    }
    let resolver_pins: Vec<_> = parsed_pins
        .iter()
        .map(
            |(_, logical_id, revision_id, revision_digest, _)| ContributionRevisionPin {
                revision_record_id: revision_id,
                object_digest: *revision_digest,
                contribution_id: logical_id,
            },
        )
        .collect();
    let resolved_revisions = resolve_bound_contribution_revisions(store, &approved, &resolver_pins)
        .map_err(|_| TargetError::RevisionBindingUnsupported)?;
    let mut parsed_revisions = BTreeMap::new();
    let mut identity_transitions = BTreeMap::new();

    for (target_id, logical_id, revision_id, revision_digest, lineage) in parsed_pins {
        let contribution = contributions
            .get(target_id.as_str())
            .ok_or_else(|| TargetError::Invalid("D10 target contribution".into()))?;
        let resolved = resolved_revisions
            .get(&revision_id)
            .ok_or(TargetError::RevisionBindingUnsupported)?;
        let revision = resolved.revision();
        if resolved.record_id() != &revision_id
            || resolved.object_digest() != revision_digest
            || revision.project_id() != &project_id
            || revision.contribution_id() != &logical_id
            || revision.capture_record_id() != &lineage
            || revision.capture_record_id().as_str() != contribution.lineage
        {
            return Err(TargetError::Invalid("D10 revision binding".into()));
        }
        let (revision_bytes, observed_revision_id) = revision.to_record();
        if observed_revision_id != revision_id
            || ObjectDigest::of(&revision_bytes) != revision_digest
        {
            return Err(TargetError::Invalid("D10 revision record identity".into()));
        }
        total_bytes = total_bytes
            .checked_add(revision_bytes.len())
            .ok_or_else(|| TargetError::Invalid("D10 target size".into()))?;
        if total_bytes > MAX_D10_TARGET_BYTES {
            return Err(TargetError::Invalid("D10 target size".into()));
        }
        let retained = archive::retained_contribution(store, &lineage)?.ok_or(TargetError::Missing)?;
        if revision.base() != &retained.base.snapshot_id
            || revision.result() != &retained.result.snapshot_id
        {
            return Err(TargetError::Invalid("D10 capture snapshot binding".into()));
        }
        if resolved.capture_lineage() != &lineage
            || resolved.base_state().snapshot_id() != revision.base()
            || resolved.result_state().snapshot_id() != revision.result()
        {
            return Err(TargetError::Invalid(
                "D10 revision does not match the approved identity history".into(),
            ));
        }
        identity_transitions.insert(target_id.clone(), resolved.identity_transition().clone());
        if let Some(brief) = revision.brief() {
            let digest = archive::parse_digest(&brief.object_sha256)
                .ok_or_else(|| TargetError::Invalid("D10 brief object digest".into()))?;
            let brief_bytes = resolved
                .brief_record_bytes()
                .ok_or_else(|| TargetError::Invalid("D10 brief bytes are unavailable".into()))?;
            total_bytes = total_bytes
                .checked_add(brief_bytes.len())
                .ok_or_else(|| TargetError::Invalid("D10 target size".into()))?;
            if total_bytes > MAX_D10_TARGET_BYTES {
                return Err(TargetError::Invalid("D10 target size".into()));
            }
            let (_, observed_brief_id) = Brief::from_record(&brief_bytes)
                .map_err(|error| TargetError::Invalid(format!("D10 brief: {error}")))?;
            if observed_brief_id != brief.record_id {
                return Err(TargetError::Invalid("D10 brief binding".into()));
            }
            if resolved.brief_digest() != Some(digest) {
                return Err(TargetError::Invalid(
                    "D10 brief does not match the approved revision object".into(),
                ));
            }
            object_digests.insert(digest);
        } else if resolved.brief_digest().is_some() {
            return Err(TargetError::Invalid(
                "D10 revision has an unexpected brief object".into(),
            ));
        }
        object_digests.insert(revision_digest);
        revision_ids.insert(target_id.clone(), revision_id.clone());
        parsed_revisions.insert(target_id, revision.clone());
    }

    validate_dependencies(spec, &bindings.revisions, &parsed_revisions)?;

    Ok(LoadedD10Bindings {
        bindings,
        revision_ids,
        object_digests,
        identity_transitions,
    })
}

fn validate_selected_map(spec: &TargetSpec, revisions: &[D10RevisionPin]) -> Result<(), TargetError> {
    let selected: BTreeSet<_> = spec.selected.iter().map(String::as_str).collect();
    let mut supplied = BTreeSet::new();
    for revision in revisions {
        if !supplied.insert(revision.contribution.as_str()) {
            return Err(TargetError::Invalid("duplicate D10 contribution mapping".into()));
        }
    }
    if selected != supplied {
        return Err(TargetError::Invalid(
            "D10 revision map must exactly match selected contributions".into(),
        ));
    }
    Ok(())
}

pub(super) fn validate_selected_identity_transitions(
    selected: &[String],
    transitions: &BTreeMap<String, FileIdentityTransition>,
) -> Result<(), TargetError> {
    let mut by_proposal = BTreeMap::<String, FileIdentityTransition>::new();
    let mut proposals = Vec::new();
    for contribution in selected {
        let transition = transitions
            .get(contribution)
            .ok_or_else(|| TargetError::Invalid("D10 selected transition is missing".into()))?;
        let proposal_id = transition.proposal_id().to_string();
        if let Some(previous) = by_proposal.get(&proposal_id) {
            if previous != transition {
                return Err(TargetError::FileNamespaceConflict(
                    "one logical contribution has different selected identity transitions".into(),
                ));
            }
            continue;
        }
        by_proposal.insert(proposal_id, transition.clone());
        proposals.push(transition.clone());
    }
    aethyme_contracts::experimental_v0::validate_file_proposals(&proposals)
        .map_err(|error| TargetError::FileNamespaceConflict(error.to_string()))
}

fn validate_dependencies(
    spec: &TargetSpec,
    pins: &[D10RevisionPin],
    revisions: &BTreeMap<String, ContributionRevision>,
) -> Result<(), TargetError> {
    let pins: BTreeMap<_, _> = pins
        .iter()
        .map(|pin| (pin.contribution.as_str(), pin))
        .collect();
    let contributions: BTreeMap<_, _> = spec
        .contributions
        .iter()
        .map(|contribution| (contribution.id.as_str(), contribution))
        .collect();
    for (target_id, revision) in revisions {
        let contribution = contributions
            .get(target_id.as_str())
            .ok_or_else(|| TargetError::Invalid("D10 target contribution".into()))?;
        let mut expected = contribution
            .requires
            .iter()
            .map(|dependency| {
                pins.get(dependency.as_str())
                    .ok_or_else(|| TargetError::Invalid("D10 dependency is not selected".into()))
                    .and_then(|pin| record_id(&pin.revision_record_id))
            })
            .collect::<Result<Vec<_>, _>>()?;
        expected.sort();
        if revision.depends_on() != expected {
            return Err(TargetError::Invalid(
                "D10 dependency mapping does not match target requirements".into(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_target::{
        RequestedContribution, acquire_controller, propose_revision, release_revision,
        retain_artifacts, select_revision,
    };
    use aethyme_contracts::experimental_v0::{
        Brief, BriefReference, EntryKind, ProjectDescriptor, SourceEntry, SourceSnapshot,
        SourceSnapshotId, advance_file_identities,
    };

    fn record_id(digit: char) -> RecordId {
        RecordId::parse(&format!("sha256:{}", digit.to_string().repeat(64)))
            .expect("record ID")
    }

    fn source_id(digit: char) -> SourceSnapshotId {
        SourceSnapshotId::parse(&format!("sha256:{}", digit.to_string().repeat(64)))
            .expect("snapshot ID")
    }

    fn contribution_id(encoded: &str) -> ContributionId {
        ContributionId::parse(encoded).expect("logical contribution ID")
    }

    fn age_objects(store: &CollaborationStore) {
        for prefix in std::fs::read_dir(archive::objects_dir(store)).unwrap() {
            for object in std::fs::read_dir(prefix.unwrap().path()).unwrap() {
                std::fs::File::open(object.unwrap().path())
                    .unwrap()
                    .set_times(
                        std::fs::FileTimes::new()
                            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1)),
                    )
                    .unwrap();
            }
        }
    }

    fn archive_object_relpath(digest: &ObjectDigest) -> String {
        let hex = digest.hex();
        format!("objects/sha256/{}/{}", &hex[..2], &hex[2..])
    }

    #[test]
    fn immutable_revision_and_brief_roots_survive_target_supersession_with_recipe_candidate() {
        let mut fixture = crate::local_target::tests::Fixture::new();
        let owner = acquire_controller(&mut fixture.store, "target", "appointed").unwrap();
        let base = archive::snapshot_of_commit(fixture._repo.path(), &fixture.a.base.commit)
            .unwrap();
        let result = archive::snapshot_of_commit(fixture._repo.path(), &fixture.a.result.commit)
            .unwrap();
        assert_eq!(base.id(), fixture.a.base.snapshot_id);
        assert_eq!(result.id(), fixture.a.result.snapshot_id);

        let project_id = ProjectId::parse("proj:aaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let descriptor = ProjectDescriptor::from_snapshot(
            project_id.clone(),
            fixture.a.base.commit.as_str(),
            &base,
        )
        .unwrap();
        let (descriptor_bytes, descriptor_id) = descriptor.to_record();
        let descriptor_object = archive::put_object(&fixture.store, &descriptor_bytes).unwrap();
        let logical_id = contribution_id(&format!("contrib:{}e", "b".repeat(25)));
        let transition = advance_file_identities(
            &project_id,
            &descriptor.initial_file_identity_state().unwrap(),
            logical_id.as_str(),
            &base,
            &result,
            &[],
        )
        .unwrap();
        let source_changes = transition
            .changes()
            .iter()
            .map(|change| change.to_value())
            .collect();
        let brief = Brief::from_decision_file(
            br#"{"intent":"keep the decision with its source revision","decisions":[{"scope_ref":"source","choice":"retain","reason":"downstream readers need this exact context"}]}"#,
        )
        .unwrap();
        let (brief_bytes, brief_id) = brief.to_record();
        let brief_object = archive::put_object(&fixture.store, &brief_bytes).unwrap();
        let revision = ContributionRevision::new(
            project_id,
            logical_id,
            fixture.a.lineage_record_id.clone(),
            fixture.a.base.snapshot_id.clone(),
            fixture.a.result.snapshot_id.clone(),
            source_changes,
            Some(
                BriefReference::new(brief_id, brief_object.hex()).expect("brief reference"),
            ),
            Vec::new(),
        )
        .unwrap();
        let (revision_bytes, revision_id) = revision.to_record();
        assert_eq!(ContributionRevision::from_record(&revision_bytes).unwrap().1, revision_id);
        let revision_object = archive::put_object(&fixture.store, &revision_bytes).unwrap();
        let recipe_bytes = serde_json::to_vec(&serde_json::json!({
            "schema": "test.composition-recipe",
            "revision": revision_id.as_str(),
        }))
        .unwrap();
        let recipe_object = archive::put_object(&fixture.store, &recipe_bytes).unwrap();

        let first = propose_revision(&mut fixture.store, &owner, fixture.spec()).unwrap();
        retain_artifacts(
            &mut fixture.store,
            &owner,
            &first.id,
            &[fixture.a.result.snapshot_id.clone()],
            &[
                descriptor_object.clone(),
                revision_object.clone(),
                brief_object.clone(),
                recipe_object.clone(),
            ],
        )
        .unwrap();
        select_revision(&mut fixture.store, &owner, &first.id).unwrap();

        let mut next_spec = fixture.spec();
        next_spec.objective = "superseding target keeps prior recipe history rooted".into();
        let next = propose_revision(&mut fixture.store, &owner, next_spec).unwrap();
        select_revision(&mut fixture.store, &owner, &next.id).unwrap();
        let active: String = fixture
            .store
            .read_connection()
            .query_row(
                "SELECT active_revision FROM local_targets WHERE target='target'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(active, next.id.as_str());

        age_objects(&fixture.store);
        let options = crate::collaboration_gc::GcOptions::default();
        let plan = crate::collaboration_gc::plan(&mut fixture.store, &options).unwrap();
        for digest in [
            &descriptor_object,
            &revision_object,
            &brief_object,
            &recipe_object,
        ] {
            assert!(!plan.reclaimable.iter().any(|item| {
                item.kind == crate::collaboration_gc::ItemKind::Object
                    && item.relpath == archive_object_relpath(digest)
            }));
        }

        release_revision(&mut fixture.store, &owner, &first.id).unwrap();
        let released = crate::collaboration_gc::plan(&mut fixture.store, &options).unwrap();
        for digest in [
            &descriptor_object,
            &revision_object,
            &brief_object,
            &recipe_object,
        ] {
            assert!(released.reclaimable.iter().any(|item| {
                item.kind == crate::collaboration_gc::ItemKind::Object
                    && item.relpath == archive_object_relpath(digest)
            }));
        }
        assert_eq!(descriptor_id.as_str().starts_with("sha256:"), true);
    }

    fn spec() -> TargetSpec {
        TargetSpec {
            objective_authorization: record_id('1').to_string(),
            objective: "combine".into(),
            baseline: source_id('0').to_string(),
            contributions: vec![
                RequestedContribution {
                    id: "A".into(),
                    lineage: record_id('a').to_string(),
                    requires: Vec::new(),
                    atomic_group: None,
                    revision_of: None,
                    derived_from: Vec::new(),
                },
                RequestedContribution {
                    id: "B".into(),
                    lineage: record_id('b').to_string(),
                    requires: vec!["A".into()],
                    atomic_group: None,
                    revision_of: None,
                    derived_from: Vec::new(),
                },
            ],
            selected: vec!["A".into(), "B".into()],
            requirements: Vec::new(),
            acceptance_policy: record_id('2').to_string(),
            composer_profile: "text/provisional".into(),
        }
    }

    fn bindings() -> D10RevisionBindings {
        D10RevisionBindings {
            profile: D10_TARGET_PROFILE.into(),
            project_id: "proj:00000000000000000000000000".into(),
            descriptor_record_id: record_id('3').to_string(),
            descriptor_object_sha256: "4".repeat(64),
            revisions: vec![
                D10RevisionPin {
                    contribution: "A".into(),
                    logical_contribution_id: "contrib:00000000000000000000000000".into(),
                    revision_record_id: record_id('5').to_string(),
                    revision_object_sha256: "6".repeat(64),
                },
                D10RevisionPin {
                    contribution: "B".into(),
                    logical_contribution_id: "contrib:00000000000000000000000004".into(),
                    revision_record_id: record_id('7').to_string(),
                    revision_object_sha256: "8".repeat(64),
                },
            ],
        }
    }

    fn parsed_revisions(depends_on_b: RecordId) -> BTreeMap<String, ContributionRevision> {
        let project = ProjectId::parse("proj:00000000000000000000000000").expect("project ID");
        BTreeMap::from([
            (
                "A".into(),
                ContributionRevision::new(
                    project.clone(),
                    contribution_id("contrib:00000000000000000000000000"),
                    record_id('a'),
                    source_id('0'),
                    source_id('1'),
                    Vec::new(),
                    None,
                    Vec::new(),
                )
                .expect("revision A"),
            ),
            (
                "B".into(),
                ContributionRevision::new(
                    project,
                    contribution_id("contrib:00000000000000000000000004"),
                    record_id('b'),
                    source_id('1'),
                    source_id('2'),
                    Vec::new(),
                    None,
                    vec![depends_on_b],
                )
                .expect("revision B"),
            ),
        ])
    }

    #[test]
    fn selected_revision_map_must_be_exact() {
        let spec = spec();
        let bindings = bindings();
        assert!(validate_selected_map(&spec, &bindings.revisions).is_ok());
        let mut missing = bindings.revisions.clone();
        missing.pop();
        assert!(validate_selected_map(&spec, &missing).is_err());
        let mut extra = bindings.revisions;
        let mut extra_pin = extra[0].clone();
        extra_pin.contribution = "C".into();
        extra.push(extra_pin);
        assert!(validate_selected_map(&spec, &extra).is_err());
    }

    #[test]
    fn revision_dependencies_must_equal_target_requirement_pins() {
        let spec = spec();
        let bindings = bindings();
        let revision_a = record_id('5');
        assert!(validate_dependencies(
            &spec,
            &bindings.revisions,
            &parsed_revisions(revision_a.clone()),
        )
        .is_ok());
        assert!(validate_dependencies(
            &spec,
            &bindings.revisions,
            &parsed_revisions(record_id('7')),
        )
        .is_err());
    }

    #[test]
    fn selected_d10_proposals_conflict_before_legacy_lineage_dedup() {
        let project_id = ProjectId::parse("proj:aaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let base = SourceSnapshot::new(std::iter::empty::<SourceEntry>()).unwrap();
        let result = SourceSnapshot::new([SourceEntry::from_content(
            b"src/shared.rs".to_vec(),
            EntryKind::Regular,
            b"same bytes",
        )])
        .unwrap();
        let descriptor = ProjectDescriptor::from_snapshot(
            project_id.clone(),
            "1111111111111111111111111111111111111111",
            &base,
        )
        .unwrap();
        let base_state = descriptor.initial_file_identity_state().unwrap();
        let first = advance_file_identities(
            &project_id,
            &base_state,
            "contrib:aaaaaaaaaaaaaaaaaaaaaaaaaa",
            &base,
            &result,
            &[],
        )
        .unwrap();
        let second = advance_file_identities(
            &project_id,
            &base_state,
            "contrib:bbbbbbbbbbbbbbbbbbbbbbbbbe",
            &base,
            &result,
            &[],
        )
        .unwrap();
        assert_eq!(first.base(), second.base());
        assert_eq!(first.result(), second.result());
        assert_ne!(
            first.result_files()[0].file_id,
            second.result_files()[0].file_id
        );

        let selected = vec!["A".into(), "B".into()];
        let transitions = BTreeMap::from([
            ("A".into(), first.clone()),
            ("B".into(), second),
        ]);
        assert!(matches!(
            validate_selected_identity_transitions(&selected, &transitions),
            Err(TargetError::FileNamespaceConflict(_))
        ));

        let replayed = BTreeMap::from([
            ("A".into(), first.clone()),
            ("B".into(), first),
        ]);
        assert!(validate_selected_identity_transitions(&selected, &replayed).is_ok());
    }

    #[test]
    fn duplicate_capture_lineage_is_allowed_only_for_the_bound_d10_profile() {
        let mut spec = spec();
        spec.contributions[1].lineage = spec.contributions[0].lineage.clone();

        assert!(crate::local_target::normalize(spec.clone()).is_err());
        assert!(crate::local_target::normalize_with_d10(spec, true).is_ok());
    }
}
