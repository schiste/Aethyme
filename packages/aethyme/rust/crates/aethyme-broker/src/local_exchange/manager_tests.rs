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
    crate::local_authority::freeze(&mut store, &epoch, "controlled restore").unwrap();
    assert!(matches!(
        projection(&mut store, &appointed),
        Err(ExchangeError::Authority(
            crate::local_authority::AuthorityError::Frozen
        ))
    ));
}
