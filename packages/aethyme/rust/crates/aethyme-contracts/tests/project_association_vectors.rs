use aethyme_contracts::experimental_v0::{
    ProjectAssociationDescriptor, ProjectAssociationNonce, ProjectId, Record, RecordId,
    SourceSnapshotId,
    project_association::{
        MAX_PROJECT_ASSOCIATION_BYTES, PROJECT_ASSOCIATION_PROPOSAL_NONCE_CAPABILITY,
        PROJECT_ASSOCIATION_SCHEMA, PROJECT_ASSOCIATION_SCHEMA_NAME,
    },
    record::{FieldKind, FieldSpec, RecordError, RecordSchema},
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
        ProjectAssociationNonce::parse(format!("sha256:{}", "5".repeat(64))).unwrap(),
    )
    .unwrap()
}

#[test]
fn association_is_bounded_canonical_and_round_trips_both_project_ids() {
    let association = association();
    let (bytes, id) = association.to_record();
    assert!(bytes.len() <= MAX_PROJECT_ASSOCIATION_BYTES);
    assert!(
        std::str::from_utf8(&bytes)
            .unwrap()
            .starts_with("{\"local_anchor_snapshot\":")
    );
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
    assert!(
        ProjectAssociationDescriptor::from_record(&vec![b' '; MAX_PROJECT_ASSOCIATION_BYTES + 1])
            .is_err()
    );

    let error = ProjectAssociationDescriptor::new(
        "local-one",
        "peer-two",
        ProjectId::parse("proj:aaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
        ProjectId::parse("proj:aaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
        RecordId::parse(&format!("sha256:{}", "1".repeat(64))).unwrap(),
        RecordId::parse(&format!("sha256:{}", "2".repeat(64))).unwrap(),
        SourceSnapshotId::parse(&format!("sha256:{}", "3".repeat(64))).unwrap(),
        SourceSnapshotId::parse(&format!("sha256:{}", "4".repeat(64))).unwrap(),
        ProjectAssociationNonce::parse(format!("sha256:{}", "5".repeat(64))).unwrap(),
    )
    .unwrap_err();
    assert_eq!(error.code(), "same_project_id");
}

#[test]
fn each_explicit_proposal_has_a_fresh_bounded_record_id_and_old_readers_refuse_it() {
    let first = association();
    let second = ProjectAssociationDescriptor::new(
        first.local_project_key(),
        first.peer_project_key(),
        first.local_project_id().clone(),
        first.peer_project_id().clone(),
        first.local_descriptor_record_id().clone(),
        first.peer_descriptor_record_id().clone(),
        first.local_anchor_snapshot().clone(),
        first.peer_anchor_snapshot().clone(),
        ProjectAssociationNonce::parse(format!("sha256:{}", "6".repeat(64))).unwrap(),
    )
    .unwrap();
    let (bytes, first_id) = first.to_record();
    let (_, second_id) = second.to_record();
    assert_ne!(first_id, second_id);
    assert!(
        String::from_utf8_lossy(&bytes).contains(PROJECT_ASSOCIATION_PROPOSAL_NONCE_CAPABILITY)
    );

    const OLD_FIELDS: &[FieldSpec] = &[
        FieldSpec {
            name: "local_project_key",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
        FieldSpec {
            name: "peer_project_key",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
        FieldSpec {
            name: "local_project_id",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
        FieldSpec {
            name: "peer_project_id",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
        FieldSpec {
            name: "local_descriptor_record_id",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
        FieldSpec {
            name: "peer_descriptor_record_id",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
        FieldSpec {
            name: "local_anchor_snapshot",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
        FieldSpec {
            name: "peer_anchor_snapshot",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
    ];
    let old_schema = RecordSchema {
        name: PROJECT_ASSOCIATION_SCHEMA_NAME,
        fields: OLD_FIELDS,
        capabilities: &[],
    };
    assert!(matches!(
        Record::decode(&bytes, &[&old_schema]),
        Err(RecordError::UnsupportedCapability { capability })
            if capability == PROJECT_ASSOCIATION_PROPOSAL_NONCE_CAPABILITY
    ));
    let mut without_capability: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    without_capability
        .as_object_mut()
        .unwrap()
        .remove("requires");
    let without_capability = serde_json::to_vec(&without_capability).unwrap();
    assert!(matches!(
        Record::decode(&without_capability, &[&PROJECT_ASSOCIATION_SCHEMA]),
        Err(RecordError::UndeclaredCapability { field, capability })
            if field == "proposal_nonce" && capability == PROJECT_ASSOCIATION_PROPOSAL_NONCE_CAPABILITY
    ));
    assert!(ProjectAssociationNonce::parse("sha256:ABCDEF").is_err());
}
