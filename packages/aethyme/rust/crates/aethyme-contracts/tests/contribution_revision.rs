//! Contribution revision qualification vectors for the bounded Local profile.

use aethyme_contracts::experimental_v0::{
    BriefReference, ContributionId, ContributionRevision, EntryKind, FileId, ProjectDescriptor,
    ProjectId, RecordId, SourceEntry, SourceSnapshot, advance_file_identities,
    canonical_json::{MAX_STRING_BYTES, Object, Value},
    contribution_revision::{
        ContributionRevisionError, MAX_CONTRIBUTION_REVISION_BYTES, MAX_REVISION_CHANGES,
    },
};

fn project() -> ProjectId {
    ProjectId::parse("proj:aaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap()
}

fn empty_revision_with_brief(
    brief: Option<BriefReference>,
) -> Result<ContributionRevision, ContributionRevisionError> {
    let snapshot = SourceSnapshot::new(std::iter::empty::<SourceEntry>()).unwrap();
    ContributionRevision::new(
        project(),
        ContributionId::parse(&format!("contrib:{}e", "b".repeat(25))).unwrap(),
        RecordId::parse(&format!("sha256:{}", "1".repeat(64))).unwrap(),
        snapshot.id(),
        snapshot.id(),
        Vec::new(),
        brief,
        Vec::new(),
    )
}

fn lower_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = Vec::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(DIGITS[usize::from(byte >> 4)]);
        encoded.push(DIGITS[usize::from(byte & 0x0f)]);
    }
    String::from_utf8(encoded).unwrap()
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

#[test]
fn constructor_rejects_a_revision_larger_than_the_canonical_record_limit() {
    let snapshot = SourceSnapshot::new(std::iter::empty::<SourceEntry>()).unwrap();
    let contribution = ContributionId::parse(&format!("contrib:{}e", "b".repeat(25))).unwrap();
    let capture_record_id = RecordId::parse(&format!("sha256:{}", "1".repeat(64))).unwrap();
    let file_id = FileId::parse(&format!("file:sha256:{}", "2".repeat(64))).unwrap();
    let changes = (0..130)
        .map(|index| {
            let prefix = format!("{index:03}");
            let path = format!("{prefix}{}", "x".repeat(4096 - prefix.len()));
            let after = Object::new(vec![
                ("content_sha256".into(), Value::String("3".repeat(64))),
                ("file_id".into(), Value::String(file_id.as_str().into())),
                ("mode".into(), Value::String("100644".into())),
                ("path_hex".into(), Value::String(lower_hex(path.as_bytes()))),
            ])
            .unwrap();
            Value::Object(
                Object::new(vec![
                    ("after".into(), Value::Object(after)),
                    ("derived_from".into(), Value::Array(Vec::new())),
                    ("possible_sources".into(), Value::Array(Vec::new())),
                    ("relation".into(), Value::String("new".into())),
                ])
                .unwrap(),
            )
        })
        .collect();

    let result = ContributionRevision::new(
        project(),
        contribution,
        capture_record_id,
        snapshot.id(),
        snapshot.id(),
        changes,
        None,
        Vec::new(),
    );
    assert!(matches!(
        result,
        Err(ContributionRevisionError::TooLarge(bytes))
            if bytes > MAX_CONTRIBUTION_REVISION_BYTES
    ));
}

#[test]
fn constructor_preflights_aggregate_values_before_semantic_cloning() {
    let snapshot = SourceSnapshot::new(std::iter::empty::<SourceEntry>()).unwrap();
    let contribution = ContributionId::parse(&format!("contrib:{}e", "b".repeat(25))).unwrap();
    let capture_record_id = RecordId::parse(&format!("sha256:{}", "1".repeat(64))).unwrap();
    let changes = (0..MAX_REVISION_CHANGES)
        .map(|_| Value::String("x".repeat(300)))
        .collect();

    let result = ContributionRevision::new(
        project(),
        contribution,
        capture_record_id,
        snapshot.id(),
        snapshot.id(),
        changes,
        None,
        Vec::new(),
    );
    assert!(matches!(
        result,
        Err(ContributionRevisionError::TooLarge(bytes))
            if bytes > MAX_CONTRIBUTION_REVISION_BYTES
    ));
}

#[test]
fn constructor_revalidates_public_brief_reference_literals() {
    let record_id = RecordId::parse(&format!("sha256:{}", "2".repeat(64))).unwrap();
    let malformed = BriefReference {
        record_id: record_id.clone(),
        object_sha256: "x".into(),
    };
    assert!(matches!(
        empty_revision_with_brief(Some(malformed)),
        Err(ContributionRevisionError::InvalidBriefDigest)
    ));

    let oversized = BriefReference {
        record_id,
        object_sha256: "a".repeat(MAX_STRING_BYTES + 1),
    };
    assert!(matches!(
        empty_revision_with_brief(Some(oversized)),
        Err(ContributionRevisionError::InvalidBriefDigest)
    ));
}
