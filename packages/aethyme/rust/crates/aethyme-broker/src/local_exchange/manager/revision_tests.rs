use super::paths_within_peer_scope;
use crate::collaboration_capture::{self, OperationId};
use crate::collaboration_cli::project_test_support::BoundProjectFixture;
use crate::collaboration_gc::{self, GcOptions};
use crate::collaboration_identity_store::{
    ContributionRevisionRequest, ResolvedContributionRevision, resolve_bound_contribution_revision,
    revise_contribution_for_operation,
};
use crate::local_exchange::{
    Cursor, Event, Metadata, MetadataAck, NativeAuthorizationLease, PROFILE, PeerSpec, Scope,
    SnapshotDescriptor, SnapshotPage, SourceAvailability, StreamWitness, TrustedPeer,
};
use aethyme_contracts::experimental_v0::brief::Brief;
use aethyme_contracts::experimental_v0::{Record, RecordId};
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn empty_or_out_of_scope_packet_paths_are_not_visible_to_path_scope() {
    let peer_scope = vec!["src".to_string()];

    assert!(!paths_within_peer_scope(&peer_scope, &[]));
    assert!(paths_within_peer_scope(
        &peer_scope,
        &["src/lib.rs".to_string()]
    ));
    assert!(!paths_within_peer_scope(
        &peer_scope,
        &["tests/case.rs".to_string()]
    ));
    assert!(!paths_within_peer_scope(
        &peer_scope,
        &["src-other/lib.rs".to_string()]
    ));
}

struct SnapshotLease {
    stream: StreamWitness,
    expires_ms: i64,
}

impl NativeAuthorizationLease for SnapshotLease {
    fn stream(&self) -> &StreamWitness {
        &self.stream
    }

    fn valid_until_ms(&self) -> i64 {
        self.expires_ms
    }

    fn check_current(&self) -> Result<(), super::ExchangeError> {
        Ok(())
    }

    fn visible(&self, _packet: &Metadata) -> bool {
        true
    }
}

struct RevisionSnapshotPeer {
    packet: Metadata,
}

impl RevisionSnapshotPeer {
    fn stream(spec: &PeerSpec, binding: &RecordId) -> StreamWitness {
        StreamWitness {
            binding: binding.to_string(),
            reader: spec.reader.clone(),
            scope_digest: super::scope_id(spec).unwrap().to_string(),
            visibility_generation: "acl-0".into(),
            incarnation: "revision-snapshot-test".into(),
            remote_authority_epoch: "revision-snapshot-epoch".into(),
        }
    }
}

impl TrustedPeer for RevisionSnapshotPeer {
    fn profile(&self) -> &str {
        PROFILE
    }

    fn service_id(&self) -> &str {
        "coord.example"
    }

    fn authorize(
        &self,
        spec: &PeerSpec,
        binding: &RecordId,
    ) -> Result<Box<dyn NativeAuthorizationLease + '_>, super::ExchangeError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| super::ExchangeError::Invalid("clock".into()))?
            .as_millis() as i64;
        Ok(Box::new(SnapshotLease {
            stream: Self::stream(spec, binding),
            expires_ms: now + 60_000,
        }))
    }

    fn send(
        &self,
        _lease: &dyn NativeAuthorizationLease,
        _message: &Record,
    ) -> Result<MetadataAck, super::ExchangeError> {
        Err(super::ExchangeError::Unsupported)
    }

    fn events(
        &self,
        _lease: &dyn NativeAuthorizationLease,
        _cursor: &Cursor,
    ) -> Result<Vec<Event>, super::ExchangeError> {
        Ok(Vec::new())
    }

    fn snapshot(
        &self,
        lease: &dyn NativeAuthorizationLease,
        pinned: Option<&SnapshotDescriptor>,
        offset: u32,
    ) -> Result<SnapshotPage, super::ExchangeError> {
        if offset != 0 || pinned.is_some() {
            return Err(super::ExchangeError::Incomplete);
        }
        let identity = RecordId::parse(&format!("sha256:{}", "c".repeat(64)))
            .map_err(|_| super::ExchangeError::Invalid("snapshot identity".into()))?;
        let stream = lease.stream().clone();
        let event = Event {
            stream: stream.clone(),
            sequence: 1,
            packet: self.packet.clone(),
        };
        Ok(SnapshotPage {
            snapshot: SnapshotDescriptor {
                stream,
                identity,
                item_count: 1,
                watermark: 1,
            },
            offset: 0,
            items: vec![event],
            next_offset: None,
        })
    }
}

fn revision_packet(
    fixture: &BoundProjectFixture,
    capture: &crate::collaboration_capture::CaptureReceipt,
    revision: &ResolvedContributionRevision,
) -> Metadata {
    let paths = revision
        .identity_transition()
        .changes()
        .iter()
        .flat_map(|change| [change.before.as_ref(), change.after.as_ref()])
        .flatten()
        .map(|state| String::from_utf8(state.path.clone()).unwrap())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    Metadata::revision_contribution(
        fixture.binding.project_id(),
        fixture.binding.descriptor_record_id(),
        &fixture.binding.descriptor_object().hex(),
        &capture.receipt_record_id,
        &revision.revision().to_record().0,
        revision.brief_record_bytes(),
        SourceAvailability::LocalOnly,
        paths,
    )
    .unwrap()
}

#[test]
fn received_unindexed_revision_keeps_its_source_and_transitive_proof_after_capture_release() {
    let mut source = BoundProjectFixture::new().unwrap();
    let result = source
        .commit_file("src/peer.rs", b"pub fn received() {}\n")
        .unwrap();
    let source_capture = source
        .capture_operation("source-revision", &source.bootstrap_commit, &result)
        .unwrap();
    let operation = OperationId::parse("source-revision").unwrap();
    let brief = Brief {
        intent: "retain the exact rationale with the received source revision".into(),
        ..Brief::default()
    };
    let source_revision = revise_contribution_for_operation(
        &mut source.store,
        &source.binding,
        ContributionRevisionRequest {
            operation_id: &operation,
            file_operations: None,
            brief: Some(&brief),
            depends_on: None,
        },
    )
    .unwrap();
    let packet = revision_packet(&source, &source_capture, &source_revision);

    let mut receiver = source.clone_and_bind().unwrap();
    let receiver_capture = receiver
        .capture_operation("receiver-source", &source.bootstrap_commit, &result)
        .unwrap();
    let native = RevisionSnapshotPeer {
        packet: packet.clone(),
    };
    let spec = PeerSpec {
        profile: PROFILE.into(),
        service_id: "coord.example".into(),
        project: "playground".into(),
        audience: "local-test".into(),
        reader: "revision-reader".into(),
        scope: Scope::Project,
        max_attempts: 1,
        deadline_seconds: 5,
    };
    let appointed = super::appoint(&mut receiver.store, spec, &native).unwrap();
    super::recover_snapshot(&mut receiver.store, &appointed, "receiver-proof").unwrap();

    let indexed: bool = receiver
        .store
        .read_connection()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM contribution_revisions WHERE revision_record_id=?1)",
            [source_revision.record_id().as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        !indexed,
        "received revision unexpectedly gained a local index row"
    );
    assert!(
        resolve_bound_contribution_revision(
            &receiver.store,
            &receiver.binding,
            source_revision.record_id(),
            source_revision.object_digest(),
            source_revision.revision().contribution_id(),
        )
        .is_err()
    );

    collaboration_capture::release_retention(
        &mut receiver.store,
        &receiver_capture.operation_id,
        &receiver_capture.receipt_record_id,
    )
    .unwrap();
    let verified = super::validate_revision_project_binding(&mut receiver.store, &packet)
        .unwrap()
        .expect("revision packet has a validated proof bundle");
    let expected_current_snapshots = std::collections::BTreeSet::from([
        source_revision.revision().base().clone(),
        source_revision.revision().result().clone(),
    ]);
    assert!(
        verified
            .source_snapshots
            .is_superset(&expected_current_snapshots)
    );

    for snapshot in &verified.source_snapshots {
        let holder = format!("exchange-revision-source:{}", snapshot.as_str());
        let rooted: bool = receiver
            .store
            .read_connection()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM reader_leases
                 WHERE target_kind='snapshot' AND target=?1 AND holder=?2
                   AND released_ms IS NULL)",
                rusqlite::params![snapshot.as_str(), holder],
                |row| row.get(0),
            )
            .unwrap();
        assert!(rooted, "received source snapshot was not retained");
    }
    for digest in &verified.object_digests {
        let holder = format!("exchange-revision-proof:{}", digest.hex());
        let rooted: bool = receiver
            .store
            .read_connection()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM object_pins
                 WHERE class='cited_evidence' AND object_sha256=?1 AND holder=?2
                   AND released_ms IS NULL)",
                rusqlite::params![digest.hex(), holder],
                |row| row.get(0),
            )
            .unwrap();
        assert!(rooted, "received proof object was not retained");
    }

    let gc_plan = collaboration_gc::plan(&mut receiver.store, &GcOptions::default()).unwrap();
    assert!(gc_plan.reclaimable.iter().all(|item| {
        verified.source_snapshots.iter().all(|snapshot| {
            item.entity != collaboration_gc::GcEntity::Snapshot(snapshot.as_str().into())
        })
    }));
    assert!(gc_plan.reclaimable.iter().all(|item| {
        verified.object_digests.iter().all(|digest| {
            item.relpath
                != format!(
                    "objects/sha256/{}/{}",
                    &digest.hex()[..2],
                    &digest.hex()[2..]
                )
        })
    }));
}
