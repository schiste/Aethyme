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
use std::io::{Read, Seek, Write};
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
const CAPTURE_NOTICE_INGRESS_LOCK_PATH: &str = "spool/capture-notice-ingress.lock";

#[cfg(test)]
thread_local! {
    static INTERRUPT_AFTER_MANAGED_TABLE_DELETE: Cell<bool> = const { Cell::new(false) };
}

/// Remove only schema-24 administrative-response tables from a synthetic
/// pre-24 database used by historical-migration tests. Production migrations
/// remain responsible for creating these tables on open.
#[cfg(test)]
pub(crate) fn drop_sensitive_response_tables_for_legacy_fixture(
    connection: &rusqlite::Connection,
) -> rusqlite::Result<()> {
    connection.execute_batch(
        "DROP TABLE IF EXISTS local_sensitive_table_inventory;
         DROP TABLE IF EXISTS local_sensitive_residuals;
         DROP TABLE IF EXISTS local_sensitive_items;
         DROP TABLE IF EXISTS local_sensitive_exports;
         DROP TABLE IF EXISTS local_sensitive_incidents;",
    )
}

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
    pub kind: String,
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

impl ResponseUse {
    pub(crate) fn covers(&self, store: &CollaborationStore) -> bool {
        self.path == lock_path(store)
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

fn open_existing_response_lock_read_only(
    store: &CollaborationStore,
) -> Result<File, SensitiveError> {
    let path = lock_path(store);
    let parent = path
        .parent()
        .ok_or_else(|| SensitiveError::Invalid("response lock directory".into()))?;
    for directory in [
        parent,
        parent
            .parent()
            .ok_or_else(|| SensitiveError::Invalid("response lock authority directory".into()))?,
    ] {
        let metadata = fs::symlink_metadata(directory)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(SensitiveError::Invalid(format!(
                "response lock directory {} is not a real directory",
                directory.display()
            )));
        }
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = options.open(&path)?;
    if !file.metadata()?.is_file() {
        return Err(SensitiveError::Invalid(format!(
            "response lock {} is not a regular file",
            path.display()
        )));
    }
    Ok(file)
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

/// Hold the existing response barrier without creating or modifying files.
/// Peer-project verification uses this so a read never initializes that
/// project's state as a side effect.
pub(crate) fn response_use_read_only(
    store: &CollaborationStore,
) -> Result<ResponseUse, SensitiveError> {
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
    let file = open_existing_response_lock_read_only(store)?;
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
        // SAFETY: `geteuid` has no preconditions and only reads the effective UID.
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
        let path: String = row.get(0)?;
        Ok(ItemStatus {
            kind: if is_directory_item(&path) {
                "directory"
            } else {
                "file"
            }
            .into(),
            path,
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
            "validated capture-operation lock files and authority lock roots remain unmanaged control-state residuals",
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

/// Record a hold file removed by the narrow post-tombstone metadata operation.
/// This is a no-op before an incident inventory exists. Once inventoried, a
/// missing file is accepted only after this durable state transition.
pub(crate) fn record_released_view_hold(
    store: &mut CollaborationStore,
    path: &Path,
) -> Result<(), SensitiveError> {
    let Some(marker) = read_marker(store)? else {
        return Ok(());
    };
    let relative = path
        .strip_prefix(store.project_dir())
        .ok()
        .and_then(Path::to_str)
        .ok_or_else(|| {
            SensitiveError::Invalid("released view hold path is outside project".into())
        })?;
    let parts = relative.split('/').collect::<Vec<_>>();
    if parts.len() != 4 || parts[0] != "views" || parts[2] != "holds" || parts[3].is_empty() {
        return Err(SensitiveError::Invalid(
            "released view hold path is not a single inventoried hold entry".into(),
        ));
    }
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
        Ok(_) => {
            return Err(SensitiveError::Invalid(
                "released view hold still exists after removal".into(),
            ));
        }
    }
    let state: Option<String> = store
        .read_connection()
        .query_row(
            "SELECT state FROM local_sensitive_items
             WHERE incident_id=?1 AND relative_path=?2",
            rusqlite::params![marker.incident_id, relative],
            |row| row.get(0),
        )
        .optional()?;
    match state.as_deref() {
        None | Some("deleted") => Ok(()),
        Some("pending") => {
            store.connection().execute(
                "UPDATE local_sensitive_items SET state='deleted'
                 WHERE incident_id=?1 AND relative_path=?2 AND state='pending'",
                rusqlite::params![marker.incident_id, relative],
            )?;
            Ok(())
        }
        Some("residual") | Some(_) => Err(SensitiveError::Mismatch),
    }
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

#[derive(Debug, Clone, PartialEq, Eq)]
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

fn managed_copy_directory_digest(relative_path: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"aethyme-local-sensitive-managed-copy-directory-v1\0");
    hash.update(relative_path.as_bytes());
    format!("sha256:{:x}", hash.finalize())
}

/// Inventory generation and import trees with their exact directory shape as
/// well as file bytes. Their Git metadata contains empty directories, which
/// cannot be safely removed from a file-only inventory.
fn collect_managed_copy_tree(
    store: &CollaborationStore,
    relative: &Path,
    files: &mut Vec<FileItem>,
    unknown: &mut Vec<String>,
) -> Result<(), SensitiveError> {
    let path = store.project_dir().join(relative);
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
        let Some(relative_path) = directory_item_path(relative) else {
            unknown.push(relative.to_string_lossy().into_owned());
            return Ok(());
        };
        files.push(FileItem {
            sha256: managed_copy_directory_digest(&relative_path),
            relative_path,
            bytes: 0,
        });
        let mut entries = fs::read_dir(&path)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            collect_managed_copy_tree(store, &relative.join(entry.file_name()), files, unknown)?;
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

/// Prove that a known capture-control path is an empty, regular, unlocked
/// file whose identity stayed stable while it was opened and checked.
fn empty_unheld_control_lock(path: &Path) -> Result<bool, ()> {
    let before = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(()),
    };
    if before.file_type().is_symlink() || !before.file_type().is_file() || before.len() != 0 {
        return Err(());
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(not(unix))]
    {
        // This profile cannot prove that opening a child will not follow a
        // concurrently substituted link, so preserve the path as unknown.
        return Err(());
    }
    let file = options.open(path).map_err(|_| ())?;
    let opened = file.metadata().map_err(|_| ())?;
    if !opened.is_file() || opened.len() != 0 {
        return Err(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if opened.dev() != before.dev() || opened.ino() != before.ino() {
            return Err(());
        }
    }
    let Some(_unlocked) = crate::file_lock::ExclusiveFileLock::try_acquire(file).map_err(|_| ())?
    else {
        return Err(());
    };
    let after = fs::symlink_metadata(path).map_err(|_| ())?;
    if after.file_type().is_symlink() || !after.file_type().is_file() || after.len() != 0 {
        return Err(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if after.dev() != opened.dev() || after.ino() != opened.ino() {
            return Err(());
        }
    }
    Ok(true)
}

/// Recognize only empty, unlocked capture-control files. The archive and
/// response barriers exclude active capture entry points while this inventory
/// is prepared; control paths remain as unmanaged residuals and are never
/// removed by purge.
fn capture_lock_control_paths(project_dir: &Path) -> Result<Option<Vec<String>>, ()> {
    let root = project_dir.join("spool/capture");
    let before = match fs::symlink_metadata(&root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
    };
    if before.file_type().is_symlink() || !before.is_dir() {
        return Err(());
    }

    let mut entries = fs::read_dir(&root)
        .map_err(|_| ())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ())?;
    entries.sort_by_key(|entry| entry.file_name());

    let mut paths = vec!["spool/capture".to_owned()];
    for entry in entries {
        let name = entry.file_name().into_string().map_err(|_| ())?;
        let operation = name.strip_suffix(".lock").ok_or(())?;
        crate::collaboration_capture::OperationId::parse(operation).map_err(|_| ())?;
        let path = root.join(&name);
        if !empty_unheld_control_lock(&path)? {
            return Err(());
        }
        paths.push(format!("spool/capture/{name}"));
    }

    let after = fs::symlink_metadata(&root).map_err(|_| ())?;
    if after.file_type().is_symlink() || !after.is_dir() {
        return Err(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if after.dev() != before.dev() || after.ino() != before.ino() {
            return Err(());
        }
    }
    Ok(Some(paths))
}

/// The notice-ingress lock lives directly under spool. Validate the parent
/// before examining the fixed child so a symlinked spool root is never followed.
fn capture_notice_ingress_lock_control_path(project_dir: &Path) -> Result<bool, ()> {
    let spool = project_dir.join("spool");
    let before = match fs::symlink_metadata(&spool) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(()),
    };
    if before.file_type().is_symlink() || !before.is_dir() {
        return Err(());
    }
    let recognized =
        empty_unheld_control_lock(&project_dir.join(CAPTURE_NOTICE_INGRESS_LOCK_PATH))?;
    let after = fs::symlink_metadata(&spool).map_err(|_| ())?;
    if after.file_type().is_symlink() || !after.is_dir() {
        return Err(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if after.dev() != before.dev() || after.ino() != before.ino() {
            return Err(());
        }
    }
    Ok(recognized)
}

fn is_directory_item(relative_path: &str) -> bool {
    relative_path.ends_with('/')
}

fn is_managed_copy_tree_item(relative_path: &str) -> bool {
    relative_path.starts_with("generations/") || relative_path.starts_with("import-rehearsals/")
}

fn directory_item_path(relative_path: &Path) -> Option<String> {
    let mut value = relative_path.to_str()?.to_owned();
    if !value.ends_with('/') {
        value.push('/');
    }
    Some(value)
}

fn directory_item_digest(relative_path: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"aethyme-local-sensitive-view-directory-v1\0");
    hash.update(relative_path.as_bytes());
    format!("sha256:{:x}", hash.finalize())
}

/// Inventory every entry under the analysis-view root. Directories are stored
/// in the existing immutable item table with trailing-slash paths and a
/// path/type digest; regular file bytes retain their ordinary SHA-256. This is
/// intentionally limited to views, where restart-safe pin deletion requires
/// an exact original directory layout.
fn collect_view_tree(
    store: &CollaborationStore,
    relative: &Path,
    files: &mut Vec<FileItem>,
    unknown: &mut Vec<String>,
) -> Result<(), SensitiveError> {
    let path = store.project_dir().join(relative);
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
        let Some(relative_path) = directory_item_path(relative) else {
            unknown.push(relative.to_string_lossy().into_owned());
            return Ok(());
        };
        files.push(FileItem {
            sha256: directory_item_digest(&relative_path),
            relative_path,
            bytes: 0,
        });
        let mut entries = fs::read_dir(&path)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            collect_view_tree(store, &relative.join(entry.file_name()), files, unknown)?;
        }
        return Ok(());
    }
    if metadata.is_file() {
        let Some(relative_path) = relative.to_str().map(str::to_owned) else {
            unknown.push(relative.to_string_lossy().into_owned());
            return Ok(());
        };
        let (sha256, bytes) = if relative.file_name().and_then(|name| name.to_str())
            == Some("CURRENT")
        {
            let bytes = read_bounded_regular_file(&path, 25).map_err(|_| {
                SensitiveError::Invalid("analysis-view CURRENT is unreadable or oversized".into())
            })?;
            (
                format!("sha256:{:x}", Sha256::digest(&bytes)),
                bytes.len() as u64,
            )
        } else {
            hash_file(&path)?
        };
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
    hash.update(b"aethyme-local-sensitive-inventory-v3\0");
    for item in files {
        hash.update([if is_directory_item(&item.relative_path) {
            b'g'
        } else {
            b'f'
        }]);
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
    ] {
        collect_tree(store, path, &mut files, &mut unknown)?;
    }
    for path in [Path::new("generations"), Path::new("import-rehearsals")] {
        let root = path.to_string_lossy().into_owned();
        let prefix = format!("{root}/");
        if collect_managed_copy_tree(store, path, &mut files, &mut unknown).is_err() {
            // Keep no partial per-file inventory when a source tree could not
            // be inspected completely. The durable root residual blocks a
            // managed-cleanup completion claim while preserving visibility.
            files.retain(|item| !item.relative_path.starts_with(&prefix));
            unknown.push(prefix);
        }
    }
    if collect_view_tree(store, Path::new("views"), &mut files, &mut unknown).is_err() {
        // Do not create a partial per-file inventory for a view tree that could
        // not be read completely. The durable root residual prevents any later
        // group deletion while still making the refusal visible in status.
        files.retain(|item| !item.relative_path.starts_with("views/"));
        unknown.push("views/".into());
    }
    let capture_control_paths = match capture_lock_control_paths(store.project_dir()) {
        Ok(Some(paths)) => paths,
        Ok(None) => Vec::new(),
        Err(()) => {
            unknown.push("spool/capture".into());
            Vec::new()
        }
    };
    let capture_notice_ingress_lock_control =
        match capture_notice_ingress_lock_control_path(store.project_dir()) {
            Ok(present) => present,
            Err(()) => {
                unknown.push(CAPTURE_NOTICE_INGRESS_LOCK_PATH.into());
                false
            }
        };
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
                ) || (child == "capture" && !capture_control_paths.is_empty())
                    || (child == "capture-notice-ingress.lock"
                        && capture_notice_ingress_lock_control)
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
    let source_repositories = {
        let mut statement = store
            .read_connection()
            .prepare("SELECT DISTINCT repository FROM capture_operations ORDER BY repository")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
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
    for path in &capture_control_paths {
        tx.execute(
            "INSERT OR IGNORE INTO local_sensitive_residuals
             (incident_id,code,relative_path,permanence)
             VALUES (?1,'capture_operation_lock_control',?2,'unmanaged')",
            rusqlite::params![marker.incident_id, path],
        )?;
    }
    if capture_notice_ingress_lock_control {
        tx.execute(
            "INSERT OR IGNORE INTO local_sensitive_residuals
             (incident_id,code,relative_path,permanence)
             VALUES (?1,'capture_notice_ingress_lock_control',?2,'unmanaged')",
            rusqlite::params![marker.incident_id, CAPTURE_NOTICE_INGRESS_LOCK_PATH],
        )?;
    }
    let view_roots = unknown
        .iter()
        .filter_map(|path| {
            if path == "views" || path == "views/" {
                return Some("views/".to_string());
            }
            let relative = path.strip_prefix("views/")?;
            let view = relative.split('/').next().filter(|view| !view.is_empty())?;
            Some(format!("views/{view}/"))
        })
        .collect::<BTreeSet<_>>();
    for view_root in view_roots {
        tx.execute(
            "INSERT OR IGNORE INTO local_sensitive_residuals
             (incident_id,code,relative_path,permanence)
             VALUES (?1,'analysis_view_layout_unclassified',?2,'unclassified')",
            rusqlite::params![marker.incident_id, view_root],
        )?;
    }
    for source_path in &source_repositories {
        tx.execute(
            "INSERT OR IGNORE INTO local_sensitive_residuals
             (incident_id,code,relative_path,permanence)
             VALUES (?1,'captured_source_repository_copy',?2,'unmanaged')",
            rusqlite::params![marker.incident_id, source_path],
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

#[derive(Debug)]
struct ViewGroup {
    relative_root: String,
    items: Vec<FileItem>,
}

struct LockedViewPin {
    item: FileItem,
    _file: File,
}

#[derive(Debug)]
enum ViewBlock {
    Busy,
    Unclassified,
}

fn split_view_item(relative_path: &str) -> Option<(&str, &str)> {
    let suffix = relative_path.strip_prefix("views/")?;
    let (name, suffix) = suffix.split_once('/')?;
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    Some((name, suffix))
}

fn view_groups(items: &[FileItem]) -> (Vec<ViewGroup>, Vec<String>) {
    let mut grouped = BTreeMap::<String, Vec<FileItem>>::new();
    let mut unknown = Vec::new();
    for item in items {
        if item.relative_path == "views/" {
            continue;
        }
        if !item.relative_path.starts_with("views/") {
            continue;
        }
        let Some((name, _)) = split_view_item(&item.relative_path) else {
            unknown.push(item.relative_path.clone());
            continue;
        };
        grouped
            .entry(name.to_owned())
            .or_default()
            .push(item.clone());
    }
    let groups = grouped
        .into_iter()
        .map(|(name, mut items)| {
            items.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
            ViewGroup {
                relative_root: format!("views/{name}/"),
                items,
            }
        })
        .collect();
    (groups, unknown)
}

fn parse_generation_component(component: &str) -> Option<u64> {
    let number = component.strip_prefix("gen-")?.parse::<u64>().ok()?;
    (number > 0 && component == format!("gen-{number}")).then_some(number)
}

fn validate_view_inventory(group: &ViewGroup) -> Result<BTreeSet<u64>, ()> {
    let mut generations = BTreeSet::new();
    let mut has_root = false;
    let mut has_current = false;
    let mut has_holds = false;
    let mut has_hold_entries = false;
    let mut generation_rows = BTreeSet::new();
    let mut pin_rows = BTreeSet::new();
    let mut manifest_rows = BTreeSet::new();
    let mut facts_rows = BTreeSet::new();

    for item in &group.items {
        let Some(relative) = item.relative_path.strip_prefix(&group.relative_root) else {
            return Err(());
        };
        if relative.is_empty() {
            if !is_directory_item(&item.relative_path) {
                return Err(());
            }
            has_root = true;
            continue;
        }
        let trimmed = relative.strip_suffix('/').unwrap_or(relative);
        let components = trimmed.split('/').collect::<Vec<_>>();
        if components
            .iter()
            .any(|part| part.is_empty() || *part == "." || *part == "..")
        {
            return Err(());
        }
        match components.as_slice() {
            ["CURRENT"] if !is_directory_item(&item.relative_path) => has_current = true,
            [".lifecycle.lock"] | [".build.lock"] | ["RETIRED"]
                if !is_directory_item(&item.relative_path) => {}
            ["holds"] if is_directory_item(&item.relative_path) => has_holds = true,
            ["holds", _] if !is_directory_item(&item.relative_path) => {
                has_hold_entries = true;
            }
            [generation] if is_directory_item(&item.relative_path) => {
                let number = parse_generation_component(generation).ok_or(())?;
                generations.insert(number);
                generation_rows.insert(number);
            }
            [generation, ".pin"] if !is_directory_item(&item.relative_path) => {
                let number = parse_generation_component(generation).ok_or(())?;
                generations.insert(number);
                pin_rows.insert(number);
            }
            [generation, "manifest.json"] if !is_directory_item(&item.relative_path) => {
                let number = parse_generation_component(generation).ok_or(())?;
                generations.insert(number);
                manifest_rows.insert(number);
            }
            [generation, "facts"] if is_directory_item(&item.relative_path) => {
                let number = parse_generation_component(generation).ok_or(())?;
                generations.insert(number);
                facts_rows.insert(number);
            }
            [generation, "facts", ..] if components.len() > 2 => {
                let number = parse_generation_component(generation).ok_or(())?;
                generations.insert(number);
            }
            _ => return Err(()),
        }
    }
    if !has_root || !has_current || generations.is_empty() || has_hold_entries && !has_holds {
        return Err(());
    }
    if generations.iter().any(|number| {
        !generation_rows.contains(number)
            || !pin_rows.contains(number)
            || !manifest_rows.contains(number)
            || !facts_rows.contains(number)
    }) {
        return Err(());
    }
    Ok(generations)
}

fn view_item_states(
    store: &CollaborationStore,
    incident_id: &str,
    items: &[FileItem],
) -> Result<BTreeMap<String, String>, SensitiveError> {
    let mut states = BTreeMap::new();
    let mut statement = store
        .read_connection()
        .prepare("SELECT relative_path,state FROM local_sensitive_items WHERE incident_id=?1")?;
    let rows = statement.query_map([incident_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (path, state) = row?;
        if items.iter().any(|item| item.relative_path == path) {
            states.insert(path, state);
        }
    }
    if items
        .iter()
        .any(|item| !states.contains_key(&item.relative_path))
    {
        return Err(SensitiveError::Invalid(
            "view item is absent from the immutable inventory state".into(),
        ));
    }
    Ok(states)
}

fn has_prior_view_refusal(
    store: &CollaborationStore,
    incident_id: &str,
    relative_root: &str,
) -> Result<bool, SensitiveError> {
    Ok(store.read_connection().query_row(
        "SELECT EXISTS(
             SELECT 1 FROM local_sensitive_residuals
             WHERE incident_id=?1 AND permanence='unclassified'
               AND (relative_path=?2 OR substr(relative_path,1,length(?2))=?2)
         )",
        rusqlite::params![incident_id, relative_root],
        |row| row.get(0),
    )?)
}

fn record_view_block(
    store: &mut CollaborationStore,
    incident_id: &str,
    relative_root: &str,
    block: ViewBlock,
) -> Result<bool, SensitiveError> {
    match block {
        ViewBlock::Busy => add_residual(
            store,
            incident_id,
            "analysis_view_reader_or_hold_active",
            relative_root,
            "transient",
        )?,
        ViewBlock::Unclassified => add_residual(
            store,
            incident_id,
            "analysis_view_layout_unclassified",
            relative_root,
            "unclassified",
        )?,
    }
    Ok(false)
}

#[cfg(unix)]
fn read_bounded_regular_file(path: &Path, maximum: usize) -> Result<Vec<u8>, ()> {
    let before = fs::symlink_metadata(path).map_err(|_| ())?;
    if !before.file_type().is_file() || before.len() > maximum as u64 {
        return Err(());
    }
    let mut options = OpenOptions::new();
    options.read(true);
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| ())?;
    let opened = file.metadata().map_err(|_| ())?;
    if !opened.is_file() || opened.len() != before.len() || opened.len() > maximum as u64 {
        return Err(());
    }
    {
        use std::os::unix::fs::MetadataExt;
        if opened.dev() != before.dev() || opened.ino() != before.ino() {
            return Err(());
        }
    }
    let mut bytes = Vec::with_capacity(maximum);
    let mut limited = file.take(maximum as u64 + 1);
    limited.read_to_end(&mut bytes).map_err(|_| ())?;
    if bytes.len() > maximum || bytes.len() as u64 != opened.len() {
        return Err(());
    }
    let after = fs::symlink_metadata(path).map_err(|_| ())?;
    {
        use std::os::unix::fs::MetadataExt;
        if !after.file_type().is_file()
            || after.dev() != opened.dev()
            || after.ino() != opened.ino()
            || after.len() != opened.len()
        {
            return Err(());
        }
    }
    Ok(bytes)
}

#[cfg(not(unix))]
fn read_bounded_regular_file(_path: &Path, _maximum: usize) -> Result<Vec<u8>, ()> {
    Err(())
}

#[cfg(unix)]
fn strict_current_generation(path: &Path) -> Result<u64, ()> {
    let bytes = read_bounded_regular_file(path, 25)?;
    let text = bytes.strip_suffix(b"\n").ok_or(())?;
    let text = std::str::from_utf8(text).map_err(|_| ())?;
    let number = text
        .strip_prefix("gen-")
        .ok_or(())?
        .parse::<u64>()
        .map_err(|_| ())?;
    if number == 0 || text != format!("gen-{number}") {
        return Err(());
    }
    Ok(number)
}

#[cfg(not(unix))]
fn strict_current_generation(_path: &Path) -> Result<u64, ()> {
    Err(())
}

fn scan_view_tree(
    store: &CollaborationStore,
    group: &ViewGroup,
) -> Result<(BTreeMap<String, FileItem>, Vec<String>), SensitiveError> {
    let mut actual = Vec::new();
    let mut unknown = Vec::new();
    collect_view_tree(
        store,
        Path::new(group.relative_root.trim_end_matches('/')),
        &mut actual,
        &mut unknown,
    )?;
    Ok((
        actual
            .into_iter()
            .map(|item| (item.relative_path.clone(), item))
            .collect(),
        unknown,
    ))
}

fn actual_view_matches_inventory(
    group: &ViewGroup,
    states: &BTreeMap<String, String>,
    actual: &BTreeMap<String, FileItem>,
    unknown: &[String],
) -> bool {
    if !unknown.is_empty() {
        return false;
    }
    if actual
        .keys()
        .any(|path| !group.items.iter().any(|item| item.relative_path == *path))
    {
        return false;
    }
    for item in &group.items {
        let Some(state) = states.get(&item.relative_path) else {
            return false;
        };
        match (actual.get(&item.relative_path), state.as_str()) {
            (Some(found), "pending")
                if found.sha256 == item.sha256
                    && found.bytes == item.bytes
                    && is_directory_item(&found.relative_path)
                        == is_directory_item(&item.relative_path) => {}
            (None, "deleted") => {}
            _ => return false,
        }
    }
    true
}

fn hold_state(view_path: &Path) -> Result<Option<bool>, ()> {
    let holds = view_path.join("holds");
    match fs::symlink_metadata(&holds) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => return Err(()),
    }
    let entries = fs::read_dir(&holds).map_err(|_| ())?;
    let mut has_hold = false;
    let mut has_unknown = false;
    for entry in entries {
        let entry = entry.map_err(|_| ())?;
        let metadata = entry.file_type().map_err(|_| ())?;
        let filename = entry.file_name().to_string_lossy().into_owned();
        let entry_metadata = fs::symlink_metadata(entry.path()).map_err(|_| ())?;
        if !metadata.is_file()
            || metadata.is_symlink()
            || !entry_metadata.file_type().is_file()
            || entry_metadata.len() > 4096
        {
            has_unknown = true;
            continue;
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let bytes = match options.open(entry.path()).and_then(|file| {
            let opened = file.metadata()?;
            if !opened.is_file() || opened.len() != entry_metadata.len() {
                return Err(std::io::Error::other("view hold changed during open"));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if opened.dev() != entry_metadata.dev() || opened.ino() != entry_metadata.ino() {
                    return Err(std::io::Error::other("view hold changed during open"));
                }
            }
            let mut bytes = Vec::new();
            let mut limited = file.take(4097);
            limited.read_to_end(&mut bytes)?;
            if bytes.len() > 4096 {
                return Err(std::io::Error::other("view hold exceeds metadata limit"));
            }
            Ok(bytes)
        }) {
            Ok(bytes) => bytes,
            Err(_) => {
                has_unknown = true;
                continue;
            }
        };
        match serde_json::from_slice::<crate::analysis_view::ViewHold>(&bytes) {
            Ok(hold)
                if !hold.holder.is_empty()
                    && hold.holder.len() <= 200
                    && filename == hold_filename(&hold.holder) =>
            {
                // Even expired holds remain blockers until explicitly released.
                has_hold = true;
            }
            _ => has_unknown = true,
        }
    }
    if has_unknown {
        Err(())
    } else {
        Ok(has_hold.then_some(true))
    }
}

fn hold_filename(holder: &str) -> String {
    let digest = format!("{:x}", Sha256::digest(holder.as_bytes()));
    format!("{}.json", &digest[..32])
}

#[cfg(unix)]
fn open_and_lock_view_pin(path: &Path, item: &FileItem) -> Result<File, ViewBlock> {
    let before = fs::symlink_metadata(path).map_err(|_| ViewBlock::Unclassified)?;
    if !before.file_type().is_file() {
        return Err(ViewBlock::Unclassified);
    }
    let (before_digest, before_bytes) = hash_file(path).map_err(|_| ViewBlock::Unclassified)?;
    if before_digest != item.sha256 || before_bytes != item.bytes {
        return Err(ViewBlock::Unclassified);
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options.open(path).map_err(|_| ViewBlock::Unclassified)?;
    let opened = file.metadata().map_err(|_| ViewBlock::Unclassified)?;
    if !opened.is_file() || opened.len() != before.len() {
        return Err(ViewBlock::Unclassified);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if opened.dev() != before.dev() || opened.ino() != before.ino() {
            return Err(ViewBlock::Unclassified);
        }
    }
    match file.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Err(ViewBlock::Busy),
        Err(std::fs::TryLockError::Error(_)) => return Err(ViewBlock::Unclassified),
    }
    file.rewind().map_err(|_| ViewBlock::Unclassified)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| ViewBlock::Unclassified)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    if format!("sha256:{:x}", hash.finalize()) != item.sha256 {
        return Err(ViewBlock::Unclassified);
    }
    Ok(file)
}

#[cfg(not(unix))]
fn open_and_lock_view_pin(_path: &Path, _item: &FileItem) -> Result<File, ViewBlock> {
    Err(ViewBlock::Unclassified)
}

fn set_inventory_item_deleted(
    store: &mut CollaborationStore,
    incident_id: &str,
    item: &FileItem,
) -> Result<(), SensitiveError> {
    let changed = store.connection().execute(
        "UPDATE local_sensitive_items SET state='deleted'
         WHERE incident_id=?1 AND relative_path=?2 AND state='pending'",
        rusqlite::params![incident_id, item.relative_path],
    )?;
    if changed == 0 {
        let state: Option<String> = store
            .read_connection()
            .query_row(
                "SELECT state FROM local_sensitive_items
                 WHERE incident_id=?1 AND relative_path=?2",
                rusqlite::params![incident_id, item.relative_path],
                |row| row.get(0),
            )
            .optional()?;
        if state.as_deref() != Some("deleted") {
            return Err(SensitiveError::Invalid(
                "managed item state could not be durably advanced".into(),
            ));
        }
    }
    Ok(())
}

fn delete_view_file(
    store: &mut CollaborationStore,
    incident_id: &str,
    item: &FileItem,
) -> Result<(), SensitiveError> {
    let path = safe_managed_path(store.project_dir(), &item.relative_path)?;
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let state: Option<String> = store
                .read_connection()
                .query_row(
                    "SELECT state FROM local_sensitive_items
                     WHERE incident_id=?1 AND relative_path=?2",
                    rusqlite::params![incident_id, item.relative_path],
                    |row| row.get(0),
                )
                .optional()?;
            if state.as_deref() != Some("deleted") {
                return Err(SensitiveError::Invalid(
                    "pending view file disappeared before durable state".into(),
                ));
            }
        }
        Err(error) => return Err(error.into()),
        Ok(metadata) => {
            if !metadata.file_type().is_file() {
                return Err(SensitiveError::Invalid(
                    "view file changed type after preflight".into(),
                ));
            }
            let (digest, bytes) = hash_file(&path)?;
            if digest != item.sha256 || bytes != item.bytes {
                return Err(SensitiveError::Invalid(
                    "view file changed after preflight".into(),
                ));
            }
            fs::remove_file(&path)?;
            if let Some(parent) = path.parent() {
                sync_directory(parent)?;
            }
            set_inventory_item_deleted(store, incident_id, item)?;
        }
    }
    Ok(())
}

fn delete_view_directory(
    store: &mut CollaborationStore,
    incident_id: &str,
    item: &FileItem,
) -> Result<(), SensitiveError> {
    let path = safe_managed_path(store.project_dir(), &item.relative_path)?;
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let state: Option<String> = store
                .read_connection()
                .query_row(
                    "SELECT state FROM local_sensitive_items
                     WHERE incident_id=?1 AND relative_path=?2",
                    rusqlite::params![incident_id, item.relative_path],
                    |row| row.get(0),
                )
                .optional()?;
            if state.as_deref() != Some("deleted") {
                return Err(SensitiveError::Invalid(
                    "pending view directory disappeared before durable state".into(),
                ));
            }
        }
        Err(error) => return Err(error.into()),
        Ok(metadata) => {
            if !metadata.file_type().is_dir() {
                return Err(SensitiveError::Invalid(
                    "view directory changed type after preflight".into(),
                ));
            }
            if fs::read_dir(&path)?.next().transpose()?.is_some() {
                return Err(SensitiveError::Invalid(
                    "view directory is not empty after child deletion".into(),
                ));
            }
            fs::remove_dir(&path)?;
            if let Some(parent) = path.parent() {
                sync_directory(parent)?;
            }
            set_inventory_item_deleted(store, incident_id, item)?;
        }
    }
    Ok(())
}

fn delete_view_group(
    store: &mut CollaborationStore,
    incident_id: &str,
    group: &ViewGroup,
) -> Result<bool, SensitiveError> {
    if has_prior_view_refusal(store, incident_id, &group.relative_root)? {
        return Ok(false);
    }
    let states = view_item_states(store, incident_id, &group.items)?;
    let generations = match validate_view_inventory(group) {
        Ok(generations) => generations,
        Err(()) => {
            return record_view_block(
                store,
                incident_id,
                &group.relative_root,
                ViewBlock::Unclassified,
            );
        }
    };
    let (actual, unknown) = match scan_view_tree(store, group) {
        Ok(found) => found,
        Err(_) => {
            return record_view_block(
                store,
                incident_id,
                &group.relative_root,
                ViewBlock::Unclassified,
            );
        }
    };
    if !actual_view_matches_inventory(group, &states, &actual, &unknown) {
        return record_view_block(
            store,
            incident_id,
            &group.relative_root,
            ViewBlock::Unclassified,
        );
    }
    if group.items.iter().all(|item| {
        states
            .get(&item.relative_path)
            .is_some_and(|state| state == "deleted")
    }) {
        return Ok(true);
    }

    let view_path = store
        .project_dir()
        .join(group.relative_root.trim_end_matches('/'));
    match hold_state(&view_path) {
        Ok(Some(true)) => {
            return record_view_block(store, incident_id, &group.relative_root, ViewBlock::Busy);
        }
        Err(()) => {
            return record_view_block(
                store,
                incident_id,
                &group.relative_root,
                ViewBlock::Unclassified,
            );
        }
        Ok(None | Some(false)) => {}
    }

    let current_item = group
        .items
        .iter()
        .find(|item| item.relative_path == format!("{}CURRENT", group.relative_root))
        .ok_or_else(|| SensitiveError::Invalid("view inventory has no CURRENT row".into()))?;
    let current_present = actual.contains_key(&current_item.relative_path);
    if current_present {
        match strict_current_generation(&store.project_dir().join(&current_item.relative_path)) {
            Ok(current) if generations.contains(&current) => {}
            _ => {
                return record_view_block(
                    store,
                    incident_id,
                    &group.relative_root,
                    ViewBlock::Unclassified,
                );
            }
        }
    }
    let non_pin_files_deleted = group
        .items
        .iter()
        .filter(|item| {
            !is_directory_item(&item.relative_path) && !item.relative_path.ends_with("/.pin")
        })
        .all(|item| {
            states
                .get(&item.relative_path)
                .is_some_and(|state| state == "deleted")
                && !actual.contains_key(&item.relative_path)
        });
    let terminal_metadata_only = !current_present
        && states
            .get(&current_item.relative_path)
            .is_some_and(|state| state == "deleted")
        && non_pin_files_deleted;
    if !current_present && !terminal_metadata_only {
        return record_view_block(
            store,
            incident_id,
            &group.relative_root,
            ViewBlock::Unclassified,
        );
    }

    let mut pins = Vec::new();
    for generation in &generations {
        let pin_path = format!("{}gen-{generation}/.pin", group.relative_root);
        let Some(pin_item) = group
            .items
            .iter()
            .find(|item| item.relative_path == pin_path)
        else {
            return record_view_block(
                store,
                incident_id,
                &group.relative_root,
                ViewBlock::Unclassified,
            );
        };
        match actual.get(&pin_path) {
            Some(_)
                if states
                    .get(&pin_path)
                    .is_some_and(|state| state == "pending") =>
            {
                let path = store.project_dir().join(&pin_path);
                match open_and_lock_view_pin(&path, pin_item) {
                    Ok(file) => pins.push(LockedViewPin {
                        item: pin_item.clone(),
                        _file: file,
                    }),
                    Err(block) => {
                        return record_view_block(store, incident_id, &group.relative_root, block);
                    }
                }
            }
            None if states
                .get(&pin_path)
                .is_some_and(|state| state == "deleted")
                && terminal_metadata_only => {}
            _ => {
                return record_view_block(
                    store,
                    incident_id,
                    &group.relative_root,
                    ViewBlock::Unclassified,
                );
            }
        }
    }

    let deletion = (|| -> Result<(), SensitiveError> {
        // Content files go first, with CURRENT and every pin still in place.
        for item in &group.items {
            if !is_directory_item(&item.relative_path)
                && item
                    .relative_path
                    .starts_with(&format!("{}gen-", group.relative_root))
                && !item.relative_path.ends_with("/.pin")
            {
                delete_view_file(store, incident_id, item)?;
            }
        }
        // Other inventoried metadata files are removed only after generation
        // content, while CURRENT and pins still protect the old view shape.
        for item in &group.items {
            if is_directory_item(&item.relative_path)
                || item.relative_path == current_item.relative_path
                || item.relative_path.ends_with("/.pin")
                || item
                    .relative_path
                    .starts_with(&format!("{}gen-", group.relative_root))
            {
                continue;
            }
            delete_view_file(store, incident_id, item)?;
        }
        if current_present {
            delete_view_file(store, incident_id, current_item)?;
        }
        // All extant pin locks remain held through the last pin unlink.
        for pin in &pins {
            delete_view_file(store, incident_id, &pin.item)?;
        }
        let mut directories = group
            .items
            .iter()
            .filter(|item| is_directory_item(&item.relative_path))
            .collect::<Vec<_>>();
        directories.sort_by_key(|item| std::cmp::Reverse(item.relative_path.matches('/').count()));
        for item in directories {
            delete_view_directory(store, incident_id, item)?;
        }
        Ok(())
    })();
    match deletion {
        Ok(()) => Ok(true),
        Err(SensitiveError::Sqlite(error)) => Err(SensitiveError::Sqlite(error)),
        Err(SensitiveError::State(error)) => Err(SensitiveError::State(error)),
        Err(_) => record_view_block(
            store,
            incident_id,
            &group.relative_root,
            ViewBlock::Unclassified,
        ),
    }
}

fn delete_views_root(
    store: &mut CollaborationStore,
    incident_id: &str,
    inventory: &[FileItem],
    states: &BTreeMap<String, String>,
    groups_complete: bool,
) -> Result<bool, SensitiveError> {
    let Some(item) = inventory.iter().find(|item| item.relative_path == "views/") else {
        let child_views = inventory
            .iter()
            .any(|candidate| candidate.relative_path.starts_with("views/"));
        let root = store.project_dir().join("views");
        if child_views || fs::symlink_metadata(root).is_ok() {
            add_residual(
                store,
                incident_id,
                "analysis_view_inventory_incomplete",
                "views/",
                "unclassified",
            )?;
            return Ok(false);
        }
        return Ok(true);
    };
    let child_views = inventory
        .iter()
        .filter(|candidate| {
            candidate.relative_path.starts_with("views/") && candidate.relative_path != "views/"
        })
        .collect::<Vec<_>>();
    if child_views.is_empty() {
        return record_view_block(store, incident_id, "views/", ViewBlock::Unclassified);
    }
    if !groups_complete {
        return Ok(false);
    }
    let Some(state) = states.get("views/") else {
        return Err(SensitiveError::Invalid(
            "views root is absent from the immutable state map".into(),
        ));
    };
    let path = safe_managed_path(store.project_dir(), "views/")?;
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && state == "deleted" => {
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            record_view_block(store, incident_id, "views/", ViewBlock::Unclassified)
        }
        Err(error) => Err(error.into()),
        Ok(metadata) if metadata.file_type().is_dir() && state == "pending" => {
            if fs::read_dir(&path)?.next().transpose()?.is_some() {
                return record_view_block(store, incident_id, "views/", ViewBlock::Unclassified);
            }
            fs::remove_dir(&path)?;
            sync_directory(store.project_dir())?;
            set_inventory_item_deleted(store, incident_id, item)?;
            Ok(true)
        }
        Ok(_) => record_view_block(store, incident_id, "views/", ViewBlock::Unclassified),
    }
}

fn delete_inventory_item(
    store: &mut CollaborationStore,
    incident_id: &str,
    item: &FileItem,
) -> Result<bool, SensitiveError> {
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

/// Remove only empty generation/import directories present in the immutable
/// typed inventory. An untracked entry, changed type, symlink, or inspection/
/// sync error records an unclassified residual and prevents a completion
/// claim.
fn delete_managed_copy_directories(
    store: &mut CollaborationStore,
    incident_id: &str,
    inventory: &[FileItem],
) -> Result<bool, SensitiveError> {
    let mut complete = true;
    for root in ["generations", "import-rehearsals"] {
        let prefix = format!("{root}/");
        let items = inventory
            .iter()
            .filter(|item| item.relative_path.starts_with(&prefix))
            .collect::<Vec<_>>();
        if items.is_empty() {
            let root_path = store.project_dir().join(root);
            match fs::symlink_metadata(&root_path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                _ => {
                    add_residual(
                        store,
                        incident_id,
                        "unclassified_managed_path",
                        root,
                        "unclassified",
                    )?;
                    complete = false;
                }
            }
            continue;
        }

        let mut known = BTreeMap::<String, &FileItem>::new();
        let mut directories = BTreeSet::new();
        let mut states = BTreeMap::<String, String>::new();
        let mut blocked_path = None;
        for item in &items {
            if !item.relative_path.starts_with(&prefix)
                || known.insert(item.relative_path.clone(), item).is_some()
            {
                blocked_path = Some(item.relative_path.clone());
                break;
            }
            let is_directory = is_directory_item(&item.relative_path);
            if is_directory {
                if item.bytes != 0
                    || item.sha256 != managed_copy_directory_digest(&item.relative_path)
                {
                    blocked_path = Some(item.relative_path.clone());
                    break;
                }
                directories.insert(item.relative_path.clone());
            }
            let state: Option<String> = store
                .read_connection()
                .query_row(
                    "SELECT state FROM local_sensitive_items
                     WHERE incident_id=?1 AND relative_path=?2",
                    rusqlite::params![incident_id, item.relative_path],
                    |row| row.get(0),
                )
                .optional()?;
            match (is_directory, state.as_deref()) {
                (true, Some("pending")) => {
                    states.insert(item.relative_path.clone(), "pending".into());
                }
                (true, Some("deleted")) => {
                    states.insert(item.relative_path.clone(), "deleted".into());
                }
                (false, Some("deleted")) => {
                    states.insert(item.relative_path.clone(), "deleted".into());
                }
                (_, None) => {
                    add_residual(
                        store,
                        incident_id,
                        "unclassified_managed_path",
                        &item.relative_path,
                        "unclassified",
                    )?;
                    blocked_path = Some(item.relative_path.clone());
                    break;
                }
                _ => {
                    blocked_path = Some(item.relative_path.clone());
                    break;
                }
            }
        }
        if blocked_path.is_none() && !directories.contains(&format!("{root}/")) {
            blocked_path = Some(format!("{root}/"));
        }

        // Every inventoried path must have its inventoried parent directory.
        if blocked_path.is_none() {
            for item in &items {
                if item.relative_path == format!("{root}/") {
                    continue;
                }
                let Some(parent) = Path::new(&item.relative_path).parent() else {
                    blocked_path = Some(item.relative_path.clone());
                    break;
                };
                let Some(parent) = directory_item_path(parent) else {
                    blocked_path = Some(item.relative_path.clone());
                    break;
                };
                if !directories.contains(&parent) {
                    blocked_path = Some(item.relative_path.clone());
                    break;
                }
            }
        }

        // Files must already have a durable deletion state and must be absent.
        if blocked_path.is_none() {
            for item in &items {
                if is_directory_item(&item.relative_path) {
                    continue;
                }
                let path = match safe_managed_path(store.project_dir(), &item.relative_path) {
                    Ok(path) => path,
                    Err(_) => {
                        blocked_path = Some(item.relative_path.clone());
                        break;
                    }
                };
                match fs::symlink_metadata(&path) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    _ => {
                        blocked_path = Some(item.relative_path.clone());
                        break;
                    }
                }
            }
        }

        // Preflight every inventoried directory before unlinking any. Every
        // present child must have an exact file or directory inventory row.
        if blocked_path.is_none() {
            for item in &items {
                if !is_directory_item(&item.relative_path) {
                    continue;
                }
                let path = match safe_managed_path(store.project_dir(), &item.relative_path) {
                    Ok(path) => path,
                    Err(_) => {
                        blocked_path = Some(item.relative_path.clone());
                        break;
                    }
                };
                let metadata = match fs::symlink_metadata(&path) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(_) => {
                        blocked_path = Some(item.relative_path.clone());
                        break;
                    }
                    Ok(metadata) => metadata,
                };
                if !metadata.file_type().is_dir() {
                    blocked_path = Some(item.relative_path.clone());
                    break;
                }
                if states
                    .get(&item.relative_path)
                    .is_some_and(|state| state == "deleted")
                {
                    blocked_path = Some(item.relative_path.clone());
                    break;
                }
                let entries = match fs::read_dir(&path) {
                    Ok(entries) => entries,
                    Err(_) => {
                        blocked_path = Some(item.relative_path.clone());
                        break;
                    }
                };
                for entry in entries {
                    let entry = match entry {
                        Ok(entry) => entry,
                        Err(_) => {
                            blocked_path = Some(item.relative_path.clone());
                            break;
                        }
                    };
                    let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                        blocked_path = Some(item.relative_path.clone());
                        break;
                    };
                    let child_path = path.join(&name);
                    let child_metadata = match fs::symlink_metadata(&child_path) {
                        Ok(metadata) => metadata,
                        Err(_) => {
                            blocked_path = Some(item.relative_path.clone());
                            break;
                        }
                    };
                    let child_relative = if child_metadata.file_type().is_dir() {
                        format!("{}{name}/", item.relative_path)
                    } else if child_metadata.file_type().is_file() {
                        format!("{}{name}", item.relative_path)
                    } else {
                        blocked_path = Some(item.relative_path.clone());
                        break;
                    };
                    if !known.contains_key(&child_relative) {
                        blocked_path = Some(child_relative);
                        break;
                    }
                }
                if blocked_path.is_some() {
                    break;
                }
            }
        }

        if let Some(relative) = blocked_path {
            add_residual(
                store,
                incident_id,
                "unclassified_managed_path",
                &relative,
                "unclassified",
            )?;
            complete = false;
            continue;
        }

        let mut ordered = directories.into_iter().collect::<Vec<_>>();
        ordered.sort_by(|left, right| {
            Path::new(right)
                .components()
                .count()
                .cmp(&Path::new(left).components().count())
                .then_with(|| right.cmp(left))
        });
        for relative in ordered {
            let Some(item) = known.get(&relative).copied() else {
                complete = false;
                break;
            };
            let path = match safe_managed_path(store.project_dir(), &relative) {
                Ok(path) => path,
                Err(_) => {
                    add_residual(
                        store,
                        incident_id,
                        "unclassified_managed_path",
                        &relative,
                        "unclassified",
                    )?;
                    complete = false;
                    break;
                }
            };
            match fs::symlink_metadata(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    set_inventory_item_deleted(store, incident_id, item)?;
                }
                Err(_) => {
                    add_residual(
                        store,
                        incident_id,
                        "unclassified_managed_path",
                        &relative,
                        "unclassified",
                    )?;
                    complete = false;
                    break;
                }
                Ok(metadata) if metadata.file_type().is_dir() => {
                    let mut entries = match fs::read_dir(&path) {
                        Ok(entries) => entries,
                        Err(_) => {
                            add_residual(
                                store,
                                incident_id,
                                "unclassified_managed_path",
                                &relative,
                                "unclassified",
                            )?;
                            complete = false;
                            break;
                        }
                    };
                    match entries.next() {
                        None => {}
                        Some(Ok(entry)) => {
                            let child = entry.file_name().to_string_lossy().into_owned();
                            add_residual(
                                store,
                                incident_id,
                                "unclassified_managed_path",
                                &format!("{}{child}", relative),
                                "unclassified",
                            )?;
                            complete = false;
                            break;
                        }
                        Some(Err(_)) => {
                            add_residual(
                                store,
                                incident_id,
                                "unclassified_managed_path",
                                &relative,
                                "unclassified",
                            )?;
                            complete = false;
                            break;
                        }
                    }
                    if fs::remove_dir(&path).is_err() {
                        add_residual(
                            store,
                            incident_id,
                            "unclassified_managed_path",
                            &relative,
                            "unclassified",
                        )?;
                        complete = false;
                        break;
                    }
                    let Some(parent) = path.parent() else {
                        add_residual(
                            store,
                            incident_id,
                            "unclassified_managed_path",
                            &relative,
                            "unclassified",
                        )?;
                        complete = false;
                        break;
                    };
                    if sync_directory(parent).is_err() {
                        add_residual(
                            store,
                            incident_id,
                            "unclassified_managed_path",
                            &relative,
                            "unclassified",
                        )?;
                        complete = false;
                        break;
                    }
                    set_inventory_item_deleted(store, incident_id, item)?;
                }
                Ok(_) => {
                    add_residual(
                        store,
                        incident_id,
                        "unclassified_managed_path",
                        &relative,
                        "unclassified",
                    )?;
                    complete = false;
                    break;
                }
            }
        }
    }
    Ok(complete)
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

        // Exercise the crash boundary after managed table rows are deleted
        // but before the durable cleanup receipt can be published.
        #[cfg(test)]
        if INTERRUPT_AFTER_MANAGED_TABLE_DELETE.with(|interrupt| interrupt.replace(false)) {
            return Err(SensitiveError::Invalid(
                "injected interruption after managed table deletion".into(),
            ));
        }

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
    let (groups, malformed_view_paths) = view_groups(&inventory);
    let has_views_root = inventory.iter().any(|item| item.relative_path == "views/");
    let has_view_items = inventory
        .iter()
        .any(|item| item.relative_path.starts_with("views/"));
    let mut groups_complete = true;
    if !malformed_view_paths.is_empty() || (has_view_items && !has_views_root) {
        add_residual(
            store,
            incident_id,
            "analysis_view_inventory_incomplete",
            "views/",
            "unclassified",
        )?;
        groups_complete = false;
    }
    for item in &inventory {
        if item.relative_path == "views/" || item.relative_path.starts_with("views/") {
            continue;
        }
        if is_managed_copy_tree_item(&item.relative_path) && is_directory_item(&item.relative_path)
        {
            continue;
        }
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
    if !delete_managed_copy_directories(store, incident_id, &inventory)? {
        completed = false;
    }
    if groups_complete {
        for group in &groups {
            if !delete_view_group(store, incident_id, group)? {
                completed = false;
                groups_complete = false;
            }
        }
    } else if !groups.is_empty() {
        completed = false;
    }
    let states = view_item_states(store, incident_id, &inventory)?;
    if !delete_views_root(store, incident_id, &inventory, &states, groups_complete)? {
        completed = false;
    }
    let unclassified: bool = store.read_connection().query_row(
        "SELECT EXISTS(SELECT 1 FROM local_sensitive_residuals WHERE incident_id=?1 AND permanence='unclassified')",
        [incident_id],
        |row| row.get(0),
    )?;
    // A view group is deleted only while every inventoried generation pin is
    // held exclusively. Legacy readers without that pin contract and already
    // materialized process copies remain outside this cooperative boundary.
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
        // Older readers and already-materialized process copies cannot be
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

    // Another test thread can fork while a lock descriptor is open; its Git
    // child briefly inherits that descriptor until exec. Retry only transient
    // contention, leaving every semantic error and production refusal intact.
    fn purge_after_transient_contention(
        store: &mut CollaborationStore,
        incident_id: &str,
        confirmation: &str,
    ) -> Result<PurgeOutcome, SensitiveError> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match super::purge(store, incident_id, confirmation) {
                Err(SensitiveError::Busy) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                result => return result,
            }
        }
    }

    struct ResetDatabaseCleanupInterruption;

    impl Drop for ResetDatabaseCleanupInterruption {
        fn drop(&mut self) {
            INTERRUPT_AFTER_MANAGED_TABLE_DELETE.with(|interrupt| interrupt.set(false));
        }
    }

    fn complete_fixture_wave(fixture: &mut crate::local_generation::tests::Fixture) {
        let binding = crate::local_acceptance::load(&fixture.v.f.store, &fixture.binding).unwrap();
        let intent_id = RecordId::parse(&binding.spec().intent).unwrap();
        let intent = crate::local_source_transport::load(&fixture.v.f.store, &intent_id).unwrap();
        let wave_id = RecordId::parse(&intent.spec.wave).unwrap();
        let grant = crate::local_acceptance::qualify(
            &mut fixture.v.f.store,
            &fixture.v.owner,
            &mut fixture.broker,
            &fixture.carrier,
            &fixture.binding,
            &fixture.v.execution,
            &fixture.v.artifact,
            &fixture.v.permit,
            &fixture.evidence,
        )
        .unwrap();
        crate::local_controller::complete(
            &mut fixture.v.f.store,
            &fixture.v.owner,
            &mut fixture.broker,
            &fixture.carrier,
            &wave_id,
            &grant,
        )
        .unwrap();
    }

    fn insert_purging_incident(store: &mut CollaborationStore, incident_id: &str) {
        let project_key = store.project().as_str().to_owned();
        let created_ms = now_ms();
        store
            .connection()
            .execute(
                "INSERT INTO local_sensitive_incidents
                 (id,project_key,issuer_label,operator_uid,authority_epoch,scope_kind,scope_id,
                  reason_code,request_digest,state,created_ms,updated_ms)
                 VALUES (?1,?2,'test-operator',0,'test-epoch','project',?2,
                         'operator_requested_purge',?3,'purging',?4,?4)",
                rusqlite::params![incident_id, project_key, "0".repeat(64), created_ms],
            )
            .unwrap();
    }

    #[test]
    fn interrupted_database_cleanup_does_not_publish_early_completion_receipt() {
        use crate::collaboration_archive::ObjectDigest;
        use crate::collaboration_capture as capture;

        let mut fixture = crate::local_generation::tests::Fixture::new();
        complete_fixture_wave(&mut fixture);
        let source_repository = fixture.v.f._repo.path().to_path_buf();
        let capture_request = capture::CaptureRequest {
            operation_id: capture::OperationId::mint().unwrap(),
            repository: source_repository,
            base: fixture.v.f.a.base.commit.clone(),
            result: fixture.v.f.a.result.commit.clone(),
            policy: capture::CapturePolicy::Required,
            retention: capture::RetentionBoundary::UntilReleased,
        };
        assert!(matches!(
            capture::capture(&mut fixture.v.f.store, &capture_request).unwrap(),
            capture::CaptureOutcome::Acknowledged(_)
        ));

        let store = &mut fixture.v.f.store;
        let frozen = crate::local_authority::freeze(
            store,
            fixture.v.owner.authority_epoch(),
            "T56 managed database cleanup interruption control",
        )
        .unwrap();
        let incident_id = format!(
            "sha256:{}",
            ObjectDigest::of(b"t56-managed-database-cleanup-interruption").hex()
        );
        let quarantined = quarantine(
            store,
            &incident_id,
            "local-test-operator",
            frozen.epoch.as_str(),
            ReasonCode::SuspectedTestSecretExposure,
        )
        .unwrap();

        let _reset = ResetDatabaseCleanupInterruption;
        INTERRUPT_AFTER_MANAGED_TABLE_DELETE.with(|interrupt| {
            assert!(
                !interrupt.replace(true),
                "stale cleanup interruption control"
            );
        });
        let interrupted =
            purge_after_transient_contention(store, &incident_id, &quarantined.request_digest);
        assert!(matches!(
            interrupted,
            Err(SensitiveError::Invalid(ref message))
                if message == "injected interruption after managed table deletion"
        ));

        let interrupted_status = status(store, &incident_id).unwrap();
        assert_eq!(interrupted_status.database_cleanup_completed_ms, None);
        assert!(!interrupted_status.managed_cleanup_complete);
        assert!(interrupted_status.residuals.iter().any(|residual| {
            residual.code == "capture_notice_ingress_lock_control"
                && residual.path == CAPTURE_NOTICE_INGRESS_LOCK_PATH
                && residual.permanence == "unmanaged"
        }));
        let object_items = interrupted_status
            .items
            .iter()
            .filter(|item| item.path.starts_with("objects/"))
            .collect::<Vec<_>>();
        assert!(!object_items.is_empty());
        assert!(
            object_items.iter().all(|item| item.state == "deleted"),
            "archive object files must be removed before this database interruption"
        );
        assert!(!interrupted_status.database_tables.is_empty());
        assert!(
            interrupted_status
                .database_tables
                .iter()
                .all(|table| table.state == "pending"),
            "database cleanup inventory must remain pending until its receipt"
        );
        let remaining_waves: i64 = store
            .read_connection()
            .query_row("SELECT count(*) FROM local_waves", [], |row| row.get(0))
            .unwrap();
        assert_eq!(remaining_waves, 0);

        let retried =
            purge_after_transient_contention(store, &incident_id, &quarantined.request_digest)
                .unwrap();
        assert!(retried.status.database_cleanup_completed_ms.is_some());
        assert!(retried.status.managed_cleanup_complete);
    }

    #[cfg(unix)]
    #[test]
    fn capture_notice_ingress_lock_control_accepts_only_empty_unheld_file() {
        let root = tempfile::tempdir().unwrap();
        let spool = root.path().join("spool");
        std::fs::create_dir(&spool).unwrap();
        let lock = spool.join("capture-notice-ingress.lock");
        std::fs::write(&lock, b"").unwrap();

        assert!(capture_notice_ingress_lock_control_path(root.path()).unwrap());
        assert_eq!(std::fs::read(&lock).unwrap(), b"");
    }

    #[test]
    fn capture_notice_ingress_lock_control_refuses_nonempty_file() {
        let root = tempfile::tempdir().unwrap();
        let spool = root.path().join("spool");
        std::fs::create_dir(&spool).unwrap();
        let lock = spool.join("capture-notice-ingress.lock");
        std::fs::write(&lock, b"unclassified bytes").unwrap();

        assert!(capture_notice_ingress_lock_control_path(root.path()).is_err());
        assert_eq!(std::fs::read(&lock).unwrap(), b"unclassified bytes");
    }

    #[cfg(unix)]
    #[test]
    fn capture_notice_ingress_lock_control_refuses_held_file_then_retries_after_release() {
        let root = tempfile::tempdir().unwrap();
        let spool = root.path().join("spool");
        std::fs::create_dir(&spool).unwrap();
        let lock = spool.join("capture-notice-ingress.lock");
        std::fs::write(&lock, b"").unwrap();

        let held = crate::file_lock::ExclusiveFileLock::acquire(
            crate::file_lock::open_lock_file(&lock).unwrap(),
        )
        .unwrap();
        assert!(capture_notice_ingress_lock_control_path(root.path()).is_err());

        drop(held);
        assert!(capture_notice_ingress_lock_control_path(root.path()).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn capture_notice_ingress_lock_control_refuses_symlink_without_following() {
        let root = tempfile::tempdir().unwrap();
        let spool = root.path().join("spool");
        std::fs::create_dir(&spool).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let sentinel = outside.path().join("sentinel");
        std::fs::write(&sentinel, b"outside managed tree").unwrap();
        std::os::unix::fs::symlink(&sentinel, spool.join("capture-notice-ingress.lock")).unwrap();

        assert!(capture_notice_ingress_lock_control_path(root.path()).is_err());
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"outside managed tree");
    }

    #[test]
    fn invalid_capture_notice_ingress_lock_blocks_database_cleanup() {
        use crate::collaboration_archive::ObjectDigest;
        use crate::collaboration_capture as capture;

        let mut fixture = crate::local_generation::tests::Fixture::new();
        complete_fixture_wave(&mut fixture);
        let source_repository = fixture.v.f._repo.path().to_path_buf();
        let capture_request = capture::CaptureRequest {
            operation_id: capture::OperationId::mint().unwrap(),
            repository: source_repository,
            base: fixture.v.f.a.base.commit.clone(),
            result: fixture.v.f.a.result.commit.clone(),
            policy: capture::CapturePolicy::Required,
            retention: capture::RetentionBoundary::UntilReleased,
        };
        assert!(matches!(
            capture::capture(&mut fixture.v.f.store, &capture_request).unwrap(),
            capture::CaptureOutcome::Acknowledged(_)
        ));

        let store = &mut fixture.v.f.store;
        let frozen = crate::local_authority::freeze(
            store,
            fixture.v.owner.authority_epoch(),
            "T56 invalid capture notice ingress lock control",
        )
        .unwrap();
        let incident_id = format!(
            "sha256:{}",
            ObjectDigest::of(b"invalid-capture-notice-ingress-lock-control").hex()
        );
        let quarantined = quarantine(
            store,
            &incident_id,
            "local-test-operator",
            frozen.epoch.as_str(),
            ReasonCode::SuspectedTestSecretExposure,
        )
        .unwrap();

        let lock = store.project_dir().join(CAPTURE_NOTICE_INGRESS_LOCK_PATH);
        std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
        std::fs::write(&lock, b"unclassified bytes").unwrap();

        let refused =
            purge_after_transient_contention(store, &incident_id, &quarantined.request_digest)
                .unwrap();
        assert!(!refused.completed);
        assert_eq!(refused.status.database_cleanup_completed_ms, None);
        assert!(!refused.status.managed_cleanup_complete);
        assert!(!refused.status.database_tables.is_empty());
        assert!(
            refused
                .status
                .database_tables
                .iter()
                .all(|table| table.state == "pending")
        );
        assert!(refused.status.residuals.iter().any(|residual| {
            residual.code == "unclassified_managed_path"
                && residual.path == CAPTURE_NOTICE_INGRESS_LOCK_PATH
                && residual.resolved_ms.is_none()
        }));
        assert_eq!(std::fs::read(&lock).unwrap(), b"unclassified bytes");
    }

    #[test]
    fn capture_lock_control_inventory_accepts_only_empty_root_and_closed_zero_byte_locks() {
        let root = tempfile::tempdir().unwrap();
        let capture_root = root.path().join("spool/capture");
        std::fs::create_dir_all(&capture_root).unwrap();
        assert_eq!(
            capture_lock_control_paths(root.path()).unwrap(),
            Some(vec!["spool/capture".into()])
        );

        let operation = "cap-0123456789abcdef0123456789abcdef";
        let lock = capture_root.join(format!("{operation}.lock"));
        std::fs::write(&lock, b"").unwrap();
        assert_eq!(
            capture_lock_control_paths(root.path()).unwrap(),
            Some(vec![
                "spool/capture".into(),
                format!("spool/capture/{operation}.lock")
            ])
        );
    }

    #[test]
    fn capture_lock_control_inventory_rejects_nonempty_lock_files() {
        let root = tempfile::tempdir().unwrap();
        let capture_root = root.path().join("spool/capture");
        std::fs::create_dir_all(&capture_root).unwrap();
        let lock = capture_root.join("cap-0123456789abcdef0123456789abcdef.lock");
        std::fs::write(&lock, b"unclassified bytes").unwrap();

        assert!(capture_lock_control_paths(root.path()).is_err());
        assert_eq!(std::fs::read(&lock).unwrap(), b"unclassified bytes");
    }

    #[test]
    fn capture_lock_control_inventory_refuses_held_lock_then_retries_after_release() {
        let root = tempfile::tempdir().unwrap();
        let capture_root = root.path().join("spool/capture");
        std::fs::create_dir_all(&capture_root).unwrap();
        let lock = capture_root.join("cap-0123456789abcdef0123456789abcdef.lock");
        std::fs::write(&lock, b"").unwrap();

        let held = crate::file_lock::ExclusiveFileLock::acquire(
            crate::file_lock::open_lock_file(&lock).unwrap(),
        )
        .unwrap();
        assert!(capture_lock_control_paths(root.path()).is_err());

        drop(held);
        assert!(capture_lock_control_paths(root.path()).is_ok());
    }

    #[test]
    fn capture_lock_control_inventory_rejects_unknown_children() {
        let root = tempfile::tempdir().unwrap();
        let capture_root = root.path().join("spool/capture");
        std::fs::create_dir_all(&capture_root).unwrap();
        std::fs::write(capture_root.join("notes"), b"").unwrap();

        assert!(capture_lock_control_paths(root.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn capture_lock_control_inventory_rejects_symlinked_lock_children() {
        let root = tempfile::tempdir().unwrap();
        let capture_root = root.path().join("spool/capture");
        std::fs::create_dir_all(&capture_root).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let sentinel = outside.path().join("sentinel");
        std::fs::write(&sentinel, b"outside managed tree").unwrap();
        std::os::unix::fs::symlink(
            &sentinel,
            capture_root.join("cap-0123456789abcdef0123456789abcdef.lock"),
        )
        .unwrap();

        assert!(capture_lock_control_paths(root.path()).is_err());
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"outside managed tree");
    }

    fn create_managed_copy_file(store: &CollaborationStore, relative_path: &str) {
        let path = store.project_dir().join(relative_path);
        let parent = path.parent().unwrap();
        std::fs::create_dir_all(parent).unwrap();
        std::fs::write(&path, b"synthetic managed fixture bytes").unwrap();
    }

    fn register_deleted_managed_tree(
        store: &mut CollaborationStore,
        incident_id: &str,
    ) -> Vec<FileItem> {
        let mut items = Vec::new();
        let mut unknown = Vec::new();
        for root in [Path::new("generations"), Path::new("import-rehearsals")] {
            collect_managed_copy_tree(store, root, &mut items, &mut unknown).unwrap();
        }
        assert!(unknown.is_empty(), "unexpected fixture paths: {unknown:?}");
        items.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        for item in &items {
            let state = if is_directory_item(&item.relative_path) {
                "pending"
            } else {
                std::fs::remove_file(store.project_dir().join(&item.relative_path)).unwrap();
                "deleted"
            };
            store
                .connection()
                .execute(
                    "INSERT INTO local_sensitive_items
                     (incident_id,relative_path,sha256,bytes,state)
                     VALUES (?1,?2,?3,?4,?5)",
                    rusqlite::params![
                        incident_id,
                        item.relative_path,
                        item.sha256,
                        item.bytes,
                        state
                    ],
                )
                .unwrap();
        }
        items
    }

    #[test]
    fn managed_copy_cleanup_recovers_partial_removal_and_cleans_generation_and_import_parents() {
        let mut fixture = crate::local_generation::tests::Fixture::new();
        let store = &mut fixture.v.f.store;
        let incident_id = format!("sha256:{}", "a".repeat(64));
        insert_purging_incident(store, &incident_id);
        create_managed_copy_file(store, "generations/t56-generation/source/src/file.txt");
        create_managed_copy_file(store, "import-rehearsals/t56-import/source/src/file.txt");
        std::fs::create_dir_all(
            store
                .project_dir()
                .join("generations/t56-generation/source/.git/objects/info"),
        )
        .unwrap();
        let inventory = register_deleted_managed_tree(store, &incident_id);
        // Model a restart after the deepest empty child was already removed.
        std::fs::remove_dir(
            store
                .project_dir()
                .join("generations/t56-generation/source/src"),
        )
        .unwrap();

        assert!(delete_managed_copy_directories(store, &incident_id, &inventory).unwrap());
        assert!(!store.project_dir().join("generations").exists());
        assert!(!store.project_dir().join("import-rehearsals").exists());
        let pending: i64 = store
            .read_connection()
            .query_row(
                "SELECT count(*) FROM local_sensitive_items
                 WHERE incident_id=?1 AND state='pending'",
                [&incident_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pending, 0);
    }

    #[test]
    fn managed_copy_cleanup_removes_inventory_of_empty_roots() {
        let mut fixture = crate::local_generation::tests::Fixture::new();
        let store = &mut fixture.v.f.store;
        let incident_id = format!("sha256:{}", "e".repeat(64));
        insert_purging_incident(store, &incident_id);
        std::fs::create_dir_all(store.project_dir().join("generations")).unwrap();
        std::fs::create_dir_all(store.project_dir().join("import-rehearsals")).unwrap();
        let inventory = register_deleted_managed_tree(store, &incident_id);

        assert_eq!(inventory.len(), 2);
        assert!(
            inventory
                .iter()
                .all(|item| is_directory_item(&item.relative_path))
        );
        assert!(delete_managed_copy_directories(store, &incident_id, &inventory).unwrap());
        assert!(!store.project_dir().join("generations").exists());
        assert!(!store.project_dir().join("import-rehearsals").exists());
        let deleted: i64 = store
            .read_connection()
            .query_row(
                "SELECT count(*) FROM local_sensitive_items
                 WHERE incident_id=?1 AND state='deleted'",
                [&incident_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(deleted, 2);
    }

    #[test]
    fn managed_copy_cleanup_preserves_and_records_an_untracked_entry_then_retries() {
        let mut fixture = crate::local_generation::tests::Fixture::new();
        let store = &mut fixture.v.f.store;
        let incident_id = format!("sha256:{}", "b".repeat(64));
        insert_purging_incident(store, &incident_id);
        create_managed_copy_file(store, "generations/t56-generation/source/src/file.txt");
        let inventory = register_deleted_managed_tree(store, &incident_id);
        let untracked = store
            .project_dir()
            .join("generations/t56-generation/source/untracked.txt");
        std::fs::write(&untracked, b"unclassified fixture entry").unwrap();

        assert!(!delete_managed_copy_directories(store, &incident_id, &inventory).unwrap());
        assert!(untracked.exists());
        assert!(
            store
                .project_dir()
                .join("generations/t56-generation/source")
                .is_dir()
        );
        let residual: (String, Option<i64>) = store
            .read_connection()
            .query_row(
                "SELECT relative_path,resolved_ms FROM local_sensitive_residuals
                 WHERE incident_id=?1 AND code='unclassified_managed_path'",
                [&incident_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            residual,
            (
                "generations/t56-generation/source/untracked.txt".into(),
                None
            )
        );

        std::fs::remove_file(&untracked).unwrap();
        assert!(delete_managed_copy_directories(store, &incident_id, &inventory).unwrap());
        assert!(!store.project_dir().join("generations").exists());
        let unresolved: bool = store
            .read_connection()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM local_sensitive_residuals
                 WHERE incident_id=?1 AND resolved_ms IS NULL)",
                [&incident_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(unresolved, "the unclassified incident remains incomplete");
    }

    #[test]
    fn managed_copy_cleanup_refuses_changed_directory_type_and_retries_after_restoration() {
        let mut fixture = crate::local_generation::tests::Fixture::new();
        let store = &mut fixture.v.f.store;
        let incident_id = format!("sha256:{}", "c".repeat(64));
        insert_purging_incident(store, &incident_id);
        create_managed_copy_file(store, "generations/t56-generation/source/nested/file.txt");
        let inventory = register_deleted_managed_tree(store, &incident_id);
        let changed = store
            .project_dir()
            .join("generations/t56-generation/source/nested");
        std::fs::remove_dir(&changed).unwrap();
        std::fs::write(&changed, b"changed type").unwrap();

        assert!(!delete_managed_copy_directories(store, &incident_id, &inventory).unwrap());
        assert!(changed.is_file());
        assert_eq!(std::fs::read(&changed).unwrap(), b"changed type");

        std::fs::remove_file(&changed).unwrap();
        std::fs::create_dir(&changed).unwrap();
        assert!(delete_managed_copy_directories(store, &incident_id, &inventory).unwrap());
        assert!(!store.project_dir().join("generations").exists());
        let unresolved: bool = store
            .read_connection()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM local_sensitive_residuals
                 WHERE incident_id=?1 AND resolved_ms IS NULL)",
                [&incident_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(unresolved, "the changed-type incident remains incomplete");
    }

    #[cfg(unix)]
    #[test]
    fn managed_copy_cleanup_refuses_symlink_without_following_and_retries_after_restoration() {
        let mut fixture = crate::local_generation::tests::Fixture::new();
        let store = &mut fixture.v.f.store;
        let incident_id = format!("sha256:{}", "d".repeat(64));
        insert_purging_incident(store, &incident_id);
        create_managed_copy_file(store, "generations/t56-generation/source/nested/file.txt");
        let inventory = register_deleted_managed_tree(store, &incident_id);
        let changed = store
            .project_dir()
            .join("generations/t56-generation/source/nested");
        std::fs::remove_dir(&changed).unwrap();
        let target = tempfile::tempdir().unwrap();
        let sentinel = target.path().join("unmodified.txt");
        std::fs::write(&sentinel, b"outside managed tree").unwrap();
        std::os::unix::fs::symlink(target.path(), &changed).unwrap();

        assert!(!delete_managed_copy_directories(store, &incident_id, &inventory).unwrap());
        assert!(
            std::fs::symlink_metadata(&changed)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"outside managed tree");

        std::fs::remove_file(&changed).unwrap();
        std::fs::create_dir(&changed).unwrap();
        assert!(delete_managed_copy_directories(store, &incident_id, &inventory).unwrap());
        assert!(!store.project_dir().join("generations").exists());
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"outside managed tree");
        let unresolved: bool = store
            .read_connection()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM local_sensitive_residuals
                 WHERE incident_id=?1 AND resolved_ms IS NULL)",
                [&incident_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(unresolved, "the symlink incident remains incomplete");
    }

    fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
        !needle.is_empty()
            && haystack
                .windows(needle.len())
                .any(|window| window == needle)
    }

    fn contains_bytes_in_tree(root: &Path, needle: &[u8]) -> std::io::Result<bool> {
        let metadata = std::fs::symlink_metadata(root)?;
        if metadata.file_type().is_symlink() {
            return Ok(false);
        }
        if metadata.is_file() {
            return Ok(contains_bytes(&std::fs::read(root)?, needle));
        }
        if metadata.is_dir() {
            for entry in std::fs::read_dir(root)? {
                if contains_bytes_in_tree(&entry?.path(), needle)? {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    fn view_snapshot(store: &CollaborationStore, view: &str) -> Vec<FileItem> {
        let mut items = Vec::new();
        let mut unknown = Vec::new();
        collect_view_tree(
            store,
            Path::new(&format!("views/{view}")),
            &mut items,
            &mut unknown,
        )
        .unwrap();
        assert!(unknown.is_empty());
        items.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        items
    }

    #[cfg(unix)]
    fn view_snapshot_except_path(
        store: &CollaborationStore,
        view: &str,
        excluded_relative: &str,
    ) -> Vec<FileItem> {
        let mut items = Vec::new();
        let mut unknown = Vec::new();
        collect_view_tree(
            store,
            Path::new(&format!("views/{view}")),
            &mut items,
            &mut unknown,
        )
        .unwrap();
        items.retain(|item| item.relative_path.trim_end_matches('/') != excluded_relative);
        unknown.retain(|path| path.trim_end_matches('/') != excluded_relative);
        assert!(unknown.is_empty());
        items.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        items
    }

    #[cfg(unix)]
    fn view_layout_refusal_case(case: &str) {
        use crate::collaboration_archive::ObjectDigest;
        use crate::collaboration_capture as capture;

        let mut fixture = crate::local_generation::tests::Fixture::new();
        let source_repository = fixture.v.f._repo.path().to_path_buf();
        let capture_request = capture::CaptureRequest {
            operation_id: capture::OperationId::mint().unwrap(),
            repository: source_repository.clone(),
            base: fixture.v.f.a.base.commit.clone(),
            result: fixture.v.f.a.result.commit.clone(),
            policy: capture::CapturePolicy::Required,
            retention: capture::RetentionBoundary::UntilReleased,
        };
        let capture_receipt =
            match capture::capture(&mut fixture.v.f.store, &capture_request).unwrap() {
                capture::CaptureOutcome::Acknowledged(receipt) => receipt,
                _ => panic!("capture did not acknowledge"),
            };
        let repo_name = format!("t56-pin-{case}");
        let profile = crate::analysis_view::ViewProfile::current(&repo_name);
        let options = crate::analysis_view::ViewOptions::new(profile);
        let view =
            crate::analysis_view::build_view(&fixture.v.f.store, &capture_receipt.result, &options)
                .unwrap();
        crate::analysis_view::hold_view(&fixture.v.f.store, &view, "t56-pin-control-holder", None)
            .unwrap();

        complete_fixture_wave(&mut fixture);
        let store = &mut fixture.v.f.store;
        let frozen = crate::local_authority::freeze(
            store,
            fixture.v.owner.authority_epoch(),
            "T56 synthetic pin-layout control",
        )
        .unwrap();
        let incident_id = format!(
            "sha256:{}",
            ObjectDigest::of(format!("t56-pin-control-{case}").as_bytes()).hex()
        );
        let quarantined = quarantine(
            store,
            &incident_id,
            "local-test-operator",
            frozen.epoch.as_str(),
            ReasonCode::SuspectedTestSecretExposure,
        )
        .unwrap();

        let held =
            purge_after_transient_contention(store, &incident_id, &quarantined.request_digest)
                .unwrap();
        assert!(!held.completed);
        assert!(!held.status.managed_cleanup_complete);
        assert!(store.project_dir().join("views").join(&view).exists());
        assert!(held.status.residuals.iter().any(|residual| {
            residual.code == "analysis_view_reader_or_hold_active"
                && residual.path == format!("views/{view}/")
        }));

        assert!(
            crate::analysis_view::release_view_hold(store, &view, "t56-pin-control-holder")
                .unwrap()
        );
        let current = strict_current_generation(
            &store
                .project_dir()
                .join("views")
                .join(&view)
                .join("CURRENT"),
        )
        .unwrap();
        let pin_relative = format!("views/{view}/gen-{current}/.pin");
        let current_relative = format!("views/{view}/CURRENT");
        let is_pin_case = matches!(case, "missing-pin" | "directory-pin" | "symlink-pin");
        let excluded_relative = if is_pin_case {
            pin_relative.as_str()
        } else {
            current_relative.as_str()
        };
        let before = view_snapshot_except_path(store, &view, excluded_relative);
        let fault_path = store.project_dir().join(excluded_relative);
        match case {
            "missing-pin" => std::fs::remove_file(&fault_path).unwrap(),
            "directory-pin" => {
                std::fs::remove_file(&fault_path).unwrap();
                std::fs::create_dir(&fault_path).unwrap();
            }
            "symlink-pin" => {
                std::fs::remove_file(&fault_path).unwrap();
                std::os::unix::fs::symlink("/dev/null", &fault_path).unwrap();
            }
            "missing-current" => std::fs::remove_file(&fault_path).unwrap(),
            "symlink-current" => {
                std::fs::remove_file(&fault_path).unwrap();
                std::os::unix::fs::symlink("/dev/null", &fault_path).unwrap();
            }
            "malformed-current" => std::fs::write(&fault_path, b"not-a-generation\n").unwrap(),
            _ => panic!("unknown view-layout control"),
        }
        let after_mutation = view_snapshot_except_path(store, &view, excluded_relative);
        assert_eq!(before, after_mutation);

        let refused =
            purge_after_transient_contention(store, &incident_id, &quarantined.request_digest)
                .unwrap();
        assert!(!refused.completed);
        assert!(!refused.status.managed_cleanup_complete);
        assert!(refused.status.residuals.iter().any(|residual| {
            residual.code == "analysis_view_layout_unclassified"
                && residual.path == format!("views/{view}/")
                && residual.resolved_ms.is_none()
        }));
        let view_item_states = refused
            .status
            .items
            .iter()
            .filter(|item| item.path.starts_with(&format!("views/{view}/")))
            .map(|item| format!("{}={}", item.path, item.state))
            .collect::<Vec<_>>();
        assert!(
            refused.status.items.iter().all(|item| {
                !item.path.starts_with(&format!("views/{view}/"))
                    || item.state != "deleted"
                    || item.path
                        == format!(
                            "views/{view}/holds/{}",
                            hold_filename("t56-pin-control-holder")
                        )
            }),
            "view item states after refusal: {view_item_states:?}; residuals: {:?}",
            refused.status.residuals
        );
        assert_eq!(
            view_snapshot_except_path(store, &view, excluded_relative),
            after_mutation
        );

        let retry =
            purge_after_transient_contention(store, &incident_id, &quarantined.request_digest)
                .unwrap();
        assert!(!retry.completed);
        assert!(!retry.status.managed_cleanup_complete);
        assert_eq!(
            view_snapshot_except_path(store, &view, excluded_relative),
            after_mutation
        );
        match case {
            "missing-pin" | "missing-current" => assert!(!fault_path.exists()),
            "directory-pin" => assert!(fault_path.is_dir()),
            "symlink-pin" => assert!(
                std::fs::symlink_metadata(&fault_path)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            ),
            "symlink-current" => assert!(
                std::fs::symlink_metadata(&fault_path)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            ),
            "malformed-current" => {
                assert_eq!(std::fs::read(&fault_path).unwrap(), b"not-a-generation\n")
            }
            _ => unreachable!(),
        }
    }

    #[cfg(unix)]
    fn preinventory_view_layout_refusal_case(case: &str) {
        use crate::collaboration_archive::ObjectDigest;
        use crate::collaboration_capture as capture;

        let mut fixture = crate::local_generation::tests::Fixture::new();
        let source_repository = fixture.v.f._repo.path().to_path_buf();
        let capture_request = capture::CaptureRequest {
            operation_id: capture::OperationId::mint().unwrap(),
            repository: source_repository.clone(),
            base: fixture.v.f.a.base.commit.clone(),
            result: fixture.v.f.a.result.commit.clone(),
            policy: capture::CapturePolicy::Required,
            retention: capture::RetentionBoundary::UntilReleased,
        };
        let receipt = match capture::capture(&mut fixture.v.f.store, &capture_request).unwrap() {
            capture::CaptureOutcome::Acknowledged(receipt) => receipt,
            _ => panic!("capture did not acknowledge"),
        };
        let options = crate::analysis_view::ViewOptions::new(
            crate::analysis_view::ViewProfile::current("t56-preinventory-view-fixture"),
        );
        let view = crate::analysis_view::build_view(&fixture.v.f.store, &receipt.result, &options)
            .unwrap();
        let view_root = fixture.v.f.store.project_dir().join("views").join(&view);
        let fault_relative = match case {
            "preinventory-symlink-pin" => {
                let current = strict_current_generation(&view_root.join("CURRENT")).unwrap();
                format!("views/{view}/gen-{current}/.pin")
            }
            "preinventory-symlink-current"
            | "preinventory-malformed-current"
            | "preinventory-oversized-current" => format!("views/{view}/CURRENT"),
            _ => panic!("unknown pre-inventory view-layout control"),
        };
        let fault_path = fixture.v.f.store.project_dir().join(&fault_relative);
        match case {
            "preinventory-symlink-pin" | "preinventory-symlink-current" => {
                std::fs::remove_file(&fault_path).unwrap();
                std::os::unix::fs::symlink("/dev/null", &fault_path).unwrap();
            }
            "preinventory-malformed-current" => {
                std::fs::write(&fault_path, b"not-a-generation\n").unwrap();
            }
            "preinventory-oversized-current" => {
                std::fs::write(&fault_path, vec![b'x'; 26]).unwrap();
            }
            _ => unreachable!(),
        }

        complete_fixture_wave(&mut fixture);
        let store = &mut fixture.v.f.store;
        let frozen = crate::local_authority::freeze(
            store,
            fixture.v.owner.authority_epoch(),
            "T56 pre-inventory view-layout control",
        )
        .unwrap();
        let incident_id = format!(
            "sha256:{}",
            ObjectDigest::of(format!("t56-preinventory-{case}").as_bytes()).hex()
        );
        let quarantined = quarantine(
            store,
            &incident_id,
            "local-test-operator",
            frozen.epoch.as_str(),
            ReasonCode::SuspectedTestSecretExposure,
        )
        .unwrap();
        let refused =
            purge_after_transient_contention(store, &incident_id, &quarantined.request_digest)
                .unwrap();
        assert!(!refused.completed);
        assert!(!refused.status.managed_cleanup_complete);
        assert!(view_root.exists());
        assert!(refused.status.residuals.iter().any(|residual| {
            residual.code == "analysis_view_layout_unclassified"
                && (residual.path == format!("views/{view}/") || residual.path == "views/")
                && residual.resolved_ms.is_none()
        }));
        match case {
            "preinventory-symlink-pin" | "preinventory-symlink-current" => {
                assert!(
                    std::fs::symlink_metadata(&fault_path)
                        .unwrap()
                        .file_type()
                        .is_symlink()
                )
            }
            "preinventory-malformed-current" => {
                assert_eq!(std::fs::read(&fault_path).unwrap(), b"not-a-generation\n")
            }
            "preinventory-oversized-current" => {
                assert_eq!(std::fs::read(&fault_path).unwrap(), vec![b'x'; 26])
            }
            _ => unreachable!(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn preinventory_symlinked_view_pin_is_recorded_as_a_root_residual() {
        preinventory_view_layout_refusal_case("preinventory-symlink-pin");
    }

    #[cfg(unix)]
    #[test]
    fn preinventory_symlinked_current_is_recorded_as_a_root_residual() {
        preinventory_view_layout_refusal_case("preinventory-symlink-current");
    }

    #[cfg(unix)]
    #[test]
    fn preinventory_malformed_current_is_recorded_as_a_root_residual() {
        preinventory_view_layout_refusal_case("preinventory-malformed-current");
    }

    #[cfg(unix)]
    #[test]
    fn preinventory_oversized_current_is_recorded_as_a_root_residual() {
        preinventory_view_layout_refusal_case("preinventory-oversized-current");
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_view_inventory_discards_partial_items_and_persists_root_residual() {
        use crate::collaboration_archive::ObjectDigest;
        use crate::collaboration_capture as capture;
        use std::os::unix::fs::PermissionsExt;

        struct RestorePermissions {
            path: PathBuf,
            mode: u32,
        }

        impl Drop for RestorePermissions {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(
                    &self.path,
                    std::fs::Permissions::from_mode(self.mode),
                );
            }
        }

        let mut fixture = crate::local_generation::tests::Fixture::new();
        let source_repository = fixture.v.f._repo.path().to_path_buf();
        let capture_request = capture::CaptureRequest {
            operation_id: capture::OperationId::mint().unwrap(),
            repository: source_repository.clone(),
            base: fixture.v.f.a.base.commit.clone(),
            result: fixture.v.f.a.result.commit.clone(),
            policy: capture::CapturePolicy::Required,
            retention: capture::RetentionBoundary::UntilReleased,
        };
        let receipt = match capture::capture(&mut fixture.v.f.store, &capture_request).unwrap() {
            capture::CaptureOutcome::Acknowledged(receipt) => receipt,
            _ => panic!("capture did not acknowledge"),
        };
        let options = crate::analysis_view::ViewOptions::new(
            crate::analysis_view::ViewProfile::current("t56-unreadable-view-fixture"),
        );
        let view = crate::analysis_view::build_view(&fixture.v.f.store, &receipt.result, &options)
            .unwrap();
        let view_root = fixture.v.f.store.project_dir().join("views").join(&view);
        complete_fixture_wave(&mut fixture);
        let store = &mut fixture.v.f.store;
        let frozen = crate::local_authority::freeze(
            store,
            fixture.v.owner.authority_epoch(),
            "T56 unreadable view inventory control",
        )
        .unwrap();
        let incident_id = format!(
            "sha256:{}",
            ObjectDigest::of(b"t56-unreadable-view-control").hex()
        );
        let quarantined = quarantine(
            store,
            &incident_id,
            "local-test-operator",
            frozen.epoch.as_str(),
            ReasonCode::SuspectedTestSecretExposure,
        )
        .unwrap();

        let mode = std::fs::metadata(&view_root).unwrap().permissions().mode();
        let restore_permissions = RestorePermissions {
            path: view_root.clone(),
            mode,
        };
        std::fs::set_permissions(&view_root, std::fs::Permissions::from_mode(0o0)).unwrap();
        assert!(std::fs::read_dir(&view_root).is_err());
        let result =
            purge_after_transient_contention(store, &incident_id, &quarantined.request_digest);
        drop(restore_permissions);

        let refused = result.unwrap();
        assert!(!refused.completed);
        assert!(!refused.status.managed_cleanup_complete);
        assert!(view_root.exists());
        assert!(
            refused
                .status
                .items
                .iter()
                .all(|item| !item.path.starts_with("views/"))
        );
        assert!(refused.status.residuals.iter().any(|residual| {
            residual.code == "analysis_view_layout_unclassified"
                && residual.path == "views/"
                && residual.resolved_ms.is_none()
        }));
    }

    #[cfg(unix)]
    #[test]
    fn missing_view_pin_with_content_is_a_refusal_only_residual() {
        view_layout_refusal_case("missing-pin");
    }

    #[cfg(unix)]
    #[test]
    fn non_regular_view_pin_with_content_is_a_refusal_only_residual() {
        view_layout_refusal_case("directory-pin");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_view_pin_with_content_is_a_refusal_only_residual() {
        view_layout_refusal_case("symlink-pin");
    }

    #[cfg(unix)]
    #[test]
    fn missing_current_with_content_is_a_refusal_only_residual() {
        view_layout_refusal_case("missing-current");
    }

    #[cfg(unix)]
    #[test]
    fn malformed_current_with_content_is_a_refusal_only_residual() {
        view_layout_refusal_case("malformed-current");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_current_with_content_is_a_refusal_only_residual() {
        view_layout_refusal_case("symlink-current");
    }

    #[cfg(unix)]
    #[test]
    fn oversized_current_control_file_is_rejected_before_parsing() {
        let root = tempfile::tempdir().unwrap();
        let current = root.path().join("CURRENT");
        std::fs::write(&current, vec![b'x'; 26]).unwrap();
        assert!(strict_current_generation(&current).is_err());
    }

    fn view_hold_control_case(case: &str) {
        use crate::collaboration_archive::ObjectDigest;
        use crate::collaboration_capture as capture;

        let mut fixture = crate::local_generation::tests::Fixture::new();
        let source_repository = fixture.v.f._repo.path().to_path_buf();
        let capture_request = capture::CaptureRequest {
            operation_id: capture::OperationId::mint().unwrap(),
            repository: source_repository.clone(),
            base: fixture.v.f.a.base.commit.clone(),
            result: fixture.v.f.a.result.commit.clone(),
            policy: capture::CapturePolicy::Required,
            retention: capture::RetentionBoundary::UntilReleased,
        };
        let receipt = match capture::capture(&mut fixture.v.f.store, &capture_request).unwrap() {
            capture::CaptureOutcome::Acknowledged(receipt) => receipt,
            _ => panic!("capture did not acknowledge"),
        };
        let options = crate::analysis_view::ViewOptions::new(
            crate::analysis_view::ViewProfile::current("t56-hold-fixture"),
        );
        let view = crate::analysis_view::build_view(&fixture.v.f.store, &receipt.result, &options)
            .unwrap();
        let holder = format!("t56-{case}-holder");
        let expiry = (case == "expired-hold").then_some(0);
        crate::analysis_view::hold_view(&fixture.v.f.store, &view, &holder, expiry).unwrap();
        let view_root = fixture.v.f.store.project_dir().join("views").join(&view);
        let hold_path = view_root.join("holds").join(hold_filename(&holder));
        if case == "malformed-hold" {
            std::fs::write(&hold_path, b"{").unwrap();
        }

        complete_fixture_wave(&mut fixture);
        let store = &mut fixture.v.f.store;
        let frozen = crate::local_authority::freeze(
            store,
            fixture.v.owner.authority_epoch(),
            "T56 synthetic hold control",
        )
        .unwrap();
        let incident_id = format!(
            "sha256:{}",
            ObjectDigest::of(format!("t56-hold-control-{case}").as_bytes()).hex()
        );
        let quarantined = quarantine(
            store,
            &incident_id,
            "local-test-operator",
            frozen.epoch.as_str(),
            ReasonCode::SuspectedTestSecretExposure,
        )
        .unwrap();
        let blocked =
            purge_after_transient_contention(store, &incident_id, &quarantined.request_digest)
                .unwrap();
        assert!(!blocked.completed);
        assert!(view_root.exists());
        let expected_code = if case == "expired-hold" {
            "analysis_view_reader_or_hold_active"
        } else {
            "analysis_view_layout_unclassified"
        };
        assert!(blocked.status.residuals.iter().any(|residual| {
            residual.code == expected_code
                && residual.path == format!("views/{view}/")
                && residual.resolved_ms.is_none()
        }));

        if case == "expired-hold" {
            assert!(crate::analysis_view::release_view_hold(store, &view, &holder).unwrap());
            let retry =
                purge_after_transient_contention(store, &incident_id, &quarantined.request_digest)
                    .unwrap();
            assert!(!retry.completed);
            assert!(!view_root.exists());
            assert!(retry.status.residuals.iter().any(|residual| {
                residual.code == expected_code && residual.resolved_ms.is_some()
            }));
        } else {
            let recorded = std::fs::read(&hold_path).unwrap();
            let retry =
                purge_after_transient_contention(store, &incident_id, &quarantined.request_digest)
                    .unwrap();
            assert!(!retry.completed);
            assert!(view_root.exists());
            assert_eq!(std::fs::read(&hold_path).unwrap(), recorded);
            assert!(retry.status.residuals.iter().any(|residual| {
                residual.code == expected_code && residual.resolved_ms.is_none()
            }));
        }
    }

    #[cfg(unix)]
    #[test]
    fn expired_view_hold_remains_a_blocker_until_explicit_release() {
        view_hold_control_case("expired-hold");
    }

    #[cfg(unix)]
    #[test]
    fn malformed_view_hold_is_a_refusal_only_residual() {
        view_hold_control_case("malformed-hold");
    }

    #[cfg(unix)]
    #[test]
    fn terminal_view_state_after_last_pin_unlink_finishes_directory_cleanup() {
        use crate::collaboration_capture as capture;

        let mut fixture = crate::local_generation::tests::Fixture::new();
        let source_repository = fixture.v.f._repo.path().to_path_buf();
        let capture_request = capture::CaptureRequest {
            operation_id: capture::OperationId::mint().unwrap(),
            repository: source_repository.clone(),
            base: fixture.v.f.a.base.commit.clone(),
            result: fixture.v.f.a.result.commit.clone(),
            policy: capture::CapturePolicy::Required,
            retention: capture::RetentionBoundary::UntilReleased,
        };
        let receipt = match capture::capture(&mut fixture.v.f.store, &capture_request).unwrap() {
            capture::CaptureOutcome::Acknowledged(receipt) => receipt,
            _ => panic!("capture did not acknowledge"),
        };
        let options = crate::analysis_view::ViewOptions::new(
            crate::analysis_view::ViewProfile::current("t56-terminal-view-fixture"),
        );
        let view = crate::analysis_view::build_view(&fixture.v.f.store, &receipt.result, &options)
            .unwrap();
        complete_fixture_wave(&mut fixture);
        let store = &mut fixture.v.f.store;
        let frozen = crate::local_authority::freeze(
            store,
            fixture.v.owner.authority_epoch(),
            "T56 terminal view partition control",
        )
        .unwrap();
        let incident_id = format!(
            "sha256:{}",
            crate::collaboration_archive::ObjectDigest::of(b"t56-terminal-view-control").hex()
        );
        let quarantined = quarantine(
            store,
            &incident_id,
            "local-test-operator",
            frozen.epoch.as_str(),
            ReasonCode::SuspectedTestSecretExposure,
        )
        .unwrap();
        let marker = marker_for_incident(store, &incident_id, &quarantined.request_digest).unwrap();

        let Some(_archive) = crate::collaboration_gc::try_admin_exclusive(store).unwrap() else {
            panic!("archive barrier was unexpectedly busy");
        };
        let Some(_response) = try_response_exclusive(store).unwrap() else {
            panic!("response barrier was unexpectedly busy");
        };
        let witness = crate::local_authority::status(store).unwrap();
        let _frozen = crate::local_authority::frozen_guard(store, &witness.epoch).unwrap();
        let inventory = prepare_inventory(store, &marker).unwrap();
        let (groups, malformed) = view_groups(&inventory);
        assert!(malformed.is_empty());
        assert_eq!(groups.len(), 1);
        let group = &groups[0];
        assert_eq!(group.relative_root, format!("views/{view}/"));
        let current = group
            .items
            .iter()
            .find(|item| item.relative_path == format!("views/{view}/CURRENT"))
            .unwrap();
        let mut pins = Vec::new();
        for item in group
            .items
            .iter()
            .filter(|item| item.relative_path.ends_with("/.pin"))
        {
            let path = store.project_dir().join(&item.relative_path);
            let file = open_and_lock_view_pin(&path, item).unwrap();
            pins.push(LockedViewPin {
                item: item.clone(),
                _file: file,
            });
        }
        assert_eq!(pins.len(), 1);

        for item in &group.items {
            if !is_directory_item(&item.relative_path)
                && item
                    .relative_path
                    .starts_with(&format!("{}gen-", group.relative_root))
                && !item.relative_path.ends_with("/.pin")
            {
                delete_view_file(store, &incident_id, item).unwrap();
            }
        }
        for item in &group.items {
            if is_directory_item(&item.relative_path)
                || item.relative_path == current.relative_path
                || item.relative_path.ends_with("/.pin")
                || item
                    .relative_path
                    .starts_with(&format!("{}gen-", group.relative_root))
            {
                continue;
            }
            delete_view_file(store, &incident_id, item).unwrap();
        }
        delete_view_file(store, &incident_id, current).unwrap();
        for pin in &pins {
            delete_view_file(store, &incident_id, &pin.item).unwrap();
        }
        drop(pins);
        drop(_frozen);
        drop(_response);
        drop(_archive);

        let resumed =
            purge_after_transient_contention(store, &incident_id, &quarantined.request_digest)
                .unwrap();
        assert!(!resumed.completed);
        let pending_items = resumed
            .status
            .items
            .iter()
            .filter(|item| item.state != "deleted")
            .map(|item| format!("{}={}", item.path, item.state))
            .collect::<Vec<_>>();
        let unclassified_residuals = resumed
            .status
            .residuals
            .iter()
            .filter(|residual| residual.permanence == "unclassified")
            .map(|residual| format!("{}:{}", residual.code, residual.path))
            .collect::<Vec<_>>();
        let uncleared_tables = resumed
            .status
            .database_tables
            .iter()
            .filter(|table| table.state != "cleared")
            .map(|table| format!("{}={}", table.table, table.state))
            .collect::<Vec<_>>();
        assert!(
            resumed.status.managed_cleanup_complete,
            "managed cleanup incomplete: pending_items={pending_items:?}, unclassified={unclassified_residuals:?}, uncleared_tables={uncleared_tables:?}, cleanup_receipt={:?}",
            resumed.status.database_cleanup_completed_ms
        );
        assert!(!store.project_dir().join("views").join(&view).exists());
        assert!(!store.project_dir().join("views").exists());
        assert!(
            resumed
                .status
                .items
                .iter()
                .filter(|item| { item.path.starts_with(&format!("views/{view}/")) })
                .all(|item| item.state == "deleted")
        );
    }

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

    #[cfg(unix)]
    #[test]
    fn synthetic_source_token_is_blocked_and_managed_copies_are_removed_with_residuals() {
        use crate::collaboration_archive::{self as archive, ObjectDigest};
        use crate::collaboration_capture as capture;
        use crate::collaboration_context as context;

        let playground_parent = std::path::Path::new("/private/tmp");
        let run_root = if playground_parent.is_dir() {
            tempfile::Builder::new()
                .prefix("aethyme-t56-playground-")
                .tempdir_in(playground_parent)
                .unwrap()
        } else {
            tempfile::Builder::new()
                .prefix("aethyme-t56-playground-")
                .tempdir()
                .unwrap()
        };
        let nonce = capture::OperationId::mint().unwrap();
        let token = format!("secret_{}", nonce.as_str().trim_start_matches("cap-"));
        let token_bytes = token.as_bytes();
        let mut fixture = crate::local_generation::tests::Fixture::with_source_marker_in(
            token_bytes,
            run_root.path(),
        );
        let prepared = fixture.prepare("t56-generation").unwrap();
        // Keep the fixture's own wave terminal so the synthetic unknown job
        // below is the only unresolved-work witness exercised by purge.
        complete_fixture_wave(&mut fixture);
        let generation_root = prepared.absolute_root().to_path_buf();
        assert!(
            contains_bytes(
                &std::fs::read(generation_root.join("src/credential.rs")).unwrap(),
                token_bytes
            ),
            "prepared generation omitted the synthetic source marker"
        );

        let source_repository = fixture.v.f._repo.path().to_path_buf();
        let capture_request = capture::CaptureRequest {
            operation_id: capture::OperationId::mint().unwrap(),
            repository: source_repository.clone(),
            base: fixture.v.f.a.base.commit.clone(),
            result: fixture.v.f.a.result.commit.clone(),
            policy: capture::CapturePolicy::Required,
            retention: capture::RetentionBoundary::UntilReleased,
        };
        let capture_receipt =
            match capture::capture(&mut fixture.v.f.store, &capture_request).unwrap() {
                capture::CaptureOutcome::Acknowledged(receipt) => receipt,
                _ => panic!("capture did not acknowledge"),
            };
        let snapshot =
            archive::snapshot_of_commit(&source_repository, &fixture.v.f.a.result.commit).unwrap();
        let credential = snapshot
            .entries()
            .iter()
            .find(|entry| entry.path() == b"src/credential.rs")
            .expect("source snapshot contains the synthetic source file");
        let content_digest = ObjectDigest::from_bytes(*credential.content_sha256());
        assert!(contains_bytes(
            &archive::read_object(&fixture.v.f.store, &content_digest).unwrap(),
            token_bytes
        ));

        let brief_bytes = format!(r#"{{"intent":"retain {token}"}}"#);
        let brief = aethyme_contracts::experimental_v0::brief::Brief::from_decision_file(
            brief_bytes.as_bytes(),
        )
        .unwrap();
        context::attach_brief(
            &mut fixture.v.f.store,
            &capture_receipt.contribution,
            &brief,
        )
        .unwrap();
        let query = context::ContextQuery {
            // The captured contribution changes one.txt; credential.rs is
            // present in both source snapshots and therefore is not a match.
            scope: vec![b"one.txt".to_vec()],
            source: Some(capture_receipt.result.clone()),
            analysis: Vec::new(),
            budget: Default::default(),
        };
        let cached =
            context::retrieve_cached(&mut fixture.v.f.store, &query, &context::LocalProject)
                .unwrap();
        assert!(contains_bytes(&cached.record, token_bytes));

        let view_options = crate::analysis_view::ViewOptions::new(
            crate::analysis_view::ViewProfile::current("t56-fixture"),
        );
        let view = crate::analysis_view::build_view(
            &fixture.v.f.store,
            &capture_receipt.result,
            &view_options,
        )
        .unwrap();
        let superseded_reader = crate::analysis_view::open_view(&fixture.v.f.store, &view).unwrap();
        assert_eq!(superseded_reader.manifest().generation, 1);
        let rebuilt = crate::analysis_view::build_view(
            &fixture.v.f.store,
            &capture_receipt.result,
            &view_options,
        )
        .unwrap();
        assert_eq!(rebuilt, view);
        let reader = crate::analysis_view::open_view(&fixture.v.f.store, &view).unwrap();
        assert_eq!(reader.manifest().generation, 2);
        let references = reader.find_references(&token, 16).unwrap();
        let reference_record = references.to_record().0;
        assert!(contains_bytes(&reference_record, b"src/credential.rs"));
        assert!(!contains_bytes(&reference_record, token_bytes));
        let view_root = fixture.v.f.store.project_dir().join("views").join(&view);
        let generation = strict_current_generation(&view_root.join("CURRENT")).unwrap();
        assert!(
            contains_bytes_in_tree(
                &view_root.join(format!("gen-{generation}/facts/.aethyme/graph")),
                token_bytes
            )
            .unwrap()
        );
        crate::analysis_view::hold_view(&fixture.v.f.store, &view, "t56-synthetic-holder", None)
            .unwrap();

        let scope = crate::local_restore::Scope {
            origin: fixture.v.f.store.project().as_str().to_string(),
            visibility: "local_project".into(),
            audience: "t56-fixture-receiver".into(),
        };
        let selection = crate::local_restore::Selection {
            sources: vec![capture_receipt.result.clone()],
            records: Vec::new(),
        };
        let export_path = run_root.path().join("external-export.bundle");
        crate::local_restore::export_to_path(
            &mut fixture.v.f.store,
            &fixture.v.owner,
            &scope,
            &selection,
            &export_path,
        )
        .unwrap();
        let exported = std::fs::read(&export_path).unwrap();
        assert!(contains_bytes(&exported, token_bytes));
        let verified = crate::local_restore::verify(
            &exported,
            &crate::local_restore::ReceiverPolicy {
                bundle: ObjectDigest::of(&exported),
                scope: scope.clone(),
            },
        )
        .unwrap();
        let binding =
            crate::local_restore::bind_receiver(&fixture.v.f.store, &fixture.v.owner, &verified)
                .unwrap();
        let imported = crate::local_restore::import(
            &mut fixture.v.f.store,
            &fixture.v.owner,
            &binding,
            &verified,
        )
        .unwrap();
        crate::local_restore::show(&fixture.v.f.store, &imported.id).unwrap();
        let rehearsed_root = crate::local_restore::rehearse(
            &mut fixture.v.f.store,
            &fixture.v.owner,
            &imported.id,
            &capture_receipt.result,
            "t56-rehearsal",
        )
        .unwrap();
        assert!(contains_bytes(
            &std::fs::read(rehearsed_root.join("src/credential.rs")).unwrap(),
            token_bytes
        ));

        let source_worktree_copy = source_repository.join("sensitive-test-material.txt");
        std::fs::write(&source_worktree_copy, token_bytes).unwrap();
        let project_dir = fixture.v.f.store.project_dir().to_path_buf();
        let (host_root, fixture_repository, remote_root) = fixture.root_paths();
        assert_eq!(source_repository, fixture_repository);
        let store = &mut fixture.v.f.store;
        let wave: String = store
            .read_connection()
            .query_row(
                "SELECT wave FROM local_waves WHERE target=?1 LIMIT 1",
                [fixture.v.owner.target()],
                |row| row.get(0),
            )
            .unwrap();
        let synthetic_job = "t56-synthetic-unknown-job";
        store
            .connection()
            .execute(
                "INSERT INTO local_controller_runs(wave,id,epoch,state,body,observation)
                 VALUES(?1,?2,?3,'unknown',?4,?5)",
                rusqlite::params![
                    wave,
                    synthetic_job,
                    fixture.v.owner.authority_epoch().as_str(),
                    b"synthetic unknown-work witness".as_slice(),
                    b"test-only; no native dispatch".as_slice(),
                ],
            )
            .unwrap();

        let incident_id = format!("sha256:{}", ObjectDigest::of(token_bytes).hex());
        let frozen = crate::local_authority::freeze(
            store,
            fixture.v.owner.authority_epoch(),
            "T56 synthetic fixture",
        )
        .unwrap();
        let quarantined = quarantine(
            store,
            &incident_id,
            "local-test-operator",
            frozen.epoch.as_str(),
            ReasonCode::SuspectedTestSecretExposure,
        )
        .unwrap();
        assert!(matches!(
            ensure_available(store),
            Err(SensitiveError::Quarantined)
        ));
        assert!(archive::read_object(store, &content_digest).is_err());
        assert!(archive::has_object(store, &content_digest).is_err());
        assert!(capture::receipt(store, &capture_request.operation_id).is_err());
        assert!(capture::recover(store).is_err());
        assert!(capture::abort(store, &capture_request.operation_id).is_err());
        assert!(capture::capture(store, &capture_request).is_err());
        assert!(context::retrieve_cached(store, &query, &context::LocalProject).is_err());
        assert!(
            crate::analysis_view::build_view(store, &capture_receipt.result, &view_options)
                .is_err()
        );
        assert!(crate::analysis_view::open_view(store, &view).is_err());
        assert!(reader.find_references(&token, 16).is_err());
        assert!(superseded_reader.find_references(&token, 16).is_err());
        assert!(crate::local_generation::load_prepared(store, prepared.id()).is_err());
        assert!(crate::local_restore::show(store, &imported.id).is_err());
        assert!(crate::local_restore::bind_receiver(store, &fixture.v.owner, &verified).is_err());
        assert!(
            crate::local_restore::import(store, &fixture.v.owner, &binding, &verified).is_err()
        );
        assert!(
            crate::local_restore::rehearse(
                store,
                &fixture.v.owner,
                &imported.id,
                &capture_receipt.result,
                "t56-after-quarantine"
            )
            .is_err()
        );
        assert!(
            crate::local_restore::export_to_path(
                store,
                &fixture.v.owner,
                &scope,
                &selection,
                &run_root.path().join("blocked-export.bundle")
            )
            .is_err()
        );
        assert_eq!(quarantined.state, "quarantined");

        assert!(matches!(
            purge_after_transient_contention(store, &incident_id, &quarantined.request_digest),
            Err(SensitiveError::UnknownWork)
        ));
        store
            .connection()
            .execute(
                "UPDATE local_controller_runs SET state='completed' WHERE id=?1",
                [synthetic_job],
            )
            .unwrap();
        let first_purge =
            purge_after_transient_contention(store, &incident_id, &quarantined.request_digest)
                .unwrap();
        assert!(!first_purge.completed);
        assert!(
            !generation_root.exists(),
            "retained generated source root {}; generation inventory states: {:?}; unclassified residuals: {:?}",
            generation_root
                .strip_prefix(&project_dir)
                .unwrap_or(&generation_root)
                .display(),
            first_purge
                .status
                .items
                .iter()
                .filter(|item| item.path.starts_with("generations/"))
                .map(|item| format!("{}={}", item.path, item.state))
                .collect::<Vec<_>>(),
            first_purge
                .status
                .residuals
                .iter()
                .filter(|residual| residual.permanence == "unclassified")
                .map(|residual| format!("{}:{}", residual.code, residual.path))
                .collect::<Vec<_>>()
        );
        assert!(!rehearsed_root.exists());
        assert!(host_root.exists());
        assert!(project_dir.join("views").join(&view).exists());
        let held_view = view_snapshot(store, &view);
        assert!(first_purge.status.residuals.iter().any(|residual| {
            residual.code == "analysis_view_reader_or_hold_active"
                && residual.path == format!("views/{view}/")
                && residual.resolved_ms.is_none()
        }));
        assert!(source_worktree_copy.is_file());
        assert!(contains_bytes(
            &std::fs::read(&source_worktree_copy).unwrap(),
            token_bytes
        ));
        assert!(contains_bytes(
            &std::fs::read(&export_path).unwrap(),
            token_bytes
        ));
        assert!(remote_root.exists());
        let remote_source = archive::git(&remote_root)
            .args(["show", "refs/heads/accepted:src/credential.rs"])
            .output()
            .unwrap();
        assert!(
            remote_source.status.success(),
            "provider mirror source is present"
        );
        assert!(contains_bytes(&remote_source.stdout, token_bytes));
        assert!(first_purge.status.residuals.iter().any(|residual| {
            residual.code == "captured_source_repository_copy"
                && residual.path == source_repository.to_string_lossy()
        }));
        assert!(first_purge.status.residuals.iter().any(|residual| {
            residual.code == "git_worktrees_and_remotes" && residual.path.is_empty()
        }));
        assert!(first_purge.status.residuals.iter().any(|residual| {
            residual.code == "previous_external_export_copy"
                && residual.path == export_path.to_string_lossy()
        }));
        assert!(!first_purge.status.managed_cleanup_complete);

        assert!(
            crate::analysis_view::release_view_hold(store, &view, "t56-synthetic-holder").unwrap()
        );
        let released_hold_view = view_snapshot(store, &view);
        assert_ne!(held_view, released_hold_view);
        let pin_busy_purge =
            purge_after_transient_contention(store, &incident_id, &quarantined.request_digest)
                .unwrap();
        assert!(!pin_busy_purge.completed);
        assert_eq!(view_snapshot(store, &view), released_hold_view);
        assert!(pin_busy_purge.status.residuals.iter().any(|residual| {
            residual.code == "analysis_view_reader_or_hold_active"
                && residual.path == format!("views/{view}/")
                && residual.resolved_ms.is_none()
        }));
        drop(reader);
        let superseded_pin_purge =
            purge_after_transient_contention(store, &incident_id, &quarantined.request_digest)
                .unwrap();
        assert!(!superseded_pin_purge.completed);
        assert_eq!(view_snapshot(store, &view), released_hold_view);
        assert!(
            superseded_pin_purge
                .status
                .residuals
                .iter()
                .any(|residual| {
                    residual.code == "analysis_view_reader_or_hold_active"
                        && residual.path == format!("views/{view}/")
                        && residual.resolved_ms.is_none()
                })
        );
        drop(superseded_reader);
        let final_purge =
            purge_after_transient_contention(store, &incident_id, &quarantined.request_digest)
                .unwrap();
        assert!(!final_purge.completed);
        let final_pending_items = final_purge
            .status
            .items
            .iter()
            .filter(|item| item.state != "deleted")
            .map(|item| format!("{}={}", item.path, item.state))
            .collect::<Vec<_>>();
        let final_unclassified = final_purge
            .status
            .residuals
            .iter()
            .filter(|residual| residual.permanence == "unclassified")
            .map(|residual| format!("{}:{}", residual.code, residual.path))
            .collect::<Vec<_>>();
        assert!(
            final_purge.status.managed_cleanup_complete,
            "final managed cleanup incomplete: inventory={:?}, pending_items={final_pending_items:?}, unclassified={final_unclassified:?}, cleanup_receipt={:?}",
            final_purge.status.managed_inventory_digest,
            final_purge.status.database_cleanup_completed_ms
        );
        assert!(!project_dir.join("views").join(&view).exists());
        assert!(!project_dir.join("views").exists());
        assert!(contains_bytes(
            &std::fs::read(&export_path).unwrap(),
            token_bytes
        ));
        assert_eq!(final_purge.status.state, "quarantined");
    }
}
