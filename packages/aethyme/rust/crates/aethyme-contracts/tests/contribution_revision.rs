//! Contribution revision qualification vectors for the bounded Local profile.

use aethyme_contracts::experimental_v0::{
    BriefReference, ContributionId, ContributionRevision, EntryKind, ProjectDescriptor, ProjectId,
    RecordId, SourceEntry, SourceSnapshot, advance_file_identities,
};

fn project() -> ProjectId {
    ProjectId::parse("proj:aaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap()
}

#[test]
fn rationale_only_brief_change_creates_a_distinct_immutable_revision() {
    let snapshot = SourceSnapshot::new([SourceEntry::from_content(
        b"src/lib.rs".to_vec(),
        EntryKind::Regular,
        b"fn main() {}\n",
    )])
    .unwrap();
    let contribution = ContributionId::parse(&format!("contrib:{}", "b".repeat(25) + "e")).unwrap();
    let capture_record_id = RecordId::parse(&format!("sha256:{}", "1".repeat(64))).unwrap();
    let original = ContributionRevision::new(
        project(),
        contribution.clone(),
        capture_record_id.clone(),
        snapshot.id(),
        snapshot.id(),
        Vec::new(),
        None,
        Vec::new(),
    )
    .unwrap();
    let brief = BriefReference::new(
        RecordId::parse(&format!("sha256:{}", "2".repeat(64))).unwrap(),
        "3".repeat(64),
    )
    .unwrap();
    let revised = ContributionRevision::new(
        project(),
        contribution,
        capture_record_id,
        snapshot.id(),
        snapshot.id(),
        Vec::new(),
        Some(brief),
        Vec::new(),
    )
    .unwrap();

    let (original_bytes, original_id) = original.to_record();
    let (revised_bytes, revised_id) = revised.to_record();
    assert_ne!(original_id, revised_id);
    assert_ne!(original_bytes, revised_bytes);
    let (decoded, decoded_id) = ContributionRevision::from_record(&revised_bytes).unwrap();
    assert_eq!(decoded, revised);
    assert_eq!(decoded_id, revised_id);
}

#[test]
fn new_file_revision_round_trips_without_json_null_state() {
    let project = project();
    let base = SourceSnapshot::new(std::iter::empty::<SourceEntry>()).unwrap();
    let descriptor = ProjectDescriptor::from_snapshot(
        project.clone(),
        "1111111111111111111111111111111111111111",
        &base,
    )
    .unwrap();
    let result = SourceSnapshot::new([SourceEntry::from_content(
        b"new.rs".to_vec(),
        EntryKind::Regular,
        b"new file",
    )])
    .unwrap();
    let transition = advance_file_identities(
        &project,
        &descriptor.initial_file_identity_state().unwrap(),
        "proposal-new-file",
        &base,
        &result,
        &[],
    )
    .unwrap();
    let source_changes: Vec<_> = transition
        .changes()
        .iter()
        .map(|change| change.to_value())
        .collect();
    let aethyme_contracts::experimental_v0::canonical_json::Value::Object(change) =
        &source_changes[0]
    else {
        panic!("source change is an object");
    };
    assert!(change.get("before").is_none());
    assert!(change.get("after").is_some());

    let revision = ContributionRevision::new(
        project,
        ContributionId::parse(&format!("contrib:{}e", "b".repeat(25))).unwrap(),
        RecordId::parse(&format!("sha256:{}", "1".repeat(64))).unwrap(),
        base.id(),
        result.id(),
        source_changes,
        None,
        Vec::new(),
    )
    .unwrap();
    let (bytes, id) = revision.to_record();
    let (decoded, decoded_id) = ContributionRevision::from_record(&bytes).unwrap();
    assert_eq!(decoded, revision);
    assert_eq!(decoded_id, id);
}
