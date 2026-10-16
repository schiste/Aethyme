//! Project and FileId qualification vectors for the bounded Local profile.

use aethyme_contracts::experimental_v0::{
    EntryKind, FileId, FileOperation, IdentityRelation, ProjectDescriptor, ProjectId,
    ProjectIdentityError, SourceEntry, SourceSnapshot, advance_file_identities,
    project_identity::{MAX_DESCRIPTOR_BYTES, MAX_DESCRIPTOR_FILES},
    validate_file_proposals,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fixture {
    descriptor: serde_json::Value,
    content_hex: String,
    descriptor_record_id: String,
}

fn fixture() -> Fixture {
    serde_json::from_str(include_str!(
        "../fixtures/experimental-v0/project_identity.json"
    ))
    .expect("independent project identity fixture parses")
}

fn hex(bytes: &[u8]) -> Vec<u8> {
    data_encoding::HEXLOWER.decode(bytes).expect("fixture hex")
}

fn project() -> ProjectId {
    ProjectId::parse("proj:aaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap()
}

fn snapshot(files: &[(&[u8], &[u8])]) -> SourceSnapshot {
    SourceSnapshot::new(files.iter().map(|(path, content)| {
        SourceEntry::from_content(path.to_vec(), EntryKind::Regular, content)
    }))
    .unwrap()
}

#[test]
fn independent_descriptor_vector_round_trips_exactly() {
    let fixture = fixture();
    let content = hex(fixture.content_hex.as_bytes());
    let source = SourceSnapshot::new([SourceEntry::from_content(
        b"src/lib.rs".to_vec(),
        EntryKind::Regular,
        &content,
    )])
    .unwrap();
    let descriptor = ProjectDescriptor::from_snapshot(
        project(),
        "1111111111111111111111111111111111111111",
        &source,
    )
    .unwrap();
    let (bytes, id) = descriptor.to_record();
    assert_eq!(id.as_str(), fixture.descriptor_record_id);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
        fixture.descriptor
    );
    let (decoded, decoded_id) = ProjectDescriptor::from_record(&bytes).unwrap();
    assert_eq!(decoded, descriptor);
    assert_eq!(decoded_id, id);

    let mut extended: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    extended["extensions"] = serde_json::json!({"future": "opaque"});
    let extended = serde_json::to_vec(&extended).unwrap();
    assert!(ProjectDescriptor::from_record(&extended).is_err());
}

#[test]
fn file_ids_are_project_scoped_and_repeatable_for_the_same_project_id() {
    let source = snapshot(&[(b"src/a.rs", b"same")]);
    let project_a = project();
    let project_b = ProjectId::parse(&format!("proj:{}a", "b".repeat(25))).unwrap();
    let a = ProjectDescriptor::from_snapshot(
        project_a,
        "1111111111111111111111111111111111111111",
        &source,
    )
    .unwrap();
    let b = ProjectDescriptor::from_snapshot(
        project_b,
        "1111111111111111111111111111111111111111",
        &source,
    )
    .unwrap();
    assert_ne!(a.files()[0].file_id, b.files()[0].file_id);

    let mapped_clone = ProjectDescriptor::from_snapshot(
        project(),
        "1111111111111111111111111111111111111111",
        &source,
    )
    .unwrap();
    assert_eq!(a.files()[0].file_id, mapped_clone.files()[0].file_id);
}

#[test]
fn exact_rename_preserves_id_but_unannotated_rename_stays_uncertain() {
    let base = snapshot(&[(b"old.rs", b"same bytes"), (b"keep.rs", b"keep")]);
    let descriptor = ProjectDescriptor::from_snapshot(
        project(),
        "1111111111111111111111111111111111111111",
        &base,
    )
    .unwrap();
    let base_ids = descriptor.file_map();
    let base_state = descriptor.initial_file_identity_state().unwrap();
    let renamed = snapshot(&[(b"new.rs", b"same bytes"), (b"keep.rs", b"keep")]);

    let explicit = advance_file_identities(
        &project(),
        &base_state,
        "proposal-rename",
        &base,
        &renamed,
        &[FileOperation::Rename {
            from: b"old.rs".to_vec(),
            to: b"new.rs".to_vec(),
        }],
    )
    .unwrap();
    assert_eq!(
        explicit
            .result_files()
            .iter()
            .find(|file| file.path == b"new.rs")
            .unwrap()
            .file_id,
        base_ids[&b"old.rs"[..]]
    );
    assert!(
        explicit
            .changes()
            .iter()
            .any(|change| change.relation == IdentityRelation::ExplicitRename)
    );

    let inferred = advance_file_identities(
        &project(),
        &base_state,
        "proposal-inferred",
        &base,
        &renamed,
        &[],
    )
    .unwrap();
    let old_id = &base_ids[&b"old.rs"[..]];
    let new_id = &inferred
        .result_files()
        .iter()
        .find(|file| file.path == b"new.rs")
        .unwrap()
        .file_id;
    assert_ne!(old_id, new_id);
    assert!(inferred.changes().iter().any(|change| {
        change.relation == IdentityRelation::UncertainRename
            && change.possible_sources == vec![old_id.clone()]
    }));
}

#[test]
fn edited_rename_is_not_claimed_as_a_validated_rename() {
    let base = snapshot(&[(b"old.rs", b"before")]);
    let descriptor = ProjectDescriptor::from_snapshot(
        project(),
        "1111111111111111111111111111111111111111",
        &base,
    )
    .unwrap();
    let base_state = descriptor.initial_file_identity_state().unwrap();
    let result = snapshot(&[(b"new.rs", b"after")]);
    assert!(
        advance_file_identities(
            &project(),
            &base_state,
            "proposal-edited-rename",
            &base,
            &result,
            &[FileOperation::Rename {
                from: b"old.rs".to_vec(),
                to: b"new.rs".to_vec(),
            }],
        )
        .is_err()
    );
    let unknown = advance_file_identities(
        &project(),
        &base_state,
        "proposal-unknown-edit",
        &base,
        &result,
        &[],
    )
    .unwrap();
    assert!(
        unknown
            .changes()
            .iter()
            .any(|change| change.relation == IdentityRelation::New)
    );
}

#[test]
fn copy_and_split_create_new_ids_with_explicit_derivation() {
    let base = snapshot(&[(b"source.rs", b"original")]);
    let descriptor = ProjectDescriptor::from_snapshot(
        project(),
        "1111111111111111111111111111111111111111",
        &base,
    )
    .unwrap();
    let base_ids = descriptor.file_map();
    let base_state = descriptor.initial_file_identity_state().unwrap();
    let copy_result = snapshot(&[(b"copy.rs", b"original"), (b"source.rs", b"original")]);
    let copy = advance_file_identities(
        &project(),
        &base_state,
        "proposal-copy",
        &base,
        &copy_result,
        &[FileOperation::Copy {
            from: b"source.rs".to_vec(),
            to: b"copy.rs".to_vec(),
        }],
    )
    .unwrap();
    let copied = copy
        .result_files()
        .iter()
        .find(|file| file.path == b"copy.rs")
        .unwrap();
    assert_ne!(copied.file_id, base_ids[&b"source.rs"[..]]);
    assert!(copy.changes().iter().any(|change| {
        change.relation == IdentityRelation::ExplicitCopy
            && change.derived_from == vec![base_ids[&b"source.rs"[..]].clone()]
    }));

    let split_result = snapshot(&[(b"part-a.rs", b"first"), (b"part-b.rs", b"second")]);
    let split = advance_file_identities(
        &project(),
        &base_state,
        "proposal-split",
        &base,
        &split_result,
        &[FileOperation::Split {
            from: b"source.rs".to_vec(),
            to: vec![b"part-a.rs".to_vec(), b"part-b.rs".to_vec()],
        }],
    )
    .unwrap();
    let ids: Vec<&FileId> = split
        .result_files()
        .iter()
        .map(|file| &file.file_id)
        .collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
    assert!(
        split
            .changes()
            .iter()
            .filter(|change| { change.relation == IdentityRelation::ExplicitSplit })
            .all(|change| change.derived_from == vec![base_ids[&b"source.rs"[..]].clone()])
    );
}

#[test]
fn competing_new_files_at_the_same_path_conflict_even_with_equal_bytes() {
    let base = snapshot(&[(b"base.rs", b"base")]);
    let descriptor = ProjectDescriptor::from_snapshot(
        project(),
        "1111111111111111111111111111111111111111",
        &base,
    )
    .unwrap();
    let base_state = descriptor.initial_file_identity_state().unwrap();
    let result = snapshot(&[(b"base.rs", b"base"), (b"new.rs", b"same")]);
    let left = advance_file_identities(
        &project(),
        &base_state,
        "proposal-left",
        &base,
        &result,
        &[],
    )
    .unwrap();
    let right = advance_file_identities(
        &project(),
        &base_state,
        "proposal-right",
        &base,
        &result,
        &[],
    )
    .unwrap();
    let conflict = validate_file_proposals(&[left, right]).unwrap_err();
    assert_eq!(conflict.path, b"new.rs");
    assert_eq!(conflict.proposal_ids, ["proposal-left", "proposal-right"]);
}

#[test]
fn competing_explicit_renames_to_the_same_new_path_conflict() {
    let base = snapshot(&[(b"old-a.rs", b"same"), (b"old-b.rs", b"same")]);
    let descriptor = ProjectDescriptor::from_snapshot(
        project(),
        "1111111111111111111111111111111111111111",
        &base,
    )
    .unwrap();
    let base_state = descriptor.initial_file_identity_state().unwrap();
    let result = snapshot(&[(b"new.rs", b"same")]);
    let left = advance_file_identities(
        &project(),
        &base_state,
        "proposal-rename-a",
        &base,
        &result,
        &[FileOperation::Rename {
            from: b"old-a.rs".to_vec(),
            to: b"new.rs".to_vec(),
        }],
    )
    .unwrap();
    let right = advance_file_identities(
        &project(),
        &base_state,
        "proposal-rename-b",
        &base,
        &result,
        &[FileOperation::Rename {
            from: b"old-b.rs".to_vec(),
            to: b"new.rs".to_vec(),
        }],
    )
    .unwrap();

    let conflict = validate_file_proposals(&[left, right]).unwrap_err();
    assert_eq!(
        conflict.kind,
        aethyme_contracts::experimental_v0::FileNamespaceConflictKind::CompetingPath
    );
    assert_eq!(conflict.path, b"new.rs");
    assert_eq!(
        conflict.proposal_ids,
        ["proposal-rename-a", "proposal-rename-b"]
    );
}

#[test]
fn foreign_project_identity_state_and_wrong_base_snapshot_are_refused() {
    let base = snapshot(&[(b"a.rs", b"a")]);
    let result = snapshot(&[(b"a.rs", b"a")]);
    let foreign = ProjectDescriptor::from_snapshot(
        ProjectId::parse(&format!("proj:{}a", "b".repeat(25))).unwrap(),
        "1111111111111111111111111111111111111111",
        &base,
    )
    .unwrap()
    .initial_file_identity_state()
    .unwrap();
    assert!(
        advance_file_identities(
            &project(),
            &foreign,
            "proposal-foreign",
            &base,
            &result,
            &[]
        )
        .is_err()
    );

    let local = ProjectDescriptor::from_snapshot(
        project(),
        "1111111111111111111111111111111111111111",
        &base,
    )
    .unwrap()
    .initial_file_identity_state()
    .unwrap();
    let wrong_base = snapshot(&[(b"a.rs", b"changed")]);
    assert!(
        advance_file_identities(
            &project(),
            &local,
            "proposal-stale-state",
            &wrong_base,
            &result,
            &[]
        )
        .is_err()
    );
}

#[test]
fn descriptor_admission_matches_the_canonical_document_and_container_limits() {
    use aethyme_contracts::experimental_v0::canonical_json::{
        MAX_CONTAINER_ENTRIES, MAX_DOCUMENT_BYTES,
    };

    assert_eq!(MAX_DESCRIPTOR_BYTES, MAX_DOCUMENT_BYTES);
    assert_eq!(MAX_DESCRIPTOR_FILES, MAX_CONTAINER_ENTRIES);

    let too_many_entries = SourceSnapshot::new((0..=MAX_DESCRIPTOR_FILES).map(|index| {
        SourceEntry::from_content(
            format!("file-{index:04}.txt").into_bytes(),
            EntryKind::Regular,
            b"x",
        )
    }))
    .unwrap();
    assert!(matches!(
        ProjectDescriptor::from_snapshot(
            project(),
            "1111111111111111111111111111111111111111",
            &too_many_entries,
        ),
        Err(ProjectIdentityError::TooManyFiles { count })
            if count == MAX_DESCRIPTOR_FILES + 1
    ));

    let long_paths = SourceSnapshot::new((0..128).map(|index| {
        let mut path = format!("f{index:03}/").into_bytes();
        path.resize(4096, b'a');
        SourceEntry::from_content(path, EntryKind::Regular, b"x")
    }))
    .unwrap();
    assert!(matches!(
        ProjectDescriptor::from_snapshot(
            project(),
            "1111111111111111111111111111111111111111",
            &long_paths,
        ),
        Err(ProjectIdentityError::DescriptorTooLarge { bytes })
            if bytes > MAX_DESCRIPTOR_BYTES
    ));
}
