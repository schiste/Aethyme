use super::*;
use crate::local_target::tests::{Fixture, git};

struct TransportFixture {
    f: Fixture,
    _remote: tempfile::TempDir,
    owner: ControllerOwner,
    carrier: GitCarrier,
    broker: Broker,
    wave: RecordId,
}
impl TransportFixture {
    fn new() -> Self {
        let mut f = Fixture::new();
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
        wave::available(&mut f.store, &owner, "A", 0).unwrap();
        let wave = match wave::admit(
            &mut f.store,
            &owner,
            wave::WavePolicy {
                debounce_ms: 0,
                ..Default::default()
            },
            1,
        )
        .unwrap()
        {
            wave::Admission::Frozen(w) => w,
            other => panic!("{other:?}"),
        };
        let composed = wave::compose(&mut f.store, &owner, f._repo.path(), &wave.id).unwrap();
        assert_eq!(composed.phase, WavePhase::Verifying);
        let version = target::status(&f.store, "target").unwrap().version;
        target::resume(&mut f.store, &owner, version).unwrap();
        let mut broker = Broker::open(f._repo.path())
            .unwrap()
            .with_host_operation_database(f._host.path().join("host-operations.db"))
            .with_worktree_root(f._host.path().join("worktrees"));
        let session = broker
            .adopt(f._repo.path(), Some("exact transport fixture"))
            .unwrap();
        let carrier = GitCarrier::new(
            &mut broker,
            session.id,
            "publication",
            "refs/heads/candidate",
            30,
            "fixture operator authorizes exact conditional candidate publication",
            true,
        )
        .unwrap();
        Self {
            f,
            _remote: remote,
            owner,
            carrier,
            broker,
            wave: wave.id,
        }
    }
    fn prepare(&mut self, n: char, old: Option<&CommitOid>) -> Intent {
        prepare(
            &mut self.f.store,
            &self.owner,
            &self.carrier,
            &self.wave,
            &n.to_string().repeat(64),
            old,
        )
        .unwrap()
    }
    fn inspect(&mut self, i: &Intent) -> Receipt {
        inspect(
            &mut self.f.store,
            &self.owner,
            &self.carrier,
            &mut self.broker,
            &i.id,
        )
        .unwrap()
    }
}

#[test]
fn native_exact_publication_and_read_receipts_never_accept_source() {
    let mut t = TransportFixture::new();
    let before = target::status(&t.f.store, "target").unwrap();
    let head = git(t.f._repo.path(), &["rev-parse", "HEAD"]);
    let i = t.prepare('1', None);
    assert_eq!(t.prepare('1', None), i);
    let absent = t.inspect(&i);
    assert_eq!(absent.spec.observation, Observation::Absent);
    assert!(!absent.spec.content_checked);
    let dispatched = publish(&mut t.f.store, &t.owner, &t.carrier, &mut t.broker, &i.id).unwrap();
    assert_eq!(dispatched.outcome, OperationStatus::Succeeded);
    let receipt = t.inspect(&i);
    assert_eq!(receipt.spec.observation, Observation::Available);
    assert!(receipt.spec.content_checked);
    assert_eq!(
        receipt.spec.observed_oid.as_deref(),
        Some(i.spec.commit.as_str())
    );
    assert_eq!(receipt.spec.write_operation, Some(dispatched.operation));
    assert_eq!(receipt.spec.write_outcome.as_deref(), Some("succeeded"));
    assert!(receipt.spec.read_operation.is_some());
    assert_eq!(target::status(&t.f.store, "target").unwrap(), before);
    assert_eq!(git(t.f._repo.path(), &["rev-parse", "HEAD"]), head);
    assert!(!t.f._repo.path().join(".git/FETCH_HEAD").exists());
    assert!(matches!(
        publish(&mut t.f.store, &t.owner, &t.carrier, &mut t.broker, &i.id),
        Err(TransportError::AlreadyDispatched)
    ));
}

#[test]
fn lost_caller_response_is_inspected_without_a_second_push() {
    let mut t = TransportFixture::new();
    let i = t.prepare('1', None);
    let report = publish(&mut t.f.store, &t.owner, &t.carrier, &mut t.broker, &i.id).unwrap();
    // The application loses the returned report. The native broker journal is
    // separate and survives; Local never uses a missing response as permission.
    drop(t.broker);
    t.broker = Broker::open(t.f._repo.path())
        .unwrap()
        .with_host_operation_database(t.f._host.path().join("host-operations.db"));
    assert!(matches!(
        prepare(
            &mut t.f.store,
            &t.owner,
            &t.carrier,
            &t.wave,
            &"2".repeat(64),
            None
        ),
        Err(TransportError::Unresolved)
    ));
    let receipt = t.inspect(&i);
    assert_eq!(receipt.spec.write_operation, Some(report.operation));
    assert_eq!(receipt.spec.observation, Observation::Available);
    assert!(receipt.spec.content_checked);
    assert!(matches!(
        publish(&mut t.f.store, &t.owner, &t.carrier, &mut t.broker, &i.id),
        Err(TransportError::AlreadyDispatched)
    ));
}

#[test]
fn exact_wrong_ref_other_object_and_conditional_write_race() {
    let mut t = TransportFixture::new();
    let i = t.prepare('1', None);
    git(
        t.f._repo.path(),
        &[
            "push",
            "-q",
            "publication",
            &format!("{}:refs/heads/unrelated", i.spec.commit),
        ],
    );
    assert_eq!(t.inspect(&i).spec.observation, Observation::Absent);
    let wrong = t.f.a.base.commit.clone();
    git(
        t.f._repo.path(),
        &[
            "push",
            "-q",
            "publication",
            &format!("{}:refs/heads/candidate", wrong.as_str()),
        ],
    );
    let receipt = t.inspect(&i);
    assert_eq!(receipt.spec.observation, Observation::Other);
    assert_eq!(receipt.spec.observed_oid.as_deref(), Some(wrong.as_str()));
    assert!(!receipt.spec.content_checked);
    let refused = publish(&mut t.f.store, &t.owner, &t.carrier, &mut t.broker, &i.id).unwrap();
    assert_eq!(refused.outcome, OperationStatus::Failed);
    let receipt = t.inspect(&i);
    assert_eq!(receipt.spec.observation, Observation::Other);
    assert_eq!(receipt.spec.write_outcome.as_deref(), Some("failed"));
    let next = t.prepare('2', Some(&wrong));
    assert_ne!(next.id, i.id);
    assert_eq!(
        publish(
            &mut t.f.store,
            &t.owner,
            &t.carrier,
            &mut t.broker,
            &next.id
        )
        .unwrap()
        .outcome,
        OperationStatus::Succeeded
    );
    assert!(t.inspect(&next).spec.content_checked);
}

#[test]
fn stopped_target_cannot_dispatch_but_can_observe_historical_availability() {
    let mut t = TransportFixture::new();
    let i = t.prepare('1', None);
    let state = target::status(&t.f.store, "target").unwrap();
    target::transition(
        &mut t.f.store,
        &t.owner,
        state.version,
        target::Phase::StopRequested,
    )
    .unwrap();
    assert!(publish(&mut t.f.store, &t.owner, &t.carrier, &mut t.broker, &i.id).is_err());
    let dispatched: bool =
        t.f.store
            .read_connection()
            .query_row(
                "SELECT dispatched FROM local_source_transports WHERE intent=?1",
                [i.id.as_str()],
                |r| r.get(0),
            )
            .unwrap();
    assert!(!dispatched);
    assert_eq!(t.inspect(&i).spec.observation, Observation::Absent);
}

#[test]
fn remote_configuration_change_and_forged_index_refuse_before_writing() {
    let mut t = TransportFixture::new();
    let i = t.prepare('1', None);
    let foreign = tempfile::tempdir().unwrap();
    git(foreign.path(), &["init", "--bare", "-q"]);
    git(
        t.f._repo.path(),
        &[
            "remote",
            "set-url",
            "publication",
            foreign.path().to_str().unwrap(),
        ],
    );
    assert!(publish(&mut t.f.store, &t.owner, &t.carrier, &mut t.broker, &i.id).is_err());
    assert!(
        t.carrier
            .repo
            .remote_ref_oids("publication", &["refs/heads/candidate".into()])
            .unwrap()["refs/heads/candidate"]
            .is_none()
    );
    git(
        t.f._repo.path(),
        &[
            "remote",
            "set-url",
            "publication",
            t._remote.path().to_str().unwrap(),
        ],
    );
    t.f.store
        .read_connection()
        .execute(
            "UPDATE local_source_transports SET destination='refs/heads/forged' WHERE intent=?1",
            [i.id.as_str()],
        )
        .unwrap();
    assert!(matches!(
        load(&t.f.store, &i.id),
        Err(TransportError::Invalid(_))
    ));
}

#[test]
fn crash_before_native_operation_remains_uncertain_until_exact_inspection() {
    let mut t = TransportFixture::new();
    let i = t.prepare('1', None);
    t.f.store
        .read_connection()
        .execute(
            "UPDATE local_source_transports SET dispatched=1 WHERE intent=?1",
            [i.id.as_str()],
        )
        .unwrap();
    assert!(matches!(
        publish(&mut t.f.store, &t.owner, &t.carrier, &mut t.broker, &i.id),
        Err(TransportError::AlreadyDispatched)
    ));
    assert!(matches!(
        prepare(
            &mut t.f.store,
            &t.owner,
            &t.carrier,
            &t.wave,
            &"2".repeat(64),
            None
        ),
        Err(TransportError::Unresolved)
    ));
    let receipt = t.inspect(&i);
    assert_eq!(receipt.spec.observation, Observation::Absent);
    assert_eq!(receipt.spec.write_operation, None);
    assert!(receipt.spec.write_outcome.is_none());
    let next = t.prepare('2', None);
    assert_eq!(
        publish(
            &mut t.f.store,
            &t.owner,
            &t.carrier,
            &mut t.broker,
            &next.id
        )
        .unwrap()
        .outcome,
        OperationStatus::Succeeded
    );
}

#[test]
fn source_digest_and_native_broker_binding_are_checked_before_dispatch() {
    let mut t = TransportFixture::new();
    let i = t.prepare('1', None);
    let mut foreign = TransportFixture::new();
    assert!(matches!(
        publish(
            &mut t.f.store,
            &t.owner,
            &t.carrier,
            &mut foreign.broker,
            &i.id
        ),
        Err(TransportError::Invalid(_))
    ));
    t.f.store
        .read_connection()
        .execute(
            "UPDATE local_waves SET candidate_commit=?2 WHERE wave=?1",
            params![t.wave.as_str(), t.f.a.base.commit.as_str()],
        )
        .unwrap();
    assert!(matches!(
        prepare(
            &mut t.f.store,
            &t.owner,
            &t.carrier,
            &t.wave,
            &"2".repeat(64),
            None
        ),
        Err(TransportError::Invalid(_))
    ));
    assert!(publish(&mut t.f.store, &t.owner, &t.carrier, &mut t.broker, &i.id).is_err());
    assert_eq!(t.inspect(&i).spec.observation, Observation::Absent);
}

#[test]
fn historical_receipt_integrity_and_restore_epoch_fence() {
    let mut t = TransportFixture::new();
    let i = t.prepare('1', None);
    assert_eq!(last_receipt(&t.f.store, &i.id).unwrap(), None);
    let r = t.inspect(&i);
    assert_eq!(last_receipt(&t.f.store, &i.id).unwrap(), Some(r));
    t.f.store
        .read_connection()
        .execute(
            "UPDATE local_source_transports SET observation='available' WHERE intent=?1",
            [i.id.as_str()],
        )
        .unwrap();
    assert!(matches!(
        last_receipt(&t.f.store, &i.id),
        Err(TransportError::Invalid(_))
    ));
    crate::local_authority::freeze(
        &mut t.f.store,
        t.owner.authority_epoch(),
        "fixture announced restore",
    )
    .unwrap();
    assert!(publish(&mut t.f.store, &t.owner, &t.carrier, &mut t.broker, &i.id).is_err());
    assert!(inspect(&mut t.f.store, &t.owner, &t.carrier, &mut t.broker, &i.id).is_err());
}

#[test]
fn two_prepared_intents_cannot_bypass_uncertain_dispatch_fence() {
    let mut t = TransportFixture::new();
    let first = t.prepare('1', None);
    let second = t.prepare('2', None);
    publish(
        &mut t.f.store,
        &t.owner,
        &t.carrier,
        &mut t.broker,
        &first.id,
    )
    .unwrap();
    assert!(matches!(
        publish(
            &mut t.f.store,
            &t.owner,
            &t.carrier,
            &mut t.broker,
            &second.id
        ),
        Err(TransportError::Unresolved)
    ));
    assert_eq!(t.inspect(&first).spec.observation, Observation::Available);
    assert_eq!(
        publish(
            &mut t.f.store,
            &t.owner,
            &t.carrier,
            &mut t.broker,
            &second.id
        )
        .unwrap()
        .outcome,
        OperationStatus::Succeeded
    );
}

#[test]
fn independent_receipt_identity_vector_binds_every_subject_and_outcome_field() {
    let v: serde_json::Value = serde_json::from_str(include_str!("receipt-vector.json")).unwrap();
    let bytes = v["canonical"].as_str().unwrap().as_bytes();
    let record = Record::decode(bytes, &[&RECEIPT_SCHEMA]).unwrap();
    assert_eq!(record.canonical_bytes(), bytes);
    assert_eq!(record.id().as_str(), v["id"].as_str().unwrap());
    let data: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    let spec: ReceiptSpec = serde_json::from_value(data["spec"].clone()).unwrap();
    assert_eq!(
        canonical(&RECEIPT_SCHEMA, &spec).unwrap().canonical_bytes(),
        bytes
    );
    for key in data["spec"].as_object().unwrap().keys() {
        let mut changed = data.clone();
        changed["spec"][key] = match &changed["spec"][key] {
            serde_json::Value::Bool(b) => serde_json::json!(!b),
            serde_json::Value::Number(n) => serde_json::json!(n.as_i64().unwrap() + 1),
            serde_json::Value::String(s) => serde_json::json!(format!("{s}-changed")),
            _ => panic!("unexpected vector type"),
        };
        let changed =
            Record::decode(&serde_json::to_vec(&changed).unwrap(), &[&RECEIPT_SCHEMA]).unwrap();
        assert_ne!(changed.id(), record.id(), "field {key}");
    }
}

#[test]
fn prewrite_observation_cannot_reconcile_a_later_dispatch() {
    let mut t = TransportFixture::new();
    let i = t.prepare('1', None);
    let old = t.inspect(&i);
    assert_eq!(old.spec.observation, Observation::Absent);
    publish(&mut t.f.store, &t.owner, &t.carrier, &mut t.broker, &i.id).unwrap();
    assert_eq!(last_receipt(&t.f.store, &i.id).unwrap(), None);
    assert!(matches!(
        prepare(
            &mut t.f.store,
            &t.owner,
            &t.carrier,
            &t.wave,
            &"2".repeat(64),
            None
        ),
        Err(TransportError::Unresolved)
    ));
    let receipt = t.inspect(&i);
    assert_eq!(receipt.spec.observation, Observation::Available);
    let count: i64 =
        t.f.store
            .read_connection()
            .query_row(
                "SELECT count(*) FROM local_source_transport_receipts WHERE intent=?1",
                [i.id.as_str()],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(count, 2);
    assert_ne!(old.id, receipt.id);
}

#[test]
fn stopped_revision_roots_wait_for_native_outcome_and_exact_inspection() {
    let mut t = TransportFixture::new();
    let i = t.prepare('1', None);
    let old_revision = parse_id(&i.spec.revision).unwrap();
    publish(&mut t.f.store, &t.owner, &t.carrier, &mut t.broker, &i.id).unwrap();
    wave::stop(&mut t.f.store, &t.owner, &t.wave).unwrap();
    let mut spec = t.f.spec();
    spec.objective = "new authorized objective".into();
    let next = target::propose_revision(&mut t.f.store, &t.owner, spec).unwrap();
    target::select_revision(&mut t.f.store, &t.owner, &next.id).unwrap();
    assert!(matches!(
        target::release_revision(&mut t.f.store, &t.owner, &old_revision),
        Err(target::TargetError::Frozen)
    ));
    let receipt = t.inspect(&i);
    assert_eq!(receipt.spec.observation, Observation::Available);
    assert_eq!(receipt.spec.write_outcome.as_deref(), Some("succeeded"));
    t.f.store
        .read_connection()
        .execute(
            "UPDATE local_source_transports SET write_outcome='outcome_unknown' WHERE intent=?1",
            [i.id.as_str()],
        )
        .unwrap();
    assert!(matches!(
        target::release_revision(&mut t.f.store, &t.owner, &old_revision),
        Err(target::TargetError::Frozen)
    ));
    t.inspect(&i); // Native journal, rather than a Local flag, settles the copy.
    target::release_revision(&mut t.f.store, &t.owner, &old_revision).unwrap();
}

#[test]
fn native_operation_reference_cannot_point_to_another_operation() {
    let mut t = TransportFixture::new();
    let i = t.prepare('1', None);
    publish(&mut t.f.store, &t.owner, &t.carrier, &mut t.broker, &i.id).unwrap();
    let r = t.inspect(&i);
    t.f.store
        .read_connection()
        .execute(
            "UPDATE local_source_transports SET operation=?2 WHERE intent=?1",
            params![i.id.as_str(), r.spec.read_operation.unwrap()],
        )
        .unwrap();
    assert!(matches!(
        inspect(&mut t.f.store, &t.owner, &t.carrier, &mut t.broker, &i.id),
        Err(TransportError::Invalid(_))
    ));
}

#[test]
fn replacement_refs_cannot_change_the_retained_commit_tree() {
    let mut t = TransportFixture::new();
    let original = t.prepare('1', None);
    let replacement = t.f.a.base.commit.as_str();
    let replacement_tree = git(
        t.f._repo.path(),
        &["rev-parse", &format!("{replacement}^{{tree}}")],
    );
    assert_ne!(original.spec.tree, replacement_tree);
    git(
        t.f._repo.path(),
        &["replace", &original.spec.commit, replacement],
    );
    assert_eq!(
        git(
            t.f._repo.path(),
            &["rev-parse", &format!("{}^{{tree}}", original.spec.commit)]
        ),
        replacement_tree
    );
    let i = t.prepare('2', None);
    assert_eq!(i.spec.tree, original.spec.tree);
    assert_eq!(i.spec.source, original.spec.source);
    assert_eq!(
        publish(&mut t.f.store, &t.owner, &t.carrier, &mut t.broker, &i.id)
            .unwrap()
            .outcome,
        OperationStatus::Succeeded
    );
    let receipt = t.inspect(&i);
    assert_eq!(receipt.spec.observation, Observation::Available);
    assert!(receipt.spec.content_checked);
    assert_eq!(receipt.spec.tree, original.spec.tree);
    assert_eq!(
        git(
            t._remote.path(),
            &["rev-parse", &format!("{}^{{tree}}", i.spec.commit)]
        ),
        original.spec.tree
    );
}

#[test]
fn chained_url_rewrites_inspect_the_configured_peer_once() {
    let mut t = TransportFixture::new();
    let second = tempfile::tempdir().unwrap();
    git(second.path(), &["init", "--bare", "-q"]);
    let first_url = t._remote.path().to_str().unwrap();
    let second_url = second.path().to_str().unwrap();
    git(
        t.f._repo.path(),
        &[
            "config",
            &format!("url.{first_url}.insteadOf"),
            "transport-origin",
        ],
    );
    git(
        t.f._repo.path(),
        &["config", &format!("url.{second_url}.insteadOf"), first_url],
    );
    git(
        t.f._repo.path(),
        &["remote", "set-url", "publication", "transport-origin"],
    );
    assert_eq!(
        git(
            t.f._repo.path(),
            &["remote", "get-url", "--push", "publication"]
        ),
        first_url
    );
    t.carrier = GitCarrier::new(
        &mut t.broker,
        t.carrier.session,
        "publication",
        "refs/heads/candidate",
        30,
        "fixture operator authorizes exact conditional candidate publication",
        true,
    )
    .unwrap();
    let i = t.prepare('1', None);
    assert_eq!(
        publish(&mut t.f.store, &t.owner, &t.carrier, &mut t.broker, &i.id)
            .unwrap()
            .outcome,
        OperationStatus::Succeeded
    );
    assert_eq!(
        git(t._remote.path(), &["rev-parse", "refs/heads/candidate"]),
        i.spec.commit
    );
    let destinations = ["refs/heads/candidate".to_string()];
    assert_eq!(
        t.carrier
            .repo
            .remote_ref_oids("publication", &destinations)
            .unwrap()["refs/heads/candidate"]
            .as_deref(),
        Some(i.spec.commit.as_str())
    );
    assert_eq!(
        t.carrier
            .repo
            .checked_push_remote_ref_oids(
                "publication",
                &t.carrier.identity.coordination_key,
                &destinations,
            )
            .unwrap()["refs/heads/candidate"]
            .as_deref(),
        Some(i.spec.commit.as_str())
    );
    assert!(
        git(
            second.path(),
            &[
                "for-each-ref",
                "--format=%(objectname)",
                "refs/heads/candidate"
            ]
        )
        .is_empty()
    );
    let receipt = t.inspect(&i);
    assert_eq!(receipt.spec.observation, Observation::Available);
    assert!(receipt.spec.content_checked);
}

#[test]
fn remote_query_deadline_and_failures_never_mean_absent() {
    use std::os::unix::fs::PermissionsExt;
    let mut t = TransportFixture::new();
    let i = t.prepare('1', None);
    let helper = t.f._host.path().join("upload-pack-probe");
    let marker = t.f._host.path().join("query-started");
    std::fs::write(
        &helper,
        format!(
            "#!/bin/sh\nprintf started > '{}'\nexec /bin/sleep 4\n",
            marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
    git(
        t.f._repo.path(),
        &[
            "config",
            "remote.publication.uploadpack",
            helper.to_str().unwrap(),
        ],
    );
    let result = crate::git::with_git_deadline(Duration::from_secs(2), || {
        t.carrier
            .repo
            .remote_fetch_ref_oids("publication", &["refs/heads/candidate".into()])
    });
    assert!(
        marker.exists(),
        "the actual remote query must run before timeout"
    );
    assert!(
        matches!(result, Err(crate::GitError::TimedOut { .. })),
        "{result:?}"
    );
    // A deterministic provider failure is unavailable even when the remote has
    // no candidate ref. It cannot count as evidence of absence.
    std::fs::write(&helper, "#!/bin/sh\nexit 1\n").unwrap();
    let receipt = t.inspect(&i);
    assert_eq!(receipt.spec.observation, Observation::Unavailable);
    assert!(receipt.spec.observed_oid.is_none());
    assert!(!receipt.spec.content_checked);
}
