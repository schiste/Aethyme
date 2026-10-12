//! Provisional cooperative Local authority fencing (#669).
//! The witness is outside a project's restorable directory. Operators must
//! preserve it and freeze ingress before announced restore; arbitrary whole-
//! host rollback, hostile same-user writers and external effects are unqualified.

use crate::collaboration_state::{CollaborationStateError, CollaborationStore, sync_directory};
use crate::file_lock::{ExclusiveFileLock, open_lock_file};
use aethyme_contracts::experimental_v0::RecordId;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

// A 4096-byte reason/detail may require six JSON bytes per escaped byte.
const MAX_FILE_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Epoch(String);
impl Epoch {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Active,
    Frozen,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Witness {
    pub version: u32,
    pub epoch: Epoch,
    pub status: Status,
    pub checkpoint: Option<String>,
    pub reason: Option<String>,
}
#[derive(Debug, thiserror::Error)]
pub enum AuthorityError {
    #[error("Local authority is frozen; ingress must remain closed")]
    Frozen,
    #[error("Local authority epoch or data checkpoint differs; reconcile before resuming")]
    Stale,
    #[error("Local authority witness is missing or invalid: {0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    State(#[from] CollaborationStateError),
}

/// Lifetime guard shared by all controller mutations. Freeze waits for admitted
/// library operations to finish, then closes ingress before changing the DB.
#[derive(Debug)]
pub(crate) struct Guard {
    _file: File,
}
fn directory(store: &CollaborationStore) -> Result<PathBuf, AuthorityError> {
    directory_for(store.project_dir())
}
fn directory_for(project: &Path) -> Result<PathBuf, AuthorityError> {
    let parent = project
        .parent()
        .ok_or_else(|| AuthorityError::Invalid("project directory".into()))?;
    let key = project
        .file_name()
        .ok_or_else(|| AuthorityError::Invalid("project key".into()))?;
    Ok(parent.join(".local-authority").join(key))
}
fn prepare(path: &Path) -> Result<(), AuthorityError> {
    let parent = path
        .parent()
        .ok_or_else(|| AuthorityError::Invalid("witness directory".into()))?;
    std::fs::create_dir_all(parent)?;
    crate::host_state::protect_host_state_path(parent, true)?;
    std::fs::create_dir_all(path)?;
    crate::host_state::protect_host_state_path(path, true)?;
    sync_directory(parent)?;
    Ok(())
}
fn read(path: &Path) -> Result<Witness, AuthorityError> {
    let witness = path.join("witness.json");
    if !std::fs::symlink_metadata(&witness)?.file_type().is_file() {
        return Err(AuthorityError::Invalid(
            "witness must be a regular file".into(),
        ));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(&witness)?;
    if !file.metadata()?.is_file() {
        return Err(AuthorityError::Invalid(
            "witness must be a regular file".into(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(AuthorityError::Invalid("witness size".into()));
    }
    let w: Witness = serde_json::from_slice(&bytes)?;
    if w.version != 1
        || RecordId::parse(w.epoch.as_str()).is_err()
        || w.checkpoint
            .as_ref()
            .is_some_and(|v| RecordId::parse(v).is_err())
        || (w.status == Status::Active && w.checkpoint.is_some())
    {
        return Err(AuthorityError::Invalid("witness fields".into()));
    }
    Ok(w)
}
fn write(path: &Path, w: &Witness) -> Result<(), AuthorityError> {
    let bytes = serde_json::to_vec(w)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(AuthorityError::Invalid("witness size".into()));
    }
    let mut file = tempfile::Builder::new()
        .prefix(".epoch-")
        .tempfile_in(path)?;
    file.write_all(&bytes)?;
    file.as_file().sync_all()?;
    file.persist(path.join("witness.json"))
        .map_err(|e| e.error)?;
    sync_directory(path)?;
    Ok(())
}
fn fresh_epoch() -> Result<Epoch, AuthorityError> {
    // Reuse the host resource ledger's randomblob nonce pattern. This is a
    // cooperative generation marker, not a signing key or authentication secret.
    let conn = Connection::open_in_memory()?;
    let value: String =
        conn.query_row("SELECT 'sha256:' || lower(hex(randomblob(32)))", [], |r| {
            r.get(0)
        })?;
    Ok(Epoch(value))
}
fn bound(conn: &Connection) -> Result<Option<Epoch>, AuthorityError> {
    Ok(conn
        .query_row(
            "SELECT epoch FROM local_authority_state WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .optional()?
        .map(Epoch))
}
fn matching(conn: &Connection, w: &Witness, epoch: &Epoch) -> Result<(), AuthorityError> {
    if w.status != Status::Active {
        return Err(AuthorityError::Frozen);
    }
    if &w.epoch != epoch || bound(conn)?.as_ref() != Some(epoch) {
        return Err(AuthorityError::Stale);
    }
    Ok(())
}

/// Initialize only an unbound store with no witness. An interrupted initial
/// installation stays frozen and needs explicit trusted-operator recovery.
pub(crate) fn acquire(store: &mut CollaborationStore) -> Result<(Epoch, Guard), AuthorityError> {
    let path = directory(store)?;
    prepare(&path)?;
    let exclusive = ExclusiveFileLock::acquire(open_lock_file(&path.join("ingress.lock"))?)?;
    if !path.join("witness.json").exists() {
        if bound(store.read_connection())?.is_some() {
            return Err(AuthorityError::Invalid("lost witness".into()));
        }
        let epoch = fresh_epoch()?;
        let frozen = Witness {
            version: 1,
            epoch: epoch.clone(),
            status: Status::Frozen,
            checkpoint: None,
            reason: Some("initial installation interrupted".into()),
        };
        write(&path, &frozen)?;
        let tx = store
            .connection()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO local_authority_state(singleton,epoch) VALUES(1,?1)",
            [epoch.as_str()],
        )?;
        tx.commit()?;
        write(
            &path,
            &Witness {
                status: Status::Active,
                checkpoint: None,
                reason: None,
                ..frozen
            },
        )?;
    }
    let w = read(&path)?;
    matching(store.read_connection(), &w, &w.epoch)?;
    let epoch = w.epoch;
    drop(exclusive);
    let guard = guard(store, &epoch)?;
    Ok((epoch, guard))
}
pub(crate) fn guard(store: &CollaborationStore, epoch: &Epoch) -> Result<Guard, AuthorityError> {
    let path = directory(store)?;
    let file = open_lock_file(&path.join("ingress.lock"))?;
    file.lock_shared()?;
    let w = read(&path)?;
    matching(store.read_connection(), &w, epoch)?;
    Ok(Guard { _file: file })
}
/// Reading status grants no mutation authority.
pub fn status(store: &CollaborationStore) -> Result<Witness, AuthorityError> {
    read(&directory(store)?)
}

/// Historical status only. Serialized observation cannot acquire a guard.
#[derive(Debug, Serialize)]
pub(crate) struct AuthorityObservation {
    pub state: &'static str,
    pub witness: Option<Witness>,
    pub bound_epoch: Option<String>,
    pub binding_matches: bool,
}
/// Inspect a checked read-only snapshot without initializing authority, opening
/// an ingress lock, changing pragmas, or creating files. Concurrent changes can
/// make this observation stale; the mutation APIs still require their guards.
pub(crate) fn inspect(
    snapshot: &crate::collaboration_state::CollaborationSnapshot,
) -> Result<AuthorityObservation, AuthorityError> {
    let binding = if snapshot.has_table("local_authority_state") {
        bound(snapshot.connection())?
    } else {
        None
    };
    let witness = match read(&directory_for(&snapshot.project_dir)?) {
        Ok(witness) => Some(witness),
        Err(AuthorityError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let binding_matches = witness
        .as_ref()
        .is_some_and(|w| binding.as_ref() == Some(&w.epoch));
    let state = match (&witness, &binding) {
        (None, None) => "unbound",
        (None, Some(_)) => "witness_missing",
        (Some(w), _) if w.status == Status::Frozen => "frozen",
        (Some(_), _) if binding_matches => "active",
        (Some(_), _) => "drift",
    };
    Ok(AuthorityObservation {
        state,
        witness,
        bound_epoch: binding.map(|epoch| epoch.0),
        binding_matches,
    })
}

// Stream a stable checkpoint of authority, roots, wave and budget ledgers.
// Captures and derived context are data, and intentionally do not rotate authority.
fn checkpoint(conn: &Connection) -> Result<String, AuthorityError> {
    let tables:Vec<String>=conn.prepare("SELECT name FROM sqlite_master WHERE type='table' AND (name LIKE 'local_%' OR name='resolution_attempts') AND name!='local_authority_state' ORDER BY name")?.query_map([],|r|r.get(0))?.collect::<Result<_,_>>()?;
    let mut digest = Sha256::new();
    digest.update(b"aethyme Local authority checkpoint v0\0");
    for table in tables {
        // sqlite_master names are quoted, not interpolated as unchecked syntax.
        let quoted = table.replace('"', "\"\"");
        let names: Vec<String> = conn
            .prepare(&format!("SELECT * FROM \"{quoted}\""))?
            .column_names()
            .iter()
            .map(|n| n.to_string())
            .collect();
        let order = (1..=names.len())
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let mut stmt = conn.prepare(&format!("SELECT * FROM \"{quoted}\" ORDER BY {order}"))?;
        digest.update(serde_json::to_vec(&(table, names.clone()))?);
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let mut encoded = Vec::new();
            for i in 0..names.len() {
                use rusqlite::types::ValueRef;
                let value = match row.get_ref(i)? {
                    ValueRef::Null => serde_json::json!(["null"]),
                    ValueRef::Integer(v) => serde_json::json!(["integer", v]),
                    ValueRef::Real(v) => serde_json::json!(["real", v.to_bits()]),
                    ValueRef::Text(v) => serde_json::json!(["text", v]),
                    ValueRef::Blob(v) => serde_json::json!(["blob", v]),
                };
                encoded.push(value);
            }
            let bytes = serde_json::to_vec(&encoded)?;
            digest.update((bytes.len() as u64).to_be_bytes());
            digest.update(bytes);
        }
    }
    Ok(format!("sha256:{:x}", digest.finalize()))
}

/// Trusted operator kill switch/announced restore entry. Closes cooperative
/// ingress and rotates the witness before any DB write. Existing effects are
/// not cancelled or rolled back; their liveness and consumption remain unknown.
pub fn freeze(
    store: &mut CollaborationStore,
    expected: &Epoch,
    reason: &str,
) -> Result<Witness, AuthorityError> {
    if reason.trim().is_empty() || reason.len() > 4096 {
        return Err(AuthorityError::Invalid("freeze reason".into()));
    }
    let path = directory(store)?;
    let _exclusive = ExclusiveFileLock::acquire(open_lock_file(&path.join("ingress.lock"))?)?;
    let current = read(&path)?;
    if current.status != Status::Active
        || &current.epoch != expected
        || bound(store.read_connection())?.as_ref() != Some(expected)
    {
        return Err(AuthorityError::Stale);
    }
    let mut next = Witness {
        version: 1,
        epoch: fresh_epoch()?,
        status: Status::Frozen,
        checkpoint: None,
        reason: Some(reason.into()),
    };
    write(&path, &next)?; // Any subsequent failure leaves ingress closed.
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    quarantine(&tx)?;
    next.checkpoint = Some(checkpoint(&tx)?);
    tx.commit()?;
    write(&path, &next)?;
    Ok(next)
}
fn quarantine(conn: &Connection) -> Result<(), AuthorityError> {
    // Freeze is not proof that the worker died, or that an external effect failed.
    conn.execute("UPDATE local_budget_reservations SET phase='consumption_unknown',detail='authority frozen; attempt liveness unknown',version=version+1 WHERE phase='completion_pending'",[])?;
    conn.execute("UPDATE local_waves SET phase='outcome_unknown',detail='authority frozen; dispatch or verification requires reconciliation' WHERE phase IN ('composing','verifying')",[])?;
    conn.execute("UPDATE local_waves SET phase='stopped',detail='authority frozen before dispatch' WHERE phase='frozen'",[])?;
    conn.execute("UPDATE local_targets SET phase=CASE WHEN EXISTS(SELECT 1 FROM local_waves w WHERE w.target=local_targets.target AND w.phase='outcome_unknown') THEN 'stop_requested' ELSE 'stopped' END,resume_phase=NULL,version=version+1",[])?;
    Ok(())
}

/// Resume only an exact reconciled checkpoint. Rewound/missing ledger history
/// stays frozen; reconstructing that history from trusted archived receipts is
/// a future adapter, not an unchecked acknowledgement flag. Rebinding rotates
/// again so old owners/attempts never become current, even for the same revision.
pub fn resume(store: &mut CollaborationStore, expected: &Epoch) -> Result<Witness, AuthorityError> {
    let path = directory(store)?;
    let _exclusive = ExclusiveFileLock::acquire(open_lock_file(&path.join("ingress.lock"))?)?;
    let current = read(&path)?;
    if current.status != Status::Frozen || &current.epoch != expected {
        return Err(AuthorityError::Stale);
    }
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let saved = current.checkpoint.as_deref().ok_or(AuthorityError::Stale)?;
    if checkpoint(&tx)? != saved {
        return Err(AuthorityError::Stale);
    }
    let epoch = fresh_epoch()?;
    // Persist the new frozen token first. A crash cannot reactivate the old DB.
    let mut next = Witness {
        epoch: epoch.clone(),
        ..current
    };
    write(&path, &next)?;
    tx.execute("INSERT INTO local_authority_state(singleton,epoch) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET epoch=excluded.epoch",[epoch.as_str()])?;
    tx.commit()?;
    next.status = Status::Active;
    next.checkpoint = None;
    next.reason = None;
    write(&path, &next)?;
    Ok(next)
}

/// Crash before a freeze checkpoint was published cannot silently resume.
/// A trusted operator may complete quarantine against the exact frozen token;
/// it is forbidden after a checkpoint exists (which could be an announced restore).
pub fn complete_interrupted_freeze(
    store: &mut CollaborationStore,
    expected: &Epoch,
) -> Result<Witness, AuthorityError> {
    let path = directory(store)?;
    let _exclusive = ExclusiveFileLock::acquire(open_lock_file(&path.join("ingress.lock"))?)?;
    let mut current = read(&path)?;
    if current.status != Status::Frozen
        || &current.epoch != expected
        || current.checkpoint.is_some()
    {
        return Err(AuthorityError::Stale);
    }
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    quarantine(&tx)?;
    current.checkpoint = Some(checkpoint(&tx)?);
    tx.commit()?;
    write(&path, &current)?;
    Ok(current)
}

#[cfg(test)]
mod tests;

/// GC must not reclaim from restored roots while authority is frozen or stale.
/// Archives without an initialized controller retain their existing GC behavior.
pub(crate) fn archive_guard(store: &CollaborationStore) -> Result<Guard, AuthorityError> {
    let path = directory(store)?;
    prepare(&path)?;
    let file = open_lock_file(&path.join("ingress.lock"))?;
    file.lock_shared()?;
    if !path.join("witness.json").exists() {
        if bound(store.read_connection())?.is_some() {
            return Err(AuthorityError::Invalid("lost witness".into()));
        }
        // First installation must wait for already admitted reclamation.
        return Ok(Guard { _file: file });
    }
    let w = read(&path)?;
    matching(store.read_connection(), &w, &w.epoch)?;
    Ok(Guard { _file: file })
}

/// An unverified historical observation. It grants no budget settlement,
/// producer status, candidate verification, acceptance or publication authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoricalObservation {
    pub version: u32,
    pub project: String,
    pub epoch: Epoch,
    pub reservation: String,
    pub attempt: u32,
    pub receipt: String,
    pub detail: String,
}
/// Retain a stale worker's observation outside restored data. Only an issued
/// model attempt from this project can name the observation slot; duplicate
/// bytes are idempotent and different receipts cannot overwrite its history.
pub fn record_late_outcome(
    store: &CollaborationStore,
    attempt: &crate::local_budget::Attempt,
    receipt: &RecordId,
    detail: &str,
) -> Result<HistoricalObservation, AuthorityError> {
    record_late_outcome_with(store, attempt, receipt, detail, |path| {
        Ok(sync_directory(path)?)
    })
}
fn record_late_outcome_with(
    store: &CollaborationStore,
    attempt: &crate::local_budget::Attempt,
    receipt: &RecordId,
    detail: &str,
    mut sync: impl FnMut(&Path) -> Result<(), AuthorityError>,
) -> Result<HistoricalObservation, AuthorityError> {
    if attempt.project_dir != store.project_dir() || detail.len() > 4096 {
        return Err(AuthorityError::Invalid("historical scope or size".into()));
    }
    let path = directory(store)?;
    let file = open_lock_file(&path.join("ingress.lock"))?;
    file.lock_shared()?;
    let witness = read(&path)?;
    if witness.status == Status::Active && witness.epoch == attempt.epoch {
        return Err(AuthorityError::Invalid(
            "current outcome requires its accounting adapter".into(),
        ));
    }
    let observation = HistoricalObservation {
        version: 1,
        project: store.project().as_str().into(),
        epoch: attempt.epoch.clone(),
        reservation: attempt.reservation().to_string(),
        attempt: attempt.number(),
        receipt: receipt.to_string(),
        detail: detail.into(),
    };
    let history = path.join("history");
    std::fs::create_dir_all(&history)?;
    crate::host_state::protect_host_state_path(&history, true)?;
    let epoch = attempt
        .epoch
        .as_str()
        .strip_prefix("sha256:")
        .ok_or_else(|| AuthorityError::Invalid("epoch".into()))?;
    let reservation = attempt
        .reservation()
        .as_str()
        .strip_prefix("sha256:")
        .ok_or_else(|| AuthorityError::Invalid("reservation".into()))?;
    let destination = history.join(format!("{epoch}-{reservation}-{}.json", attempt.number()));
    let bytes = serde_json::to_vec(&observation)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(AuthorityError::Invalid("historical size".into()));
    }
    let mut temporary = tempfile::Builder::new()
        .prefix(".late-")
        .tempfile_in(&history)?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    match temporary.persist_noclobber(&destination) {
        Ok(_) => {}
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
            if std::fs::read(&destination)? != bytes {
                return Err(AuthorityError::Invalid(
                    "historical slot already recorded differently".into(),
                ));
            }
        }
        Err(e) => return Err(e.error.into()),
    }
    // An identical replay must complete a possibly interrupted first flush.
    sync(&history)?;
    sync(&path)?;
    Ok(observation)
}
/// Inspect bounded unverified historical slots; no controller state is changed.
pub fn historical_outcomes(
    store: &CollaborationStore,
) -> Result<Vec<HistoricalObservation>, AuthorityError> {
    let history = directory(store)?.join("history");
    let entries = match std::fs::read_dir(history) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e.into()),
    };
    let mut observations = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with(".late-") {
            continue;
        }
        let meta = entry.metadata()?;
        if !meta.is_file() || meta.len() > MAX_FILE_BYTES as u64 {
            return Err(AuthorityError::Invalid("historical file".into()));
        }
        let value: HistoricalObservation = serde_json::from_slice(&std::fs::read(entry.path())?)?;
        if value.version != 1
            || value.project != store.project().as_str()
            || RecordId::parse(value.epoch.as_str()).is_err()
            || RecordId::parse(&value.reservation).is_err()
            || RecordId::parse(&value.receipt).is_err()
            || value.attempt == 0
            || value.attempt > 8
            || value.detail.len() > 4096
        {
            return Err(AuthorityError::Invalid("historical record".into()));
        }
        observations.push(value);
    }
    observations.sort_by(|a, b| {
        (&a.epoch.0, &a.reservation, a.attempt).cmp(&(&b.epoch.0, &b.reservation, b.attempt))
    });
    Ok(observations)
}
