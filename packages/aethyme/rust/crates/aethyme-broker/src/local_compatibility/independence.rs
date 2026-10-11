//! Compatibility across native Local boundaries with a peer unavailable. The
//! peer fixture models only its explicit offline response; Git and archive,
//! controller, context, import and provider routes use the actual implementation.
use super::*;
use crate::collaboration_archive as archive;
use crate::local_exchange as exchange;
use crate::local_target as target;
use aethyme_contracts::experimental_v0::{Record, RecordId};
struct OfflinePeer;
impl exchange::TrustedPeer for OfflinePeer {
    fn profile(&self) -> &str {
        exchange::PROFILE
    }
    fn service_id(&self) -> &str {
        "unavailable-peer"
    }
    fn authorize(
        &self,
        _: &exchange::PeerSpec,
        _: &RecordId,
    ) -> Result<Box<dyn exchange::NativeAuthorizationLease + '_>, exchange::ExchangeError> {
        Err(exchange::ExchangeError::Offline)
    }
    fn send(
        &self,
        _: &dyn exchange::NativeAuthorizationLease,
        _: &Record,
    ) -> Result<exchange::MetadataAck, exchange::ExchangeError> {
        panic!("offline authorization must prevent send")
    }
    fn events(
        &self,
        _: &dyn exchange::NativeAuthorizationLease,
        _: &exchange::Cursor,
    ) -> Result<Vec<exchange::Event>, exchange::ExchangeError> {
        panic!("offline authorization must prevent events")
    }
    fn snapshot(
        &self,
        _: &dyn exchange::NativeAuthorizationLease,
        _: Option<&exchange::SnapshotDescriptor>,
        _: u32,
    ) -> Result<exchange::SnapshotPage, exchange::ExchangeError> {
        panic!("offline authorization must prevent snapshot")
    }
}
fn unavailable_peer<'a>(
    store: &mut CollaborationStore,
    adapter: &'a OfflinePeer,
) -> exchange::AppointedPeer<'a> {
    exchange::appoint(
        store,
        exchange::PeerSpec {
            profile: exchange::PROFILE.into(),
            service_id: "unavailable-peer".into(),
            project: "Playground".into(),
            audience: "local-readers".into(),
            reader: "compatibility".into(),
            scope: exchange::Scope::Project,
            max_attempts: 2,
            deadline_seconds: 1,
        },
        adapter,
    )
    .unwrap()
}
fn table_count(store: &CollaborationStore, table: &str) -> i64 {
    store
        .read_connection()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}
#[test]
fn offline_peer_preserves_actual_capture_context_controller_and_authorized_git_route() {
    let mut f = crate::local_target::tests::Fixture::new();
    let adapter = OfflinePeer;
    let peer = unavailable_peer(&mut f.store, &adapter);
    assert!(matches!(
        exchange::poll(&mut f.store, &peer, "offline-observation"),
        Err(exchange::ExchangeError::Offline)
    ));
    assert!(matches!(
        exchange::projection(&mut f.store, &peer),
        Err(exchange::ExchangeError::Offline)
    ));
    let receipt = crate::collaboration_capture::capture(
        &mut f.store,
        &crate::collaboration_capture::CaptureRequest {
            operation_id: crate::collaboration_capture::OperationId::parse("offline-capture")
                .unwrap(),
            repository: f._repo.path().into(),
            base: f.a.base.commit.clone(),
            result: f.a.result.commit.clone(),
            policy: crate::collaboration_capture::CapturePolicy::Required,
            retention: crate::collaboration_capture::RetentionBoundary::UntilReleased,
        },
    )
    .unwrap();
    let receipt = match receipt {
        crate::collaboration_capture::CaptureOutcome::Acknowledged(r) => r,
        _ => panic!("actual Local capture must remain available"),
    };
    let query = crate::collaboration_context::ContextQuery {
        scope: vec![b"one.txt".to_vec()],
        source: Some(receipt.result.clone()),
        analysis: vec![],
        budget: Default::default(),
    };
    let context = crate::collaboration_context::retrieve(
        &mut f.store,
        &query,
        &crate::collaboration_context::LocalProject,
    )
    .unwrap();
    assert_eq!(
        context.contributions,
        vec![receipt.contribution.to_string()]
    );
    let remote = tempfile::tempdir().unwrap();
    git(remote.path(), &["init", "--bare", "-q"]);
    git(
        f._repo.path(),
        &[
            "remote",
            "add",
            "publication",
            remote.path().to_str().unwrap(),
        ],
    );
    let owner = target::acquire_controller(&mut f.store, "target", "appointed").unwrap();
    let spec = f.spec();
    let revision = target::propose_revision(&mut f.store, &owner, spec).unwrap();
    target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
    crate::local_wave::available(&mut f.store, &owner, "A", 0).unwrap();
    let wave = match crate::local_wave::admit(
        &mut f.store,
        &owner,
        crate::local_wave::WavePolicy {
            debounce_ms: 0,
            ..Default::default()
        },
        1,
    )
    .unwrap()
    {
        crate::local_wave::Admission::Frozen(w) => w,
        _ => panic!(),
    };
    let wave = crate::local_wave::compose(&mut f.store, &owner, f._repo.path(), &wave.id).unwrap();
    let version = target::status(&f.store, "target").unwrap().version;
    target::resume(&mut f.store, &owner, version).unwrap();
    let mut broker = Broker::open(f._repo.path())
        .unwrap()
        .with_host_operation_database(f._host.path().join("host-operations.db"))
        .with_worktree_root(f._host.path().join("worktrees"));
    let session = broker
        .adopt(
            f._repo.path(),
            Some("offline compatibility Playground route"),
        )
        .unwrap();
    let carrier = crate::local_source_transport::GitCarrier::new(
        &mut broker,
        session.id,
        "publication",
        "refs/heads/accepted",
        30,
        "operator authorizes only this disposable Playground source route",
        true,
    )
    .unwrap();
    git(
        f._repo.path(),
        &[
            "push",
            "-q",
            "publication",
            &format!("{}:refs/heads/accepted", f.a.base.commit.as_str()),
        ],
    );
    let intent = crate::local_source_transport::prepare(
        &mut f.store,
        &owner,
        &carrier,
        &wave.id,
        &"a".repeat(64),
        Some(&f.a.base.commit),
    )
    .unwrap();
    let binding = crate::local_acceptance::appoint(
        &mut f.store,
        &owner,
        &mut broker,
        &carrier,
        &intent.id,
        crate::local_acceptance::ProviderRoute::GenericGit,
        "refs/heads/accepted",
        2,
    )
    .unwrap();
    let head = git(f._repo.path(), &["rev-parse", "HEAD"]);
    crate::local_source_transport::publish(&mut f.store, &owner, &carrier, &mut broker, &intent.id)
        .unwrap();
    let observation =
        crate::local_acceptance::observe(&mut f.store, &owner, &mut broker, &carrier, binding.id())
            .unwrap();
    assert_eq!(
        observation.spec.decision,
        crate::local_acceptance::ProviderDecision::Accepted
    );
    assert_eq!(
        observation.spec.accepted_commit.as_deref(),
        Some(intent.spec.commit.as_str())
    );
    assert_eq!(git(f._repo.path(), &["rev-parse", "HEAD"]), head);
    assert_eq!(
        table_count(&f.store, "local_acceptance_qualifications"),
        0,
        "provider observation never supplies trusted verification"
    );
    assert_eq!(table_count(&f.store, "local_verifier_producers"), 0);
    let reconstruction = tempfile::tempdir().unwrap();
    let root = reconstruction.path().join("source");
    archive::reconstruct(&f.store, &receipt.result, &root).unwrap();
    assert_eq!(std::fs::read(root.join("one.txt")).unwrap(), b"A\n");
    assert!(matches!(
        exchange::projection(&mut f.store, &peer),
        Err(exchange::ExchangeError::Offline)
    ));
}
#[test]
fn separate_receiver_imports_history_but_requires_fresh_native_target_policy_and_epoch() {
    let mut origin = crate::local_target::tests::Fixture::new();
    let exporter =
        target::acquire_controller(&mut origin.store, "origin-target", "origin-operator").unwrap();
    let scope = crate::local_restore::Scope {
        origin: origin.store.project().as_str().into(),
        visibility: "local_project".into(),
        audience: "named-receiver".into(),
    };
    let selection = crate::local_restore::Selection {
        sources: vec![
            origin.a.base.snapshot_id.clone(),
            origin.a.result.snapshot_id.clone(),
        ],
        records: vec![],
    };
    let wire = crate::local_restore::export(&origin.store, &exporter, &scope, &selection).unwrap();
    let policy = crate::local_restore::ReceiverPolicy {
        bundle: archive::ObjectDigest::of(&wire),
        scope: scope.clone(),
    };
    let bundle = crate::local_restore::verify(&wire, &policy).unwrap();
    let host = tempfile::tempdir().unwrap();
    let mut receiver = CollaborationStore::open(
        &CollaborationRoot::under_host_state(host.path()),
        &ProjectKey::parse("independent-receiver").unwrap(),
        &[],
    )
    .unwrap();
    let appointed =
        target::acquire_controller(&mut receiver, "receiver-target", "receiver-operator").unwrap();
    assert_ne!(exporter.authority_epoch(), appointed.authority_epoch());
    assert!(crate::local_restore::bind_receiver(&receiver, &exporter, &bundle).is_err());
    let binding = crate::local_restore::bind_receiver(&receiver, &appointed, &bundle).unwrap();
    let history =
        crate::local_restore::import(&mut receiver, &appointed, &binding, &bundle).unwrap();
    assert_eq!(history.sources.len(), 2);
    assert_eq!(
        crate::local_restore::show(&receiver, &history.id)
            .unwrap()
            .bundle,
        policy.bundle
    );
    for table in [
        "retained_snapshots",
        "retained_contributions",
        "local_target_revisions",
        "local_acceptance_bindings",
        "local_acceptance_receipts",
        "local_acceptance_qualifications",
        "local_verifier_producers",
        "local_verification_evidence",
        "local_agent_sessions",
        "local_adoption_receipts",
        "local_exchange_bindings",
    ] {
        assert_eq!(
            table_count(&receiver, table),
            0,
            "exported history cannot appoint {table}"
        );
    }
    assert!(
        target::propose_revision(&mut receiver, &appointed, origin.spec()).is_err(),
        "import is not a native current target baseline/contribution registration"
    );
    let frozen = appointed.authority_epoch().clone();
    crate::local_authority::freeze(
        &mut receiver,
        &frozen,
        "receiver operator revokes current policy",
    )
    .unwrap();
    assert!(crate::local_restore::import(&mut receiver, &appointed, &binding, &bundle).is_err());
    assert_eq!(
        crate::local_restore::show(&receiver, &history.id)
            .unwrap()
            .bundle,
        policy.bundle,
        "revocation preserves history without preserving authority"
    );
}
#[cfg(target_os = "macos")]
#[test]
#[ignore = "actual native Local routes under the macOS kernel network-denial execution profile"]
fn actual_native_local_routes_survive_kernel_network_denial() {
    super::old_binary::assert_network_denied();
    for test in [
        "offline_peer_preserves_actual_capture_context_controller_and_authorized_git_route",
        "separate_receiver_imports_history_but_requires_fresh_native_target_policy_and_epoch",
    ] {
        let full = format!("local_compatibility::independence::{test}");
        let output = std::process::Command::new("/usr/bin/sandbox-exec")
            .args(["-p", "(version 1) (allow default) (deny network*)"])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", &full, "--nocapture", "--test-threads=1"])
            .env_clear()
            .env("PATH", "/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed; 0 failed"),
            "named native child must actually run"
        );
        eprintln!(
            "compatibility current native transcript {}",
            serde_json::json!({"test":full,"network":"kernel-denied","status":output.status.code(),"stdout_sha256":archive::ObjectDigest::of(&output.stdout).hex(),"stderr_sha256":archive::ObjectDigest::of(&output.stderr).hex()})
        );
    }
}
