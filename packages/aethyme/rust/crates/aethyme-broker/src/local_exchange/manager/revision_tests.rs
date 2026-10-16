use super::paths_within_peer_scope;
use crate::collaboration_archive::{self as archive, ObjectDigest};
use crate::collaboration_capture::{self, OperationId};
use crate::collaboration_cli::project_test_support::BoundProjectFixture;
use crate::collaboration_gc::{self, GcOptions};
use crate::collaboration_identity_store::{
    ContributionRevisionRequest, resolve_bound_contribution_revision,
    revise_contribution_for_operation,
};
use crate::local_exchange::{
    Cursor, Event, Metadata, MetadataAck, NativeAuthorizationLease, PROFILE, PeerSpec, Scope,
    SnapshotDescriptor, SnapshotPage, SourceAvailability, StreamWitness, TrustedPeer,
};
use aethyme_contracts::experimental_v0::brief::Brief;
use aethyme_contracts::experimental_v0::{
    BriefReference, ContributionId, ContributionRevision, Record, RecordId, advance_file_identities,
};
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
                identity: identity.to_string(),
                item_count: 1,
                watermark: 1,
            },
            offset: 0,
            items: vec![event],
            next_offset: None,
        })
    }
}

#[test]
fn received_unindexed_revision_keeps_its_source_and_transitive_proof_after_capture_release() {
    let mut receiver = BoundProjectFixture::new().unwrap();
    let parent_result = receiver
        .commit_file("src/parent.rs", b"pub fn parent() {}\n")
        .unwrap();
    let bootstrap_commit = receiver.bootstrap_commit.clone();
    let parent_capture = receiver
        .capture_operation("parent-revision", &bootstrap_commit, &parent_result)
        .unwrap();
    let parent_operation = OperationId::parse("parent-revision").unwrap();
    let parent_brief = Brief {
        intent: "retain the exact rationale for the indexed parent revision".into(),
        ..Brief::default()
    };
    let parent_revision = revise_contribution_for_operation(
        &mut receiver.store,
        &receiver.binding,
        ContributionRevisionRequest {
            operation_id: &parent_operation,
            file_operations: None,
            brief: Some(&parent_brief),
            depends_on: None,
        },
    )
    .unwrap();

    let child_result = receiver
        .commit_file("src/peer.rs", b"pub fn received() {}\n")
        .unwrap();
    let child_capture = receiver
        .capture_operation("child-revision", &parent_result, &child_result)
        .unwrap();
    let base_source = archive::snapshot_of_commit(receiver.repo_path(), &parent_result).unwrap();
    let result_source = archive::snapshot_of_commit(receiver.repo_path(), &child_result).unwrap();
    assert_eq!(
        base_source.id(),
        parent_revision.revision().result().clone()
    );

    let child_contribution_id = if parent_revision.revision().contribution_id().as_str()
        == "contrib:aaaaaaaaaaaaaaaaaaaaaaaaaa"
    {
        ContributionId::parse("contrib:bbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap()
    } else {
        ContributionId::parse("contrib:aaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap()
    };
    let child_transition = advance_file_identities(
        receiver.binding.project_id(),
        parent_revision.result_state().state(),
        child_contribution_id.as_str(),
        &base_source,
        &result_source,
        &[],
    )
    .unwrap();
    let child_brief = Brief {
        intent: "retain the exact rationale for the unindexed child revision".into(),
        ..Brief::default()
    };
    let (child_brief_bytes, child_brief_id) = child_brief.to_record();
    let child_brief_digest = ObjectDigest::of(&child_brief_bytes);
    let child_brief_reference =
        BriefReference::new(child_brief_id, child_brief_digest.hex()).unwrap();
    let child_source_changes = child_transition
        .changes()
        .iter()
        .map(|change| change.to_value())
        .collect();
    let child_revision = ContributionRevision::new(
        receiver.binding.project_id().clone(),
        child_contribution_id.clone(),
        child_capture.contribution.clone(),
        child_capture.base.clone(),
        child_capture.result.clone(),
        child_source_changes,
        Some(child_brief_reference),
        vec![parent_revision.record_id().clone()],
    )
    .unwrap();
    let (child_revision_bytes, child_revision_id) = child_revision.to_record();
    let paths = child_transition
        .changes()
        .iter()
        .flat_map(|change| [change.before.as_ref(), change.after.as_ref()])
        .flatten()
        .map(|state| String::from_utf8(state.path.clone()).unwrap())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let packet = Metadata::revision_contribution(
        receiver.binding.project_id(),
        receiver.binding.descriptor_record_id(),
        &receiver.binding.descriptor_object().hex(),
        &child_capture.receipt_record_id,
        &child_revision_bytes,
        Some(&child_brief_bytes),
        SourceAvailability::LocalOnly,
        paths,
    )
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
            [child_revision_id.as_str()],
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
            &child_revision_id,
            ObjectDigest::of(&child_revision_bytes),
            &child_contribution_id,
        )
        .is_err()
    );

    collaboration_capture::release_retention(
        &mut receiver.store,
        &parent_capture.operation_id,
        &parent_capture.receipt_record_id,
    )
    .unwrap();
    collaboration_capture::release_retention(
        &mut receiver.store,
        &child_capture.operation_id,
        &child_capture.receipt_record_id,
    )
    .unwrap();
    let verified = super::validate_revision_project_binding(&mut receiver.store, &packet)
        .unwrap()
        .expect("revision packet has a validated proof bundle");
    let expected_proof_snapshots = std::collections::BTreeSet::from([
        parent_revision.revision().base().clone(),
        parent_revision.revision().result().clone(),
        child_revision.base().clone(),
        child_revision.result().clone(),
    ]);
    assert_eq!(verified.source_snapshots, expected_proof_snapshots);
    assert!(
        verified
            .object_digests
            .contains(&parent_revision.object_digest())
    );
    assert!(
        verified
            .object_digests
            .contains(&parent_revision.brief_digest().unwrap())
    );
    assert!(
        verified
            .object_digests
            .contains(&ObjectDigest::of(&child_revision_bytes))
    );
    assert!(verified.object_digests.contains(&child_brief_digest));

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
    assert!(verified.source_snapshots.iter().all(|snapshot| {
        !gc_plan
            .entities
            .contains(&collaboration_gc::GcEntity::Snapshot(
                snapshot.as_str().into(),
            ))
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

    let mut unknown_receiver = receiver.clone_and_bind().unwrap();
    let unknown_native = RevisionSnapshotPeer { packet };
    let unknown_spec = PeerSpec {
        profile: PROFILE.into(),
        service_id: "coord.example".into(),
        project: "playground".into(),
        audience: "local-test".into(),
        reader: "unknown-base-reader".into(),
        scope: Scope::Project,
        max_attempts: 1,
        deadline_seconds: 5,
    };
    let unknown_appointed =
        super::appoint(&mut unknown_receiver.store, unknown_spec, &unknown_native).unwrap();
    let before: (i64, Option<i64>, i64) = unknown_receiver
        .store
        .read_connection()
        .query_row(
            "SELECT
               (SELECT count(*) FROM local_exchange_events WHERE binding=?1),
               (SELECT position FROM local_exchange_streams WHERE binding=?1),
               (SELECT count(*) FROM local_exchange_projection WHERE binding=?1)",
            [unknown_appointed.id().as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let refusal = super::recover_snapshot(
        &mut unknown_receiver.store,
        &unknown_appointed,
        "unknown-base",
    );
    assert!(matches!(
        refusal,
        Err(super::ExchangeError::Invalid(reason))
            if reason == "revision base identity state is unknown"
    ));
    let after: (i64, Option<i64>, i64) = unknown_receiver
        .store
        .read_connection()
        .query_row(
            "SELECT
               (SELECT count(*) FROM local_exchange_events WHERE binding=?1),
               (SELECT position FROM local_exchange_streams WHERE binding=?1),
               (SELECT count(*) FROM local_exchange_projection WHERE binding=?1)",
            [unknown_appointed.id().as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(after, before);
    assert_eq!(after, (0, None, 0));
}
