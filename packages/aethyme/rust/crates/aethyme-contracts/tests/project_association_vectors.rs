use aethyme_contracts::experimental_v0::{
    ProjectAssociationDescriptor, ProjectId, RecordId, SourceSnapshotId,
    project_association::MAX_PROJECT_ASSOCIATION_BYTES,
};

fn association() -> ProjectAssociationDescriptor {
    ProjectAssociationDescriptor::new(
        "local-one",
        "peer-two",
        ProjectId::parse("proj:aaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
        ProjectId::parse("proj:bbbbbbbbbbbbbbbbbbbbbbbbba").unwrap(),
        RecordId::parse(&format!("sha256:{}", "1".repeat(64))).unwrap(),
        RecordId::parse(&format!("sha256:{}", "2".repeat(64))).unwrap(),
        SourceSnapshotId::parse(&format!("sha256:{}", "3".repeat(64))).unwrap(),
        SourceSnapshotId::parse(&format!("sha256:{}", "4".repeat(64))).unwrap(),
    )
    .unwrap()
}

#[test]
fn association_is_bounded_canonical_and_round_trips_both_project_ids() {
    let association = association();
    let (bytes, id) = association.to_record();
    assert!(bytes.len() <= MAX_PROJECT_ASSOCIATION_BYTES);
    assert!(std::str::from_utf8(&bytes).unwrap().starts_with("{\"local_anchor_snapshot\":"));
    let (decoded, decoded_id) = ProjectAssociationDescriptor::from_record(&bytes).unwrap();
    assert_eq!(decoded, association);
    assert_eq!(decoded_id, id);
    assert_ne!(
        decoded.local_project_id(),
        decoded.peer_project_id(),
        "association never collapses independently enrolled identities"
    );
}

#[test]
fn association_rejects_noncanonical_oversized_and_same_identity_inputs() {
    let (bytes, _) = association().to_record();
    let noncanonical = [b" ".as_slice(), &bytes].concat();
    assert!(ProjectAssociationDescriptor::from_record(&noncanonical).is_err());
    assert!(ProjectAssociationDescriptor::from_record(&vec![b' '; MAX_PROJECT_ASSOCIATION_BYTES + 1]).is_err());

    let error = ProjectAssociationDescriptor::new(
        "local-one",
        "peer-two",
        ProjectId::parse("proj:aaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
        ProjectId::parse("proj:aaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
        RecordId::parse(&format!("sha256:{}", "1".repeat(64))).unwrap(),
        RecordId::parse(&format!("sha256:{}", "2".repeat(64))).unwrap(),
        SourceSnapshotId::parse(&format!("sha256:{}", "3".repeat(64))).unwrap(),
        SourceSnapshotId::parse(&format!("sha256:{}", "4".repeat(64))).unwrap(),
    )
    .unwrap_err();
    assert_eq!(error.code(), "same_project_id");
}
