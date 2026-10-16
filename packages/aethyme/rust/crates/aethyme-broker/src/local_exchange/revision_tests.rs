use super::*;
use aethyme_contracts::experimental_v0::brief::Brief;
use aethyme_contracts::experimental_v0::{
    BriefReference, ContributionRevision, EntryKind, ProjectDescriptor, SourceEntry,
    SourceSnapshot, advance_file_identities,
};

fn sha(byte: char) -> String {
    format!("sha256:{}", byte.to_string().repeat(64))
}

fn project_id() -> ProjectId {
    ProjectId::parse("proj:aaaaaaaaaaaaaaaaaaaaaaaaaa").expect("project ID")
}

fn logical_id() -> ContributionId {
    ContributionId::parse(&format!("contrib:{}e", "b".repeat(25))).expect("logical ID")
}

fn captured_revision(
    brief: Option<(Vec<u8>, RecordId, String)>,
    many_changes: bool,
) -> (Vec<u8>, RecordId) {
    let (base, source, source_changes) = if many_changes {
        let base =
            SourceSnapshot::new(std::iter::empty::<SourceEntry>()).expect("empty source snapshot");
        let project = project_id();
        let descriptor = ProjectDescriptor::from_snapshot(
            project.clone(),
            "1111111111111111111111111111111111111111",
            &base,
        )
        .expect("descriptor");
        let source = SourceSnapshot::new((0..128).map(|index| {
            SourceEntry::from_content(
                format!("src/module-{index:04}.rs").into_bytes(),
                EntryKind::Regular,
                b"pub fn item() {}\n",
            )
        }))
        .expect("large source snapshot");
        let transition = advance_file_identities(
            &project,
            &descriptor
                .initial_file_identity_state()
                .expect("initial IDs"),
            "exchange-large-test",
            &base,
            &source,
            &[],
        )
        .expect("identity transition");
        let changes = transition
            .changes()
            .iter()
            .map(|change| change.to_value())
            .collect();
        (base, source, changes)
    } else {
        let source = SourceSnapshot::new([SourceEntry::from_content(
            b"src/lib.rs".to_vec(),
            EntryKind::Regular,
            b"fn main() {}\n",
        )])
        .expect("source snapshot");
        (source.clone(), source, Vec::new())
    };
    let reference = brief.map(|(bytes, id, digest)| {
        assert!(!bytes.is_empty());
        BriefReference::new(id, digest).expect("brief reference")
    });
    ContributionRevision::new(
        project_id(),
        logical_id(),
        RecordId::parse(&sha('c')).expect("capture lineage"),
        base.id(),
        source.id(),
        source_changes,
        reference,
        Vec::new(),
    )
    .expect("revision")
    .to_record()
}

fn packet_with_brief(include_brief: bool, many_changes: bool) -> Metadata {
    let brief = include_brief.then(|| {
        let brief = Brief::from_decision_file(
            br#"{"intent":"preserve revision meaning","decisions":[{"scope_ref":"source","choice":"keep","reason":"the exact decision is evidence"}]}"#,
        )
        .expect("brief");
        let (bytes, id) = brief.to_record();
        let digest = crate::collaboration_archive::ObjectDigest::of(&bytes)
            .hex()
            .to_string();
        (bytes, id, digest)
    });
    let (revision_bytes, revision_id) = captured_revision(brief.clone(), many_changes);
    let revision_digest = crate::collaboration_archive::ObjectDigest::of(&revision_bytes)
        .hex()
        .to_string();
    Metadata::RevisionContribution {
        project_id: project_id().to_string(),
        descriptor_record_id: sha('d'),
        descriptor_object_sha256: "e".repeat(64),
        logical_contribution_id: logical_id().to_string(),
        capture_record_id: sha('c'),
        capture_receipt_record_id: sha('f'),
        revision_record_id: revision_id.to_string(),
        revision_object_sha256: revision_digest,
        revision_bytes_hex: BoundedHexPayload::from_bytes(&revision_bytes)
            .expect("bounded revision payload"),
        brief_record_id: brief.as_ref().map(|(_, id, _)| id.to_string()),
        brief_object_sha256: brief.as_ref().map(|(_, _, digest)| digest.clone()),
        brief_bytes_hex: brief.as_ref().map(|(bytes, _, _)| lower_hex(bytes)),
        source: ContributionRevision::from_record(&revision_bytes)
            .expect("revision")
            .0
            .result()
            .to_string(),
        availability: SourceAvailability::LocalOnly,
        scope: vec!["src/lib.rs".into()],
    }
}

fn large_packet_with_scope(
    revision_bytes: &[u8],
    scope: Vec<String>,
    capture_receipt_record_id: String,
) -> Metadata {
    let (revision, revision_id) =
        ContributionRevision::from_record(revision_bytes).expect("large revision");
    Metadata::RevisionContribution {
        project_id: revision.project_id().to_string(),
        descriptor_record_id: sha('d'),
        descriptor_object_sha256: "e".repeat(64),
        logical_contribution_id: revision.contribution_id().to_string(),
        capture_record_id: revision.capture_record_id().to_string(),
        capture_receipt_record_id,
        revision_record_id: revision_id.to_string(),
        revision_object_sha256: crate::collaboration_archive::ObjectDigest::of(revision_bytes)
            .hex()
            .to_string(),
        revision_bytes_hex: BoundedHexPayload::from_bytes(revision_bytes)
            .expect("bounded revision payload"),
        brief_record_id: None,
        brief_object_sha256: None,
        brief_bytes_hex: None,
        source: revision.result().to_string(),
        availability: SourceAvailability::LocalOnly,
        scope,
    }
}

fn large_scope_paths() -> Vec<String> {
    let path_bytes = 1024;
    (0..64)
        .map(|index| {
            let prefix = format!("src/{index:04}-");
            format!("{}{}", prefix, "x".repeat(path_bytes - prefix.len()))
        })
        .collect()
}

fn large_revision_bytes() -> Vec<u8> {
    let base = SourceSnapshot::new(std::iter::empty::<SourceEntry>()).expect("empty base snapshot");
    let project = project_id();
    let descriptor = ProjectDescriptor::from_snapshot(
        project.clone(),
        "1111111111111111111111111111111111111111",
        &base,
    )
    .expect("descriptor");
    let result = SourceSnapshot::new(large_scope_paths().iter().map(|path| {
        SourceEntry::from_content(
            path.as_bytes().to_vec(),
            EntryKind::Regular,
            b"new source\n",
        )
    }))
    .expect("large result snapshot");
    let logical_id = logical_id();
    let transition = advance_file_identities(
        &project,
        &descriptor
            .initial_file_identity_state()
            .expect("initial IDs"),
        logical_id.as_str(),
        &base,
        &result,
        &[],
    )
    .expect("new-file identity transition");
    let changes = transition
        .changes()
        .iter()
        .map(|change| change.to_value())
        .collect();
    let (bytes, _) = ContributionRevision::new(
        project,
        logical_id,
        RecordId::parse(&sha('c')).expect("capture lineage"),
        base.id(),
        result.id(),
        changes,
        None,
        // These are valid unique wire references; resolution is outside this
        // transport-size fixture, which exercises only canonical byte bounds.
        (1..=3000)
            .map(|index| {
                RecordId::parse(&format!("sha256:{index:064x}"))
                    .expect("valid dependency record ID")
            })
            .collect(),
    )
    .expect("large revision")
    .to_record();
    assert!(bytes.len() <= MAX_REVISION_OBJECT_BYTES_IN_PACKET);
    bytes
}

#[test]
fn revision_exchange_binds_exact_records_and_preserves_advisory_role() {
    let packet = packet_with_brief(true, true);
    let Metadata::RevisionContribution {
        revision_bytes_hex: BoundedHexPayload(HexPayloadWire::Chunks(chunks)),
        ..
    } = &packet
    else {
        panic!("large revision must use bounded chunks");
    };
    assert!(chunks.len() > 1);
    let record = packet.record().expect("bounded revision packet");
    assert!(record.canonical_bytes().len() > MAX_PACKET_BYTES);
    assert!(record.canonical_bytes().len() <= MAX_REVISION_PACKET_BYTES);
    let value: serde_json::Value = serde_json::from_slice(&record.canonical_bytes()).unwrap();
    let decoded: Metadata = serde_json::from_value(value["spec"].clone()).unwrap();
    assert_eq!(decoded.record().unwrap().id(), record.id());
    assert_eq!(DATA_ROLE, "untrusted_advisory_metadata");
}

#[test]
fn revision_exchange_refuses_mismatched_project_lineage_source_and_digest() {
    let packet = packet_with_brief(false, false);
    let mut changed = packet.clone();
    if let Metadata::RevisionContribution { project_id, .. } = &mut changed {
        *project_id = format!("proj:{}e", "c".repeat(25));
    }
    assert!(changed.record().is_err());

    let mut changed = packet.clone();
    if let Metadata::RevisionContribution {
        logical_contribution_id,
        ..
    } = &mut changed
    {
        *logical_contribution_id = format!("contrib:{}e", "c".repeat(25));
    }
    assert!(changed.record().is_err());

    let mut changed = packet.clone();
    if let Metadata::RevisionContribution { source, .. } = &mut changed {
        *source = sha('9');
    }
    assert!(changed.record().is_err());

    let mut changed = packet;
    if let Metadata::RevisionContribution {
        revision_object_sha256,
        ..
    } = &mut changed
    {
        *revision_object_sha256 = "0".repeat(64);
    }
    assert!(changed.record().is_err());
}

#[test]
fn revision_exchange_rejects_incomplete_brief_and_oversized_hex_before_decode() {
    let mut incomplete = packet_with_brief(true, false);
    if let Metadata::RevisionContribution {
        brief_bytes_hex, ..
    } = &mut incomplete
    {
        *brief_bytes_hex = None;
    }
    assert!(incomplete.record().is_err());

    let mut oversized = packet_with_brief(false, false);
    if let Metadata::RevisionContribution {
        revision_bytes_hex, ..
    } = &mut oversized
    {
        *revision_bytes_hex = BoundedHexPayload(HexPayloadWire::Inline(
            "0".repeat(MAX_REVISION_HEX_CHUNK_CHARS + 2),
        ));
    }
    assert!(matches!(oversized.record(), Err(ExchangeError::Limit)));
}

#[test]
fn revision_hex_chunks_require_one_bounded_canonical_shape() {
    let raw = vec![0x5a; MAX_REVISION_HEX_CHUNK_BYTES + 1];
    let encoded = BoundedHexPayload::from_bytes(&raw).unwrap();
    let HexPayloadWire::Chunks(chunks) = &encoded.0 else {
        panic!("payload over the inline threshold must be chunked");
    };
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].len(), MAX_REVISION_HEX_CHUNK_CHARS);
    assert_eq!(chunks[1].len(), 2);
    assert_eq!(
        encoded.decode(MAX_REVISION_OBJECT_BYTES_IN_PACKET).unwrap(),
        raw
    );

    let unnormalized =
        BoundedHexPayload(HexPayloadWire::Chunks(vec!["aabb".into(), "ccdd".into()]));
    assert!(matches!(
        unnormalized.decode(16),
        Err(ExchangeError::Invalid(_))
    ));

    let short_nonfinal_over_inline_threshold = BoundedHexPayload(HexPayloadWire::Chunks(vec![
        "aa".into(),
        "b".repeat(MAX_REVISION_HEX_CHUNK_CHARS),
    ]));
    let HexPayloadWire::Chunks(chunks) = &short_nonfinal_over_inline_threshold.0 else {
        unreachable!();
    };
    assert_eq!(
        chunks.iter().map(|chunk| chunk.len()).sum::<usize>(),
        MAX_REVISION_HEX_CHUNK_CHARS + 2
    );
    assert!(matches!(
        short_nonfinal_over_inline_threshold.decode(MAX_REVISION_OBJECT_BYTES_IN_PACKET),
        Err(ExchangeError::Invalid(_))
    ));

    let uppercase = BoundedHexPayload(HexPayloadWire::Chunks(vec![
        "A".repeat(MAX_REVISION_HEX_CHUNK_CHARS),
        "bb".into(),
    ]));
    assert!(matches!(
        uppercase.decode(MAX_REVISION_OBJECT_BYTES_IN_PACKET),
        Err(ExchangeError::Invalid(_))
    ));

    let too_many = BoundedHexPayload(HexPayloadWire::Chunks(vec![
        String::new();
        MAX_REVISION_HEX_CHUNKS + 1
    ]));
    assert!(matches!(
        too_many.decode(MAX_REVISION_OBJECT_BYTES_IN_PACKET),
        Err(ExchangeError::Limit)
    ));
}

#[test]
fn complete_event_wire_size_is_preflighted_under_the_generic_record_cap() {
    let event = Event {
        stream: StreamWitness {
            binding: sha('1'),
            reader: "reader".into(),
            scope_digest: sha('2'),
            visibility_generation: "visibility-1".into(),
            incarnation: "incarnation-1".into(),
            remote_authority_epoch: "authority-1".into(),
        },
        sequence: 1,
        packet: packet_with_brief(true, true),
    };
    let record = event.record().expect("complete bounded event");
    assert!(record.canonical_bytes().len() <= MAX_REVISION_PACKET_BYTES);

    let mut oversized_packet = packet_with_brief(false, false);
    let Metadata::RevisionContribution {
        revision_bytes_hex, ..
    } = &mut oversized_packet
    else {
        unreachable!();
    };
    *revision_bytes_hex = BoundedHexPayload(HexPayloadWire::Chunks(vec![
        "0".repeat(
            MAX_REVISION_HEX_CHUNK_CHARS
        );
        MAX_REVISION_HEX_CHUNKS
    ]));
    let oversized_event = Event {
        stream: StreamWitness {
            binding: sha('1'),
            reader: "reader".into(),
            scope_digest: sha('2'),
            visibility_generation: "visibility-1".into(),
            incarnation: "incarnation-1".into(),
            remote_authority_epoch: "authority-1".into(),
        },
        sequence: 1,
        packet: oversized_packet,
    };
    assert!(matches!(
        canonical_with_limit(&EVENT, &oversized_event, MAX_REVISION_PACKET_BYTES),
        Err(ExchangeError::Limit)
    ));
}

#[test]
fn small_revision_payloads_keep_the_legacy_inline_shape() {
    let packet = packet_with_brief(false, false);
    let Metadata::RevisionContribution {
        revision_bytes_hex: BoundedHexPayload(HexPayloadWire::Inline(_)),
        ..
    } = &packet
    else {
        panic!("small revision must retain inline encoding");
    };
    assert!(packet.record().unwrap().canonical_bytes().len() <= MAX_PACKET_BYTES);
}

#[test]
fn legacy_exchange_packet_keeps_the_original_size_profile() {
    let legacy = Metadata::Contribution {
        contribution: sha('a'),
        source: sha('b'),
        availability: SourceAvailability::LocalOnly,
        scope: vec!["src/lib.rs".into()],
        brief: None,
    };
    let expected = format!(
        "{{\"schema\":\"aethyme.exchange-packet/provisional-v0\",\"spec\":{{\"availability\":{{\"kind\":\"local_only\"}},\"contribution\":\"{}\",\"kind\":\"contribution\",\"scope\":[\"src/lib.rs\"],\"source\":\"{}\"}}}}",
        sha('a'),
        sha('b')
    );
    let record = legacy.record().unwrap();
    assert_eq!(record.canonical_bytes(), expected.as_bytes());
    assert!(record.canonical_bytes().len() <= MAX_PACKET_BYTES);
}

#[test]
fn snapshot_page_rejects_aggregate_revision_bytes_over_the_page_cap() {
    // The source paths match the packet scope exactly; valid revision references
    // make each item large enough for one bounded page to cross its 32 MiB cap.
    let revision = large_revision_bytes();
    let scope = large_scope_paths();
    let stream = StreamWitness {
        binding: sha('1'),
        reader: "reader".into(),
        scope_digest: sha('2'),
        visibility_generation: "visibility-1".into(),
        incarnation: "incarnation-1".into(),
        remote_authority_epoch: "authority-1".into(),
    };
    let first_event = Event {
        stream: stream.clone(),
        sequence: 1,
        packet: large_packet_with_scope(&revision, scope.clone(), format!("sha256:{:064x}", 0)),
    };
    let first_event_bytes = first_event.record().unwrap().canonical_bytes().len();
    let item_count = MAX_SNAPSHOT_PAGE_BYTES / first_event_bytes + 1;
    assert!(item_count <= MAX_BATCH_ITEMS);
    let items = (0..item_count)
        .map(|index| Event {
            stream: stream.clone(),
            sequence: index as u64 + 1,
            // Each packet needs its own record identity so the aggregate byte
            // limit, rather than duplicate-event validation, is the first guard.
            packet: large_packet_with_scope(
                &revision,
                scope.clone(),
                format!("sha256:{:064x}", index),
            ),
        })
        .collect::<Vec<_>>();
    assert!(items.len() > 1);
    let receipt_ids = items
        .iter()
        .map(|event| match &event.packet {
            Metadata::RevisionContribution {
                capture_receipt_record_id,
                ..
            } => capture_receipt_record_id,
            _ => panic!("large page fixture must contain revision packets"),
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(receipt_ids.len(), items.len());
    assert!(
        items
            .iter()
            .all(|event| event.record().unwrap().canonical_bytes().len()
                <= MAX_REVISION_PACKET_BYTES)
    );
    assert!(
        items
            .iter()
            .map(|event| event.record().unwrap().canonical_bytes().len())
            .sum::<usize>()
            > MAX_SNAPSHOT_PAGE_BYTES
    );
    let page = SnapshotPage {
        snapshot: SnapshotDescriptor {
            stream: stream.clone(),
            identity: sha('3'),
            item_count: items.len() as u32,
            watermark: items.len() as u64,
        },
        offset: 0,
        items,
        next_offset: None,
    };
    assert!(matches!(
        page.validate_for(&stream, None, 0),
        Err(ExchangeError::Limit)
    ));
}
