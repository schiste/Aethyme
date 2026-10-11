//! Durable Local consumer of the explicit trusted native peer interface.
use super::*;
use crate::collaboration_archive::{self as archive, ObjectDigest};
use crate::collaboration_gc::{self as gc, ArchiveUse};
use crate::collaboration_state::CollaborationStore;
use crate::file_lock::{ExclusiveFileLock, open_lock_file};
use crate::local_authority::{self as authority, Epoch, Guard};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_BINDINGS: i64 = 64;
const MAX_HISTORY_EVENTS: i64 = 4096;
const MAX_OBSERVATION_OPERATIONS: i64 = 1024;
const KIND: &str = "exchange_metadata_v0";
static BINDING: RecordSchema = RecordSchema {
    name: "aethyme.exchange-binding/provisional-v0",
    fields: &[FieldSpec {
        name: "spec",
        required: true,
        kind: FieldKind::Opaque,
        capability: None,
    }],
    capabilities: &[],
};
static SCOPE: RecordSchema = RecordSchema {
    name: "aethyme.exchange-scope/provisional-v0",
    fields: &[FieldSpec {
        name: "spec",
        required: true,
        kind: FieldKind::Opaque,
        capability: None,
    }],
    capabilities: &[],
};
static STREAM: RecordSchema = RecordSchema {
    name: "aethyme.exchange-stream/provisional-v0",
    fields: &[FieldSpec {
        name: "spec",
        required: true,
        kind: FieldKind::Opaque,
        capability: None,
    }],
    capabilities: &[],
};
#[derive(Serialize)]
struct BindingBody<'a> {
    epoch: &'a str,
    peer: &'a PeerSpec,
}
fn operator_guard(store: &mut CollaborationStore) -> Result<(Epoch, Guard), ExchangeError> {
    match authority::status(store) {
        Ok(witness) => {
            let guard = authority::guard(store, &witness.epoch)?;
            Ok((witness.epoch, guard))
        }
        Err(authority::AuthorityError::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(authority::acquire(store)?)
        }
        Err(error) => Err(error.into()),
    }
}
fn now() -> Result<i64, ExchangeError> {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| invalid("clock"))?
            .as_millis(),
    )
    .map_err(|_| invalid("clock"))
}
pub(super) fn scope_id(spec: &PeerSpec) -> Result<RecordId, ExchangeError> {
    Ok(canonical(&SCOPE, &spec.scope)?.id())
}
fn stream_id(stream: &StreamWitness) -> Result<RecordId, ExchangeError> {
    stream.validate()?;
    Ok(canonical(&STREAM, stream)?.id())
}
fn pin(tx: &Transaction<'_>, digest: &ObjectDigest, holder: &str) -> Result<(), ExchangeError> {
    tx.execute("INSERT INTO object_pins(class,object_sha256,holder,created_ms) VALUES('cited_evidence',?1,?2,?3)",params![digest.hex(),holder,now()?])?;
    Ok(())
}
fn packet_from(body: &[u8], expected: &str) -> Result<Metadata, ExchangeError> {
    if body.len() > MAX_PACKET_BYTES {
        return Err(ExchangeError::Limit);
    }
    let rec = Record::decode(body, &[&PACKET])?;
    if rec.id().as_str() != expected || rec.canonical_bytes() != body {
        return Err(invalid("stored packet identity"));
    }
    let value: serde_json::Value = serde_json::from_slice(body)?;
    let packet: Metadata = serde_json::from_value(value["spec"].clone())?;
    packet.validate()?;
    Ok(packet)
}
fn event_from(body: &[u8], expected: &str) -> Result<Event, ExchangeError> {
    let rec = Record::decode(body, &[&EVENT])?;
    if rec.id().as_str() != expected || rec.canonical_bytes() != body {
        return Err(invalid("stored event identity"));
    }
    let value: serde_json::Value = serde_json::from_slice(body)?;
    let event: Event = serde_json::from_value(value["spec"].clone())?;
    event.record()?;
    Ok(event)
}
/// Appointed operator code and an actual Local authority epoch. This handle
/// cannot be deserialized from a peer's record, a backup or a cached cursor.
pub struct AppointedPeer<'a> {
    id: RecordId,
    spec: PeerSpec,
    epoch: Epoch,
    adapter: &'a dyn TrustedPeer,
}
impl AppointedPeer<'_> {
    pub fn id(&self) -> &RecordId {
        &self.id
    }
    pub fn spec(&self) -> &PeerSpec {
        &self.spec
    }
}
/// Opt-in appointment pins the adapter's service identity and profile. No peer
/// is created by opening the collaboration store or by receiving a packet.
pub fn appoint<'a>(
    store: &mut CollaborationStore,
    spec: PeerSpec,
    adapter: &'a dyn TrustedPeer,
) -> Result<AppointedPeer<'a>, ExchangeError> {
    spec.validate()?;
    if adapter.profile() != spec.profile || adapter.service_id() != spec.service_id {
        return Err(ExchangeError::Unsupported);
    }
    let (epoch, _guard) = operator_guard(store)?;
    let _archive = gc::archive_use(store)?;
    let record = canonical(
        &BINDING,
        &BindingBody {
            epoch: epoch.as_str(),
            peer: &spec,
        },
    )?;
    let digest = archive::put_object(store, &record.canonical_bytes())?;
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM local_exchange_bindings WHERE id=?1)",
        [record.id().as_str()],
        |r| r.get(0),
    )?;
    if !exists {
        let count: i64 = tx.query_row("SELECT count(*) FROM local_exchange_bindings", [], |r| {
            r.get(0)
        })?;
        if count >= MAX_BINDINGS {
            return Err(ExchangeError::Limit);
        }
        tx.execute(
            "INSERT INTO local_exchange_bindings(id,epoch,body,enabled) VALUES(?1,?2,?3,1)",
            params![
                record.id().as_str(),
                epoch.as_str(),
                record.canonical_bytes()
            ],
        )?;
        pin(&tx, &digest, record.id().as_str())?;
    }
    tx.commit()?;
    Ok(AppointedPeer {
        id: record.id(),
        spec,
        epoch,
        adapter,
    })
}
fn clear_projection(conn: &rusqlite::Connection, id: &RecordId) -> Result<(), ExchangeError> {
    conn.execute(
        "DELETE FROM local_exchange_projection WHERE binding=?1",
        [id.as_str()],
    )?;
    conn.execute(
        "UPDATE local_exchange_events SET applied=2 WHERE binding=?1 AND applied=0",
        [id.as_str()],
    )?;
    conn.execute(
        "UPDATE local_exchange_streams SET valid=0 WHERE binding=?1",
        [id.as_str()],
    )?;
    Ok(())
}
fn revoke(store: &mut CollaborationStore, id: &RecordId) -> Result<(), ExchangeError> {
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    clear_projection(&tx, id)?;
    tx.commit()?;
    Ok(())
}
fn is_revocation(e: &ExchangeError) -> bool {
    matches!(
        e,
        ExchangeError::Forbidden | ExchangeError::Stale | ExchangeError::Expired
    )
}
/// Disable is operator action under the Local epoch; retained history and
/// independently retained source remain intact, remote current views are gone.
pub fn disable(
    store: &mut CollaborationStore,
    peer: &AppointedPeer<'_>,
) -> Result<(), ExchangeError> {
    let _guard = authority::guard(store, &peer.epoch)?;
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "UPDATE local_exchange_bindings SET enabled=0 WHERE id=?1",
        [peer.id.as_str()],
    )?;
    clear_projection(&tx, &peer.id)?;
    tx.commit()?;
    Ok(())
}
struct CurrentPeer<'a> {
    lease: Box<dyn NativeAuthorizationLease + 'a>,
    _local: Guard,
    _archive: ArchiveUse,
    deadline: i64,
}
impl CurrentPeer<'_> {
    fn check(&self) -> Result<(), ExchangeError> {
        self.lease.check_current()?;
        let t = now()?;
        if t >= self.lease.valid_until_ms() || t >= self.deadline {
            return Err(ExchangeError::Expired);
        }
        Ok(())
    }
    fn check_in(
        &self,
        conn: &rusqlite::Connection,
        peer: &AppointedPeer<'_>,
    ) -> Result<(), ExchangeError> {
        let (body, epoch, enabled): (Vec<u8>, String, bool) = conn.query_row(
            "SELECT body,epoch,enabled FROM local_exchange_bindings WHERE id=?1",
            [peer.id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let record = canonical(
            &BINDING,
            &BindingBody {
                epoch: peer.epoch.as_str(),
                peer: &peer.spec,
            },
        )?;
        if !enabled
            || epoch != peer.epoch.as_str()
            || record.id() != peer.id
            || body != record.canonical_bytes()
        {
            return Err(ExchangeError::Forbidden);
        }
        self.check()
    }
    fn visible(&self, spec: &PeerSpec, packet: &Metadata) -> Result<(), ExchangeError> {
        packet.validate()?;
        if !self.lease.visible(packet) {
            return Err(ExchangeError::Forbidden);
        }
        if let Scope::Paths(paths) = &spec.scope {
            match packet {
                Metadata::Contribution { scope, .. }
                    if scope.iter().all(|v| {
                        paths.iter().any(|p| {
                            v == p || v.strip_prefix(p).is_some_and(|rest| rest.starts_with('/'))
                        })
                    }) => {}
                _ => return Err(ExchangeError::Forbidden),
            }
        }
        Ok(())
    }
}
fn current<'a>(
    store: &mut CollaborationStore,
    peer: &'a AppointedPeer<'a>,
) -> Result<CurrentPeer<'a>, ExchangeError> {
    let local = authority::guard(store, &peer.epoch)?;
    let use_archive = gc::archive_use(store)?;
    let (body, epoch, enabled): (Vec<u8>, String, bool) = store.read_connection().query_row(
        "SELECT body,epoch,enabled FROM local_exchange_bindings WHERE id=?1",
        [peer.id.as_str()],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let rec = canonical(
        &BINDING,
        &BindingBody {
            epoch: peer.epoch.as_str(),
            peer: &peer.spec,
        },
    )?;
    if !enabled
        || body != rec.canonical_bytes()
        || rec.id() != peer.id
        || epoch != peer.epoch.as_str()
    {
        return Err(ExchangeError::Forbidden);
    }
    if peer.adapter.profile() != peer.spec.profile
        || peer.adapter.service_id() != peer.spec.service_id
    {
        return Err(ExchangeError::Unsupported);
    }
    let deadline = now()?
        .checked_add(i64::from(peer.spec.deadline_seconds) * 1000)
        .ok_or(ExchangeError::Limit)?;
    let result = (|| {
        let lease = peer.adapter.authorize(&peer.spec, &peer.id)?;
        let stream = lease.stream();
        stream.validate()?;
        if stream.binding != peer.id.as_str()
            || stream.reader != peer.spec.reader
            || stream.scope_digest != scope_id(&peer.spec)?.as_str()
        {
            return Err(ExchangeError::Stale);
        }
        let proof = CurrentPeer {
            lease,
            _local: local,
            _archive: use_archive,
            deadline,
        };
        proof.check()?;
        Ok(proof)
    })();
    if result.as_ref().err().is_some_and(is_revocation) {
        revoke(store, &peer.id)?;
    }
    result
}
fn peer_lock(
    store: &CollaborationStore,
    peer: &AppointedPeer<'_>,
) -> Result<ExclusiveFileLock, ExchangeError> {
    Ok(ExclusiveFileLock::acquire(open_lock_file(
        &store.project_dir().join(format!(
            "exchange-{}.lock",
            peer.id.as_str().trim_start_matches("sha256:")
        )),
    )?)?)
}
/// Queue metadata and a CAS retention pin in one durable transaction using the
/// existing capture outbox. A queue row promises no delivery or source access.
pub fn enqueue(store: &mut CollaborationStore, packet: &Metadata) -> Result<i64, ExchangeError> {
    let (_epoch, _local) = operator_guard(store)?;
    let _use = gc::archive_use(store)?;
    let record = packet.record()?;
    let operation = format!("exchange:{}", record.id().as_str());
    if let Some(sequence) = store
        .read_connection()
        .query_row(
            "SELECT sequence FROM outbox WHERE operation_id=?1",
            [&operation],
            |r| r.get(0),
        )
        .optional()?
    {
        return Ok(sequence);
    }
    let digest = archive::put_object(store, &record.canonical_bytes())?;
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let count: i64 = tx.query_row("SELECT count(*) FROM local_exchange_messages", [], |r| {
        r.get(0)
    })?;
    if count >= MAX_VISIBLE_ITEMS as i64 {
        return Err(ExchangeError::Limit);
    }
    tx.execute("INSERT INTO outbox(operation_id,kind,payload_record_id,created_ms) VALUES(?1,?2,?3,?4) ON CONFLICT(operation_id) DO NOTHING",params![operation,KIND,record.id().as_str(),now()?])?;
    let sequence: i64 = tx.query_row(
        "SELECT sequence FROM outbox WHERE operation_id=?1",
        [&operation],
        |r| r.get(0),
    )?;
    tx.execute(
        "INSERT INTO local_exchange_messages(sequence,body) VALUES(?1,?2) ON CONFLICT DO NOTHING",
        params![sequence, record.canonical_bytes()],
    )?;
    pin(&tx, &digest, &operation)?;
    tx.commit()?;
    Ok(sequence)
}
fn validate_capture_envelope(
    store: &mut CollaborationStore,
    operation: &str,
    payload: &str,
    packet: &Metadata,
) -> Result<(), ExchangeError> {
    let operation = crate::collaboration_capture::OperationId::parse(operation)?;
    let receipt = crate::collaboration_capture::receipt(store, &operation)?
        .ok_or(ExchangeError::Incomplete)?;
    match packet {
        Metadata::Contribution {
            contribution,
            source,
            ..
        } if contribution == receipt.contribution.as_str()
            && source == receipt.result.as_str()
            && payload == receipt.receipt_record_id.as_str() =>
        {
            Ok(())
        }
        _ => Err(invalid("capture envelope subject")),
    }
}
/// Attach one immutable metadata envelope to a real committed capture outbox
/// row. The original sequence, capture operation, payload receipt and global
/// delivery marker remain the capture's; the wire envelope has its own identity.
pub fn stage_capture_metadata(
    store: &mut CollaborationStore,
    sequence: i64,
    packet: &Metadata,
) -> Result<RecordId, ExchangeError> {
    let (_epoch, _local) = operator_guard(store)?;
    let _use = gc::archive_use(store)?;
    let (operation, kind, payload): (String, String, String) = store.read_connection().query_row(
        "SELECT operation_id,kind,payload_record_id FROM outbox WHERE sequence=?1",
        [sequence],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if kind != "capture_receipt" {
        return Err(ExchangeError::Unsupported);
    }
    let record = packet.record()?;
    validate_capture_envelope(store, &operation, &payload, packet)?;
    let digest = archive::put_object(store, &record.canonical_bytes())?;
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let old: Option<Vec<u8>> = tx
        .query_row(
            "SELECT body FROM local_exchange_messages WHERE sequence=?1",
            [sequence],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(body) = old {
        if body != record.canonical_bytes() {
            return Err(invalid("capture envelope is immutable"));
        }
        return Ok(record.id());
    }
    let count: i64 = tx.query_row("SELECT count(*) FROM local_exchange_messages", [], |r| {
        r.get(0)
    })?;
    if count >= MAX_VISIBLE_ITEMS as i64 {
        return Err(ExchangeError::Limit);
    }
    tx.execute(
        "INSERT INTO local_exchange_messages(sequence,body) VALUES(?1,?2)",
        params![sequence, record.canonical_bytes()],
    )?;
    pin(&tx, &digest, &format!("exchange-capture:{sequence}"))?;
    tx.commit()?;
    Ok(record.id())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Both variants describe a historical metadata acknowledgment only.
/// They grant no current remote visibility, source access or adoption.
pub enum Delivery {
    Acknowledged,
    AlreadyAcknowledged,
}
/// Each peer/message attempt is reserved before native I/O. Unknown results
/// keep the same message identity and consume the finite allowance on restart.
pub fn deliver(
    store: &mut CollaborationStore,
    peer: &AppointedPeer<'_>,
    sequence: i64,
) -> Result<Delivery, ExchangeError> {
    let _local = authority::guard(store, &peer.epoch)?;
    let _lock = peer_lock(store, peer)?;
    let _use = gc::archive_use(store)?;
    let (kind, payload, operation): (String, String, String) = store.read_connection().query_row(
        "SELECT kind,payload_record_id,operation_id FROM outbox WHERE sequence=?1",
        [sequence],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if kind != KIND && kind != "capture_receipt" {
        return Err(ExchangeError::Unsupported);
    }
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let enabled: bool = tx.query_row(
        "SELECT enabled FROM local_exchange_bindings WHERE id=?1",
        [peer.id.as_str()],
        |r| r.get(0),
    )?;
    if !enabled {
        return Err(ExchangeError::Forbidden);
    }
    tx.execute("INSERT INTO local_exchange_deliveries(binding,sequence,attempts,acknowledged) VALUES(?1,?2,0,0) ON CONFLICT DO NOTHING",params![peer.id.as_str(),sequence])?;
    let (attempts,ack):(i64,bool)=tx.query_row("SELECT attempts,acknowledged FROM local_exchange_deliveries WHERE binding=?1 AND sequence=?2",params![peer.id.as_str(),sequence],|r|Ok((r.get(0)?,r.get(1)?)))?;
    if ack {
        return Ok(Delivery::AlreadyAcknowledged);
    }
    if attempts >= i64::from(peer.spec.max_attempts) {
        return Err(ExchangeError::Limit);
    }
    tx.execute(
        "UPDATE local_exchange_deliveries SET attempts=attempts+1 WHERE binding=?1 AND sequence=?2",
        params![peer.id.as_str(), sequence],
    )?;
    tx.commit()?;
    let proof = current(store, peer)?;
    let result = (|| {
        let queued: Vec<u8> = store.read_connection().query_row(
            "SELECT body FROM local_exchange_messages WHERE sequence=?1",
            [sequence],
            |r| r.get(0),
        )?;
        let digest = ObjectDigest::of(&queued);
        let body = archive::read_object(store, &digest)?;
        let envelope = Record::decode(&body, &[&PACKET])?;
        let id = envelope.id();
        let packet = packet_from(&body, id.as_str())?;
        if kind == KIND {
            if id.as_str() != payload {
                return Err(invalid("outbox envelope identity"));
            }
        } else {
            validate_capture_envelope(store, &operation, &payload, &packet)?;
        }
        proof.visible(&peer.spec, &packet)?;
        let record = packet.record()?;
        let ack = peer.adapter.send(&*proof.lease, &record)?;
        ack.validate_for(proof.lease.stream(), &id)?;
        let tx = store
            .connection()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("UPDATE local_exchange_deliveries SET acknowledged=1,ack=?3 WHERE binding=?1 AND sequence=?2",params![peer.id.as_str(),sequence,serde_json::to_vec(&ack)?])?;
        proof.check_in(&tx, peer)?;
        tx.commit()?;
        Ok(Delivery::Acknowledged)
    })();
    if result.as_ref().err().is_some_and(is_revocation) {
        revoke(store, &peer.id)?;
    }
    result
}
fn reserve_observation(
    store: &mut CollaborationStore,
    peer: &AppointedPeer<'_>,
    operation: &str,
) -> Result<(), ExchangeError> {
    if !token(operation) {
        return Err(invalid("observation operation"));
    }
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let attempts: Option<i64> = tx
        .query_row(
            "SELECT attempts FROM local_exchange_observations WHERE binding=?1 AND operation=?2",
            params![peer.id.as_str(), operation],
            |r| r.get(0),
        )
        .optional()?;
    if attempts.is_none() {
        let count: i64 = tx.query_row(
            "SELECT count(*) FROM local_exchange_observations WHERE binding=?1",
            [peer.id.as_str()],
            |r| r.get(0),
        )?;
        if count >= MAX_OBSERVATION_OPERATIONS {
            return Err(ExchangeError::Limit);
        }
    }
    if attempts.unwrap_or(0) >= i64::from(peer.spec.max_attempts) {
        return Err(ExchangeError::Limit);
    }
    tx.execute("INSERT INTO local_exchange_observations(binding,operation,attempts) VALUES(?1,?2,1) ON CONFLICT(binding,operation) DO UPDATE SET attempts=attempts+1",params![peer.id.as_str(),operation])?;
    tx.commit()?;
    Ok(())
}
fn cursor_row(
    store: &CollaborationStore,
    id: &RecordId,
) -> Result<Option<(StreamWitness, u64, u64, bool)>, ExchangeError> {
    let row:Option<(Vec<u8>,u64,u64,bool)>=store.read_connection().query_row("SELECT witness,position,snapshot_watermark,valid FROM local_exchange_streams WHERE binding=?1",[id.as_str()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    row.map(|(bytes, position, snapshot, valid)| {
        let stream: StreamWitness = serde_json::from_slice(&bytes)?;
        Cursor {
            stream: stream.clone(),
            position,
        }
        .validate()?;
        if snapshot > position {
            return Err(invalid("stored snapshot watermark"));
        }
        Ok((stream, position, snapshot, valid))
    })
    .transpose()
}
fn save_event(
    tx: &Transaction<'_>,
    id: &RecordId,
    stream: &RecordId,
    event: &Event,
    digest: &ObjectDigest,
) -> Result<(), ExchangeError> {
    let record = event.record()?;
    let existing:Option<(String,Vec<u8>)>=tx.query_row("SELECT id,body FROM local_exchange_events WHERE binding=?1 AND stream=?2 AND sequence=?3",params![id.as_str(),stream.as_str(),event.sequence],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if let Some((old, body)) = existing {
        if old != record.id().as_str() || body != record.canonical_bytes() {
            return Err(invalid("event position reused"));
        }
        tx.execute("UPDATE local_exchange_events SET applied=0 WHERE binding=?1 AND stream=?2 AND sequence=?3 AND applied=2",params![id.as_str(),stream.as_str(),event.sequence])?;
        return Ok(());
    }
    let count: i64 = tx.query_row(
        "SELECT count(*) FROM local_exchange_events WHERE binding=?1",
        [id.as_str()],
        |r| r.get(0),
    )?;
    if count >= MAX_HISTORY_EVENTS {
        return Err(ExchangeError::Limit);
    }
    tx.execute("INSERT INTO local_exchange_events(binding,stream,sequence,id,body,applied) VALUES(?1,?2,?3,?4,?5,0)",params![id.as_str(),stream.as_str(),event.sequence,record.id().as_str(),record.canonical_bytes()])?;
    pin(tx, digest, record.id().as_str())?;
    Ok(())
}
fn project(tx: &Transaction<'_>, id: &RecordId, packet: &Metadata) -> Result<(), ExchangeError> {
    let record = packet.record()?;
    tx.execute("INSERT INTO local_exchange_projection(binding,packet,body) VALUES(?1,?2,?3) ON CONFLICT DO NOTHING",params![id.as_str(),record.id().as_str(),record.canonical_bytes()])?;
    let count: i64 = tx.query_row(
        "SELECT count(*) FROM local_exchange_projection WHERE binding=?1",
        [id.as_str()],
        |r| r.get(0),
    )?;
    if count > MAX_VISIBLE_ITEMS as i64 {
        return Err(ExchangeError::Limit);
    }
    Ok(())
}
fn require_stream(
    store: &mut CollaborationStore,
    peer: &AppointedPeer<'_>,
    proof: &CurrentPeer<'_>,
) -> Result<u64, ExchangeError> {
    if let Some((stream, position, _, valid)) = cursor_row(store, &peer.id)? {
        if stream != *proof.lease.stream() {
            revoke(store, &peer.id)?;
            return Err(ExchangeError::Incomplete);
        }
        if !valid {
            return Err(ExchangeError::Incomplete);
        }
        Ok(position)
    } else {
        Ok(0)
    }
}
/// Fetch one bounded batch. Receipts, contiguous cursor advancement and the
/// advisory projection commit together; gaps remain in a bounded buffer.
pub fn poll(
    store: &mut CollaborationStore,
    peer: &AppointedPeer<'_>,
    operation: &str,
) -> Result<Cursor, ExchangeError> {
    let _local = authority::guard(store, &peer.epoch)?;
    let _lock = peer_lock(store, peer)?;
    reserve_observation(store, peer, operation)?;
    let proof = current(store, peer)?;
    let position = require_stream(store, peer, &proof)?;
    let cursor = Cursor {
        stream: proof.lease.stream().clone(),
        position,
    };
    let result = (|| {
        let events = peer.adapter.events(&*proof.lease, &cursor)?;
        if events.len() > MAX_BATCH_ITEMS {
            return Err(ExchangeError::Limit);
        }
        let stream = stream_id(&cursor.stream)?;
        let mut retained = Vec::new();
        for event in &events {
            if event.stream != cursor.stream {
                return Err(ExchangeError::Stale);
            }
            proof.visible(&peer.spec, &event.packet)?;
            if event.sequence <= position {
                let known:bool=store.read_connection().query_row("SELECT EXISTS(SELECT 1 FROM local_exchange_events WHERE binding=?1 AND stream=?2 AND sequence=?3)",params![peer.id.as_str(),stream.as_str(),event.sequence],|r|r.get(0))?;
                if !known {
                    return Err(ExchangeError::Stale);
                }
            }
            let record = event.record()?;
            retained.push(archive::put_object(store, &record.canonical_bytes())?);
        }
        let tx = store
            .connection()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (event, digest) in events.iter().zip(&retained) {
            save_event(&tx, &peer.id, &stream, event, digest)?;
        }
        let mut next = position;
        loop {
            let seq = next.checked_add(1).ok_or(ExchangeError::Limit)?;
            let row:Option<(String,Vec<u8>)>=tx.query_row("SELECT id,body FROM local_exchange_events WHERE binding=?1 AND stream=?2 AND sequence=?3 AND applied=0",params![peer.id.as_str(),stream.as_str(),seq],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            let Some((id, body)) = row else {
                break;
            };
            let event = event_from(&body, &id)?;
            if event.stream != cursor.stream || event.sequence != seq {
                return Err(ExchangeError::Stale);
            }
            proof.visible(&peer.spec, &event.packet)?;
            project(&tx, &peer.id, &event.packet)?;
            tx.execute("UPDATE local_exchange_events SET applied=1 WHERE binding=?1 AND stream=?2 AND sequence=?3",params![peer.id.as_str(),stream.as_str(),seq])?;
            next = seq;
        }
        let buffered: i64 = tx.query_row(
            "SELECT count(*) FROM local_exchange_events WHERE binding=?1 AND applied=0",
            [peer.id.as_str()],
            |r| r.get(0),
        )?;
        if buffered > MAX_BUFFERED_EVENTS as i64 {
            return Err(ExchangeError::Limit);
        }
        tx.execute("INSERT INTO local_exchange_streams(binding,witness,position,snapshot_watermark,valid) VALUES(?1,?2,?3,0,1) ON CONFLICT(binding) DO UPDATE SET witness=excluded.witness,position=excluded.position,valid=1",params![peer.id.as_str(),serde_json::to_vec(&cursor.stream)?,next])?;
        proof.check_in(&tx, peer)?;
        tx.commit()?;
        Ok(Cursor {
            stream: cursor.stream.clone(),
            position: next,
        })
    })();
    if result.as_ref().err().is_some_and(is_revocation) {
        revoke(store, &peer.id)?;
    }
    result
}
/// Recover an expired/changed stream using a complete explicitly counted native
/// snapshot. Partial pages stay private memory and cannot advance durable state.
/// A crash retries from the last complete cursor under the remaining allowance.
pub fn recover_snapshot(
    store: &mut CollaborationStore,
    peer: &AppointedPeer<'_>,
    operation: &str,
) -> Result<Cursor, ExchangeError> {
    let _local = authority::guard(store, &peer.epoch)?;
    let _lock = peer_lock(store, peer)?;
    reserve_observation(store, peer, operation)?;
    let proof = current(store, peer)?;
    let old = cursor_row(store, &peer.id)?;
    if old
        .as_ref()
        .is_some_and(|(stream, _, _, _)| stream != proof.lease.stream())
    {
        revoke(store, &peer.id)?;
    }
    let result = (|| {
        let mut descriptor = None;
        let mut offset = 0;
        let mut items = Vec::new();
        let mut positions = BTreeSet::new();
        let mut packets = BTreeSet::new();
        loop {
            proof.check()?;
            let page = peer
                .adapter
                .snapshot(&*proof.lease, descriptor.as_ref(), offset)?;
            page.validate_for(proof.lease.stream(), descriptor.as_ref(), offset)?;
            if old.as_ref().is_some_and(|(stream, position, _, _)| {
                stream == &page.snapshot.stream && page.snapshot.watermark < *position
            }) {
                return Err(ExchangeError::Stale);
            }
            descriptor = Some(page.snapshot.clone());
            for event in page.items {
                proof.visible(&peer.spec, &event.packet)?;
                if !positions.insert(event.sequence) || !packets.insert(event.packet.record()?.id())
                {
                    return Err(invalid("duplicate complete snapshot item"));
                }
                items.push(event);
            }
            if items.len() > MAX_VISIBLE_ITEMS {
                return Err(ExchangeError::Limit);
            }
            match page.next_offset {
                Some(next) => offset = next,
                None => break,
            }
        }
        let desc = descriptor.ok_or(ExchangeError::Incomplete)?;
        if items.len() != desc.item_count as usize {
            return Err(ExchangeError::Incomplete);
        }
        let stream = stream_id(&desc.stream)?;
        let mut retained = Vec::new();
        for event in &items {
            retained.push(archive::put_object(
                store,
                &event.record()?.canonical_bytes(),
            )?);
        }
        let tx = store
            .connection()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        clear_projection(&tx, &peer.id)?;
        for (event, digest) in items.iter().zip(&retained) {
            save_event(&tx, &peer.id, &stream, event, digest)?;
            project(&tx, &peer.id, &event.packet)?;
            tx.execute("UPDATE local_exchange_events SET applied=1 WHERE binding=?1 AND stream=?2 AND sequence=?3",params![peer.id.as_str(),stream.as_str(),event.sequence])?;
        }
        tx.execute("INSERT INTO local_exchange_streams(binding,witness,position,snapshot_watermark,valid) VALUES(?1,?2,?3,?3,1) ON CONFLICT(binding) DO UPDATE SET witness=excluded.witness,position=excluded.position,snapshot_watermark=excluded.snapshot_watermark,valid=1",params![peer.id.as_str(),serde_json::to_vec(&desc.stream)?,desc.watermark])?;
        proof.check_in(&tx, peer)?;
        tx.commit()?;
        Ok(Cursor {
            stream: desc.stream,
            position: desc.watermark,
        })
    })();
    if result.as_ref().err().is_some_and(is_revocation) {
        revoke(store, &peer.id)?;
    }
    result
}
/// Cached remote projection is accessible only after a fresh native lease.
/// Offline/expired/revoked peer data cannot silently regain current visibility.
pub fn projection(
    store: &mut CollaborationStore,
    peer: &AppointedPeer<'_>,
) -> Result<Vec<Metadata>, ExchangeError> {
    let _lock = peer_lock(store, peer)?;
    let proof = current(store, peer)?;
    require_stream(store, peer, &proof)?;
    let result = (|| {
        let tx = store
            .connection()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let rows:Vec<(String,Vec<u8>)>=tx.prepare("SELECT packet,body FROM local_exchange_projection WHERE binding=?1 ORDER BY packet LIMIT 1025")?.query_map([peer.id.as_str()],|r|Ok((r.get(0)?,r.get(1)?)))?.collect::<Result<_,_>>()?;
        if rows.len() > MAX_VISIBLE_ITEMS {
            return Err(ExchangeError::Limit);
        }
        let mut packets = Vec::new();
        for (id, body) in rows {
            let packet = packet_from(&body, &id)?;
            proof.visible(&peer.spec, &packet)?;
            packets.push(packet);
        }
        proof.check_in(&tx, peer)?;
        tx.commit()?;
        Ok(packets)
    })();
    if result.as_ref().err().is_some_and(is_revocation) {
        revoke(store, &peer.id)?;
    }
    result
}
