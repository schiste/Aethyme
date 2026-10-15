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
    pub managed_cleanup_complete: bool,
    pub items: Vec<ItemStatus>,
    pub residuals: Vec<ResidualStatus>,
    pub limitations: Vec<&'static str>,
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

fn lock_path(store: &CollaborationStore) -> PathBuf {
    store.project_dir().join("spool/sensitive-response.lock")
}

fn marker_path(store: &CollaborationStore) -> Result<PathBuf, SensitiveError> {
    let project_dir = store.project_dir();
    let parent = project_dir
        .parent()
        .ok_or_else(|| SensitiveError::Invalid("project directory has no parent".into()))?;
    let name = project_dir
        .file_name()
        .ok_or_else(|| SensitiveError::Invalid("project key is missing".into()))?;
    Ok(parent.join(".local-authority").join(name).join(MARKER_NAME))
}

fn open_response_lock(store: &CollaborationStore) -> Result<File, SensitiveError> {
    let path = lock_path(store);
    let parent = path
        .parent()
        .ok_or_else(|| SensitiveError::Invalid("response lock directory".into()))?;
    fs::create_dir_all(parent)?;
    crate::host_state::protect_host_state_path(parent, true)?;
    Ok(open_lock_file(&path)?)
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

fn read_marker(store: &CollaborationStore) -> Result<Option<Marker>, SensitiveError> {
    let path = marker_path(store)?;
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
        || marker.scope_id != store.project().as_str()
        || marker.project_key != store.project().as_str()
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
        existing
    } else {
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

pub fn status(
    store: &CollaborationStore,
    incident_id: &str,
) -> Result<IncidentStatus, SensitiveError> {
    let marker = read_marker(store)?.ok_or(SensitiveError::Mismatch)?;
    if marker.incident_id != incident_id {
        return Err(SensitiveError::Mismatch);
    }
    let (state, digest, inventory): (String, String, Option<String>) = store
        .read_connection()
        .query_row(
            "SELECT state,request_digest,inventory_digest FROM local_sensitive_incidents WHERE id=?1",
            [incident_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
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
        managed_cleanup_complete,
        items,
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
        let relative_path = relative.to_string_lossy().into_owned();
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

fn inventory_digest(items: &[FileItem]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"aethyme-local-sensitive-inventory-v1\0");
    for item in items {
        hash.update((item.relative_path.len() as u64).to_be_bytes());
        hash.update(item.relative_path.as_bytes());
        hash.update(item.sha256.as_bytes());
        hash.update(item.bytes.to_be_bytes());
    }
    format!("sha256:{:x}", hash.finalize())
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
                bytes: row.get::<_, i64>(2)?.try_into().unwrap_or(0),
            })
        })?;
        return Ok(rows.collect::<Result<Vec<_>, _>>()?);
    }

    let mut files = Vec::new();
    let mut unknown = Vec::new();
    collect_tree(store, Path::new("objects"), &mut files, &mut unknown)?;
    collect_tree(store, Path::new("spool/archive"), &mut files, &mut unknown)?;
    collect_tree(store, Path::new("spool/gc"), &mut files, &mut unknown)?;
    collect_tree(store, Path::new("views"), &mut files, &mut unknown)?;
    // The archive and response barrier lock files must remain at stable paths.
    // Other store roots are outside this managed profile and are reported.
    for name in ["objects", "spool", "views"] {
        let path = store.project_dir().join(name);
        let entries = match fs::read_dir(&path) {
            Ok(entries) => entries.collect::<Result<Vec<_>, _>>()?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let child = entry.file_name().to_string_lossy().into_owned();
            if name == "spool"
                && matches!(
                    child.as_str(),
                    "archive" | "gc" | "gc.lock" | "sensitive-response.lock"
                )
            {
                continue;
            }
            if name == "objects" && child == "sha256" {
                continue;
            }
            if name == "views" {
                continue;
            }
            unknown.push(format!("{name}/{child}"));
        }
    }
    files.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    unknown.sort();
    unknown.dedup();
    let digest = inventory_digest(&files);
    let root_entries = fs::read_dir(store.project_dir())?.collect::<Result<Vec<_>, _>>()?;
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
    for path in &unknown {
        tx.execute(
            "INSERT OR IGNORE INTO local_sensitive_residuals
             (incident_id,code,relative_path,permanence) VALUES (?1,'unclassified_managed_path',?2,'unclassified')",
            rusqlite::params![marker.incident_id, path],
        )?;
    }
    for (code, permanence) in [
        ("unregistered_external_exports", "unmanaged"),
        ("git_worktrees_and_remotes", "unmanaged"),
        ("host_backups_and_filesystem_snapshots", "unmanaged"),
        ("logs_previews_and_process_memory", "unmanaged"),
    ] {
        tx.execute(
            "INSERT OR IGNORE INTO local_sensitive_residuals
             (incident_id,code,relative_path,permanence) VALUES (?1,?2,'',?3)",
            rusqlite::params![marker.incident_id, code, permanence],
        )?;
    }
    for entry in root_entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if matches!(
            name.as_str(),
            "objects"
                | "spool"
                | "views"
                | "state.db"
                | "state.db-wal"
                | "state.db-shm"
                | crate::collaboration_state::ROOT_MARKER
        ) {
            continue;
        }
        tx.execute(
            "INSERT OR IGNORE INTO local_sensitive_residuals
             (incident_id,code,relative_path,permanence) VALUES (?1,'unclassified_managed_path',?2,'unclassified')",
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

fn clear_managed_database(store: &mut CollaborationStore) -> Result<(), SensitiveError> {
    let database = store.project_dir().join("state.db");
    let connection = store.connection();
    connection.pragma_update(None, "secure_delete", "ON")?;
    connection.pragma_update(None, "foreign_keys", "OFF")?;
    let tables = {
        let mut statement = connection.prepare(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name DESC",
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
        ) {
            continue;
        }
        let quoted = table.replace('"', "\"\"");
        transaction.execute(&format!("DELETE FROM \"{quoted}\""), [])?;
    }
    transaction.commit()?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    let (busy, _, _): (i64, i64, i64) =
        connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    if busy != 0 {
        return Err(SensitiveError::Busy);
    }
    connection.execute_batch("VACUUM")?;
    File::open(&database)?.sync_all()?;
    sync_directory(store.project_dir())?;
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
    let legacy_readers_unresolved: bool = store.read_connection().query_row(
        "SELECT EXISTS(SELECT 1 FROM local_sensitive_residuals
         WHERE incident_id=?1 AND code='legacy_binary_readers_and_process_memory'
         AND resolved_ms IS NULL)",
        [incident_id],
        |row| row.get(0),
    )?;
    if completed && !unclassified {
        if let Err(error) = clear_managed_database(store) {
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

