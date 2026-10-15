//! Bounded, append-only ingress for a previously retained contribution.
//!
//! A notice is data, not execution authority. Ingress never acquires a target
//! controller lock or makes an input available. The appointed driver's next
//! owner tick revalidates the exact capture, alias, revision and authority
//! epoch before recording an arrival for the selected revision.

use std::time::{SystemTime, UNIX_EPOCH};

use aethyme_contracts::experimental_v0::{FieldKind, FieldSpec, Record, RecordId, RecordSchema};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::collaboration_archive::{self as archive, CommitOid};
use crate::collaboration_capture::{self as capture, OperationId, RetentionBoundary};
use crate::collaboration_state::CollaborationStore;
use crate::file_lock::{ExclusiveFileLock, open_lock_file};
use crate::local_target::{self as target, ControllerOwner, TargetRevision};

const MAX_PENDING_NOTICES: i64 = 512;
const MAX_OWNER_TICK_NOTICES: i64 = 512;
const NOTICE_SCHEMA: RecordSchema = RecordSchema {
    name: "aethyme.local-capture-notice/experimental-v0",
    fields: &[FieldSpec {
        name: "notice",
        required: true,
        kind: FieldKind::Opaque,
        capability: None,
    }],
    capabilities: &[],
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoticeReceipt {
    pub id: RecordId,
    pub status: String,
    pub received_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NoticeStatus {
    pub id: String,
    pub operation: String,
    pub target: String,
    pub revision: String,
    pub alias: String,
    pub lineage: String,
    pub status: String,
    pub detail: Option<String>,
    pub received_ms: i64,
    pub changed_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConsumeReport {
    pub examined: usize,
    pub available: usize,
    pub stale: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum NoticeError {
    #[error("capture notice is invalid: {0}")]
    Invalid(String),
    #[error("capture notice does not match the current selected revision or retained input")]
    Stale,
    #[error("capture notice pending limit is full")]
    Full,
    #[error("capture notice idempotency key conflicts with an existing notice")]
    Conflict,
    #[error(transparent)]
    Authority(#[from] crate::local_authority::AuthorityError),
    #[error(transparent)]
    Sensitive(#[from] crate::collaboration_sensitive::SensitiveError),
    #[error(transparent)]
    Capture(#[from] capture::CaptureError),
    #[error(transparent)]
    Archive(#[from] archive::ArchiveError),
    #[error(transparent)]
    Target(#[from] target::TargetError),
    #[error(transparent)]
    Wave(#[from] crate::local_wave::WaveError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NoticeBody {
    target: String,
    issuer: String,
    revision: String,
    alias: String,
    operation: String,
    lineage: String,
    receipt: String,
    authority_epoch: String,
    base_snapshot: String,
    base_commit: String,
    result_snapshot: String,
    result_commit: String,
    received_ms: i64,
}

fn now_ms() -> Result<i64, NoticeError> {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| NoticeError::Invalid(error.to_string()))?
            .as_millis(),
    )
    .map_err(|_| NoticeError::Invalid("clock overflow".into()))
}

fn ingress_lock(store: &CollaborationStore) -> Result<ExclusiveFileLock, NoticeError> {
    let spool = store.project_dir().join("spool");
    crate::host_state::protect_host_state_path(&spool, true)?;
    let path = spool.join("capture-notice-ingress.lock");
    Ok(ExclusiveFileLock::acquire(open_lock_file(&path)?)?)
}

fn record(body: &NoticeBody) -> Result<Record, NoticeError> {
    Record::decode(
        &serde_json::to_vec(&serde_json::json!({"schema":NOTICE_SCHEMA.name,"notice":body}))?,
        &[&NOTICE_SCHEMA],
    )
    .map_err(|error| NoticeError::Invalid(error.to_string()))
}

fn decode_notice(id: &str, bytes: &[u8]) -> Result<(RecordId, NoticeBody), NoticeError> {
    let expected = RecordId::parse(id).map_err(|error| NoticeError::Invalid(error.to_string()))?;
    let record = Record::decode(bytes, &[&NOTICE_SCHEMA])
        .map_err(|error| NoticeError::Invalid(error.to_string()))?;
    if record.id() != expected || record.canonical_bytes() != bytes {
        return Err(NoticeError::Invalid("notice record digest".into()));
    }
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    let body: NoticeBody = serde_json::from_value(value["notice"].clone())?;
    Ok((expected, body))
}

fn selected_alias<'a>(revision: &'a TargetRevision, alias: &str) -> Result<&'a str, NoticeError> {
    if !revision
        .spec
        .selected
        .iter()
        .any(|selected| selected == alias)
    {
        return Err(NoticeError::Stale);
    }
    let contribution = revision
        .spec
        .contributions
        .iter()
        .find(|contribution| contribution.id == alias)
        .ok_or(NoticeError::Stale)?;
    if contribution.lineage.is_empty() {
        return Err(NoticeError::Stale);
    }
    Ok(contribution.lineage.as_str())
}

/// Verify the operation row, receipt, archive lineage and exact selected
/// alias tuple while response/archive barriers remain held.
fn validate_capture(
    store: &mut CollaborationStore,
    revision: &TargetRevision,
    operation_text: &str,
    alias: &str,
    expected: Option<&NoticeBody>,
) -> Result<(NoticeBody, bool), NoticeError> {
    let operation = OperationId::parse(operation_text)
        .map_err(|error| NoticeError::Invalid(error.to_string()))?;
    let lineage_text = selected_alias(revision, alias)?;
    let lineage =
        RecordId::parse(lineage_text).map_err(|error| NoticeError::Invalid(error.to_string()))?;
    let (state, stored_lineage, base_commit, result_commit): (
        String,
        Option<String>,
        String,
        String,
    ) = store
        .read_connection()
        .query_row(
            "SELECT state,lineage_record_id,base_commit,result_commit
             FROM capture_operations WHERE operation_id=?1",
            [operation.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?
        .ok_or(NoticeError::Stale)?;
    if state != "acknowledged" || stored_lineage.as_deref() != Some(lineage.as_str()) {
        return Err(NoticeError::Stale);
    }
    let receipt = capture::receipt(store, &operation)?.ok_or(NoticeError::Stale)?;
    if receipt.status != "retained_local"
        || receipt.retention != RetentionBoundary::UntilReleased
        || receipt.contribution != lineage
    {
        return Err(NoticeError::Stale);
    }
    let base_commit =
        CommitOid::parse(&base_commit).map_err(|error| NoticeError::Invalid(error.to_string()))?;
    let result_commit = CommitOid::parse(&result_commit)
        .map_err(|error| NoticeError::Invalid(error.to_string()))?;
    let Some(retained) = archive::retained_contribution(store, &lineage)? else {
        return Err(NoticeError::Stale);
    };
    if retained.base.snapshot_id != receipt.base
        || retained.result.snapshot_id != receipt.result
        || retained.base.commit != base_commit
        || retained.result.commit != result_commit
    {
        return Err(NoticeError::Stale);
    }
    let current = target::status(store, &revision.target)?;
    if current.revision.as_ref() != Some(&revision.id) {
        return Err(NoticeError::Stale);
    }
    let epoch = crate::local_authority::status(store)?
        .epoch
        .as_str()
        .to_owned();
    let candidate = NoticeBody {
        target: revision.target.clone(),
        issuer: revision.issuer.clone(),
        revision: revision.id.as_str().into(),
        alias: alias.into(),
        operation: operation.as_str().into(),
        lineage: lineage.as_str().into(),
        receipt: receipt.receipt_record_id.as_str().into(),
        authority_epoch: epoch,
        base_snapshot: receipt.base.as_str().into(),
        base_commit: base_commit.as_str().into(),
        result_snapshot: receipt.result.as_str().into(),
        result_commit: result_commit.as_str().into(),
        received_ms: expected.map_or(0, |body| body.received_ms),
    };
    let exact = expected.is_none_or(|old| {
        old.target == candidate.target
            && old.issuer == candidate.issuer
            && old.revision == candidate.revision
            && old.alias == candidate.alias
            && old.operation == candidate.operation
            && old.lineage == candidate.lineage
            && old.receipt == candidate.receipt
            && old.authority_epoch == candidate.authority_epoch
            && old.base_snapshot == candidate.base_snapshot
            && old.base_commit == candidate.base_commit
            && old.result_snapshot == candidate.result_snapshot
            && old.result_commit == candidate.result_commit
    });
    Ok((candidate, exact))
}

/// Append a notice for an already selected alias. The command does not take
/// the target's owner lock and never writes `local_wave_arrivals`.
pub fn ingest(
    store: &mut CollaborationStore,
    target_name: &str,
    issuer: &str,
    revision_id: &RecordId,
    alias: &str,
    operation: &OperationId,
) -> Result<NoticeReceipt, NoticeError> {
    let _archive = crate::collaboration_gc::archive_use(store)?;
    let _response = crate::collaboration_sensitive::response_use(store)?;
    let witness = crate::local_authority::status(store)?;
    let _authority = crate::local_authority::guard(store, &witness.epoch)?;
    let _ingress = ingress_lock(store)?;
    let revision = target::load_revision(store, revision_id)?;
    if revision.target != target_name || revision.issuer != issuer {
        return Err(NoticeError::Stale);
    }
    let current = target::status(store, target_name)?;
    if current.revision.as_ref() != Some(revision_id) {
        return Err(NoticeError::Stale);
    }
    let (mut body, _) = validate_capture(store, &revision, operation.as_str(), alias, None)?;
    body.authority_epoch = witness.epoch.as_str().into();

    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let selected_now: Option<String> = tx
        .query_row(
            "SELECT active_revision FROM local_targets WHERE target=?1",
            [target_name],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    if selected_now.as_deref() != Some(revision_id.as_str()) {
        return Err(NoticeError::Stale);
    }
    let duplicate: Option<(String, Vec<u8>, String, i64)> = tx
        .query_row(
            "SELECT n.notice_id,n.body,o.status,n.received_ms
             FROM local_capture_notices n
             JOIN local_capture_notice_outcomes o USING(notice_id)
             WHERE n.target=?1 AND n.revision=?2 AND n.operation_id=?3
               AND n.alias=?4 AND n.authority_epoch=?5",
            params![
                target_name,
                revision_id.as_str(),
                operation.as_str(),
                alias,
                witness.epoch.as_str()
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    if let Some((id, bytes, status, received_ms)) = duplicate {
        let (_, old) = decode_notice(&id, &bytes)?;
        body.received_ms = received_ms;
        if old != body {
            return Err(NoticeError::Conflict);
        }
        let id = RecordId::parse(&id).map_err(|error| NoticeError::Invalid(error.to_string()))?;
        return Ok(NoticeReceipt {
            id,
            status,
            received_ms,
        });
    }
    let pending: i64 = tx.query_row(
        "SELECT count(*) FROM local_capture_notice_outcomes WHERE status='pending'",
        [],
        |row| row.get(0),
    )?;
    if pending >= MAX_PENDING_NOTICES {
        return Err(NoticeError::Full);
    }
    body.received_ms = now_ms()?;
    let record = record(&body)?;
    let id = record.id();
    tx.execute(
        "INSERT INTO local_capture_notices
         (notice_id,target,revision,operation_id,alias,lineage_record_id,receipt_record_id,
          authority_epoch,base_snapshot,base_commit,result_snapshot,result_commit,received_ms,body)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
        params![
            id.as_str(),
            body.target,
            body.revision,
            body.operation,
            body.alias,
            body.lineage,
            body.receipt,
            body.authority_epoch,
            body.base_snapshot,
            body.base_commit,
            body.result_snapshot,
            body.result_commit,
            body.received_ms,
            record.canonical_bytes()
        ],
    )?;
    tx.execute(
        "INSERT INTO local_capture_notice_outcomes(notice_id,status,detail,changed_ms)
         VALUES (?1,'pending',NULL,?2)",
        params![id.as_str(), body.received_ms],
    )?;
    tx.commit()?;
    Ok(NoticeReceipt {
        id,
        status: "pending".into(),
        received_ms: body.received_ms,
    })
}

/// Drain, revalidate, and admit as one serialized owner tick. An ingress
/// writer cannot enqueue between the drain and the wave freeze.
pub(crate) fn admit_for_owner(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    policy: crate::local_wave::WavePolicy,
    now_ms: i64,
) -> Result<crate::local_wave::Admission, NoticeError> {
    // All owner ticks and notice writers follow archive -> response ->
    // authority -> ingress. The ingress lock remains held through admission.
    let _archive = crate::collaboration_gc::archive_use(store)?;
    let _response = crate::collaboration_sensitive::response_use(store)?;
    // `authorize` both validates the owner identity and holds the exact active
    // authority epoch. Keep this guard for the whole tick so inner operations
    // do not reacquire it beneath the ingress lock.
    let authority = target::authorize(store, owner)?;
    let _ingress = ingress_lock(store)?;
    consume_pending_locked(store, owner, &authority)?;
    owner_tick_hook();
    crate::local_wave::admit_after_notices(store, owner, policy, now_ms, &authority)
        .map_err(Into::into)
}

/// Consume queued notices while the caller owns archive, response, authority,
/// and ingress barriers. Every source tuple is re-read before making an
/// arrival available.
fn consume_pending_locked(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    _authority: &crate::local_authority::Guard,
) -> Result<ConsumeReport, NoticeError> {
    let mut statement = store.read_connection().prepare(
        "SELECT n.notice_id,n.body FROM local_capture_notices n
         JOIN local_capture_notice_outcomes o USING(notice_id)
         WHERE n.target=?1 AND o.status='pending'
         ORDER BY n.received_ms,n.notice_id LIMIT ?2",
    )?;
    let rows = statement
        .query_map(params![owner.target(), MAX_OWNER_TICK_NOTICES], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    let mut report = ConsumeReport::default();
    for (id_text, bytes) in rows {
        report.examined += 1;
        let (id, body) = decode_notice(&id_text, &bytes)?;
        let revision_id = RecordId::parse(&body.revision)
            .map_err(|error| NoticeError::Invalid(error.to_string()))?;
        let current = target::status(store, owner.target())?;
        let stale_reason = if body.target != owner.target()
            || body.issuer != owner.issuer()
            || body.authority_epoch != owner.authority_epoch().as_str()
            || current.revision.as_ref() != Some(&revision_id)
        {
            Some("authority epoch or selected target revision changed")
        } else {
            let revision = target::load_revision(store, &revision_id)?;
            match validate_capture(store, &revision, &body.operation, &body.alias, Some(&body)) {
                Ok((_, true)) => None,
                Ok((_, false)) | Err(NoticeError::Stale) => {
                    Some("retained capture no longer matches the selected alias")
                }
                Err(error) => return Err(error),
            }
        };
        let now = now_ms()?;
        if let Some(detail) = stale_reason {
            store.connection().execute(
                "UPDATE local_capture_notice_outcomes SET status='stale',detail=?2,changed_ms=?3
                 WHERE notice_id=?1 AND status='pending'",
                params![id.as_str(), detail, now],
            )?;
            report.stale += 1;
        } else {
            crate::local_wave::available_under_owner_tick(
                store,
                owner,
                &body.alias,
                body.received_ms,
                _authority,
            )?;
            store.connection().execute(
                "UPDATE local_capture_notice_outcomes SET status='available',detail=NULL,changed_ms=?2
                 WHERE notice_id=?1 AND status='pending'",
                params![id.as_str(), now],
            )?;
            report.available += 1;
        }
    }
    Ok(report)
}

#[cfg(test)]
thread_local! {
    static OWNER_TICK_HOOK: std::cell::RefCell<Option<(std::sync::mpsc::SyncSender<()>, std::sync::mpsc::Receiver<()>)>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) struct OwnerTickProbe;

#[cfg(test)]
impl Drop for OwnerTickProbe {
    fn drop(&mut self) {
        OWNER_TICK_HOOK.with(|hook| *hook.borrow_mut() = None);
    }
}

#[cfg(test)]
pub(crate) fn observe_owner_tick(
    reached: std::sync::mpsc::SyncSender<()>,
    resume: std::sync::mpsc::Receiver<()>,
) -> OwnerTickProbe {
    OWNER_TICK_HOOK.with(|hook| *hook.borrow_mut() = Some((reached, resume)));
    OwnerTickProbe
}

#[cfg(test)]
fn owner_tick_hook() {
    OWNER_TICK_HOOK.with(|hook| {
        if let Some((reached, resume)) = hook.borrow_mut().take() {
            let _ = reached.send(());
            let _ = resume.recv_timeout(std::time::Duration::from_secs(5));
        }
    });
}

#[cfg(not(test))]
fn owner_tick_hook() {}

#[cfg(test)]
#[path = "local_capture_notice/tests.rs"]
mod tests;

/// Read the bounded durable notice history for operator reconciliation.
pub fn list_for_target(
    store: &CollaborationStore,
    target_name: &str,
    limit: usize,
) -> Result<Vec<NoticeStatus>, NoticeError> {
    if !(1..=512).contains(&limit) {
        return Err(NoticeError::Invalid("notice listing bound".into()));
    }
    let _response = crate::collaboration_sensitive::response_use(store)?;
    let mut statement = store.read_connection().prepare(
        "SELECT n.notice_id,n.operation_id,n.target,n.revision,n.alias,n.lineage_record_id,
                o.status,o.detail,n.received_ms,o.changed_ms
         FROM local_capture_notices n JOIN local_capture_notice_outcomes o USING(notice_id)
         WHERE n.target=?1 ORDER BY n.received_ms DESC,n.notice_id LIMIT ?2",
    )?;
    let rows = statement
        .query_map(params![target_name, limit as i64], |row| {
            Ok(NoticeStatus {
                id: row.get(0)?,
                operation: row.get(1)?,
                target: row.get(2)?,
                revision: row.get(3)?,
                alias: row.get(4)?,
                lineage: row.get(5)?,
                status: row.get(6)?,
                detail: row.get(7)?,
                received_ms: row.get(8)?,
                changed_ms: row.get(9)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}
