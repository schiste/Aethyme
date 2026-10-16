//! Contribution revision qualification vectors for the bounded Local profile.

use aethyme_contracts::experimental_v0::{
    BriefReference, ContributionRevision, EntryKind, ProjectId, RecordId, SourceEntry,
    SourceSnapshot,
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
    let contribution = RecordId::parse(&format!("sha256:{}", "1".repeat(64))).unwrap();
    let original = ContributionRevision::new(
        project(),
        contribution.clone(),
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
