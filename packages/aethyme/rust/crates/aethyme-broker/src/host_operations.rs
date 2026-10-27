//! Per-user serialization and crash barriers for remote mutations.
//!
//! Repository-local rows remain the session audit trail. This host ledger is
//! deliberately smaller: it stores only a credential-free remote identity and
//! enough state to stop another clone after an ambiguous outcome.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use crate::clock::epoch_ms as now_ms;
use crate::file_lock::{ExclusiveFileLock, SharedFileLock, open_lock_file};
use crate::{OperationEffect, OperationProvider, OperationStatus};

const SCHEMA_VERSION: i64 = 2;
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL);
INSERT OR IGNORE INTO meta VALUES ('schema_version', 1);
CREATE TABLE IF NOT EXISTS host_operations (
 operation_id TEXT PRIMARY KEY,
 remote_key TEXT NOT NULL,
 provider TEXT NOT NULL CHECK (provider IN ('git', 'github')),
 effect TEXT NOT NULL CHECK (effect IN ('write', 'destructive')),
 status TEXT NOT NULL CHECK (status IN (
   'prepared', 'running', 'succeeded', 'failed', 'outcome_unknown',
   'reconciled_succeeded', 'reconciled_failed'
 )),
 holder_pid INTEGER NOT NULL,
 created_at INTEGER NOT NULL,
 updated_at INTEGER NOT NULL,
 finished_at INTEGER
);
CREATE INDEX IF NOT EXISTS host_operations_unresolved
 ON host_operations(remote_key, status, created_at);
"#;

#[derive(Debug, thiserror::Error)]
pub enum HostOperationError {
    #[error(
        "host operation state at {}",
        crate::host_state::describe_host_state_io(path, source)
    )]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("host operation database: {}", crate::host_state::describe_host_state_sqlite(.0))]
    Sqlite(#[from] rusqlite::Error),
    #[error("cannot find per-user state directory; set AETHYME_HOST_STATE_DIR")]
    StateDirectoryUnavailable,
    #[error("invalid canonical remote identity: {0}")]
    InvalidRemoteKey(&'static str),
    #[error("host operation effects must be write or destructive")]
    InvalidEffect,
    #[error("unsupported host operation schema {0}")]
    UnsupportedSchema(i64),
    #[error("remote write blocked by unresolved host operation {operation_id} for {remote_key}")]
    Blocked {
        operation_id: String,
        remote_key: String,
    },
    #[error(
        "host coordination lock for {remote_key} did not become available before the caller deadline"
    )]
    LockDeadline { remote_key: String },
    #[error("host operation {0} was not found")]
    NotFound(String),
    #[error("host operation {operation_id} is {actual}, expected {expected}")]
    InvalidTransition {
        operation_id: String,
        actual: &'static str,
        expected: &'static str,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct HostOperation {
    pub operation_id: String,
    pub remote_key: String,
    pub provider: OperationProvider,
    pub effect: OperationEffect,
    pub status: OperationStatus,
    pub holder_pid: u32,
    pub created_at: i64,
    pub updated_at: i64,
    pub finished_at: Option<i64>,
}

pub fn default_host_operation_db_path() -> Result<PathBuf, HostOperationError> {
    crate::host_state::default_host_state_dir()
        .map(|directory| directory.join("host-operations.db"))
        .ok_or(HostOperationError::StateDirectoryUnavailable)
}

pub struct HostOperationGuard {
    conn: Connection,
    _lock: HostRemoteLock,
    operation: HostOperation,
    terminal: bool,
}

impl HostOperationGuard {
    pub fn begin_default(
        remote_key: &str,
        provider: OperationProvider,
        effect: OperationEffect,
    ) -> Result<Self, HostOperationError> {
        Self::begin(
            &default_host_operation_db_path()?,
            remote_key,
            provider,
            effect,
        )
    }

    pub fn begin(
        database: &Path,
        remote_key: &str,
        provider: OperationProvider,
        effect: OperationEffect,
    ) -> Result<Self, HostOperationError> {
        Self::begin_with_deadline(database, remote_key, provider, effect, None)
    }

    pub(crate) fn begin_with_deadline(
        database: &Path,
        remote_key: &str,
        provider: OperationProvider,
        effect: OperationEffect,
        deadline: Option<std::time::Instant>,
    ) -> Result<Self, HostOperationError> {
        validate_remote_key(remote_key)?;
        if !matches!(
            effect,
            OperationEffect::Write | OperationEffect::Destructive
        ) {
            return Err(HostOperationError::InvalidEffect);
        }
        let state_directory = database
            .parent()
            .ok_or(HostOperationError::StateDirectoryUnavailable)?;
        create_private_directory(state_directory)?;
        let lock = HostRemoteLock::acquire_with_deadline(state_directory, remote_key, deadline)?;
        let conn = open_database(database)?;
        recover_or_block(&conn, remote_key)?;
        let operation_id = random_hex(&conn, 16)?;
        let now = now_ms();
        conn.execute(
            "INSERT INTO host_operations (
               operation_id, remote_key, provider, effect, status,
               holder_pid, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, 'prepared', ?5, ?6, ?6)",
            params![
                operation_id,
                remote_key,
                provider.as_str(),
                effect.as_str(),
                i64::from(std::process::id()),
                now,
            ],
        )?;
        let operation = load_operation(&conn, &operation_id)?
            .ok_or_else(|| HostOperationError::NotFound(operation_id.clone()))?;
        Ok(Self {
            conn,
            _lock: lock,
            operation,
            terminal: false,
        })
    }

    pub fn operation(&self) -> &HostOperation {
        &self.operation
    }

    pub fn mark_running(&mut self) -> Result<(), HostOperationError> {
        self.transition(OperationStatus::Running, OperationStatus::Prepared)
    }

    pub fn finish(&mut self, status: OperationStatus) -> Result<(), HostOperationError> {
        if !matches!(
            status,
            OperationStatus::Succeeded | OperationStatus::Failed | OperationStatus::OutcomeUnknown
        ) {
            return Err(self.transition_error("succeeded, failed, or outcome_unknown"));
        }
        self.transition(status, OperationStatus::Running)?;
        self.terminal = true;
        Ok(())
    }

    fn transition(
        &mut self,
        status: OperationStatus,
        expected: OperationStatus,
    ) -> Result<(), HostOperationError> {
        if self.operation.status != expected {
            return Err(self.transition_error(expected.as_str()));
        }
        let now = now_ms();
        let finished_at = (status != OperationStatus::Running).then_some(now);
        self.conn.execute(
            "UPDATE host_operations SET status=?2, updated_at=?3, finished_at=?4
             WHERE operation_id=?1",
            params![
                self.operation.operation_id,
                status.as_str(),
                now,
                finished_at,
            ],
        )?;
        self.operation.status = status;
        self.operation.updated_at = now;
        self.operation.finished_at = finished_at;
        Ok(())
    }

    fn transition_error(&self, expected: &'static str) -> HostOperationError {
        HostOperationError::InvalidTransition {
            operation_id: self.operation.operation_id.clone(),
            actual: self.operation.status.as_str(),
            expected,
        }
    }
}

impl Drop for HostOperationGuard {
    fn drop(&mut self) {
        if !self.terminal && self.operation.status == OperationStatus::Prepared {
            let now = now_ms();
            crate::warn_unrecorded(
                "mark an abandoned host operation failed",
                self.conn.execute(
                    "UPDATE host_operations SET status='failed', updated_at=?2, finished_at=?2
                     WHERE operation_id=?1 AND status='prepared'",
                    params![self.operation.operation_id, now],
                ),
            );
        }
    }
}

pub fn reconcile_host_operation(
    database: &Path,
    operation_id: &str,
    succeeded: bool,
) -> Result<HostOperation, HostOperationError> {
    reconcile_host_operation_with_deadline(database, operation_id, succeeded, None)
}

pub(crate) fn reconcile_host_operation_with_deadline(
    database: &Path,
    operation_id: &str,
    succeeded: bool,
    deadline: Option<std::time::Instant>,
) -> Result<HostOperation, HostOperationError> {
    let conn = open_database(database)?;
    let operation = load_operation(&conn, operation_id)?
        .ok_or_else(|| HostOperationError::NotFound(operation_id.into()))?;
    let state_directory = database
        .parent()
        .ok_or(HostOperationError::StateDirectoryUnavailable)?;
    let _lock =
        HostRemoteLock::acquire_with_deadline(state_directory, &operation.remote_key, deadline)?;
    let operation = load_operation(&conn, operation_id)?
        .ok_or_else(|| HostOperationError::NotFound(operation_id.into()))?;
    let target = if succeeded {
        OperationStatus::ReconciledSucceeded
    } else {
        OperationStatus::ReconciledFailed
    };
    if operation.status == target {
        return Ok(operation);
    }
    if !matches!(
        operation.status,
        OperationStatus::Prepared | OperationStatus::Running | OperationStatus::OutcomeUnknown
    ) {
        return Err(HostOperationError::InvalidTransition {
            operation_id: operation_id.into(),
            actual: operation.status.as_str(),
            expected: "prepared, running, or outcome_unknown",
        });
    }
    let now = now_ms();
    conn.execute(
        "UPDATE host_operations SET status=?2, updated_at=?3, finished_at=?3
         WHERE operation_id=?1",
        params![operation_id, target.as_str(), now],
    )?;
    load_operation(&conn, operation_id)?
        .ok_or_else(|| HostOperationError::NotFound(operation_id.into()))
}

pub fn host_operation(
    database: &Path,
    operation_id: &str,
) -> Result<Option<HostOperation>, HostOperationError> {
    if !database.exists() {
        return Ok(None);
    }
    load_operation(&open_database(database)?, operation_id)
}

struct HostRemoteLock {
    _lock: ExclusiveFileLock,
    _compatibility: Option<SharedFileLock>,
}

impl HostRemoteLock {
    fn acquire_with_deadline(
        state_directory: &Path,
        remote_key: &str,
        deadline: Option<std::time::Instant>,
    ) -> Result<Self, HostOperationError> {
        let directory = state_directory.join("operation-locks");
        create_private_directory(&directory)?;
        let open = |key: &str| {
            let digest = format!("{:x}", Sha256::digest(key.as_bytes()));
            let path = directory.join(format!("{digest}.lock"));
            let file = open_lock_file(&path).map_err(|source| io_error(&path, source))?;
            protect(&path, false)?;
            Ok::<_, HostOperationError>((path, file))
        };
        let pause = || {
            if deadline.is_some_and(|at| std::time::Instant::now() >= at) {
                Err(HostOperationError::LockDeadline {
                    remote_key: remote_key.into(),
                })
            } else {
                std::thread::sleep(std::time::Duration::from_millis(20));
                Ok(())
            }
        };
        let compatibility = if let Some((parent, _)) = remote_key.split_once("::git-ref/") {
            loop {
                let (path, file) = open(parent)?;
                if let Some(lock) =
                    SharedFileLock::try_acquire(file).map_err(|source| io_error(&path, source))?
                {
                    break Some(lock);
                }
                pause()?;
            }
        } else {
            None
        };
        let lock = loop {
            let (path, file) = open(remote_key)?;
            if let Some(lock) =
                ExclusiveFileLock::try_acquire(file).map_err(|source| io_error(&path, source))?
            {
                break lock;
            }
            pause()?;
        };
        Ok(Self {
            _lock: lock,
            _compatibility: compatibility,
        })
    }
}

fn open_database(path: &Path) -> Result<Connection, HostOperationError> {
    let parent = path
        .parent()
        .ok_or(HostOperationError::StateDirectoryUnavailable)?;
    create_private_directory(parent)?;
    let conn = Connection::open(path)?;
    protect(path, false)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "journal_mode", "wal")?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.execute_batch(SCHEMA)?;
    let version: i64 = conn.query_row(
        "SELECT value FROM meta WHERE key='schema_version'",
        [],
        |row| row.get(0),
    )?;
    if version == 1 {
        // The row shape stays compatible, but v1 writers do not understand
        // parent/resource crash barriers. Their existing version check must
        // fence future writes before scoped Git coordination is enabled.
        conn.execute(
            "UPDATE meta SET value=?1 WHERE key='schema_version' AND value=1",
            [SCHEMA_VERSION],
        )?;
    } else if version != SCHEMA_VERSION {
        return Err(HostOperationError::UnsupportedSchema(version));
    }
    Ok(conn)
}

fn recover_or_block(conn: &Connection, remote_key: &str) -> Result<(), HostOperationError> {
    let mut stmt = conn.prepare(
        "SELECT operation_id, status FROM host_operations
         WHERE (remote_key=?1 OR remote_key=?2
           OR (?2 IS NULL AND substr(remote_key, 1, length(?1 || '::git-ref/')) = ?1 || '::git-ref/'))
           AND status IN ('prepared', 'running', 'outcome_unknown')
         ORDER BY created_at, operation_id",
    )?;
    let rows = stmt
        .query_map(
            params![
                remote_key,
                remote_key
                    .split_once("::git-ref/")
                    .map(|(parent, _)| parent)
            ],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    for (operation_id, status) in rows {
        if status == "prepared" {
            let now = now_ms();
            conn.execute(
                "UPDATE host_operations SET status='failed', updated_at=?2, finished_at=?2
                 WHERE operation_id=?1 AND status='prepared'",
                params![operation_id, now],
            )?;
            continue;
        }
        if status == "running" {
            let now = now_ms();
            conn.execute(
                "UPDATE host_operations SET status='outcome_unknown', updated_at=?2, finished_at=?2
                 WHERE operation_id=?1 AND status='running'",
                params![operation_id, now],
            )?;
        }
        return Err(HostOperationError::Blocked {
            operation_id,
            remote_key: remote_key.into(),
        });
    }
    Ok(())
}

fn load_operation(
    conn: &Connection,
    operation_id: &str,
) -> Result<Option<HostOperation>, HostOperationError> {
    let raw = conn
        .query_row(
            "SELECT operation_id, remote_key, provider, effect, status, holder_pid,
                    created_at, updated_at, finished_at
             FROM host_operations WHERE operation_id=?1",
            [operation_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                ))
            },
        )
        .optional()?;
    let Some((id, key, provider, effect, status, pid, created, updated, finished)) = raw else {
        return Ok(None);
    };
    Ok(Some(HostOperation {
        operation_id: id,
        remote_key: key,
        provider: OperationProvider::parse(&provider)
            .map_err(|_| HostOperationError::InvalidRemoteKey("invalid stored provider"))?,
        effect: OperationEffect::parse(&effect).map_err(|_| HostOperationError::InvalidEffect)?,
        status: OperationStatus::parse(&status)
            .map_err(|_| HostOperationError::InvalidRemoteKey("invalid stored status"))?,
        holder_pid: pid as u32,
        created_at: created,
        updated_at: updated,
        finished_at: finished,
    }))
}

fn random_hex(conn: &Connection, bytes: usize) -> Result<String, HostOperationError> {
    Ok(
        conn.query_row("SELECT lower(hex(randomblob(?1)))", [bytes as i64], |row| {
            row.get(0)
        })?,
    )
}

fn validate_remote_key(remote_key: &str) -> Result<(), HostOperationError> {
    if remote_key.is_empty() || remote_key.chars().any(char::is_control) {
        return Err(HostOperationError::InvalidRemoteKey(
            "empty or contains control characters",
        ));
    }
    if !remote_key.starts_with("local:")
        && (remote_key.contains('@')
            || remote_key.contains("://")
            || remote_key.contains('?')
            || remote_key.contains('#'))
    {
        return Err(HostOperationError::InvalidRemoteKey(
            "network identity contains credentials or URL syntax",
        ));
    }
    Ok(())
}

fn create_private_directory(path: &Path) -> Result<(), HostOperationError> {
    std::fs::create_dir_all(path).map_err(|source| io_error(path, source))?;
    protect(path, true)
}

fn protect(path: &Path, directory: bool) -> Result<(), HostOperationError> {
    crate::host_state::protect_host_state_path(path, directory)
        .map_err(|source| io_error(path, source))
}

fn io_error(path: &Path, source: std::io::Error) -> HostOperationError {
    HostOperationError::Io {
        path: path.into(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_operation_becomes_a_durable_unknown_barrier() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("host-operations.db");
        let key = "github.com/team/repo";
        let mut first = HostOperationGuard::begin(
            &database,
            key,
            OperationProvider::Git,
            OperationEffect::Write,
        )
        .unwrap();
        let first_id = first.operation().operation_id.clone();
        first.mark_running().unwrap();
        drop(first);
        let error = match HostOperationGuard::begin(
            &database,
            key,
            OperationProvider::Github,
            OperationEffect::Write,
        ) {
            Ok(_) => panic!("unresolved operation should block a second clone"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            HostOperationError::Blocked { operation_id, .. } if operation_id == first_id
        ));
        assert_eq!(
            host_operation(&database, &first_id)
                .unwrap()
                .unwrap()
                .status,
            OperationStatus::OutcomeUnknown
        );
        reconcile_host_operation(&database, &first_id, false).unwrap();
        assert!(
            HostOperationGuard::begin(
                &database,
                key,
                OperationProvider::Github,
                OperationEffect::Write,
            )
            .is_ok()
        );
    }

    #[test]
    fn prepared_drop_is_safe_and_credentials_are_refused() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("host-operations.db");
        let prepared = HostOperationGuard::begin(
            &database,
            "github.com/team/repo",
            OperationProvider::Git,
            OperationEffect::Write,
        )
        .unwrap();
        let id = prepared.operation().operation_id.clone();
        drop(prepared);
        assert_eq!(
            host_operation(&database, &id).unwrap().unwrap().status,
            OperationStatus::Failed
        );
        assert!(matches!(
            HostOperationGuard::begin(
                &database,
                "user:secret@github.com/team/repo",
                OperationProvider::Git,
                OperationEffect::Write,
            ),
            Err(HostOperationError::InvalidRemoteKey(_))
        ));
        assert!(!String::from_utf8_lossy(&std::fs::read(database).unwrap()).contains("secret"));
    }

    #[test]
    fn terminal_operation_json_contains_only_canonical_identity() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("host-operations.db");
        let mut guard = HostOperationGuard::begin(
            &database,
            "github.com/team/repo",
            OperationProvider::Git,
            OperationEffect::Destructive,
        )
        .unwrap();
        guard.mark_running().unwrap();
        guard.finish(OperationStatus::Succeeded).unwrap();
        let json = serde_json::to_string(guard.operation()).unwrap();
        assert!(json.contains("github.com/team/repo"));
        assert!(!json.contains('@'));
        assert!(!json.contains("://"));
    }
    #[test]
    fn scoped_host_lanes_coexist_but_deadline_bound_wide_writer_waits() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("host-operations.db");
        let base = "github.com/team/repo";
        let first = HostOperationGuard::begin_with_deadline(
            &database,
            &format!("{base}::git-ref/refs/heads/main"),
            OperationProvider::Git,
            OperationEffect::Write,
            Some(std::time::Instant::now()),
        )
        .unwrap();
        let second = HostOperationGuard::begin_with_deadline(
            &database,
            &format!("{base}::git-ref/refs/heads/topic"),
            OperationProvider::Git,
            OperationEffect::Write,
            Some(std::time::Instant::now()),
        )
        .unwrap();
        let wide = HostOperationGuard::begin_with_deadline(
            &database,
            base,
            OperationProvider::Git,
            OperationEffect::Write,
            Some(std::time::Instant::now()),
        );
        assert!(matches!(wide, Err(HostOperationError::LockDeadline { .. })));
        drop(first);
        drop(second);
        HostOperationGuard::begin_with_deadline(
            &database,
            base,
            OperationProvider::Git,
            OperationEffect::Write,
            Some(std::time::Instant::now()),
        )
        .unwrap();
    }

    #[test]
    fn unknown_scoped_host_write_fences_wide_but_not_unrelated_ref() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("host-operations.db");
        let base = "github.com/team/repo";
        let mut first = HostOperationGuard::begin(
            &database,
            &format!("{base}::git-ref/refs/heads/main"),
            OperationProvider::Git,
            OperationEffect::Write,
        )
        .unwrap();
        first.mark_running().unwrap();
        let id = first.operation().operation_id.clone();
        drop(first);
        let error = HostOperationGuard::begin_with_deadline(
            &database,
            base,
            OperationProvider::Git,
            OperationEffect::Write,
            Some(std::time::Instant::now()),
        );
        assert!(
            matches!(error, Err(HostOperationError::Blocked { operation_id, .. }) if operation_id == id)
        );
        let other = HostOperationGuard::begin_with_deadline(
            &database,
            &format!("{base}::git-ref/refs/heads/other"),
            OperationProvider::Git,
            OperationEffect::Write,
            Some(std::time::Instant::now()),
        )
        .unwrap();
        drop(other);
        assert_eq!(
            host_operation(&database, &id).unwrap().unwrap().status,
            OperationStatus::OutcomeUnknown
        );
    }

    #[test]
    fn host_v1_adoption_preserves_wide_unknown_and_fences_old_version_check() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("host-operations.db");
        let connection = Connection::open(&database).unwrap();
        connection.execute_batch(SCHEMA).unwrap();
        connection
            .execute(
                "INSERT INTO host_operations (operation_id, remote_key, provider, effect,
            status, holder_pid, created_at, updated_at) VALUES ('legacy', 'github.com/team/repo',
            'git', 'write', 'outcome_unknown', 123, 1, 1)",
                [],
            )
            .unwrap();
        drop(connection);
        let result = HostOperationGuard::begin_with_deadline(
            &database,
            "github.com/team/repo::git-ref/refs/heads/main",
            OperationProvider::Git,
            OperationEffect::Write,
            Some(std::time::Instant::now()),
        );
        assert!(
            matches!(result, Err(HostOperationError::Blocked { operation_id, .. }) if operation_id == "legacy")
        );
        let connection = Connection::open(&database).unwrap();
        let version: i64 = connection
            .query_row(
                "SELECT value FROM meta WHERE key='schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, 2);
        assert_ne!(
            version, 1,
            "the old runtime's exact-version check must refuse future writes"
        );
        assert_eq!(
            host_operation(&database, "legacy").unwrap().unwrap().status,
            OperationStatus::OutcomeUnknown
        );
    }
}
