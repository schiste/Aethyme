use super::*;
use crate::collaboration_state::{CollaborationRoot, CollaborationStore, ProjectKey};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::Path;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};
fn open(host: &Path) -> CollaborationStore {
    CollaborationStore::open(
        &CollaborationRoot::under_host_state(host),
        &ProjectKey::parse("exchange-playground").unwrap(),
        &[],
    )
    .unwrap()
}
fn millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
#[derive(Default)]
struct State {
    offline: bool,
    revoked: bool,
    expired: bool,
    visibility: u64,
    events: Vec<Event>,
    pages: VecDeque<SnapshotPage>,
    sends: Vec<String>,
    lost_ack: bool,
    wrong_ack: bool,
    revoke_after_send: bool,
    revoke_after_events: bool,
    revoke_after_page: bool,
    action: Option<Box<dyn FnMut()>>,
}
struct Peer {
    state: Rc<RefCell<State>>,
}
impl Peer {
    fn new() -> Self {
        Self {
            state: Rc::new(RefCell::new(State::default())),
        }
    }
    fn witness(&self, spec: &PeerSpec, id: &RecordId) -> StreamWitness {
        StreamWitness {
            binding: id.to_string(),
            reader: spec.reader.clone(),
            scope_digest: manager::scope_id(spec).unwrap().to_string(),
            visibility_generation: format!("acl-{}", self.state.borrow().visibility),
            incarnation: "stream-one".into(),
            remote_authority_epoch: "remote-one".into(),
        }
    }
    fn event(&self, spec: &PeerSpec, id: &RecordId, sequence: u64, brief: &str) -> Event {
        let mut packet = packet();
        if let Metadata::Contribution { brief: b, .. } = &mut packet {
            *b = Some(brief.into());
        }
        Event {
            stream: self.witness(spec, id),
            sequence,
            packet,
        }
    }
    fn page(
        &self,
        spec: &PeerSpec,
        id: &RecordId,
        items: Vec<Event>,
        watermark: u64,
    ) -> SnapshotPage {
        SnapshotPage {
            snapshot: SnapshotDescriptor {
                stream: self.witness(spec, id),
                identity: sha('c'),
                item_count: items.len() as u32,
                watermark,
            },
            offset: 0,
            items,
            next_offset: None,
        }
    }
}
struct Lease {
    state: Rc<RefCell<State>>,
    stream: StreamWitness,
    until: i64,
}
impl NativeAuthorizationLease for Lease {
    fn stream(&self) -> &StreamWitness {
        &self.stream
    }
    fn valid_until_ms(&self) -> i64 {
        if self.state.borrow().expired {
            0
        } else {
            self.until
        }
    }
    fn check_current(&self) -> Result<(), ExchangeError> {
        let s = self.state.borrow();
        if s.revoked {
            Err(ExchangeError::Forbidden)
        } else if s.offline {
            Err(ExchangeError::Offline)
        } else if self.stream.visibility_generation != format!("acl-{}", s.visibility) {
            Err(ExchangeError::Stale)
        } else {
            Ok(())
        }
    }
    fn visible(&self, _packet: &Metadata) -> bool {
        true
    }
}
impl TrustedPeer for Peer {
    fn profile(&self) -> &str {
        PROFILE
    }
    fn service_id(&self) -> &str {
        "coord.example"
    }
    fn authorize(
        &self,
        spec: &PeerSpec,
        id: &RecordId,
    ) -> Result<Box<dyn NativeAuthorizationLease + '_>, ExchangeError> {
        let lease = Lease {
            state: self.state.clone(),
            stream: self.witness(spec, id),
            until: millis() + 60_000,
        };
        lease.check_current()?;
        Ok(Box::new(lease))
    }
    fn send(
        &self,
        lease: &dyn NativeAuthorizationLease,
        message: &Record,
    ) -> Result<MetadataAck, ExchangeError> {
        let action = self.state.borrow_mut().action.take();
        if let Some(mut action) = action {
            action();
        }
        let mut s = self.state.borrow_mut();
        s.sends.push(message.id().to_string());
        if s.revoke_after_send {
            s.revoked = true;
        }
        if s.lost_ack {
            return Err(ExchangeError::Offline);
        }
        Ok(MetadataAck {
            stream: lease.stream().clone(),
            message: if s.wrong_ack {
                sha('f')
            } else {
                message.id().to_string()
            },
        })
    }
    fn events(
        &self,
        _lease: &dyn NativeAuthorizationLease,
        _cursor: &Cursor,
    ) -> Result<Vec<Event>, ExchangeError> {
        let action = self.state.borrow_mut().action.take();
        if let Some(mut action) = action {
            action();
        }
        let mut s = self.state.borrow_mut();
        let events = std::mem::take(&mut s.events);
        if s.revoke_after_events {
            s.revoked = true;
        }
        Ok(events)
    }
    fn snapshot(
        &self,
        _lease: &dyn NativeAuthorizationLease,
        _pinned: Option<&SnapshotDescriptor>,
        _offset: u32,
    ) -> Result<SnapshotPage, ExchangeError> {
        let action = self.state.borrow_mut().action.take();
        if let Some(mut action) = action {
            action();
        }
        let mut s = self.state.borrow_mut();
        let page = s.pages.pop_front().ok_or(ExchangeError::Incomplete)?;
        if s.revoke_after_page {
            s.revoked = true;
        }
        Ok(page)
    }
}
fn count(store: &CollaborationStore, table: &str) -> i64 {
    store
        .read_connection()
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}
#[test]
fn default_store_has_no_peer_and_metadata_does_not_retain_offered_source() {
    let host = tempfile::tempdir().unwrap();
    let mut store = open(host.path());
    assert_eq!(count(&store, "local_exchange_bindings"), 0);
    assert_eq!(count(&store, "retained_snapshots"), 0);
    let sequence = enqueue(&mut store, &packet()).unwrap();
    assert_eq!(enqueue(&mut store, &packet()).unwrap(), sequence);
    assert_eq!(count(&store, "local_exchange_messages"), 1);
    assert_eq!(count(&store, "retained_snapshots"), 0);
    let mut forged = peer();
    forged.service_id = "unappointed.example".into();
    let native = Peer::new();
    assert!(matches!(
        appoint(&mut store, forged, &native),
        Err(ExchangeError::Unsupported)
    ));
    assert_eq!(count(&store, "local_exchange_bindings"), 0);
}
#[test]
fn lost_ack_replays_exact_identity_and_finite_attempts_survive_reopen_per_peer() {
    let host = tempfile::tempdir().unwrap();
    let mut store = open(host.path());
    let native = Peer::new();
    let appointed = appoint(&mut store, peer(), &native).unwrap();
    let sequence = enqueue(&mut store, &packet()).unwrap();
    native.state.borrow_mut().lost_ack = true;
    assert!(matches!(
        deliver(&mut store, &appointed, sequence),
        Err(ExchangeError::Offline)
    ));
    drop(store);
    let mut store = open(host.path());
    assert!(matches!(
        deliver(&mut store, &appointed, sequence),
        Err(ExchangeError::Offline)
    ));
    native.state.borrow_mut().lost_ack = false;
    assert_eq!(
        deliver(&mut store, &appointed, sequence).unwrap(),
        Delivery::Acknowledged
    );
    assert_eq!(
        deliver(&mut store, &appointed, sequence).unwrap(),
        Delivery::AlreadyAcknowledged
    );
    assert_eq!(
        native.state.borrow().sends,
        vec![packet().record().unwrap().id().to_string(); 3]
    );
    let delivered: Option<i64> = store
        .read_connection()
        .query_row(
            "SELECT delivered_ms FROM outbox WHERE sequence=?1",
            [sequence],
            |r| r.get(0),
        )
        .unwrap();
    assert!(delivered.is_none());
    let mut other = peer();
    other.reader = "agent-two".into();
    let second = appoint(&mut store, other, &native).unwrap();
    assert_eq!(
        deliver(&mut store, &second, sequence).unwrap(),
        Delivery::Acknowledged
    );
    let mut next = packet();
    if let Metadata::Contribution { brief, .. } = &mut next {
        *brief = Some("other".into());
    }
    let sequence = enqueue(&mut store, &next).unwrap();
    native.state.borrow_mut().lost_ack = true;
    for _ in 0..3 {
        assert!(matches!(
            deliver(&mut store, &appointed, sequence),
            Err(ExchangeError::Offline)
        ));
    }
    let sends = native.state.borrow().sends.len();
    assert!(matches!(
        deliver(&mut store, &appointed, sequence),
        Err(ExchangeError::Limit)
    ));
    assert_eq!(native.state.borrow().sends.len(), sends);
}
#[test]
fn forged_ack_and_revocation_after_io_cannot_commit_acknowledgment() {
    let host = tempfile::tempdir().unwrap();
    let mut store = open(host.path());
    let native = Peer::new();
    let appointed = appoint(&mut store, peer(), &native).unwrap();
    let sequence = enqueue(&mut store, &packet()).unwrap();
    native.state.borrow_mut().wrong_ack = true;
    assert!(matches!(
        deliver(&mut store, &appointed, sequence),
        Err(ExchangeError::Invalid(_))
    ));
    {
        let mut s = native.state.borrow_mut();
        s.wrong_ack = false;
        s.revoke_after_send = true;
    }
    assert!(matches!(
        deliver(&mut store, &appointed, sequence),
        Err(ExchangeError::Forbidden)
    ));
    let (attempts, ack): (i64, bool) = store
        .read_connection()
        .query_row(
            "SELECT attempts,acknowledged FROM local_exchange_deliveries",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(attempts, 2);
    assert!(!ack);
}
#[test]
fn reorder_gaps_exact_duplicates_and_restart_advance_only_contiguous_cursor() {
    let host = tempfile::tempdir().unwrap();
    let mut store = open(host.path());
    let native = Peer::new();
    let appointed = appoint(&mut store, peer(), &native).unwrap();
    let first = native.event(appointed.spec(), appointed.id(), 1, "one");
    let second = native.event(appointed.spec(), appointed.id(), 2, "two");
    native.state.borrow_mut().events = vec![second.clone(), second.clone()];
    assert_eq!(poll(&mut store, &appointed, "sync").unwrap().position, 0);
    assert!(projection(&mut store, &appointed).unwrap().is_empty());
    drop(store);
    let mut store = open(host.path());
    native.state.borrow_mut().events = vec![first.clone()];
    assert_eq!(poll(&mut store, &appointed, "sync").unwrap().position, 2);
    assert_eq!(projection(&mut store, &appointed).unwrap().len(), 2);
    native.state.borrow_mut().events = vec![first, second];
    assert_eq!(poll(&mut store, &appointed, "sync").unwrap().position, 2);
    assert_eq!(count(&store, "local_exchange_events"), 2);
    assert!(matches!(
        poll(&mut store, &appointed, "sync"),
        Err(ExchangeError::Limit)
    ));
    let changed = native.event(appointed.spec(), appointed.id(), 2, "different");
    native.state.borrow_mut().events = vec![changed];
    assert!(matches!(
        poll(&mut store, &appointed, "next-sync"),
        Err(ExchangeError::Invalid(_))
    ));
    assert_eq!(projection(&mut store, &appointed).unwrap().len(), 2);
}
#[test]
fn authorization_revokes_cached_projection_and_buffer_but_keeps_history_and_local_source() {
    let host = tempfile::tempdir().unwrap();
    let mut store = open(host.path());
    let native = Peer::new();
    let appointed = appoint(&mut store, peer(), &native).unwrap();
    let one = native.event(appointed.spec(), appointed.id(), 1, "one");
    let gap = native.event(appointed.spec(), appointed.id(), 3, "three");
    native.state.borrow_mut().events = vec![one, gap];
    poll(&mut store, &appointed, "sync").unwrap();
    native.state.borrow_mut().offline = true;
    assert!(matches!(
        projection(&mut store, &appointed),
        Err(ExchangeError::Offline)
    ));
    assert_eq!(count(&store, "local_exchange_projection"), 1);
    {
        let mut s = native.state.borrow_mut();
        s.offline = false;
        s.revoked = true;
    }
    assert!(matches!(
        projection(&mut store, &appointed),
        Err(ExchangeError::Forbidden)
    ));
    assert_eq!(count(&store, "local_exchange_projection"), 0);
    assert_eq!(count(&store, "local_exchange_events"), 2);
    let pending: i64 = store
        .read_connection()
        .query_row(
            "SELECT count(*) FROM local_exchange_events WHERE applied=0",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pending, 0);
    native.state.borrow_mut().revoked = false;
    assert!(matches!(
        projection(&mut store, &appointed),
        Err(ExchangeError::Incomplete)
    ));
    assert_eq!(count(&store, "retained_snapshots"), 0);
}
#[test]
fn final_native_lease_failure_rolls_back_receipts_cursor_and_projection() {
    let host = tempfile::tempdir().unwrap();
    let mut store = open(host.path());
    let native = Peer::new();
    let appointed = appoint(&mut store, peer(), &native).unwrap();
    native.state.borrow_mut().events =
        vec![native.event(appointed.spec(), appointed.id(), 1, "one")];
    native.state.borrow_mut().revoke_after_events = true;
    assert!(matches!(
        poll(&mut store, &appointed, "sync"),
        Err(ExchangeError::Forbidden)
    ));
    assert_eq!(count(&store, "local_exchange_events"), 0);
    assert_eq!(count(&store, "local_exchange_streams"), 0);
    assert_eq!(count(&store, "local_exchange_projection"), 0);
}
#[test]
fn changed_visibility_requires_complete_snapshot_and_rejects_mixed_incomplete_or_rollback() {
    let host = tempfile::tempdir().unwrap();
    let mut store = open(host.path());
    let native = Peer::new();
    let appointed = appoint(&mut store, peer(), &native).unwrap();
    native.state.borrow_mut().events =
        vec![native.event(appointed.spec(), appointed.id(), 1, "old")];
    poll(&mut store, &appointed, "sync").unwrap();
    native.state.borrow_mut().visibility = 1;
    assert!(matches!(
        projection(&mut store, &appointed),
        Err(ExchangeError::Incomplete)
    ));
    assert_eq!(count(&store, "local_exchange_projection"), 0);
    let event = native.event(appointed.spec(), appointed.id(), 2, "new");
    let mut incomplete = native.page(appointed.spec(), appointed.id(), vec![event.clone()], 2);
    incomplete.snapshot.item_count = 2;
    incomplete.next_offset = Some(1);
    native.state.borrow_mut().pages = VecDeque::from([incomplete]);
    assert!(matches!(
        recover_snapshot(&mut store, &appointed, "incomplete"),
        Err(ExchangeError::Incomplete)
    ));
    assert_eq!(count(&store, "local_exchange_projection"), 0);
    let mut first = native.page(appointed.spec(), appointed.id(), vec![event.clone()], 3);
    first.snapshot.item_count = 2;
    first.next_offset = Some(1);
    let other = native.event(appointed.spec(), appointed.id(), 3, "other");
    let mut last = native.page(appointed.spec(), appointed.id(), vec![other], 3);
    last.snapshot.item_count = 2;
    last.snapshot.identity = sha('d');
    last.offset = 1;
    native.state.borrow_mut().pages = VecDeque::from([first, last]);
    assert!(matches!(
        recover_snapshot(&mut store, &appointed, "mixed"),
        Err(ExchangeError::Stale)
    ));
    assert_eq!(count(&store, "local_exchange_projection"), 0);
    let complete = native.page(appointed.spec(), appointed.id(), vec![event.clone()], 2);
    native.state.borrow_mut().pages = VecDeque::from([complete]);
    assert_eq!(
        recover_snapshot(&mut store, &appointed, "complete")
            .unwrap()
            .position,
        2
    );
    assert_eq!(
        projection(&mut store, &appointed).unwrap(),
        vec![event.packet]
    );
    let rollback = native.page(appointed.spec(), appointed.id(), vec![], 1);
    native.state.borrow_mut().pages = VecDeque::from([rollback]);
    assert!(matches!(
        recover_snapshot(&mut store, &appointed, "rollback"),
        Err(ExchangeError::Stale)
    ));
}
#[test]
fn snapshot_pages_are_complete_atomic_and_duplicates_across_pages_are_refused() {
    let host = tempfile::tempdir().unwrap();
    let mut store = open(host.path());
    let native = Peer::new();
    let appointed = appoint(&mut store, peer(), &native).unwrap();
    let one = native.event(appointed.spec(), appointed.id(), 1, "one");
    let two = native.event(appointed.spec(), appointed.id(), 2, "two");
    let mut first = native.page(appointed.spec(), appointed.id(), vec![one.clone()], 2);
    first.snapshot.item_count = 2;
    first.next_offset = Some(1);
    let mut last = native.page(appointed.spec(), appointed.id(), vec![two.clone()], 2);
    last.snapshot.item_count = 2;
    last.offset = 1;
    native.state.borrow_mut().pages = VecDeque::from([first.clone(), last]);
    assert_eq!(
        recover_snapshot(&mut store, &appointed, "snapshot")
            .unwrap()
            .position,
        2
    );
    assert_eq!(projection(&mut store, &appointed).unwrap().len(), 2);
    let mut duplicate = native.page(appointed.spec(), appointed.id(), vec![one], 2);
    duplicate.snapshot.item_count = 2;
    duplicate.offset = 1;
    native.state.borrow_mut().pages = VecDeque::from([first, duplicate]);
    assert!(matches!(
        recover_snapshot(&mut store, &appointed, "duplicate"),
        Err(ExchangeError::Invalid(_))
    ));
    assert_eq!(projection(&mut store, &appointed).unwrap().len(), 2);
    native.state.borrow_mut().visibility = 1;
    let mut new = two;
    new.stream = native.witness(appointed.spec(), appointed.id());
    let page = native.page(appointed.spec(), appointed.id(), vec![new], 2);
    native.state.borrow_mut().pages = VecDeque::from([page]);
    native.state.borrow_mut().revoke_after_page = true;
    assert!(matches!(
        recover_snapshot(&mut store, &appointed, "revoked"),
        Err(ExchangeError::Forbidden)
    ));
    assert_eq!(count(&store, "local_exchange_projection"), 0);
}
#[test]
fn expired_access_and_disable_cannot_resurrect_cached_peer_data() {
    let host = tempfile::tempdir().unwrap();
    let mut store = open(host.path());
    let native = Peer::new();
    let appointed = appoint(&mut store, peer(), &native).unwrap();
    native.state.borrow_mut().events =
        vec![native.event(appointed.spec(), appointed.id(), 1, "one")];
    poll(&mut store, &appointed, "sync").unwrap();
    native.state.borrow_mut().expired = true;
    assert!(matches!(
        projection(&mut store, &appointed),
        Err(ExchangeError::Expired)
    ));
    assert_eq!(count(&store, "local_exchange_projection"), 0);
    native.state.borrow_mut().expired = false;
    disable(&mut store, &appointed).unwrap();
    assert!(matches!(
        projection(&mut store, &appointed),
        Err(ExchangeError::Forbidden)
    ));
    let replay = appoint(&mut store, peer(), &native).unwrap();
    assert_eq!(replay.id(), appointed.id());
    assert!(matches!(
        projection(&mut store, &replay),
        Err(ExchangeError::Forbidden)
    ));
}
#[test]
fn source_scope_native_stream_and_local_epoch_are_independent_boundaries() {
    let host = tempfile::tempdir().unwrap();
    let mut store = open(host.path());
    let native = Peer::new();
    let appointed = appoint(&mut store, peer(), &native).unwrap();
    let mut event = native.event(appointed.spec(), appointed.id(), 1, "other scope");
    if let Metadata::Contribution { scope, .. } = &mut event.packet {
        *scope = vec!["private/secret.rs".into()];
    }
    native.state.borrow_mut().events = vec![event];
    assert!(matches!(
        poll(&mut store, &appointed, "scope"),
        Err(ExchangeError::Forbidden)
    ));
    assert_eq!(count(&store, "local_exchange_events"), 0);
    let epoch = crate::local_authority::status(&store).unwrap().epoch;
    let frozen = crate::local_authority::freeze(&mut store, &epoch, "controlled restore").unwrap();
    assert!(matches!(
        projection(&mut store, &appointed),
        Err(ExchangeError::Authority(
            crate::local_authority::AuthorityError::Frozen
        ))
    ));
    let resumed = crate::local_authority::resume(&mut store, &frozen.epoch).unwrap();
    assert_ne!(resumed.epoch, epoch);
    assert!(matches!(
        projection(&mut store, &appointed),
        Err(ExchangeError::Authority(
            crate::local_authority::AuthorityError::Stale
        ))
    ));
    let fresh = appoint(&mut store, peer(), &native).unwrap();
    assert_ne!(fresh.id(), appointed.id());
    assert!(projection(&mut store, &fresh).unwrap().is_empty());
}
#[test]
fn local_disable_during_native_io_cannot_commit_ack_events_or_snapshot() {
    for route in ["send", "events", "snapshot"] {
        let host = tempfile::tempdir().unwrap();
        let mut store = open(host.path());
        let native = Peer::new();
        let appointed = appoint(&mut store, peer(), &native).unwrap();
        let sequence = enqueue(&mut store, &packet()).unwrap();
        let event = native.event(appointed.spec(), appointed.id(), 1, "one");
        native.state.borrow_mut().events = vec![event.clone()];
        native.state.borrow_mut().pages =
            VecDeque::from([native.page(appointed.spec(), appointed.id(), vec![event], 1)]);
        let host_path = host.path().to_owned();
        let spec = peer();
        let adapter_state = native.state.clone();
        native.state.borrow_mut().action = Some(Box::new(move || {
            let mut second = open(&host_path);
            let operator_adapter = Peer {
                state: adapter_state.clone(),
            };
            let operator = appoint(&mut second, spec.clone(), &operator_adapter).unwrap();
            disable(&mut second, &operator).unwrap();
        }));
        let denied = match route {
            "send" => matches!(
                deliver(&mut store, &appointed, sequence),
                Err(ExchangeError::Forbidden)
            ),
            "events" => matches!(
                poll(&mut store, &appointed, "sync"),
                Err(ExchangeError::Forbidden)
            ),
            "snapshot" => matches!(
                recover_snapshot(&mut store, &appointed, "snapshot"),
                Err(ExchangeError::Forbidden)
            ),
            _ => unreachable!(),
        };
        assert!(denied, "{route}");
        assert_eq!(count(&store, "local_exchange_projection"), 0);
        assert_eq!(count(&store, "local_exchange_events"), 0);
        assert_eq!(count(&store, "local_exchange_streams"), 0);
        let ack: i64 = store
            .read_connection()
            .query_row(
                "SELECT count(*) FROM local_exchange_deliveries WHERE acknowledged=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ack, 0, "{route}");
    }
}
#[test]
fn migration_preserves_capture_rows_and_operation_constraints_without_fake_captures() {
    let host = tempfile::tempdir().unwrap();
    let store = open(host.path());
    let db = store.project_dir().join("state.db");
    drop(store);
    let conn = rusqlite::Connection::open(&db).unwrap();
    crate::collaboration_sensitive::drop_sensitive_response_tables_for_legacy_fixture(&conn)
        .unwrap();
    // Reconstruct schema 14, including removal of later stack-owned tables.
    // A current store marker alone is not a historical schema fixture.
    conn.execute_batch(
        "PRAGMA foreign_keys=ON;
        DROP TABLE IF EXISTS local_import_rehearsals;
        DROP TABLE IF EXISTS local_imported_records;
        DROP TABLE IF EXISTS local_imported_sources;
        DROP TABLE IF EXISTS local_imports;
        DROP TABLE IF EXISTS local_adoption_receipts;
        DROP TABLE IF EXISTS local_agent_reconciliations;
        DROP TABLE IF EXISTS local_agent_dispatches;
        DROP TABLE IF EXISTS local_handoffs;
        DROP TABLE IF EXISTS local_agent_sessions;",
    )
    .unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON;
      DROP TABLE local_native_gate_roots; DROP TABLE local_native_gate_jobs;
      DROP TABLE local_exchange_messages; DROP TABLE local_exchange_deliveries; DROP TABLE local_exchange_streams;
      DROP TABLE local_exchange_events; DROP TABLE local_exchange_projection; DROP TABLE local_exchange_observations; DROP TABLE local_exchange_bindings;
      DROP TRIGGER outbox_capture_insert; DROP TRIGGER outbox_capture_update; DROP TRIGGER outbox_capture_delete;
      DROP TABLE outbox;
      CREATE TABLE outbox(sequence INTEGER PRIMARY KEY AUTOINCREMENT,operation_id TEXT NOT NULL UNIQUE REFERENCES capture_operations(operation_id),kind TEXT NOT NULL,payload_record_id TEXT NOT NULL,created_ms INTEGER NOT NULL,delivered_ms INTEGER) STRICT;
      INSERT INTO capture_operations(operation_id,request_digest,repository,base_commit,result_commit,policy,reserved_bytes,state,created_ms,updated_ms) VALUES('capture-original','digest','Playground','base','result','required',0,'committed',10,11);
      INSERT INTO outbox VALUES(41,'capture-original','capture_receipt','original-receipt',12,25);
      UPDATE meta SET value='14' WHERE key IN ('schema_version','min_compatible_schema');").unwrap();
    drop(conn);
    let mut store = open(host.path());
    assert_eq!(
        store.schema_version(),
        crate::collaboration_state::COLLABORATION_STATE_SCHEMA_VERSION
    );
    let row: (i64, String, String, String, i64, Option<i64>) = store
        .read_connection()
        .query_row("SELECT * FROM outbox", [], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })
        .unwrap();
    assert_eq!(
        row,
        (
            41,
            "capture-original".into(),
            "capture_receipt".into(),
            "original-receipt".into(),
            12,
            Some(25)
        )
    );
    assert!(
        store
            .connection()
            .execute(
                "DELETE FROM capture_operations WHERE operation_id='capture-original'",
                []
            )
            .is_err()
    );
    assert!(store.connection().execute("INSERT INTO outbox(operation_id,kind,payload_record_id,created_ms) VALUES('missing','capture_receipt','receipt',1)",[]).is_err());
    assert!(
        store
            .connection()
            .execute(
                "UPDATE outbox SET operation_id='missing' WHERE sequence=41",
                []
            )
            .is_err()
    );
    let sequence = enqueue(&mut store, &packet()).unwrap();
    assert!(sequence > 41);
    assert_eq!(count(&store, "capture_operations"), 1);
}
#[test]
fn actual_capture_outbox_delivers_immutable_metadata_without_changing_capture_identity() {
    use crate::collaboration_capture::{
        self as cap, CaptureOutcome, CapturePolicy, CaptureRequest, OperationId, RetentionBoundary,
    };
    let mut f = crate::local_target::tests::Fixture::new();
    let repo = f._repo.path().to_owned();
    let base = crate::collaboration_archive::pin_commit(&repo, "HEAD~1").unwrap();
    let result = crate::collaboration_archive::pin_commit(&repo, "HEAD").unwrap();
    let operation = OperationId::mint().unwrap();
    let request = CaptureRequest {
        operation_id: operation.clone(),
        repository: repo,
        base,
        result,
        policy: CapturePolicy::Required,
        retention: RetentionBoundary::UntilReleased,
    };
    let receipt = match cap::capture(&mut f.store, &request).unwrap() {
        CaptureOutcome::Acknowledged(receipt) => receipt,
        _ => panic!("capture did not acknowledge"),
    };
    let sequence: i64 = f
        .store
        .read_connection()
        .query_row(
            "SELECT sequence FROM outbox WHERE operation_id=?1",
            [operation.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    let metadata = Metadata::Contribution {
        contribution: receipt.contribution.to_string(),
        source: receipt.result.to_string(),
        availability: SourceAvailability::LocalOnly,
        scope: vec!["two.txt".into()],
        brief: Some("captured pending work".into()),
    };
    let native = Peer::new();
    let mut spec = peer();
    spec.scope = Scope::Project;
    let appointed = appoint(&mut f.store, spec, &native).unwrap();
    for _ in 0..appointed.spec().max_attempts {
        assert!(matches!(
            deliver(&mut f.store, &appointed, sequence),
            Err(ExchangeError::Incomplete)
        ));
    }
    assert!(native.state.borrow().sends.is_empty());
    assert_eq!(count(&f.store, "local_exchange_deliveries"), 0);
    let wire = stage_capture_metadata(&mut f.store, sequence, &metadata).unwrap();
    assert_ne!(wire, receipt.receipt_record_id);
    assert_eq!(
        stage_capture_metadata(&mut f.store, sequence, &metadata).unwrap(),
        wire
    );
    native.state.borrow_mut().lost_ack = true;
    assert!(matches!(
        deliver(&mut f.store, &appointed, sequence),
        Err(ExchangeError::Offline)
    ));
    let mut changed = metadata.clone();
    if let Metadata::Contribution { brief, .. } = &mut changed {
        *brief = Some("changed after unknown send".into());
    }
    assert!(matches!(
        stage_capture_metadata(&mut f.store, sequence, &changed),
        Err(ExchangeError::Invalid(_))
    ));
    native.state.borrow_mut().lost_ack = false;
    assert_eq!(
        deliver(&mut f.store, &appointed, sequence).unwrap(),
        Delivery::Acknowledged
    );
    assert_eq!(native.state.borrow().sends, vec![wire.to_string(); 2]);
    let row: (String, String, Option<i64>) = f
        .store
        .read_connection()
        .query_row(
            "SELECT operation_id,payload_record_id,delivered_ms FROM outbox WHERE sequence=?1",
            [sequence],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        row,
        (
            operation.as_str().to_owned(),
            receipt.receipt_record_id.to_string(),
            None
        )
    );
    native.state.borrow_mut().revoked = true;
    assert!(matches!(
        projection(&mut f.store, &appointed),
        Err(ExchangeError::Forbidden)
    ));
    assert!(
        crate::collaboration_archive::retained(&f.store, &receipt.result)
            .unwrap()
            .is_some()
    );
    let current = cap::receipt(&mut f.store, &operation).unwrap().unwrap();
    assert_eq!(current.contribution, receipt.contribution);
    assert_eq!(current.status, "retained_local");
    let mut forged = metadata;
    if let Metadata::Contribution { source, .. } = &mut forged {
        *source = sha('f');
    }
    assert!(matches!(
        stage_capture_metadata(&mut f.store, sequence, &forged),
        Err(ExchangeError::Invalid(_))
    ));
}
#[test]
fn bounded_reorder_buffer_rolls_back_the_excess_batch() {
    let host = tempfile::tempdir().unwrap();
    let mut store = open(host.path());
    let native = Peer::new();
    let appointed = appoint(&mut store, peer(), &native).unwrap();
    for batch in 0..4 {
        native.state.borrow_mut().events = (0..64)
            .map(|n| {
                native.event(
                    appointed.spec(),
                    appointed.id(),
                    2 + batch * 64 + n,
                    &format!("gap-{batch}-{n}"),
                )
            })
            .collect();
        assert_eq!(
            poll(&mut store, &appointed, &format!("batch-{batch}"))
                .unwrap()
                .position,
            0
        );
    }
    assert_eq!(count(&store, "local_exchange_events"), 256);
    native.state.borrow_mut().events =
        vec![native.event(appointed.spec(), appointed.id(), 300, "too many buffered")];
    assert!(matches!(
        poll(&mut store, &appointed, "overflow"),
        Err(ExchangeError::Limit)
    ));
    assert_eq!(count(&store, "local_exchange_events"), 256);
    assert!(projection(&mut store, &appointed).unwrap().is_empty());
    native.state.borrow_mut().events =
        vec![native.event(appointed.spec(), appointed.id(), 1, "one")];
    assert_eq!(
        poll(&mut store, &appointed, "fill-gap").unwrap().position,
        257
    );
    assert_eq!(projection(&mut store, &appointed).unwrap().len(), 257);
}
#[test]
fn explicit_empty_snapshot_is_complete_and_retained_metadata_survives_actual_gc() {
    use crate::collaboration_archive::{self as archive, ObjectDigest};
    use crate::collaboration_gc::{self as gc, GcOptions};
    let host = tempfile::tempdir().unwrap();
    let mut store = open(host.path());
    let native = Peer::new();
    let appointed = appoint(&mut store, peer(), &native).unwrap();
    let queued = packet();
    enqueue(&mut store, &queued).unwrap();
    let event = native.event(appointed.spec(), appointed.id(), 1, "one");
    let event_digest = ObjectDigest::of(&event.record().unwrap().canonical_bytes());
    native.state.borrow_mut().events = vec![event];
    poll(&mut store, &appointed, "sync").unwrap();
    let queued_digest = ObjectDigest::of(&queued.record().unwrap().canonical_bytes());
    let junk = archive::put_object(&store, b"unreferenced exchange fixture").unwrap();
    for prefix in std::fs::read_dir(archive::objects_dir(&store)).unwrap() {
        for object in std::fs::read_dir(prefix.unwrap().path()).unwrap() {
            std::fs::File::open(object.unwrap().path())
                .unwrap()
                .set_times(
                    std::fs::FileTimes::new()
                        .set_modified(UNIX_EPOCH + std::time::Duration::from_secs(1)),
                )
                .unwrap();
        }
    }
    let plan = gc::plan(&mut store, &GcOptions::default()).unwrap();
    gc::apply(&mut store, &plan.digest, &GcOptions::default()).unwrap();
    assert!(archive::read_object(&store, &junk).is_err());
    assert!(archive::read_object(&store, &queued_digest).is_ok());
    assert!(archive::read_object(&store, &event_digest).is_ok());
    let page = native.page(appointed.spec(), appointed.id(), vec![], 1);
    native.state.borrow_mut().pages = VecDeque::from([page]);
    assert_eq!(
        recover_snapshot(&mut store, &appointed, "empty")
            .unwrap()
            .position,
        1
    );
    assert!(projection(&mut store, &appointed).unwrap().is_empty());
    assert_eq!(count(&store, "local_exchange_events"), 1);
    // An older historical event does not repopulate the empty current snapshot.
    native.state.borrow_mut().events =
        vec![native.event(appointed.spec(), appointed.id(), 1, "one")];
    assert_eq!(
        poll(&mut store, &appointed, "old-event").unwrap().position,
        1
    );
    assert!(projection(&mut store, &appointed).unwrap().is_empty());
}
#[test]
fn capture_envelope_checks_archived_receipt_against_damaged_index_and_missing_bytes() {
    use crate::collaboration_archive as archive;
    use crate::collaboration_capture::{
        self as cap, CaptureOutcome, CapturePolicy, CaptureRequest, OperationId, RetentionBoundary,
    };
    let mut f = crate::local_target::tests::Fixture::new();
    let repo = f._repo.path().to_owned();
    let base = archive::pin_commit(&repo, "HEAD~1").unwrap();
    let result = archive::pin_commit(&repo, "HEAD").unwrap();
    let operation = OperationId::mint().unwrap();
    let request = CaptureRequest {
        operation_id: operation.clone(),
        repository: repo,
        base,
        result,
        policy: CapturePolicy::Required,
        retention: RetentionBoundary::UntilReleased,
    };
    let receipt = match cap::capture(&mut f.store, &request).unwrap() {
        CaptureOutcome::Acknowledged(receipt) => receipt,
        _ => panic!("capture did not acknowledge"),
    };
    let sequence: i64 = f
        .store
        .read_connection()
        .query_row(
            "SELECT sequence FROM outbox WHERE operation_id=?1",
            [operation.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    let mut metadata = Metadata::Contribution {
        contribution: receipt.contribution.to_string(),
        source: sha('f'),
        availability: SourceAvailability::LocalOnly,
        scope: vec!["two.txt".into()],
        brief: None,
    };
    f.store
        .connection()
        .execute(
            "UPDATE capture_receipts SET result_snapshot=?2 WHERE operation_id=?1",
            rusqlite::params![operation.as_str(), sha('f')],
        )
        .unwrap();
    let damaged = stage_capture_metadata(&mut f.store, sequence, &metadata);
    assert!(
        matches!(&damaged, Err(ExchangeError::Invalid(_))),
        "{damaged:?}"
    );
    assert_eq!(count(&f.store, "local_exchange_messages"), 0);
    f.store
        .connection()
        .execute(
            "UPDATE capture_receipts SET result_snapshot=?2 WHERE operation_id=?1",
            rusqlite::params![operation.as_str(), receipt.result.as_str()],
        )
        .unwrap();
    if let Metadata::Contribution { source, .. } = &mut metadata {
        *source = receipt.result.to_string();
    }
    let hex: String = f
        .store
        .read_connection()
        .query_row(
            "SELECT receipt_sha256 FROM capture_receipts WHERE operation_id=?1",
            [operation.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    let digest = archive::parse_digest(&hex).unwrap();
    std::fs::remove_file(archive::object_path(&f.store, &digest)).unwrap();
    assert!(matches!(
        stage_capture_metadata(&mut f.store, sequence, &metadata),
        Err(ExchangeError::Archive(
            archive::ArchiveError::MissingObject { .. }
        ))
    ));
    assert_eq!(count(&f.store, "local_exchange_messages"), 0);
}

#[test]
fn staged_capture_metadata_keeps_authenticated_receipt_after_source_release_and_gc() {
    use crate::collaboration_archive::{self as archive, ObjectDigest};
    use crate::collaboration_capture::{
        self as cap, CaptureOutcome, CapturePolicy, CaptureRequest, OperationId, RetentionBoundary,
    };
    use crate::collaboration_gc::{self as gc, GcOptions};
    let mut f = crate::local_target::tests::Fixture::new();
    let repo = f._repo.path().to_owned();
    let operation = OperationId::mint().unwrap();
    let request = CaptureRequest {
        operation_id: operation.clone(),
        base: archive::pin_commit(&repo, "HEAD~1").unwrap(),
        result: archive::pin_commit(&repo, "HEAD").unwrap(),
        repository: repo,
        policy: CapturePolicy::Required,
        retention: RetentionBoundary::UntilReleased,
    };
    let receipt = match cap::capture(&mut f.store, &request).unwrap() {
        CaptureOutcome::Acknowledged(receipt) => receipt,
        _ => panic!("capture did not acknowledge"),
    };
    let (sequence, hex): (i64, String) = f.store.read_connection().query_row(
        "SELECT o.sequence,r.receipt_sha256 FROM outbox o JOIN capture_receipts r USING(operation_id) WHERE o.operation_id=?1",
        [operation.as_str()], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    let receipt_digest = archive::parse_digest(&hex).unwrap();
    let metadata = Metadata::Contribution {
        contribution: receipt.contribution.to_string(),
        source: receipt.result.to_string(),
        availability: SourceAvailability::LocalOnly,
        scope: vec!["two.txt".into()],
        brief: None,
    };
    let wire = stage_capture_metadata(&mut f.store, sequence, &metadata).unwrap();
    let native = Peer::new();
    let mut spec = peer();
    spec.scope = Scope::Project;
    let appointed = appoint(&mut f.store, spec, &native).unwrap();
    assert!(gc::release_retention(&mut f.store, operation.as_str()).unwrap());
    let junk = archive::put_object(&f.store, b"capture receipt gc regression junk").unwrap();
    for prefix in std::fs::read_dir(archive::objects_dir(&f.store)).unwrap() {
        for object in std::fs::read_dir(prefix.unwrap().path()).unwrap() {
            std::fs::File::open(object.unwrap().path())
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(UNIX_EPOCH))
                .unwrap();
        }
    }
    let plan = gc::plan(&mut f.store, &GcOptions::default()).unwrap();
    gc::apply(&mut f.store, &plan.digest, &GcOptions::default()).unwrap();
    assert!(archive::read_object(&f.store, &junk).is_err());
    assert!(archive::read_object(&f.store, &receipt_digest).is_ok());
    assert!(
        archive::read_object(
            &f.store,
            &ObjectDigest::of(&metadata.record().unwrap().canonical_bytes())
        )
        .is_ok()
    );
    // The metadata pin does not retain source authority or the source objects.
    assert!(
        archive::retained(&f.store, &receipt.result)
            .unwrap()
            .is_none()
    );
    assert_ne!(
        cap::receipt(&mut f.store, &operation)
            .unwrap()
            .unwrap()
            .status,
        "retained_local"
    );
    assert_eq!(
        deliver(&mut f.store, &appointed, sequence).unwrap(),
        Delivery::Acknowledged
    );
    assert_eq!(native.state.borrow().sends, vec![wire.to_string()]);
}
