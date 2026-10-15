//! Cooperative Local incident response for sensitive retained content.
//!
//! A project-wide tombstone is written outside the restorable collaboration
//! database before any managed bytes are removed. The tombstone blocks reads
//! and new captures permanently. Managed CAS/spool files and indexed SQLite
//! rows can then be purged under the archive and response barriers. External
//! exports, Git copies, backups, process memory, and filesystem snapshots are
//! reported as residuals; this is not a universal erasure claim.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::marker::PhantomData;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use aethyme_contracts::experimental_v0::RecordId;
use rusqlite::{OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::collaboration_state::{CollaborationStateError, CollaborationStore, sync_directory};
use crate::file_lock::open_lock_file;

const MARKER_NAME: &str = "sensitive-response.json";
const MAX_MARKER_BYTES: usize = 16 * 1024;
const SCOPE_KIND: &str = "project";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonCode {
    SuspectedTestSecretExposure,
    MisroutedContext,
    OperatorRequestedPurge,
}

impl ReasonCode {
    fn as_str(self) -> &'static str {
        match self {
            Self::SuspectedTestSecretExposure => "suspected_test_secret_exposure",
            Self::MisroutedContext => "misrouted_context",
            Self::OperatorRequestedPurge => "operator_requested_purge",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Marker {
    version: u32,
    project_key: String,
    incident_id: String,
    issuer_label: String,
    operator_uid: u32,
    authority_epoch: String,
    scope_kind: String,
    scope_id: String,
    reason_code: ReasonCode,
    request_digest: String,
    created_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct IncidentStatus {
    pub incident_id: String,
    pub scope: String,
    pub state: String,
    pub request_digest: String,
    pub managed_inventory_digest: Option<String>,
    pub database_cleanup_completed_ms: Option<i64>,
    pub managed_cleanup_complete: bool,
    pub items: Vec<ItemStatus>,
    pub database_tables: Vec<TableStatus>,
    pub external_exports: Vec<ExportStatus>,
    pub residuals: Vec<ResidualStatus>,
    pub limitations: Vec<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TableStatus {
    pub table: String,
    pub row_count: u64,
    pub bytes: u64,
    pub sha256: String,
    pub state: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExportStatus {
    pub path: String,
    pub kind: String,
    pub sha256: String,
    pub bytes: u64,
    pub state: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ItemStatus {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
    pub state: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResidualStatus {
    pub code: String,
    pub path: String,
    pub permanence: String,
    pub resolved_ms: Option<i64>,
}

#[derive(Debug, thiserror::Error)]
pub enum SensitiveError {
    #[error("Local content is permanently quarantined by a sensitive-response tombstone")]
    Quarantined,
    #[error("incident scope, confirmation, or authority does not match the recorded request")]
    Mismatch,
    #[error("the project must be frozen at the exact current authority epoch first")]
    NotFrozen,
    #[error(
        "content readers, writers, or reclamation are still active; the project remains quarantined"
    )]
    Busy,
    #[error("an unresolved or unknown Local job remains; the project stays quarantined")]
    UnknownWork,
    #[error("unclassified managed files remain; the project stays quarantined")]
    UnclassifiedFiles,
    #[error("sensitive-response state is invalid: {0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    State(#[from] CollaborationStateError),
    #[error(transparent)]
    Authority(#[from] crate::local_authority::AuthorityError),
    #[error(transparent)]
    Gc(#[from] crate::collaboration_gc::GcError),
}

// The response lock is shared by all content readers/writers and exclusive
// during purge. Reentrant sharing avoids opening the same flock twice when a
// high-level retrieval calls a checked archive primitive. !Send keeps the
// thread-local reentrancy count sound.
struct SharedLockState {
    _file: File,
    count: Cell<usize>,
}

thread_local! {
    static RESPONSE_READERS: RefCell<BTreeMap<PathBuf, Rc<SharedLockState>>> = const { RefCell::new(BTreeMap::new()) };
}

pub struct ResponseUse {
    path: PathBuf,
    state: Rc<SharedLockState>,
    _not_send: PhantomData<Rc<()>>,
}

impl std::fmt::Debug for ResponseUse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResponseUse")
            .field("path", &self.path)
            .finish()
    }
}

impl Drop for ResponseUse {
    fn drop(&mut self) {
        let last = self.state.count.get() == 1;
        self.state
            .count
            .set(self.state.count.get().saturating_sub(1));
        if last {
            RESPONSE_READERS.with(|readers| {
                readers.borrow_mut().remove(&self.path);
            });
        }
    }
}

pub(crate) struct ResponseExclusive {
    _file: File,
}

fn lock_path_for_project_dir(project_dir: &Path) -> PathBuf {
    project_dir.join("spool/sensitive-response.lock")
}

fn lock_path(store: &CollaborationStore) -> PathBuf {
    lock_path_for_project_dir(store.project_dir())
}

fn marker_path_for_project_dir(project_dir: &Path) -> Result<PathBuf, SensitiveError> {
    let parent = project_dir
        .parent()
        .ok_or_else(|| SensitiveError::Invalid("project directory has no parent".into()))?;
    let name = project_dir
        .file_name()
        .ok_or_else(|| SensitiveError::Invalid("project key is missing".into()))?;
    Ok(parent.join(".local-authority").join(name).join(MARKER_NAME))
}

fn marker_path(store: &CollaborationStore) -> Result<PathBuf, SensitiveError> {
    marker_path_for_project_dir(store.project_dir())
}

fn open_response_lock_at(project_dir: &Path) -> Result<File, SensitiveError> {
    let path = lock_path_for_project_dir(project_dir);
    let parent = path
        .parent()
        .ok_or_else(|| SensitiveError::Invalid("response lock directory".into()))?;
    fs::create_dir_all(parent)?;
    crate::host_state::protect_host_state_path(parent, true)?;
    Ok(open_lock_file(&path)?)
}

fn open_response_lock(store: &CollaborationStore) -> Result<File, SensitiveError> {
    open_response_lock_at(store.project_dir())
}

/// Hold a shared response barrier and reject a tombstoned project.
pub fn response_use(store: &CollaborationStore) -> Result<ResponseUse, SensitiveError> {
    let path = lock_path(store);
    if let Some(state) = RESPONSE_READERS.with(|readers| readers.borrow().get(&path).cloned()) {
        state.count.set(state.count.get().saturating_add(1));
        let guard = ResponseUse {
            path,
            state,
            _not_send: PhantomData,
        };
        ensure_available(store)?;
        return Ok(guard);
    }
    let file = open_response_lock(store)?;
    file.lock_shared()?;
    let state = Rc::new(SharedLockState {
        _file: file,
        count: Cell::new(1),
    });
    RESPONSE_READERS.with(|readers| {
        readers.borrow_mut().insert(path.clone(), state.clone());
    });
    let guard = ResponseUse {
        path,
        state,
        _not_send: PhantomData,
    };
    if let Err(error) = ensure_available(store) {
        drop(guard);
        return Err(error);
    }
    Ok(guard)
}

/// Hold the shared response barrier for metadata-only release of a view hold.
/// Unlike content readers, this narrow operation remains allowed after a
/// permanent tombstone so an operator can release a named hold; it does not
/// expose or inspect project content.
pub(crate) fn response_use_for_view_hold_release(
    store: &CollaborationStore,
) -> Result<ResponseUse, SensitiveError> {
    let path = lock_path(store);
    if let Some(state) = RESPONSE_READERS.with(|readers| readers.borrow().get(&path).cloned()) {
        state.count.set(state.count.get().saturating_add(1));
        return Ok(ResponseUse {
            path,
            state,
            _not_send: PhantomData,
        });
    }
    let file = open_response_lock(store)?;
    file.lock_shared()?;
    let state = Rc::new(SharedLockState {
        _file: file,
        count: Cell::new(1),
    });
    RESPONSE_READERS.with(|readers| {
        readers.borrow_mut().insert(path.clone(), state.clone());
    });
    Ok(ResponseUse {
        path,
        state,
        _not_send: PhantomData,
    })
}

/// Hold the same barrier when a long-lived reader has only retained its
/// immutable project directory. This is used by analysis readers, which cannot
/// own a CollaborationStore or retain a !Send guard between calls.
pub(crate) fn response_use_for_project_dir(
    project_dir: &Path,
) -> Result<ResponseUse, SensitiveError> {
    let project_key = project_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| SensitiveError::Invalid("project key is missing".into()))?;
    crate::collaboration_state::ProjectKey::parse(project_key)
        .map_err(|error| SensitiveError::Invalid(error.to_string()))?;
    let path = lock_path_for_project_dir(project_dir);
    if let Some(state) = RESPONSE_READERS.with(|readers| readers.borrow().get(&path).cloned()) {
        state.count.set(state.count.get().saturating_add(1));
        let guard = ResponseUse {
            path,
            state,
            _not_send: PhantomData,
        };
        ensure_available_at(project_dir, project_key)?;
        return Ok(guard);
    }
    let file = open_response_lock_at(project_dir)?;
    file.lock_shared()?;
    let state = Rc::new(SharedLockState {
        _file: file,
        count: Cell::new(1),
    });
    RESPONSE_READERS.with(|readers| {
        readers.borrow_mut().insert(path.clone(), state.clone());
    });
    let guard = ResponseUse {
        path,
        state,
        _not_send: PhantomData,
    };
    if let Err(error) = ensure_available_at(project_dir, project_key) {
        drop(guard);
        return Err(error);
    }
    Ok(guard)
}

/// Nonblocking shared response barrier for operations whose existing contract
/// is to report active reclamation instead of waiting.
pub(crate) fn try_response_use(
    store: &CollaborationStore,
) -> Result<Option<ResponseUse>, SensitiveError> {
    let path = lock_path(store);
    if let Some(state) = RESPONSE_READERS.with(|readers| readers.borrow().get(&path).cloned()) {
        state.count.set(state.count.get().saturating_add(1));
        let guard = ResponseUse {
            path,
            state,
            _not_send: PhantomData,
        };
        ensure_available(store)?;
        return Ok(Some(guard));
    }
    let file = open_response_lock(store)?;
    match file.try_lock_shared() {
        Ok(()) => {
            let state = Rc::new(SharedLockState {
                _file: file,
                count: Cell::new(1),
            });
            RESPONSE_READERS.with(|readers| {
                readers.borrow_mut().insert(path.clone(), state.clone());
            });
            let guard = ResponseUse {
                path,
                state,
                _not_send: PhantomData,
            };
            if let Err(error) = ensure_available(store) {
                drop(guard);
                return Err(error);
            }
            Ok(Some(guard))
        }
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(error)) => Err(SensitiveError::Io(error)),
    }
}

/// Try to take the purge barrier without waiting behind any reader.
pub(crate) fn try_response_exclusive(
    store: &CollaborationStore,
) -> Result<Option<ResponseExclusive>, SensitiveError> {
    let path = lock_path(store);
    if RESPONSE_READERS.with(|readers| readers.borrow().contains_key(&path)) {
        return Ok(None);
    }
    let file = open_response_lock(store)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(ResponseExclusive { _file: file })),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(error)) => Err(SensitiveError::Io(error)),
    }
}

pub(crate) fn tombstone_present_for_project_dir(
    project_dir: &Path,
) -> Result<bool, SensitiveError> {
    let project_key = project_dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| SensitiveError::Invalid("project key is missing".into()))?;
    crate::collaboration_state::ProjectKey::parse(project_key)
        .map_err(|error| SensitiveError::Invalid(error.to_string()))?;
    Ok(read_marker_at(project_dir, project_key)?.is_some())
}

fn read_marker(store: &CollaborationStore) -> Result<Option<Marker>, SensitiveError> {
    read_marker_at(store.project_dir(), store.project().as_str())
}

fn read_marker_at(project_dir: &Path, project_key: &str) -> Result<Option<Marker>, SensitiveError> {
    let path = marker_path_for_project_dir(project_dir)?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_file() || metadata.len() > MAX_MARKER_BYTES as u64 {
        return Err(SensitiveError::Invalid(
            "external tombstone file profile".into(),
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut bytes = Vec::new();
    options
        .open(path)?
        .take(MAX_MARKER_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_MARKER_BYTES {
        return Err(SensitiveError::Invalid("external tombstone size".into()));
    }
    let marker: Marker = serde_json::from_slice(&bytes)?;
    if marker.version != 1
        || marker.scope_kind != SCOPE_KIND
        || marker.scope_id != project_key
        || marker.project_key != project_key
        || RecordId::parse(&marker.incident_id).is_err()
        || !marker.request_digest.starts_with("sha256:")
        || !marker.authority_epoch.starts_with("sha256:")
    {
        return Err(SensitiveError::Invalid("external tombstone fields".into()));
    }
    Ok(Some(marker))
}

/// The central read/write gate. It consults both the external durable marker
/// and the database journal, so restoring a supported database snapshot cannot
/// silently reopen Local content while the host-side marker remains present.
pub fn ensure_available(store: &CollaborationStore) -> Result<(), SensitiveError> {
    if read_marker(store)?.is_some() {
        return Err(SensitiveError::Quarantined);
    }
    let active: bool = store.read_connection().query_row(
        "SELECT EXISTS(SELECT 1 FROM local_sensitive_incidents)",
        [],
        |row| row.get(0),
    )?;
    if active {
        return Err(SensitiveError::Quarantined);
    }
    Ok(())
}

fn ensure_available_at(project_dir: &Path, project_key: &str) -> Result<(), SensitiveError> {
    if read_marker_at(project_dir, project_key)?.is_some() {
        return Err(SensitiveError::Quarantined);
    }
    let database = project_dir.join("state.db");
    let metadata = fs::symlink_metadata(&database)?;
    if !metadata.file_type().is_file() {
        return Err(SensitiveError::Invalid(
            "collaboration database is not a regular file".into(),
        ));
    }
    let connection = rusqlite::Connection::open_with_flags(
        &database,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let active: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM local_sensitive_incidents)",
        [],
        |row| row.get(0),
    )?;
    if active {
        return Err(SensitiveError::Quarantined);
    }
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn valid_label(label: &str) -> bool {
    !label.trim().is_empty() && label.len() <= 128 && !label.chars().any(char::is_control)
}

fn request_digest(
    project: &str,
    incident_id: &str,
    issuer: &str,
    uid: u32,
    epoch: &str,
    reason: ReasonCode,
) -> String {
    let body = serde_json::to_vec(&(
        1u32,
        project,
        incident_id,
        issuer,
        uid,
        epoch,
        SCOPE_KIND,
        project,
        reason.as_str(),
    ))
    .expect("fixed primitive incident fields serialize");
    format!("sha256:{:x}", Sha256::digest(body))
}

fn operator_uid() -> Result<u32, SensitiveError> {
    #[cfg(unix)]
    {
        Ok(unsafe { libc::geteuid() })
    }
    #[cfg(not(unix))]
    {
        Err(SensitiveError::Invalid(
            "trusted local operator identity is unavailable on this platform".into(),
        ))
    }
}

fn create_marker(store: &CollaborationStore, marker: &Marker) -> Result<(), SensitiveError> {
    let path = marker_path(store)?;
    let parent = path
        .parent()
        .ok_or_else(|| SensitiveError::Invalid("tombstone directory".into()))?;
    fs::create_dir_all(parent)?;
    crate::host_state::protect_host_state_path(parent, true)?;
    let bytes = serde_json::to_vec(marker)?;
    if bytes.len() > MAX_MARKER_BYTES {
        return Err(SensitiveError::Invalid("external tombstone size".into()));
    }
    let mut temporary = tempfile::Builder::new()
        .prefix(".sensitive-response-")
        .tempfile_in(parent)?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    match temporary.persist_noclobber(&path) {
        Ok(_) => sync_directory(parent)?,
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = read_marker(store)?.ok_or_else(|| {
                SensitiveError::Invalid("tombstone vanished during creation".into())
            })?;
            if existing != *marker {
                return Err(SensitiveError::Mismatch);
            }
        }
        Err(error) => return Err(error.error.into()),
    }
    Ok(())
}

/// Permanently quarantine one explicitly scoped project under its current
/// frozen authority epoch. The issuer string is an audit label only; local OS
/// ownership and the exact frozen witness provide the cooperative authority.
pub fn quarantine(
    store: &mut CollaborationStore,
    incident_id: &str,
    issuer_label: &str,
    expected_frozen_epoch: &str,
    reason_code: ReasonCode,
) -> Result<IncidentStatus, SensitiveError> {
    RecordId::parse(incident_id).map_err(|_| SensitiveError::Mismatch)?;
    if !valid_label(issuer_label) {
        return Err(SensitiveError::Invalid("issuer label".into()));
    }
    let witness = crate::local_authority::status(store)?;
    if witness.status != crate::local_authority::Status::Frozen
        || witness.epoch.as_str() != expected_frozen_epoch
    {
        return Err(SensitiveError::NotFrozen);
    }
    // Drain compliant in-flight archive and response operations before the
    // permanent marker is published. A caller retries if either lock is held.
    let Some(_archive) = crate::collaboration_gc::try_admin_exclusive(store)? else {
        return Err(SensitiveError::Busy);
    };
    let Some(_response) = try_response_exclusive(store)? else {
        return Err(SensitiveError::Busy);
    };
    // Recheck under the authority ingress lock after both content barriers.
    // This serializes marker publication with a concurrent resume.
    let _frozen = crate::local_authority::frozen_guard(store, &witness.epoch)?;
    let current_witness = crate::local_authority::status(store)?;
    if current_witness != witness {
        return Err(SensitiveError::NotFrozen);
    }
    let uid = operator_uid()?;
    let digest = request_digest(
        store.project().as_str(),
        incident_id,
        issuer_label,
        uid,
        expected_frozen_epoch,
        reason_code,
    );
    let marker = Marker {
        version: 1,
        project_key: store.project().as_str().to_string(),
        incident_id: incident_id.to_string(),
        issuer_label: issuer_label.to_string(),
        operator_uid: uid,
        authority_epoch: expected_frozen_epoch.to_string(),
        scope_kind: SCOPE_KIND.to_string(),
        scope_id: store.project().as_str().to_string(),
        reason_code,
        request_digest: digest.clone(),
        created_ms: now_ms(),
    };
    let marker = if let Some(existing) = read_marker(store)? {
        if existing.incident_id != marker.incident_id
            || existing.request_digest != marker.request_digest
            || existing.authority_epoch != marker.authority_epoch
            || existing.project_key != marker.project_key
        {
            return Err(SensitiveError::Mismatch);
        }
        let other_incidents: bool = store.read_connection().query_row(
            "SELECT EXISTS(SELECT 1 FROM local_sensitive_incidents WHERE id<>?1)",
            [&existing.incident_id],
            |row| row.get(0),
        )?;
        if other_incidents {
            return Err(SensitiveError::Mismatch);
        }
        existing
    } else {
        let any_incident: bool = store.read_connection().query_row(
            "SELECT EXISTS(SELECT 1 FROM local_sensitive_incidents)",
            [],
            |row| row.get(0),
        )?;
        if any_incident {
            return Err(SensitiveError::Mismatch);
        }
        crate::local_authority::verify_frozen_checkpoint(store, &witness)?;
        create_marker(store, &marker)?;
        marker
    };
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "INSERT OR IGNORE INTO local_sensitive_incidents
         (id,project_key,issuer_label,operator_uid,authority_epoch,scope_kind,scope_id,
          reason_code,request_digest,state,created_ms,updated_ms)
         VALUES (?1,?2,?3,?4,?5,'project',?2,?6,?7,'quarantined',?8,?8)",
        rusqlite::params![
            marker.incident_id,
            marker.project_key,
            marker.issuer_label,
            i64::from(marker.operator_uid),
            marker.authority_epoch,
            marker.reason_code.as_str(),
            marker.request_digest,
            marker.created_ms,
        ],
    )?;
    let row: (String, String, String) = tx.query_row(
        "SELECT project_key, authority_epoch, request_digest
         FROM local_sensitive_incidents WHERE id=?1",
        [&marker.incident_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if row.0 != marker.project_key
        || row.1 != marker.authority_epoch
        || row.2 != marker.request_digest
    {
        return Err(SensitiveError::Mismatch);
    }
    tx.execute(
        "INSERT OR IGNORE INTO local_sensitive_residuals
         (incident_id,code,relative_path,permanence)
         VALUES (?1,'legacy_binary_readers_and_process_memory','','unmanaged')",
        [&marker.incident_id],
    )?;
    tx.commit()?;
    status(store, &marker.incident_id)
}

fn nonnegative_u64(
    row: &rusqlite::Row<'_>,
    column: usize,
    message: &'static str,
) -> rusqlite::Result<u64> {
    let value = row.get::<_, i64>(column)?;
    u64::try_from(value).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Integer,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                message,
            )),
        )
    })
}

pub fn status(
    store: &CollaborationStore,
    incident_id: &str,
) -> Result<IncidentStatus, SensitiveError> {
    let marker = read_marker(store)?.ok_or(SensitiveError::Mismatch)?;
    if marker.incident_id != incident_id {
        return Err(SensitiveError::Mismatch);
    }
    let (state, digest, inventory, database_cleanup_completed_ms): (
        String,
        String,
        Option<String>,
        Option<i64>,
    ) = store
        .read_connection()
        .query_row(
            "SELECT state,request_digest,inventory_digest,database_cleanup_completed_ms
             FROM local_sensitive_incidents WHERE id=?1",
            [incident_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?
        .ok_or(SensitiveError::Quarantined)?;
    if digest != marker.request_digest {
        return Err(SensitiveError::Mismatch);
    }
    let mut items = Vec::new();
    let mut statement = store.read_connection().prepare(
        "SELECT relative_path,sha256,bytes,state FROM local_sensitive_items
         WHERE incident_id=?1 ORDER BY relative_path",
    )?;
    let rows = statement.query_map([incident_id], |row| {
        let bytes = row.get::<_, i64>(2)?;
        let bytes = u64::try_from(bytes).map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Integer,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "negative sensitive inventory byte count",
                )),
            )
        })?;
        Ok(ItemStatus {
            path: row.get(0)?,
            sha256: row.get(1)?,
            bytes,
            state: row.get(3)?,
        })
    })?;
    items.extend(rows.collect::<Result<Vec<_>, _>>()?);
    let mut database_tables = Vec::new();
    let mut statement = store.read_connection().prepare(
        "SELECT table_name,row_count,bytes,sha256,state
         FROM local_sensitive_table_inventory
         WHERE incident_id=?1 ORDER BY table_name",
    )?;
    let rows = statement.query_map([incident_id], |row| {
        let row_count = nonnegative_u64(row, 1, "negative sensitive table row count")?;
        let bytes = nonnegative_u64(row, 2, "negative sensitive table byte count")?;
        Ok(TableStatus {
            table: row.get(0)?,
            row_count,
            bytes,
            sha256: row.get(3)?,
            state: row.get(4)?,
        })
    })?;
    database_tables.extend(rows.collect::<Result<Vec<_>, _>>()?);
    let mut external_exports = Vec::new();
    let mut statement = store.read_connection().prepare(
        "SELECT path,kind,sha256,bytes,state FROM local_sensitive_exports ORDER BY path",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(ExportStatus {
            path: row.get(0)?,
            kind: row.get(1)?,
            sha256: row.get(2)?,
            bytes: nonnegative_u64(row, 3, "negative export byte count")?,
            state: row.get(4)?,
        })
    })?;
    external_exports.extend(rows.collect::<Result<Vec<_>, _>>()?);
    let mut residuals = Vec::new();
    let mut statement = store.read_connection().prepare(
        "SELECT code,relative_path,permanence,resolved_ms FROM local_sensitive_residuals
         WHERE incident_id=?1 ORDER BY code,relative_path",
    )?;
    let rows = statement.query_map([incident_id], |row| {
        Ok(ResidualStatus {
            code: row.get(0)?,
            path: row.get(1)?,
            permanence: row.get(2)?,
            resolved_ms: row.get(3)?,
        })
    })?;
    residuals.extend(rows.collect::<Result<Vec<_>, _>>()?);
    let managed_cleanup_complete = inventory.is_some()
        && database_cleanup_completed_ms.is_some()
        && database_tables.iter().all(|table| table.state == "cleared")
        && items.iter().all(|item| item.state == "deleted")
        && !residuals
            .iter()
            .any(|item| item.permanence == "unclassified");
    Ok(IncidentStatus {
        incident_id: incident_id.to_string(),
        scope: "entire project collaboration store".into(),
        state,
        request_digest: digest,
        managed_inventory_digest: inventory,
        database_cleanup_completed_ms,
        managed_cleanup_complete,
        items,
        database_tables,
        external_exports,
        residuals,
        limitations: vec![
            "external exports, Git worktrees/remotes, backups, process memory, and filesystem snapshots are unmanaged residuals",
            "older binaries and readers that already opened the store cannot be proven quiescent; state remains quarantined after managed deletion",
            "same-OS-user direct filesystem access is outside the cooperative boundary",
            "a database replacement from an older schema is unsupported and must remain frozen",
            "physical storage remapping and SSD wear-leveling are not verified erasure",
        ],
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct PurgeOutcome {
    pub status: IncidentStatus,
    pub completed: bool,
}

fn marker_for_incident(
    store: &CollaborationStore,
    incident_id: &str,
    confirmation: &str,
) -> Result<Marker, SensitiveError> {
    let marker = read_marker(store)?.ok_or(SensitiveError::Mismatch)?;
    if marker.incident_id != incident_id || marker.request_digest != confirmation {
        return Err(SensitiveError::Mismatch);
    }
    if operator_uid()? != marker.operator_uid {
        return Err(SensitiveError::Mismatch);
    }
    let witness = crate::local_authority::status(store)?;
    if witness.status != crate::local_authority::Status::Frozen
        || witness.epoch.as_str() != marker.authority_epoch
    {
        return Err(SensitiveError::NotFrozen);
    }
    Ok(marker)
}

fn add_residual(
    store: &mut CollaborationStore,
    incident_id: &str,
    code: &str,
    path: &str,
    permanence: &str,
) -> Result<(), SensitiveError> {
    store.connection().execute(
        "INSERT OR IGNORE INTO local_sensitive_residuals
         (incident_id,code,relative_path,permanence) VALUES (?1,?2,?3,?4)
         ON CONFLICT(incident_id,code,relative_path) DO UPDATE SET resolved_ms=NULL",
        rusqlite::params![incident_id, code, path, permanence],
    )?;
    Ok(())
}

fn unresolved_work(store: &CollaborationStore) -> Result<bool, SensitiveError> {
    let checks = [
        "SELECT EXISTS(SELECT 1 FROM local_native_gate_jobs WHERE state IN ('dispatching','unknown'))",
        "SELECT EXISTS(SELECT 1 FROM local_controller_runs WHERE state IN ('dispatching','unknown'))",
        "SELECT EXISTS(SELECT 1 FROM local_controller_services WHERE state='dispatching')",
        "SELECT EXISTS(SELECT 1 FROM local_agent_dispatches WHERE state IN ('intent','unknown'))",
        "SELECT EXISTS(SELECT 1 FROM local_budget_reservations WHERE phase IN ('completion_pending','consumption_unknown'))",
        "SELECT EXISTS(SELECT 1 FROM local_waves WHERE phase IN ('composing','verifying','outcome_unknown'))",
    ];
    for sql in checks {
        let exists: bool = store
            .read_connection()
            .query_row(sql, [], |row| row.get(0))?;
        if exists {
            return Ok(true);
        }
    }
    Ok(false)
}

#[derive(Debug, Clone)]
struct FileItem {
    relative_path: String,
    sha256: String,
    bytes: u64,
}

fn hash_file(path: &Path) -> Result<(String, u64), SensitiveError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(SensitiveError::Invalid(format!(
            "managed path is not a regular file: {}",
            path.display()
        )));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options.open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() != metadata.len() {
        return Err(SensitiveError::Invalid(
            "managed file changed while opening".into(),
        ));
    }
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    let after = file.metadata()?;
    if after.len() != metadata.len() {
        return Err(SensitiveError::Invalid(
            "managed file changed while hashing".into(),
        ));
    }
    Ok((format!("sha256:{:x}", hash.finalize()), metadata.len()))
}

fn collect_tree(
    store: &CollaborationStore,
    relative: &Path,
    files: &mut Vec<FileItem>,
    unknown: &mut Vec<String>,
) -> Result<(), SensitiveError> {
    let root = store.project_dir();
    let path = root.join(relative);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
        unknown.push(relative.to_string_lossy().into_owned());
        return Ok(());
    }
    if metadata.is_dir() {
        let mut entries = fs::read_dir(&path)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let child = relative.join(entry.file_name());
            collect_tree(store, &child, files, unknown)?;
        }
        return Ok(());
    }
    if metadata.is_file() {
        let Some(relative_path) = relative.to_str().map(str::to_owned) else {
            unknown.push(relative.to_string_lossy().into_owned());
            return Ok(());
        };
        let (sha256, bytes) = hash_file(&path)?;
        files.push(FileItem {
            relative_path,
            sha256,
            bytes,
        });
    } else {
        unknown.push(relative.to_string_lossy().into_owned());
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct DatabaseItem {
    table_name: String,
    row_count: u64,
    bytes: u64,
    sha256: String,
}

#[derive(Debug, Clone)]
struct ExportItem {
    path: String,
    kind: String,
    sha256: String,
    bytes: u64,
    state: String,
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn database_inventory(store: &CollaborationStore) -> Result<Vec<DatabaseItem>, SensitiveError> {
    const PRESERVED_TABLES: &[&str] = &[
        "meta",
        "local_authority_state",
        "local_sensitive_incidents",
        "local_sensitive_items",
        "local_sensitive_residuals",
        "local_sensitive_exports",
        "local_sensitive_table_inventory",
    ];
    let connection = store.read_connection();
    let mut statement = connection.prepare(
        "SELECT name FROM sqlite_master
         WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);

    let mut inventory = Vec::new();
    for table_name in names {
        if PRESERVED_TABLES.contains(&table_name.as_str()) {
            continue;
        }
        let mut columns_statement = connection.prepare(&format!(
            "PRAGMA table_info({})",
            quote_identifier(&table_name)
        ))?;
        let columns = columns_statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(columns_statement);
        if columns.is_empty() {
            return Err(SensitiveError::Invalid(format!(
                "managed database table has no columns: {table_name}"
            )));
        }

        let sql = format!("SELECT * FROM {}", quote_identifier(&table_name));
        let mut rows_statement = connection.prepare(&sql)?;
        let column_count = rows_statement.column_count();
        let mut rows = rows_statement.query([])?;
        let mut hasher = Sha256::new();
        hasher.update(b"aethyme-local-sensitive-table-v1\0");
        hasher.update((table_name.len() as u64).to_be_bytes());
        hasher.update(table_name.as_bytes());
        for column in &columns {
            hasher.update((column.len() as u64).to_be_bytes());
            hasher.update(column.as_bytes());
        }
        let mut row_count = 0u64;
        let mut bytes = 0u64;
        while let Some(row) = rows.next()? {
            row_count = row_count
                .checked_add(1)
                .ok_or_else(|| SensitiveError::Invalid("table row count overflow".into()))?;
            hasher.update([0xff]);
            for index in 0..column_count {
                use rusqlite::types::ValueRef;
                let value = row.get_ref(index)?;
                match value {
                    ValueRef::Null => hasher.update([0]),
                    ValueRef::Integer(value) => {
                        hasher.update([1]);
                        hasher.update(value.to_be_bytes());
                        bytes = bytes.checked_add(8).ok_or_else(|| {
                            SensitiveError::Invalid("table byte count overflow".into())
                        })?;
                    }
                    ValueRef::Real(value) => {
                        hasher.update([2]);
                        hasher.update(value.to_bits().to_be_bytes());
                        bytes = bytes.checked_add(8).ok_or_else(|| {
                            SensitiveError::Invalid("table byte count overflow".into())
                        })?;
                    }
                    ValueRef::Text(value) => {
                        hasher.update([3]);
                        hasher.update((value.len() as u64).to_be_bytes());
                        hasher.update(value);
                        bytes = bytes.checked_add(value.len() as u64).ok_or_else(|| {
                            SensitiveError::Invalid("table byte count overflow".into())
                        })?;
                    }
                    ValueRef::Blob(value) => {
                        hasher.update([4]);
                        hasher.update((value.len() as u64).to_be_bytes());
                        hasher.update(value);
                        bytes = bytes.checked_add(value.len() as u64).ok_or_else(|| {
                            SensitiveError::Invalid("table byte count overflow".into())
                        })?;
                    }
                }
            }
        }
        inventory.push(DatabaseItem {
            table_name,
            row_count,
            bytes,
            sha256: format!("sha256:{:x}", hasher.finalize()),
        });
    }
    Ok(inventory)
}

fn inventory_digest(files: &[FileItem], tables: &[DatabaseItem], exports: &[ExportItem]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"aethyme-local-sensitive-inventory-v2\0");
    for item in files {
        hash.update([b'f']);
        hash.update((item.relative_path.len() as u64).to_be_bytes());
        hash.update(item.relative_path.as_bytes());
        hash.update(item.sha256.as_bytes());
        hash.update(item.bytes.to_be_bytes());
    }
    for item in tables {
        hash.update([b'd']);
        hash.update((item.table_name.len() as u64).to_be_bytes());
        hash.update(item.table_name.as_bytes());
        hash.update(item.sha256.as_bytes());
        hash.update(item.row_count.to_be_bytes());
        hash.update(item.bytes.to_be_bytes());
    }
    for item in exports {
        hash.update([b'e']);
        hash.update((item.path.len() as u64).to_be_bytes());
        hash.update(item.path.as_bytes());
        hash.update((item.kind.len() as u64).to_be_bytes());
        hash.update(item.kind.as_bytes());
        hash.update(item.sha256.as_bytes());
        hash.update(item.bytes.to_be_bytes());
        hash.update((item.state.len() as u64).to_be_bytes());
        hash.update(item.state.as_bytes());
    }
    format!("sha256:{:x}", hash.finalize())
}

fn external_exports(
    store: &CollaborationStore,
    unknown: &mut Vec<String>,
) -> Result<Vec<ExportItem>, SensitiveError> {
    let connection = store.read_connection();
    let mut statement = connection.prepare(
        "SELECT path,kind,sha256,bytes,state FROM local_sensitive_exports ORDER BY path",
    )?;
    let mut exports = statement
        .query_map([], |row| {
            Ok(ExportItem {
                path: row.get(0)?,
                kind: row.get(1)?,
                sha256: row.get(2)?,
                bytes: nonnegative_u64(row, 3, "negative export byte count")?,
                state: row.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);

    let marker = marker_path(store)?;
    let authority_dir = marker
        .parent()
        .ok_or_else(|| SensitiveError::Invalid("authority marker directory".into()))?;
    let history = authority_dir.join("history");
    let history_metadata = match fs::symlink_metadata(&history) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(exports),
        Err(error) => return Err(error.into()),
    };
    if history_metadata.file_type().is_symlink() || !history_metadata.is_dir() {
        unknown.push("<unclassified-authority-history-root>".into());
        return Ok(exports);
    }
    let entries = match fs::read_dir(&history) {
        Ok(entries) => entries.collect::<Result<Vec<_>, _>>()?,
        Err(error) => return Err(error.into()),
    };
    let known: BTreeSet<String> = exports.iter().map(|item| item.path.clone()).collect();
    for entry in entries {
        let path = entry.path();
        let Some(path_text) = path.to_str().map(str::to_owned) else {
            unknown.push("<non-utf8-authority-history-path>".into());
            continue;
        };
        if known.contains(&path_text) {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_file() {
            unknown.push(format!(
                "<unclassified-authority-history:{}>",
                path.display()
            ));
            continue;
        }
        let (sha256, bytes) = hash_file(&path)?;
        exports.push(ExportItem {
            path: path_text,
            kind: "authority_history".into(),
            sha256,
            bytes,
            state: "present".into(),
        });
    }
    exports.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(exports)
}

fn prepare_inventory(
    store: &mut CollaborationStore,
    marker: &Marker,
) -> Result<Vec<FileItem>, SensitiveError> {
    let existing: Option<String> = store
        .read_connection()
        .query_row(
            "SELECT inventory_digest FROM local_sensitive_incidents WHERE id=?1",
            [&marker.incident_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    if existing.is_some() {
        let mut statement = store.connection().prepare(
            "SELECT relative_path,sha256,bytes FROM local_sensitive_items
             WHERE incident_id=?1 ORDER BY relative_path",
        )?;
        let rows = statement.query_map([&marker.incident_id], |row| {
            Ok(FileItem {
                relative_path: row.get(0)?,
                sha256: row.get(1)?,
                bytes: nonnegative_u64(row, 2, "negative sensitive inventory byte count")?,
            })
        })?;
        return Ok(rows.collect::<Result<Vec<_>, _>>()?);
    }

    let mut files = Vec::new();
    let mut unknown = Vec::new();
    for path in [
        Path::new("objects"),
        Path::new("spool/archive"),
        Path::new("spool/gc"),
        Path::new("views"),
        Path::new("generations"),
        Path::new("import-rehearsals"),
    ] {
        collect_tree(store, path, &mut files, &mut unknown)?;
    }
    // Lock and verifier-authority state is control state, not archived source.
    // Leave it in place and report the exact roots as unmanaged residuals.
    for name in ["objects", "spool"] {
        let path = store.project_dir().join(name);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            unknown.push(name.to_string());
            continue;
        }
        let entries = match fs::read_dir(&path) {
            Ok(entries) => entries.collect::<Result<Vec<_>, _>>()?,
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let child = match entry.file_name().into_string() {
                Ok(child) => child,
                Err(_) => {
                    unknown.push(format!("{name}/<non-utf8-name>"));
                    continue;
                }
            };
            let expected = if name == "spool" {
                matches!(
                    child.as_str(),
                    "archive" | "gc" | "gc.lock" | "sensitive-response.lock"
                )
            } else {
                child == "sha256"
            };
            if !expected {
                unknown.push(format!("{name}/{child}"));
            }
        }
    }
    files.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    unknown.sort();
    unknown.dedup();
    let tables = database_inventory(store)?;
    let exports = external_exports(store, &mut unknown)?;
    let digest = inventory_digest(&files, &tables, &exports);
    let project_dir = store.project_dir().to_path_buf();
    let root_entries = fs::read_dir(&project_dir)?.collect::<Result<Vec<_>, _>>()?;
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    for item in &files {
        tx.execute(
            "INSERT INTO local_sensitive_items
             (incident_id,relative_path,sha256,bytes,state) VALUES (?1,?2,?3,?4,'pending')",
            rusqlite::params![
                marker.incident_id,
                item.relative_path,
                item.sha256,
                item.bytes
            ],
        )?;
    }
    for item in &tables {
        tx.execute(
            "INSERT INTO local_sensitive_table_inventory
             (incident_id,table_name,row_count,bytes,sha256,state)
             VALUES (?1,?2,?3,?4,?5,'pending')",
            rusqlite::params![
                marker.incident_id,
                item.table_name,
                item.row_count,
                item.bytes,
                item.sha256
            ],
        )?;
    }
    for path in &unknown {
        tx.execute(
            "INSERT OR IGNORE INTO local_sensitive_residuals
             (incident_id,code,relative_path,permanence) VALUES (?1,'unclassified_managed_path',?2,'unclassified')",
            rusqlite::params![marker.incident_id, path],
        )?;
    }
    for item in &exports {
        tx.execute(
            "INSERT OR IGNORE INTO local_sensitive_exports
             (path,kind,sha256,bytes,state,created_ms)
             VALUES (?1,?2,?3,?4,?5,?6)",
            rusqlite::params![
                item.path,
                item.kind,
                item.sha256,
                item.bytes,
                item.state,
                now_ms()
            ],
        )?;
        let recorded: (String, String, i64, String) = tx.query_row(
            "SELECT kind,sha256,bytes,state FROM local_sensitive_exports WHERE path=?1",
            [&item.path],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        if recorded.0 != item.kind
            || recorded.1 != item.sha256
            || u64::try_from(recorded.2).ok() != Some(item.bytes)
            || recorded.3 != item.state
        {
            return Err(SensitiveError::Mismatch);
        }
        tx.execute(
            "INSERT OR IGNORE INTO local_sensitive_residuals
             (incident_id,code,relative_path,permanence)
             VALUES (?1,'previous_external_export_copy',?2,'unmanaged')",
            rusqlite::params![marker.incident_id, item.path],
        )?;
    }
    for (code, permanence) in [
        ("unregistered_external_exports", "unmanaged"),
        ("git_worktrees_and_remotes", "unmanaged"),
        ("host_backups_and_filesystem_snapshots", "unmanaged"),
        ("logs_previews_and_process_memory", "unmanaged"),
        ("legacy_binary_readers_and_process_memory", "unmanaged"),
    ] {
        tx.execute(
            "INSERT OR IGNORE INTO local_sensitive_residuals
             (incident_id,code,relative_path,permanence) VALUES (?1,?2,'',?3)",
            rusqlite::params![marker.incident_id, code, permanence],
        )?;
    }
    for name in ["controller-locks", "agent-locks", "verifier-authority"] {
        let path = project_dir.join(name);
        if fs::symlink_metadata(&path).is_ok() {
            tx.execute(
                "INSERT OR IGNORE INTO local_sensitive_residuals
                 (incident_id,code,relative_path,permanence)
                 VALUES (?1,'preserved_authority_or_lock_state',?2,'unmanaged')",
                rusqlite::params![marker.incident_id, name],
            )?;
        }
    }
    for entry in root_entries {
        let name = match entry.file_name().into_string() {
            Ok(name) => name,
            Err(_) => {
                tx.execute(
                    "INSERT OR IGNORE INTO local_sensitive_residuals
                     (incident_id,code,relative_path,permanence)
                     VALUES (?1,'unclassified_managed_path','<non-utf8-name>','unclassified')",
                    [&marker.incident_id],
                )?;
                continue;
            }
        };
        if matches!(
            name.as_str(),
            "objects"
                | "spool"
                | "views"
                | "generations"
                | "import-rehearsals"
                | "controller-locks"
                | "agent-locks"
                | "verifier-authority"
                | "state.db"
                | "state.db-wal"
                | "state.db-shm"
                | crate::collaboration_state::ROOT_MARKER
        ) {
            continue;
        }
        tx.execute(
            "INSERT OR IGNORE INTO local_sensitive_residuals
             (incident_id,code,relative_path,permanence)
             VALUES (?1,'unclassified_managed_path',?2,'unclassified')",
            rusqlite::params![marker.incident_id, name],
        )?;
    }
    tx.execute(
        "UPDATE local_sensitive_incidents SET state='purging',inventory_digest=?2,updated_ms=?3 WHERE id=?1",
        rusqlite::params![marker.incident_id, digest, now_ms()],
    )?;
    tx.commit()?;
    Ok(files)
}

fn safe_managed_path(root: &Path, relative: &str) -> Result<PathBuf, SensitiveError> {
    let rel = Path::new(relative);
    if rel.is_absolute()
        || rel
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(SensitiveError::Invalid(
            "inventory path is not relative".into(),
        ));
    }
    let mut current = root.to_path_buf();
    for component in rel.components() {
        current.push(component.as_os_str());
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(current),
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() {
            return Err(SensitiveError::Invalid(
                "inventory path traverses a symlink".into(),
            ));
        }
    }
    Ok(current)
}

fn delete_inventory_item(
    store: &mut CollaborationStore,
    incident_id: &str,
    item: &FileItem,
) -> Result<bool, SensitiveError> {
    if item.relative_path == "views" || item.relative_path.starts_with("views/") {
        add_residual(
            store,
            incident_id,
            "analysis_view_retained_for_reader_safety",
            &item.relative_path,
            "unmanaged",
        )?;
        store.connection().execute(
            "UPDATE local_sensitive_items SET state='residual'
             WHERE incident_id=?1 AND relative_path=?2",
            rusqlite::params![incident_id, item.relative_path],
        )?;
        return Ok(false);
    }
    let path = safe_managed_path(store.project_dir(), &item.relative_path)?;
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
        Ok(metadata) => {
            if !metadata.file_type().is_file() {
                add_residual(
                    store,
                    incident_id,
                    "managed_path_changed_type",
                    &item.relative_path,
                    "unclassified",
                )?;
                store.connection().execute(
                    "UPDATE local_sensitive_items SET state='residual' WHERE incident_id=?1 AND relative_path=?2",
                    rusqlite::params![incident_id, item.relative_path],
                )?;
                return Ok(false);
            }
            let (digest, bytes) = hash_file(&path)?;
            if digest != item.sha256 || bytes != item.bytes {
                add_residual(
                    store,
                    incident_id,
                    "managed_file_changed_after_inventory",
                    &item.relative_path,
                    "unclassified",
                )?;
                store.connection().execute(
                    "UPDATE local_sensitive_items SET state='residual' WHERE incident_id=?1 AND relative_path=?2",
                    rusqlite::params![incident_id, item.relative_path],
                )?;
                return Ok(false);
            }
            fs::remove_file(&path)?;
            if let Some(parent) = path.parent() {
                sync_directory(parent)?;
            }
        }
    }
    store.connection().execute(
        "UPDATE local_sensitive_items SET state='deleted' WHERE incident_id=?1 AND relative_path=?2",
        rusqlite::params![incident_id, item.relative_path],
    )?;
    Ok(true)
}

fn clear_managed_database(
    store: &mut CollaborationStore,
    incident_id: &str,
) -> Result<(), SensitiveError> {
    let project_dir = store.project_dir().to_path_buf();
    let database = project_dir.join("state.db");
    let connection = store.connection();
    connection.pragma_update(None, "secure_delete", "ON")?;
    connection.pragma_update(None, "foreign_keys", "OFF")?;
    let result = (|| {
        let tables = {
            let mut statement = connection.prepare(
                "SELECT name FROM sqlite_master
                 WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name DESC",
            )?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for table in tables {
            if matches!(
                table.as_str(),
                "meta"
                    | "local_authority_state"
                    | "local_sensitive_incidents"
                    | "local_sensitive_items"
                    | "local_sensitive_residuals"
                    | "local_sensitive_exports"
                    | "local_sensitive_table_inventory"
            ) {
                continue;
            }
            let quoted = quote_identifier(&table);
            transaction.execute(&format!("DELETE FROM {quoted}"), [])?;
        }
        transaction.commit()?;

        checkpoint_truncated(connection)?;
        connection.execute_batch("VACUUM")?;
        // In WAL mode VACUUM may commit pages to the WAL. The post-VACUUM
        // truncate checkpoint is part of the deletion proof, not optional
        // cleanup; inspect the frame counts as well as SQLite's busy flag.
        checkpoint_truncated(connection)?;
        sync_file_if_present(&database)?;
        sync_file_if_present(&project_dir.join("state.db-wal"))?;
        sync_directory(&project_dir)?;

        // Receipt and cleared-state rows are committed only after content
        // deletion, checkpoint, VACUUM, and file/directory sync succeeded.
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE local_sensitive_table_inventory SET state='cleared'
             WHERE incident_id=?1 AND state='pending'",
            [incident_id],
        )?;
        tx.execute(
            "UPDATE local_sensitive_incidents
             SET database_cleanup_completed_ms=?2,updated_ms=?2
             WHERE id=?1 AND database_cleanup_completed_ms IS NULL",
            rusqlite::params![incident_id, now_ms()],
        )?;
        tx.commit()?;
        Ok(())
    })();
    let foreign_keys = connection.pragma_update(None, "foreign_keys", "ON");
    match (result, foreign_keys) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(SensitiveError::Sqlite(error)),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn checkpoint_truncated(connection: &rusqlite::Connection) -> Result<(), SensitiveError> {
    let (busy, log_frames, checkpointed_frames): (i64, i64, i64) =
        connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    if busy != 0 || (log_frames >= 0 && checkpointed_frames < log_frames) {
        return Err(SensitiveError::Busy);
    }
    Ok(())
}

fn sync_file_if_present(path: &Path) -> Result<(), SensitiveError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_file() {
        return Err(SensitiveError::Invalid(format!(
            "database durability path is not a regular file: {}",
            path.display()
        )));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(SensitiveError::Invalid(
            "database durability path changed while opening".into(),
        ));
    }
    file.sync_all()?;
    Ok(())
}

/// Resume an interrupted purge, or begin the first purge after quarantine.
/// The confirmation is the immutable request digest printed by quarantine.
pub fn purge(
    store: &mut CollaborationStore,
    incident_id: &str,
    confirmation: &str,
) -> Result<PurgeOutcome, SensitiveError> {
    let marker = marker_for_incident(store, incident_id, confirmation)?;
    let state: String = store.read_connection().query_row(
        "SELECT state FROM local_sensitive_incidents WHERE id=?1 AND request_digest=?2",
        rusqlite::params![incident_id, confirmation],
        |row| row.get(0),
    )?;
    if state == "purged" {
        return Ok(PurgeOutcome {
            status: status(store, incident_id)?,
            completed: true,
        });
    }
    let Some(_archive) = crate::collaboration_gc::try_admin_exclusive(store)? else {
        add_residual(
            store,
            incident_id,
            "archive_or_capture_active",
            "",
            "transient",
        )?;
        return Err(SensitiveError::Busy);
    };
    let Some(_response) = try_response_exclusive(store)? else {
        add_residual(
            store,
            incident_id,
            "reader_or_writer_active",
            "",
            "transient",
        )?;
        return Err(SensitiveError::Busy);
    };
    let witness = crate::local_authority::status(store)?;
    if witness.status != crate::local_authority::Status::Frozen
        || witness.epoch.as_str() != marker.authority_epoch
    {
        return Err(SensitiveError::NotFrozen);
    }
    let _frozen = crate::local_authority::frozen_guard(store, &witness.epoch)?;
    if unresolved_work(store)? {
        add_residual(
            store,
            incident_id,
            "local_job_liveness_unknown",
            "",
            "transient",
        )?;
        return Err(SensitiveError::UnknownWork);
    }
    store.connection().execute(
        "UPDATE local_sensitive_residuals SET resolved_ms=?2
         WHERE incident_id=?1 AND permanence='transient' AND resolved_ms IS NULL",
        rusqlite::params![incident_id, now_ms()],
    )?;
    let inventory = prepare_inventory(store, &marker)?;
    let mut completed = true;
    for item in &inventory {
        let item_state: Option<String> = store
            .read_connection()
            .query_row(
                "SELECT state FROM local_sensitive_items WHERE incident_id=?1 AND relative_path=?2",
                rusqlite::params![incident_id, item.relative_path],
                |row| row.get(0),
            )
            .optional()?;
        if item_state.as_deref() == Some("deleted") {
            continue;
        }
        if item_state.as_deref() == Some("residual") {
            completed = false;
            continue;
        }
        if !delete_inventory_item(store, incident_id, item)? {
            completed = false;
        }
    }
    let unclassified: bool = store.read_connection().query_row(
        "SELECT EXISTS(SELECT 1 FROM local_sensitive_residuals WHERE incident_id=?1 AND permanence='unclassified')",
        [incident_id],
        |row| row.get(0),
    )?;
    // Views are inventoried but preserved because a reader from an older
    // process can retain a generation pin without holding the response lock.
    let undeleted_non_view_files: bool = store.read_connection().query_row(
        "SELECT EXISTS(
             SELECT 1 FROM local_sensitive_items
             WHERE incident_id=?1 AND state<>'deleted'
               AND relative_path<>'views' AND relative_path NOT LIKE 'views/%'
         )",
        [incident_id],
        |row| row.get(0),
    )?;
    let cleanup_receipt: Option<i64> = store.read_connection().query_row(
        "SELECT database_cleanup_completed_ms FROM local_sensitive_incidents WHERE id=?1",
        [incident_id],
        |row| row.get(0),
    )?;
    let legacy_readers_unresolved: bool = store.read_connection().query_row(
        "SELECT EXISTS(SELECT 1 FROM local_sensitive_residuals
         WHERE incident_id=?1 AND code='legacy_binary_readers_and_process_memory'
         AND resolved_ms IS NULL)",
        [incident_id],
        |row| row.get(0),
    )?;
    if !undeleted_non_view_files && !unclassified && cleanup_receipt.is_none() {
        if let Err(error) = clear_managed_database(store, incident_id) {
            add_residual(
                store,
                incident_id,
                "managed_database_cleanup_incomplete",
                "state.db",
                "transient",
            )?;
            store.connection().execute(
                "UPDATE local_sensitive_incidents SET state='quarantined',updated_ms=?2 WHERE id=?1",
                rusqlite::params![incident_id, now_ms()],
            )?;
            return Err(error);
        }
        // Schema 23 readers and already-materialized process copies cannot be
        // observed or revoked by this cooperative lock. Keep the permanent
        // quarantine state even after exact managed bytes have been removed.
        store.connection().execute(
            "UPDATE local_sensitive_incidents SET state='quarantined',updated_ms=?2 WHERE id=?1",
            rusqlite::params![incident_id, now_ms()],
        )?;
    } else {
        store.connection().execute(
            "UPDATE local_sensitive_incidents SET state='quarantined',updated_ms=?2 WHERE id=?1",
            rusqlite::params![incident_id, now_ms()],
        )?;
    }
    Ok(PurgeOutcome {
        status: status(store, incident_id)?,
        completed: completed && !unclassified && !legacy_readers_unresolved,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_durable_marker_blocks_resume_and_quarantine_retry_repairs_missing_row() {
        let fixture = crate::local_target::tests::Fixture::new();
        let mut store = fixture.store;
        let owner =
            crate::local_target::acquire_controller(&mut store, "target", "appointed").unwrap();
        let frozen = crate::local_authority::freeze(
            &mut store,
            owner.authority_epoch(),
            "sensitive response",
        )
        .unwrap();
        let incident_id = format!("sha256:{}", "c".repeat(64));
        let issuer_label = "local-operator";
        let reason_code = ReasonCode::SuspectedTestSecretExposure;
        let uid = operator_uid().unwrap();
        let digest = request_digest(
            store.project().as_str(),
            &incident_id,
            issuer_label,
            uid,
            frozen.epoch.as_str(),
            reason_code,
        );
        create_marker(
            &store,
            &Marker {
                version: 1,
                project_key: store.project().as_str().into(),
                incident_id: incident_id.clone(),
                issuer_label: issuer_label.into(),
                operator_uid: uid,
                authority_epoch: frozen.epoch.as_str().into(),
                scope_kind: SCOPE_KIND.into(),
                scope_id: store.project().as_str().into(),
                reason_code,
                request_digest: digest.clone(),
                created_ms: now_ms(),
            },
        )
        .unwrap();

        assert!(matches!(
            crate::local_authority::resume(&mut store, &frozen.epoch),
            Err(crate::local_authority::AuthorityError::Frozen)
        ));
        let status = quarantine(
            &mut store,
            &incident_id,
            issuer_label,
            frozen.epoch.as_str(),
            reason_code,
        )
        .unwrap();
        assert_eq!(status.request_digest, digest);
        assert_eq!(status.state, "quarantined");
    }
}
