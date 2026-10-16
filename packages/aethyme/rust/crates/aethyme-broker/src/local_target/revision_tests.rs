use super::*;
use crate::collaboration_capture::OperationId;
use crate::collaboration_cli::project_test_support::BoundProjectFixture;
use crate::collaboration_identity_store::{
    ContributionRevisionRequest, ResolvedContributionRevision, revise_contribution_for_operation,
};
use aethyme_contracts::experimental_v0::Brief;
use rusqlite::params;

fn revise_operation(
    fixture: &mut BoundProjectFixture,
    operation_id: &str,
    brief: Option<&Brief>,
    depends_on: Option<&[RecordId]>,
) -> ResolvedContributionRevision {
    let operation_id = OperationId::parse(operation_id).unwrap();
    revise_contribution_for_operation(
        &mut fixture.store,
        &fixture.binding,
        ContributionRevisionRequest {
            operation_id: &operation_id,
            file_operations: None,
            brief,
            depends_on,
        },
    )
    .unwrap()
}

fn d10_target_inputs(
    fixture: &BoundProjectFixture,
    baseline: &str,
    revisions: &[(&str, &ResolvedContributionRevision)],
) -> (TargetSpec, D10RevisionBindings) {
    let contributions = revisions
        .iter()
        .map(|(alias, revision)| RequestedContribution {
            id: (*alias).into(),
            lineage: revision.capture_lineage().to_string(),
            requires: Vec::new(),
            atomic_group: None,
            revision_of: None,
            derived_from: Vec::new(),
        })
        .collect::<Vec<_>>();
    let selected = revisions
        .iter()
        .map(|(alias, _)| (*alias).to_string())
        .collect::<Vec<_>>();
    let spec = TargetSpec {
        objective_authorization: format!("sha256:{}", "1".repeat(64)),
        objective: "compose source-bound revisions".into(),
        baseline: baseline.into(),
        contributions,
        selected,
        requirements: vec![format!("sha256:{}", "2".repeat(64))],
        acceptance_policy: format!("sha256:{}", "3".repeat(64)),
        composer_profile: crate::composer::PROVISIONAL_TEXT_PROFILE.into(),
    };
    let pins = revisions
        .iter()
        .map(|(alias, revision)| D10RevisionPin {
            contribution: (*alias).into(),
            logical_contribution_id: revision.revision().contribution_id().to_string(),
            revision_record_id: revision.record_id().to_string(),
            revision_object_sha256: revision.object_digest().hex().into(),
        })
        .collect();
    let bindings = D10RevisionBindings {
        profile: D10_TARGET_PROFILE.into(),
        project_id: fixture.binding.project_id().to_string(),
        descriptor_record_id: fixture.binding.descriptor_record_id().to_string(),
        descriptor_object_sha256: fixture.binding.descriptor_object().hex().into(),
        revisions: pins,
    };
    (spec, bindings)
}

#[test]
fn source_bound_same_operation_replays_but_independent_same_pair_proposals_conflict() {
    let mut fixture = BoundProjectFixture::new().unwrap();
    let result = fixture
        .commit_file("src/shared.rs", b"pub fn shared() {}\n")
        .unwrap();
    let base = fixture.bootstrap_commit.clone();
    fixture
        .capture_operation("operation-a", &base, &result)
        .unwrap();
    fixture
        .capture_operation("operation-b", &base, &result)
        .unwrap();

    let first = revise_operation(&mut fixture, "operation-a", None, None);
    let replay = revise_operation(&mut fixture, "operation-a", None, None);
    let independent = revise_operation(&mut fixture, "operation-b", None, None);
    assert_eq!(first.record_id(), replay.record_id());
    assert_eq!(first.identity_transition(), replay.identity_transition());
    assert_eq!(first.capture_lineage(), independent.capture_lineage());
    assert_ne!(
        first.revision().contribution_id(),
        independent.revision().contribution_id()
    );
    let first_file = first
        .identity_transition()
        .result_files()
        .iter()
        .find(|file| file.path == b"src/shared.rs")
        .unwrap();
    let independent_file = independent
        .identity_transition()
        .result_files()
        .iter()
        .find(|file| file.path == b"src/shared.rs")
        .unwrap();
    assert_ne!(first_file.file_id, independent_file.file_id);

    let (spec, bindings) = d10_target_inputs(
        &fixture,
        fixture.bootstrap_snapshot.as_str(),
        &[("A", &first), ("B", &independent)],
    );
    let owner = acquire_controller(&mut fixture.store, "d10-same-pair", "issuer").unwrap();
    let target =
        propose_revision_with_d10_bindings(&mut fixture.store, &owner, spec, bindings).unwrap();
    assert!(matches!(
        validate_d10_file_namespace(&fixture.store, &target, &target.spec.selected),
        Err(TargetError::FileNamespaceConflict(_))
    ));
}

#[test]
fn source_bound_rationale_revision_changes_the_immutable_target_identity() {
    let mut fixture = BoundProjectFixture::new().unwrap();
    let result = fixture
        .commit_file("src/rationale.rs", b"pub fn rationale() {}\n")
        .unwrap();
    let base = fixture.bootstrap_commit.clone();
    fixture
        .capture_operation("rationale-operation", &base, &result)
        .unwrap();

    let original = revise_operation(&mut fixture, "rationale-operation", None, None);
    let brief = Brief {
        intent: "Keep the reviewed behavior".into(),
        ..Brief::default()
    };
    let updated = revise_operation(&mut fixture, "rationale-operation", Some(&brief), None);
    assert_eq!(original.capture_lineage(), updated.capture_lineage());
    assert_eq!(
        original.revision().contribution_id(),
        updated.revision().contribution_id()
    );
    assert_ne!(original.record_id(), updated.record_id());
    assert_eq!(
        original.identity_transition(),
        updated.identity_transition()
    );

    let (original_spec, original_bindings) = d10_target_inputs(
        &fixture,
        fixture.bootstrap_snapshot.as_str(),
        &[("A", &original)],
    );
    let (updated_spec, updated_bindings) = d10_target_inputs(
        &fixture,
        fixture.bootstrap_snapshot.as_str(),
        &[("A", &updated)],
    );
    let owner = acquire_controller(&mut fixture.store, "d10-rationale", "issuer").unwrap();
    let original_target = propose_revision_with_d10_bindings(
        &mut fixture.store,
        &owner,
        original_spec,
        original_bindings,
    )
    .unwrap();
    let updated_target = propose_revision_with_d10_bindings(
        &mut fixture.store,
        &owner,
        updated_spec,
        updated_bindings,
    )
    .unwrap();
    assert_ne!(original_target.id, updated_target.id);
    assert_ne!(original.revision().brief(), updated.revision().brief());
}

#[test]
fn source_bound_dependency_pin_must_match_the_selected_target_edge() {
    let mut fixture = BoundProjectFixture::new().unwrap();
    let first_result = fixture
        .commit_file("src/base.rs", b"pub fn base() {}\n")
        .unwrap();
    let anchor = fixture.bootstrap_commit.clone();
    fixture
        .capture_operation("dependency-a", &anchor, &first_result)
        .unwrap();
    let first = revise_operation(&mut fixture, "dependency-a", None, None);

    let second_result = fixture
        .commit_file("src/derived.rs", b"pub fn derived() {}\n")
        .unwrap();
    fixture
        .capture_operation("dependency-b", &first_result, &second_result)
        .unwrap();
    let dependency = first.record_id().clone();
    let second = revise_operation(
        &mut fixture,
        "dependency-b",
        None,
        Some(std::slice::from_ref(&dependency)),
    );

    let (mut spec, bindings) = d10_target_inputs(
        &fixture,
        fixture.bootstrap_snapshot.as_str(),
        &[("A", &first), ("B", &second)],
    );
    spec.contributions[1].requires = vec!["A".into()];
    let owner = acquire_controller(&mut fixture.store, "d10-dependencies", "issuer").unwrap();
    let target = propose_revision_with_d10_bindings(
        &mut fixture.store,
        &owner,
        spec.clone(),
        bindings.clone(),
    )
    .unwrap();
    assert!(validate_d10_file_namespace(&fixture.store, &target, &target.spec.selected).is_ok());

    let mut reversed = spec.clone();
    reversed.selected.reverse();
    let reversed_target =
        propose_revision_with_d10_bindings(&mut fixture.store, &owner, reversed, bindings.clone())
            .unwrap();
    assert!(matches!(
        validate_d10_file_namespace(
            &fixture.store,
            &reversed_target,
            &reversed_target.spec.selected
        ),
        Err(TargetError::Invalid(_))
    ));

    spec.contributions[1].requires.clear();
    assert!(matches!(
        propose_revision_with_d10_bindings(&mut fixture.store, &owner, spec, bindings,),
        Err(TargetError::Invalid(_))
    ));
}

#[test]
fn released_d10_history_does_not_require_reclaimed_archive_objects() {
    let mut fixture = tests::Fixture::new();
    let owner = acquire_controller(&mut fixture.store, "history-target", "issuer").unwrap();
    let spec = fixture.spec();
    let proposed = propose_revision(&mut fixture.store, &owner, spec).unwrap();

    // This test isolates the released-history loader contract. It installs a
    // canonical D10-shaped binding whose referenced objects are intentionally
    // absent; a released record may render stored dispositions but cannot
    // re-enter the active target API.
    let (target, body): (String, Vec<u8>) = fixture
        .store
        .read_connection()
        .query_row(
            "SELECT target, body FROM local_target_revisions WHERE revision = ?1",
            [proposed.id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let mut body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let descriptor_object = "f".repeat(64);
    let revision_object = "e".repeat(64);
    body["revision_bindings"] = serde_json::to_value(D10RevisionBindings {
        profile: D10_TARGET_PROFILE.into(),
        project_id: "unavailable-project-binding".into(),
        descriptor_record_id: format!("sha256:{}", "d".repeat(64)),
        descriptor_object_sha256: descriptor_object.clone(),
        revisions: vec![D10RevisionPin {
            contribution: "A".into(),
            logical_contribution_id: "unavailable-logical-revision".into(),
            revision_record_id: format!("sha256:{}", "c".repeat(64)),
            revision_object_sha256: revision_object.clone(),
        }],
    })
    .unwrap();
    let record = Record::decode(
        &serde_json::to_vec(&body).unwrap(),
        &[&TARGET_REVISION_SCHEMA],
    )
    .unwrap();
    let history_id = record.id();
    let history_body = record.canonical_bytes();
    let stored_dispositions: Vec<(String, String, Option<String>)> = fixture
        .store
        .read_connection()
        .prepare(
            "SELECT contribution, status, detail FROM local_target_dispositions WHERE revision = ?1 ORDER BY contribution",
        )
        .unwrap()
        .query_map([proposed.id.as_str()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();

    let tx = fixture
        .store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    tx.execute(
        "DELETE FROM local_target_dispositions WHERE revision = ?1",
        [proposed.id.as_str()],
    )
    .unwrap();
    tx.execute(
        "DELETE FROM local_target_roots WHERE revision = ?1",
        [proposed.id.as_str()],
    )
    .unwrap();
    tx.execute(
        "DELETE FROM local_target_revisions WHERE revision = ?1",
        [proposed.id.as_str()],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO local_target_revisions (revision, target, body, released) VALUES (?1, ?2, ?3, 1)",
        params![history_id.as_str(), target, history_body],
    )
    .unwrap();
    for (contribution, status, detail) in stored_dispositions {
        tx.execute(
            "INSERT INTO local_target_dispositions (revision, contribution, status, detail) VALUES (?1, ?2, ?3, ?4)",
            params![history_id.as_str(), contribution, status, detail],
        )
        .unwrap();
    }
    tx.commit().unwrap();

    assert!(matches!(
        load_revision(&fixture.store, &history_id),
        Err(TargetError::Released)
    ));
    assert!(
        !archive::has_object(
            &fixture.store,
            &archive::parse_digest(&descriptor_object).unwrap()
        )
        .unwrap()
    );
    assert!(
        !archive::has_object(
            &fixture.store,
            &archive::parse_digest(&revision_object).unwrap()
        )
        .unwrap()
    );
    let dispositions = dispositions(&fixture.store, &history_id).unwrap();
    assert_eq!(dispositions.len(), 2);
    assert!(matches!(
        select_revision(&mut fixture.store, &owner, &history_id),
        Err(TargetError::Released)
    ));

    let response = crate::collaboration_sensitive::response_use(&fixture.store).unwrap();
    let history =
        load_revision_history_under_response(&fixture.store, &history_id, &response).unwrap();
    assert_eq!(history.id, history_id);
    assert_eq!(history.target, target);
}
