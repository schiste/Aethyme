//! Typed store over `.aethyme/broker.db`.
//!
//! All SQL in the crate lives here and in `schema.rs`. Callers (CLI, TUI,
//! tests) go through these methods only — that is the API-first contract.
//!
//! One `BrokerStore` wraps one connection and is intended per-process
//! (CLI invocations are short-lived). Cross-process safety comes from
//! SQLite WAL + a 5s busy timeout; nothing here assumes in-process locks.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use crate::error::BrokerError;
use crate::schema::{self, EVENTS_SCHEMA_VERSION};
use crate::types::{
    Advisory, AdvisoryResolutionState, AdvisorySeverity, CoordinatedOperation, Event, GateDef,
    GateFailureClass, GateResult, GateStatus, Lease, LeaseKind, MAX_OPERATION_HISTORY_LIMIT,
    MergeQueueEntry, MergeStatus, NewAdvisory, NewCoordinatedOperation, NewGateResult,
    NewPrWatchState, NewSession, OperationEffect, OperationHistoryPage, OperationHistoryQuery,
    OperationIdentityProvenance, OperationProvider, OperationStatus, PrWatchState, Session,
    SessionOrigin, SessionStatus,
};

/// Milliseconds a writer waits on a locked database before erroring.
const BUSY_TIMEOUT_MS: u64 = 5_000;

/// Retries for the fresh-database open race (see [`BrokerStore::open`]).
/// Backoff is 25ms × attempt, so 10 retries bound the wait at ~1.4s —
/// far longer than the one-time WAL switch ever takes.
const OPEN_RETRIES: u64 = 10;

/// Errors that racing fresh openers legitimately see while another
/// connection holds the exclusive lock for the journal-mode switch or
/// the first migration. Everything else is real and must propagate.
fn is_transient_open_error(err: &BrokerError) -> bool {
    let BrokerError::Sqlite(sqlite_err) = err else {
        return false;
    };
    matches!(
        sqlite_err.sqlite_error_code(),
        Some(
            rusqlite::ErrorCode::DatabaseBusy
                | rusqlite::ErrorCode::DatabaseLocked
                | rusqlite::ErrorCode::SystemIoFailure
        )
    )
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub struct BrokerStore {
    conn: Connection,
    path: PathBuf,
    /// Keeps a migrated temporary snapshot alive for the connection lifetime.
    _snapshot_dir: Option<tempfile::TempDir>,
}

/// One queue-row mutation committed with an integration reconciliation.
/// Kept crate-private so the storage transaction remains a broker detail.
pub(crate) struct ReconciliationQueueUpdate {
    pub queue_entry_id: i64,
    pub status: MergeStatus,
    pub merged_tree: Option<String>,
    pub details_json: String,
    pub classification: String,
    pub old_merge_commit: String,
    pub upstream_landing: Option<String>,
    pub replayed_commit: Option<String>,
}

#[derive(Debug)]
pub(crate) struct PreparedIntegrationReconciliation {
    pub branch: String,
    pub upstream_ref: String,
    pub local_main: String,
    pub old_integration: String,
    pub upstream_commit: String,
    pub new_integration: String,
    pub plan_digest: String,
}

impl BrokerStore {
    /// Open (creating and migrating if needed) the broker database for a
    /// repository root: `<repo>/.aethyme/broker.db`.
    pub fn open_in_repo(repo_root: &Path) -> Result<Self, BrokerError> {
        Self::open(&repo_root.join(crate::BROKER_DB_RELPATH))
    }

    /// Open the current broker schema without creating, migrating, or
    /// reconciling any persisted state. Compatibility diagnostics use this
    /// path so an observational command cannot become the write that upgrades
    /// storage or refreshes a session.
    pub fn open_snapshot_in_repo(repo_root: &Path) -> Result<Self, BrokerError> {
        let path = repo_root.join(crate::BROKER_DB_RELPATH);
        if !path.is_file() {
            let conn = Connection::open_in_memory()?;
            schema::migrate(&conn)?;
            conn.pragma_update(None, "query_only", true)?;
            return Ok(Self {
                conn,
                path,
                _snapshot_dir: None,
            });
        }
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS))?;
        let found = schema::current_version(&conn)?;
        if found > crate::SCHEMA_VERSION {
            return Err(BrokerError::SchemaTooNew {
                found,
                supported: crate::SCHEMA_VERSION,
            });
        }
        if found < crate::BROKER_STORAGE_MINIMUM_SCHEMA {
            return Err(BrokerError::SnapshotSchemaMismatch {
                found,
                minimum: crate::BROKER_STORAGE_MINIMUM_SCHEMA,
                maximum: crate::SCHEMA_VERSION,
            });
        }
        if found == crate::SCHEMA_VERSION {
            conn.pragma_update(None, "query_only", true)?;
            return Ok(Self {
                conn,
                path,
                _snapshot_dir: None,
            });
        }

        // SQLite's VACUUM INTO reads a transactionally consistent image,
        // including WAL contents, into a separate file without altering the
        // source. Migrations then run only on that disposable copy.
        let snapshot_dir = tempfile::tempdir().map_err(|source| BrokerError::Io {
            path: std::env::temp_dir(),
            source,
        })?;
        let snapshot_path = snapshot_dir.path().join("broker-snapshot.db");
        conn.execute("VACUUM INTO ?1", [snapshot_path.to_string_lossy().as_ref()])?;
        drop(conn);
        let snapshot = Connection::open(&snapshot_path)?;
        snapshot.busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS))?;
        schema::migrate(&snapshot)?;
        snapshot.pragma_update(None, "query_only", true)?;
        Ok(Self {
            conn: snapshot,
            path,
            _snapshot_dir: Some(snapshot_dir),
        })
    }

    /// Open (creating and migrating if needed) a broker database at an
    /// explicit path. Parent directories are created.
    ///
    /// The very first open of a database is contended in a way steady-state
    /// opens are not: the delete→WAL journal-mode switch takes an exclusive
    /// lock that `busy_timeout` does not reliably cover, so simultaneous
    /// fresh openers can see SQLITE_BUSY — and on macOS the mid-switch
    /// shm/wal transition can surface as SQLITE_IOERR. Both are transient
    /// and resolve as soon as one opener wins, so retry with backoff
    /// instead of failing the losing agents.
    pub fn open(db_path: &Path) -> Result<Self, BrokerError> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| BrokerError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let mut attempt: u64 = 0;
        loop {
            match Self::open_once(db_path) {
                Ok(store) => return Ok(store),
                Err(err) if attempt < OPEN_RETRIES && is_transient_open_error(&err) => {
                    attempt += 1;
                    std::thread::sleep(std::time::Duration::from_millis(25 * attempt));
                }
                Err(err) => return Err(err),
            }
        }
    }

    fn open_once(db_path: &Path) -> Result<Self, BrokerError> {
        let conn = Connection::open(db_path)?;
        conn.busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS))?;
        // WAL: readers never block the writer and vice versa. NORMAL sync
        // is durable-enough for operational state (a crash may lose the
        // last transaction, never corrupt).
        conn.pragma_update(None, "journal_mode", "wal")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        schema::migrate(&conn)?;
        Ok(Self {
            conn,
            path: db_path.to_path_buf(),
            _snapshot_dir: None,
        })
    }

    pub fn db_path(&self) -> &Path {
        &self.path
    }

    /// SQLite integrity check ("ok" when the database is healthy).
    pub fn integrity_check(&self) -> Result<String, BrokerError> {
        Ok(self
            .conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))?)
    }

    // ── sessions ──────────────────────────────────────────────────────

    /// Register a session (adopt an existing worktree, or record a spawn).
    /// Also emits a `session.registered` event in the same transaction.
    pub fn register_session(&mut self, new: &NewSession) -> Result<Session, BrokerError> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        let contract = new.repository_contract.as_ref();
        let inserted = tx.execute(
            "INSERT INTO sessions (worktree_path, branch, origin, status, task, diff_base,
                                   adoption_base, adopted_head, repository_schema,
                                   deployment_state_digest, aethyme_version,
                                   gate_definition_digest, repository_contract_backfilled,
                                   pid, command, log_path, created_at, updated_at,
                                   last_activity_at)
             VALUES (?1, ?2, ?3, 'active', ?4, ?5, COALESCE(?6, ?5),
                     COALESCE(?7, ?6, ?5), ?8, ?9, ?10, ?11, ?12, ?13,
                     ?14, ?15, ?16, ?16, ?16)",
            params![
                new.worktree_path,
                new.branch,
                new.origin.as_str(),
                new.task,
                new.diff_base,
                new.adoption_base,
                new.adopted_head,
                contract.and_then(|value| value.repository_schema),
                contract.map(|value| &value.deployment_state_digest),
                contract.map(|value| &value.aethyme_version),
                contract.and_then(|value| value.gate_definition_digest.as_deref()),
                contract.is_some_and(|value| value.backfilled),
                new.pid,
                new.command,
                new.log_path,
                now,
            ],
        );
        match inserted {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                return Err(BrokerError::WorktreeAlreadyRegistered(
                    new.worktree_path.clone(),
                ));
            }
            Err(err) => return Err(err.into()),
        }
        let id = tx.last_insert_rowid();
        insert_event(
            &tx,
            now,
            crate::events::SESSION_REGISTERED,
            Some(id),
            Some(&crate::events::session_registered_payload(
                new.origin.as_str(),
                &new.branch,
                &new.worktree_path,
            )),
        )?;
        tx.commit()?;
        self.session(id)
    }

    pub fn session(&self, id: i64) -> Result<Session, BrokerError> {
        self.conn
            .query_row(
                &format!("{SESSION_SELECT} WHERE id = ?1"),
                [id],
                session_from_row,
            )
            .optional()?
            .ok_or(BrokerError::SessionNotFound(id))?
    }

    /// The non-cleaned session registered for exactly this worktree
    /// path, if any — what `adopt` consults to give a useful answer
    /// instead of a bare constraint violation.
    pub fn session_for_worktree(
        &self,
        worktree_path: &str,
    ) -> Result<Option<Session>, BrokerError> {
        self.conn
            .query_row(
                &format!(
                    "{SESSION_SELECT} WHERE worktree_path = ?1 AND status != 'cleaned'
                     ORDER BY id DESC LIMIT 1"
                ),
                params![worktree_path],
                session_from_row,
            )
            .optional()?
            .transpose()
    }

    /// Point an existing session at a follow-up task: new task text (when
    /// given), an optional explicitly-safe diff-base refresh, and activity
    /// touched. Plain active reuse preserves the ownership boundary.
    /// Emits `session.reused`.
    pub fn reuse_session(
        &mut self,
        id: i64,
        task: Option<&str>,
        diff_base: Option<&str>,
    ) -> Result<Session, BrokerError> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        let changed = tx.execute(
            "UPDATE sessions SET task = COALESCE(?2, task), diff_base = COALESCE(?3, diff_base),
                                 status = 'active', last_activity_at = ?4, updated_at = ?4
             WHERE id = ?1 AND status != 'cleaned'",
            params![id, task, diff_base, now],
        )?;
        if changed == 0 {
            return Err(BrokerError::SessionNotFound(id));
        }
        insert_event(
            &tx,
            now,
            crate::events::SESSION_REUSED,
            Some(id),
            Some(&crate::events::session_reused_payload(task, diff_base)),
        )?;
        tx.commit()?;
        self.session(id)
    }

    /// All sessions not yet cleaned, oldest first.
    pub fn live_sessions(&self) -> Result<Vec<Session>, BrokerError> {
        let mut stmt = self.conn.prepare(&format!(
            "{SESSION_SELECT} WHERE status <> 'cleaned' ORDER BY id"
        ))?;
        let rows = stmt.query_map([], session_from_row)?;
        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row??);
        }
        Ok(sessions)
    }

    /// Fill the repository contract for a live pre-v9 session exactly once.
    /// The deployment digest is the presence marker because repository schema
    /// and gate digest are both legitimately nullable.
    pub fn backfill_session_repository_contract(
        &mut self,
        id: i64,
        contract: &crate::RepositoryContract,
    ) -> Result<bool, BrokerError> {
        let changed = self.conn.execute(
            "UPDATE sessions
             SET repository_schema = ?2, deployment_state_digest = ?3,
                 aethyme_version = ?4, gate_definition_digest = ?5,
                 repository_contract_backfilled = 1, updated_at = ?6
             WHERE id = ?1 AND status != 'cleaned' AND deployment_state_digest IS NULL",
            params![
                id,
                contract.repository_schema,
                contract.deployment_state_digest,
                contract.aethyme_version,
                contract.gate_definition_digest,
                now_ms(),
            ],
        )?;
        Ok(changed == 1)
    }

    pub fn touch_session_activity(&mut self, id: i64, at_ms: i64) -> Result<(), BrokerError> {
        let changed = self.conn.execute(
            "UPDATE sessions SET last_activity_at = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, at_ms, now_ms()],
        )?;
        if changed == 0 {
            return Err(BrokerError::SessionNotFound(id));
        }
        Ok(())
    }

    /// Transition a session's status; emits `session.<status>` in the same
    /// transaction. `exit_code` is recorded for `Exited`.
    pub fn set_session_status(
        &mut self,
        id: i64,
        status: SessionStatus,
        exit_code: Option<i64>,
    ) -> Result<(), BrokerError> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        let changed = tx.execute(
            "UPDATE sessions SET status = ?2, exit_code = COALESCE(?3, exit_code),
                                 updated_at = ?4
             WHERE id = ?1",
            params![id, status.as_str(), exit_code, now],
        )?;
        if changed == 0 {
            return Err(BrokerError::SessionNotFound(id));
        }
        // `cleaned` is terminal (reuse_session excludes it), so the
        // session's leases can never matter again — drop them in the same
        // transaction. Without this every cleaned session leaves its last
        // implicit-lease snapshot behind forever (722 orphaned rows for
        // ~25 sessions observed in the 2026-07-17 dogfood database).
        if status == SessionStatus::Cleaned {
            tx.execute("DELETE FROM leases WHERE session_id = ?1", [id])?;
            tx.execute(
                "DELETE FROM session_foreign_files WHERE session_id = ?1",
                [id],
            )?;
        }
        insert_event(
            &tx,
            now,
            &format!("session.{}", status.as_str()),
            Some(id),
            exit_code
                .map(crate::events::session_exit_payload)
                .as_deref(),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Atomically close a successfully finished session and persist its
    /// redacted structured handoff after snapshotting leases upstream.
    pub fn finish_session(&mut self, id: i64, handoff_payload: &str) -> Result<(), BrokerError> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        let changed = tx.execute(
            "UPDATE sessions SET status = 'cleaned', updated_at = ?2
             WHERE id = ?1 AND status <> 'cleaned'",
            params![id, now],
        )?;
        if changed == 0 {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?1)",
                [id],
                |row| row.get(0),
            )?;
            if exists {
                return Ok(());
            }
            return Err(BrokerError::SessionNotFound(id));
        }
        tx.execute("DELETE FROM leases WHERE session_id = ?1", [id])?;
        tx.execute(
            "DELETE FROM session_foreign_files WHERE session_id = ?1",
            [id],
        )?;
        insert_event(
            &tx,
            now,
            &format!("session.{}", SessionStatus::Cleaned.as_str()),
            Some(id),
            None,
        )?;
        insert_event(
            &tx,
            now,
            crate::events::SESSION_FINISHED,
            Some(id),
            Some(handoff_payload),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Replace the adoption-time foreign-file snapshot for a session.
    /// These are files that were already untracked when the session began,
    /// so later submit/exec checks can distinguish "mine" from inherited
    /// worktree clutter.
    pub fn set_session_foreign_files(
        &mut self,
        session_id: i64,
        paths: &[String],
    ) -> Result<(), BrokerError> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM session_foreign_files WHERE session_id = ?1",
            [session_id],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO session_foreign_files (session_id, path, created_at)
                 VALUES (?1, ?2, ?3)",
            )?;
            for path in paths {
                stmt.execute(params![session_id, path, now])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Adoption-time untracked paths recorded for one session.
    pub fn session_foreign_files(&self, session_id: i64) -> Result<Vec<String>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT path FROM session_foreign_files
             WHERE session_id = ?1
             ORDER BY path",
        )?;
        let rows = stmt.query_map([session_id], |row| row.get::<_, String>(0))?;
        let mut paths = Vec::new();
        for row in rows {
            paths.push(row?);
        }
        Ok(paths)
    }

    // ── leases ────────────────────────────────────────────────────────

    /// Replace a session's implicit (diff-derived) leases with `paths`.
    /// Explicit leases are untouched.
    pub fn set_implicit_leases(
        &mut self,
        session_id: i64,
        paths: &[String],
    ) -> Result<(), BrokerError> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM leases WHERE session_id = ?1 AND kind = 'implicit'",
            [session_id],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO leases (session_id, path, kind, created_at)
                 VALUES (?1, ?2, 'implicit', ?3)",
            )?;
            for path in paths {
                stmt.execute(params![session_id, path, now])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Claim an explicit lease. `ttl_ms = None` means no expiry.
    pub fn claim_lease(
        &mut self,
        session_id: i64,
        path: &str,
        ttl_ms: Option<i64>,
    ) -> Result<Lease, BrokerError> {
        let now = now_ms();
        let expires_at = ttl_ms.map(|ttl| now + ttl);
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO leases (session_id, path, kind, created_at, expires_at)
             VALUES (?1, ?2, 'explicit', ?3, ?4)
             ON CONFLICT (session_id, path, kind)
             DO UPDATE SET created_at = excluded.created_at,
                           expires_at = excluded.expires_at,
                           released_at = NULL",
            params![session_id, path, now, expires_at],
        )?;
        insert_event(
            &tx,
            now,
            crate::events::LEASE_CLAIMED,
            Some(session_id),
            Some(&crate::events::lease_path_payload(path)),
        )?;
        tx.commit()?;
        let lease = self.conn.query_row(
            &format!("{LEASE_SELECT} WHERE session_id = ?1 AND path = ?2 AND kind = 'explicit'"),
            params![session_id, path],
            lease_from_row,
        )??;
        Ok(lease)
    }

    pub fn release_lease(&mut self, session_id: i64, path: &str) -> Result<(), BrokerError> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE leases SET released_at = ?3
             WHERE session_id = ?1 AND path = ?2 AND released_at IS NULL",
            params![session_id, path, now],
        )?;
        insert_event(
            &tx,
            now,
            crate::events::LEASE_RELEASED,
            Some(session_id),
            Some(&crate::events::lease_path_payload(path)),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Live leases across all live sessions: unreleased, unexpired, and
    /// belonging to a session that is not cleaned/exited. Overlap detection
    /// (Phase 3) is computed over this set.
    pub fn active_leases(&self) -> Result<Vec<Lease>, BrokerError> {
        let now = now_ms();
        let mut stmt = self.conn.prepare(&format!(
            "{LEASE_SELECT}
             WHERE released_at IS NULL
               AND (expires_at IS NULL OR expires_at > ?1)
               AND session_id IN
                   (SELECT id FROM sessions WHERE status IN ('active', 'idle', 'stale'))
             ORDER BY id"
        ))?;
        let rows = stmt.query_map([now], lease_from_row)?;
        let mut leases = Vec::new();
        for row in rows {
            leases.push(row??);
        }
        Ok(leases)
    }

    /// Every lease row recorded for one session, regardless of session or
    /// lease state — introspection for tests and doctor-style audits.
    pub fn session_leases(&self, session_id: i64) -> Result<Vec<Lease>, BrokerError> {
        let mut stmt = self
            .conn
            .prepare(&format!("{LEASE_SELECT} WHERE session_id = ?1 ORDER BY id"))?;
        let rows = stmt.query_map([session_id], lease_from_row)?;
        let mut leases = Vec::new();
        for row in rows {
            leases.push(row??);
        }
        Ok(leases)
    }

    /// Retention sweep for databases written before leases were purged on
    /// clean: drop lease rows whose session is already `cleaned`. Returns
    /// the number removed. Steady-state this is a no-op because
    /// [`Self::set_session_status`] now purges in the same transaction.
    pub fn purge_leases_of_cleaned_sessions(&mut self) -> Result<usize, BrokerError> {
        let removed = self.conn.execute(
            "DELETE FROM leases WHERE session_id IN
                 (SELECT id FROM sessions WHERE status = 'cleaned')",
            [],
        )?;
        Ok(removed)
    }

    // ── gates ─────────────────────────────────────────────────────────

    /// Sync the gate-definition snapshot from parsed `gates.toml` content.
    pub fn upsert_gate(&mut self, gate: &GateDef) -> Result<(), BrokerError> {
        self.conn.execute(
            "INSERT INTO gates (name, command, cost_tier, triggers_json, resources_json,
                                resource_ttl_seconds, resource_wait_seconds,
                                managed_cache_json, definition_hash, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT (name) DO UPDATE SET command = excluded.command,
                                              cost_tier = excluded.cost_tier,
                                              triggers_json = excluded.triggers_json,
                                              resources_json = excluded.resources_json,
                                              resource_ttl_seconds = excluded.resource_ttl_seconds,
                                              resource_wait_seconds = excluded.resource_wait_seconds,
                                              managed_cache_json = excluded.managed_cache_json,
                                              definition_hash = excluded.definition_hash,
                                              updated_at = excluded.updated_at",
            params![
                gate.name,
                gate.command,
                gate.cost_tier,
                gate.triggers_json,
                gate.resources_json,
                gate.resource_ttl_seconds,
                gate.resource_wait_seconds,
                gate.managed_cache_json,
                gate.definition_hash,
                now_ms()
            ],
        )?;
        Ok(())
    }

    pub fn gates(&self) -> Result<Vec<GateDef>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT name, command, cost_tier, triggers_json, resources_json,
                    resource_ttl_seconds, resource_wait_seconds, managed_cache_json,
                    definition_hash, updated_at
             FROM gates ORDER BY cost_tier, name",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(GateDef {
                name: row.get(0)?,
                command: row.get(1)?,
                cost_tier: row.get(2)?,
                triggers_json: row.get(3)?,
                resources_json: row.get(4)?,
                resource_ttl_seconds: row.get(5)?,
                resource_wait_seconds: row.get(6)?,
                managed_cache_json: row.get(7)?,
                definition_hash: row.get(8)?,
                updated_at: row.get(9)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Record a gate run; emits `gate.<status>` in the same transaction.
    pub fn record_gate_result(&mut self, result: &NewGateResult) -> Result<i64, BrokerError> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO gate_results (gate_name, tree_hash, definition_hash, status,
                                       failure_class, exit_code, duration_ms, log_path,
                                       session_id, created_at, wait_duration_ms,
                                       first_output_ms, output_bytes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                result.gate_name,
                result.tree_hash,
                result.definition_hash,
                result.status.as_str(),
                result.failure_class.map(|class| class.as_str()),
                result.exit_code,
                result.duration_ms,
                result.log_path,
                result.session_id,
                now,
                result.wait_duration_ms,
                result.first_output_ms,
                result.output_bytes,
            ],
        )?;
        let id = tx.last_insert_rowid();
        insert_event(
            &tx,
            now,
            &format!("gate.{}", result.status.as_str()),
            result.session_id,
            Some(&crate::events::gate_result_payload(
                &result.gate_name,
                &result.tree_hash,
                result.failure_class,
            )),
        )?;
        tx.commit()?;
        Ok(id)
    }

    /// Cache lookup: the most recent *conclusive* result for this gate
    /// against this exact tree. Passes are conclusive; failures are only
    /// conclusive when classified as real test failures. Cancelled,
    /// error, infra-classified, and legacy unclassified fail rows never
    /// satisfy the cache.
    pub fn cached_gate_result(
        &self,
        gate_name: &str,
        tree_hash: &str,
    ) -> Result<Option<GateResult>, BrokerError> {
        let result = self
            .conn
            .query_row(
                &format!(
                    "{GATE_RESULT_SELECT}
                     WHERE gate_name = ?1 AND tree_hash = ?2
                       AND (status = 'pass'
                            OR (status = 'fail' AND failure_class = 'test_failure'))
                     ORDER BY id DESC LIMIT 1"
                ),
                params![gate_name, tree_hash],
                gate_result_from_row,
            )
            .optional()?;
        result.transpose()
    }

    /// Definition-bound cache lookup used by execution. The two-argument
    /// reader remains available for diagnostics over historical rows.
    pub fn cached_gate_result_for_definition(
        &self,
        gate_name: &str,
        tree_hash: &str,
        definition_hash: &str,
    ) -> Result<Option<GateResult>, BrokerError> {
        let result = self
            .conn
            .query_row(
                &format!(
                    "{GATE_RESULT_SELECT}
                     WHERE gate_name = ?1 AND tree_hash = ?2 AND definition_hash = ?3
                       AND (
                            status = 'pass'
                            OR (status = 'fail' AND failure_class = 'test_failure')
                       )
                     ORDER BY id DESC LIMIT 1"
                ),
                params![gate_name, tree_hash, definition_hash],
                gate_result_from_row,
            )
            .optional()?;
        result.transpose()
    }

    /// Aggregate executed gate runs (pass/fail only): (gate, runs,
    /// total_duration_ms). For the metrics/kill-criterion report.
    pub fn gate_execution_totals(&self) -> Result<Vec<(String, i64, i64)>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT gate_name, COUNT(*), SUM(COALESCE(duration_ms, 0))
             FROM gate_results WHERE status IN ('pass', 'fail')
             GROUP BY gate_name ORDER BY gate_name",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    // ── merge queue ───────────────────────────────────────────────────

    /// Submit a session head. Idempotent per (session, head): resubmitting
    /// the same commit returns the existing entry.
    pub fn submit(
        &mut self,
        session_id: i64,
        head_commit: &str,
        base_commit: &str,
    ) -> Result<MergeQueueEntry, BrokerError> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        let inserted = tx.execute(
            "INSERT INTO merge_queue (session_id, head_commit, base_commit, created_at,
                                      updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT (session_id, head_commit) DO NOTHING",
            params![session_id, head_commit, base_commit, now],
        )?;
        if inserted > 0 {
            insert_event(
                &tx,
                now,
                "merge.submitted",
                Some(session_id),
                Some(&crate::events::merge_submitted_payload(head_commit)),
            )?;
        }
        tx.commit()?;
        let entry = self.conn.query_row(
            &format!("{MERGE_SELECT} WHERE session_id = ?1 AND head_commit = ?2"),
            params![session_id, head_commit],
            merge_from_row,
        )??;
        Ok(entry)
    }

    /// Transition a queue entry; emits `merge.<status>` in the same
    /// transaction and stores updated details/merged tree when given.
    pub fn set_merge_status(
        &mut self,
        entry_id: i64,
        status: MergeStatus,
        merged_tree: Option<&str>,
        details_json: Option<&str>,
    ) -> Result<(), BrokerError> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        let session_id: Option<i64> = tx
            .query_row(
                "SELECT session_id FROM merge_queue WHERE id = ?1",
                [entry_id],
                |row| row.get(0),
            )
            .optional()?;
        tx.execute(
            "UPDATE merge_queue
             SET status = ?2,
                 merged_tree = COALESCE(?3, merged_tree),
                 details_json = COALESCE(?4, details_json),
                 updated_at = ?5
             WHERE id = ?1",
            params![entry_id, status.as_str(), merged_tree, details_json, now],
        )?;
        insert_event(
            &tx,
            now,
            &format!("merge.{}", status.as_str()),
            session_id,
            details_json,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Mark one verified queue entry promoted and advance its session's
    /// accepted contribution checkpoint in the same SQLite transaction.
    pub fn record_merge_promotion(
        &mut self,
        entry_id: i64,
        integration_commit: &str,
        details_json: &str,
    ) -> Result<(), BrokerError> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        let entry = tx
            .query_row(
                "SELECT session_id, head_commit, merged_tree
                 FROM merge_queue WHERE id = ?1 AND status = 'verified'",
                [entry_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((session_id, session_head, Some(integration_tree))) = entry else {
            return Err(BrokerError::SessionNotFound(entry_id));
        };
        tx.execute(
            "UPDATE merge_queue
             SET status = 'promoted', details_json = ?2, updated_at = ?3
             WHERE id = ?1",
            params![entry_id, details_json, now],
        )?;
        update_accepted_checkpoint(
            &tx,
            session_id,
            &session_head,
            integration_commit,
            &integration_tree,
            entry_id,
            now,
        )?;
        insert_event(
            &tx,
            now,
            "merge.promoted",
            Some(session_id),
            Some(details_json),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Mark one simulated queue entry superseded because normalized replay
    /// proved it content-empty, and advance its session's accepted
    /// contribution checkpoint in the same SQLite transaction.
    pub fn record_content_empty_supersession(
        &mut self,
        entry_id: i64,
        integration_commit: &str,
        integration_tree: &str,
        details_json: &str,
    ) -> Result<(), BrokerError> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        let entry = tx
            .query_row(
                "SELECT session_id, head_commit
                 FROM merge_queue WHERE id = ?1 AND status = 'simulating'",
                [entry_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((session_id, session_head)) = entry else {
            return Err(BrokerError::SessionNotFound(entry_id));
        };
        tx.execute(
            "UPDATE merge_queue
             SET status = 'superseded', merged_tree = ?2,
                 details_json = ?3, updated_at = ?4
             WHERE id = ?1",
            params![entry_id, integration_tree, details_json, now],
        )?;
        update_accepted_checkpoint(
            &tx,
            session_id,
            &session_head,
            integration_commit,
            integration_tree,
            entry_id,
            now,
        )?;
        insert_event(
            &tx,
            now,
            "merge.superseded",
            Some(session_id),
            Some(details_json),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn merge_queue(&self) -> Result<Vec<MergeQueueEntry>, BrokerError> {
        let mut stmt = self.conn.prepare(&format!("{MERGE_SELECT} ORDER BY id"))?;
        let rows = stmt.query_map([], merge_from_row)?;
        let mut entries = Vec::new();
        for row in rows {
            entries.push(row??);
        }
        Ok(entries)
    }

    /// Advance the durable session baseline after a successful explicit
    /// rebase. Future repairs must only replay work created after this base.
    pub fn set_session_diff_base(
        &mut self,
        session_id: i64,
        diff_base: &str,
    ) -> Result<(), BrokerError> {
        self.conn.execute(
            "UPDATE sessions SET diff_base = ?2, updated_at = ?3 WHERE id = ?1",
            params![session_id, diff_base, now_ms()],
        )?;
        Ok(())
    }

    /// Persist the complete reconciliation plan before moving its Git ref.
    /// This is phase one of the crash-recoverable ref/database update.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepare_integration_reconciliation(
        &mut self,
        branch: &str,
        upstream_ref: &str,
        local_main: &str,
        old_integration: &str,
        upstream_commit: &str,
        new_integration: &str,
        plan_digest: &str,
        updates: &[ReconciliationQueueUpdate],
    ) -> Result<(), BrokerError> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO integration_reconciliation_intent
                (id, branch, upstream_ref, local_main_commit,
                 old_integration, upstream_commit, new_integration, plan_digest, created_at)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                branch,
                upstream_ref,
                local_main,
                old_integration,
                upstream_commit,
                new_integration,
                plan_digest,
                now,
            ],
        )?;
        for update in updates {
            tx.execute(
                "INSERT INTO integration_reconciliation_intent_entries
                    (queue_entry_id, status, merged_tree, details_json,
                     classification, old_merge_commit, upstream_landing,
                     replayed_commit)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    update.queue_entry_id,
                    update.status.as_str(),
                    update.merged_tree,
                    update.details_json,
                    update.classification,
                    update.old_merge_commit,
                    update.upstream_landing,
                    update.replayed_commit,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn prepared_integration_reconciliation(
        &self,
    ) -> Result<Option<PreparedIntegrationReconciliation>, BrokerError> {
        self.conn
            .query_row(
                "SELECT branch, upstream_ref, local_main_commit,
                        old_integration, upstream_commit, new_integration, plan_digest
                 FROM integration_reconciliation_intent WHERE id = 1",
                [],
                |row| {
                    Ok(PreparedIntegrationReconciliation {
                        branch: row.get(0)?,
                        upstream_ref: row.get(1)?,
                        local_main: row.get(2)?,
                        old_integration: row.get(3)?,
                        upstream_commit: row.get(4)?,
                        new_integration: row.get(5)?,
                        plan_digest: row.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Phase two: apply all queue rows, audit rows, and events and remove
    /// the durable intent in the same SQLite transaction.
    pub(crate) fn finalize_integration_reconciliation(&mut self) -> Result<(), BrokerError> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        let prepared = tx.query_row(
            "SELECT branch, upstream_ref, local_main_commit,
                    old_integration, upstream_commit, new_integration, plan_digest
             FROM integration_reconciliation_intent WHERE id = 1",
            [],
            |row| {
                Ok(PreparedIntegrationReconciliation {
                    branch: row.get(0)?,
                    upstream_ref: row.get(1)?,
                    local_main: row.get(2)?,
                    old_integration: row.get(3)?,
                    upstream_commit: row.get(4)?,
                    new_integration: row.get(5)?,
                    plan_digest: row.get(6)?,
                })
            },
        )?;
        let raw_updates = {
            let mut stmt = tx.prepare(
                "SELECT queue_entry_id, status, merged_tree, details_json,
                        classification, old_merge_commit, upstream_landing,
                        replayed_commit
                 FROM integration_reconciliation_intent_entries
                 ORDER BY queue_entry_id",
            )?;
            stmt.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
        };
        let updates = raw_updates
            .into_iter()
            .map(
                |(
                    queue_entry_id,
                    status,
                    merged_tree,
                    details_json,
                    classification,
                    old_merge_commit,
                    upstream_landing,
                    replayed_commit,
                )| {
                    Ok(ReconciliationQueueUpdate {
                        queue_entry_id,
                        status: MergeStatus::parse(&status)?,
                        merged_tree,
                        details_json,
                        classification,
                        old_merge_commit,
                        upstream_landing,
                        replayed_commit,
                    })
                },
            )
            .collect::<Result<Vec<_>, BrokerError>>()?;

        tx.execute(
            "INSERT INTO integration_reconciliations
                (upstream_ref, local_main_commit, old_integration,
                 upstream_commit, new_integration, plan_digest, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                prepared.upstream_ref,
                prepared.local_main,
                prepared.old_integration,
                prepared.upstream_commit,
                prepared.new_integration,
                prepared.plan_digest,
                now,
            ],
        )?;
        let reconciliation_id = tx.last_insert_rowid();
        for update in updates {
            let session_id: i64 = tx.query_row(
                "SELECT session_id FROM merge_queue WHERE id = ?1",
                [update.queue_entry_id],
                |row| row.get(0),
            )?;
            tx.execute(
                "UPDATE merge_queue
                 SET status = ?2,
                     merged_tree = COALESCE(?3, merged_tree),
                     details_json = ?4,
                     updated_at = ?5
                 WHERE id = ?1",
                params![
                    update.queue_entry_id,
                    update.status.as_str(),
                    update.merged_tree,
                    update.details_json,
                    now,
                ],
            )?;
            if update.status == MergeStatus::ExternallyLanded {
                insert_event(
                    &tx,
                    now,
                    "merge.externally_landed",
                    Some(session_id),
                    Some(&update.details_json),
                )?;
            }
            tx.execute(
                "INSERT INTO integration_reconciliation_entries
                    (reconciliation_id, queue_entry_id, classification,
                     old_merge_commit, upstream_landing, replayed_commit,
                     details_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    reconciliation_id,
                    update.queue_entry_id,
                    update.classification,
                    update.old_merge_commit,
                    update.upstream_landing,
                    update.replayed_commit,
                    update.details_json,
                ],
            )?;
        }
        tx.execute("DELETE FROM integration_reconciliation_intent_entries", [])?;
        tx.execute(
            "DELETE FROM integration_reconciliation_intent WHERE id = 1",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn abort_integration_reconciliation(&mut self) -> Result<(), BrokerError> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM integration_reconciliation_intent_entries", [])?;
        tx.execute(
            "DELETE FROM integration_reconciliation_intent WHERE id = 1",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    // ── PR watch state ───────────────────────────────────────────────

    /// Fetch the durable cursor for one PR follow-up watch.
    pub fn pr_watch_state(
        &self,
        target_branch: &str,
        pr_number: i64,
    ) -> Result<Option<PrWatchState>, BrokerError> {
        self.conn
            .query_row(
                &format!("{PR_WATCH_SELECT} WHERE target_branch = ?1 AND pr_number = ?2"),
                params![target_branch, pr_number],
                pr_watch_from_row,
            )
            .optional()?
            .transpose()
    }

    /// Insert or update one PR follow-up cursor.
    pub fn upsert_pr_watch_state(
        &mut self,
        state: &NewPrWatchState,
    ) -> Result<PrWatchState, BrokerError> {
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO pr_watch_state (
                 target_branch, pr_number, activity_fingerprint, marker,
                 last_dispatch_at, last_agent_session_id, updated_at
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(target_branch, pr_number) DO UPDATE SET
                 activity_fingerprint = excluded.activity_fingerprint,
                 marker = excluded.marker,
                 last_dispatch_at = excluded.last_dispatch_at,
                 last_agent_session_id = excluded.last_agent_session_id,
                 updated_at = excluded.updated_at",
            params![
                state.target_branch,
                state.pr_number,
                state.activity_fingerprint,
                state.marker,
                state.last_dispatch_at,
                state.last_agent_session_id,
                now,
            ],
        )?;
        Ok(self
            .pr_watch_state(&state.target_branch, state.pr_number)?
            .expect("upserted pr_watch_state row should be readable"))
    }

    // ── coordinated operations ───────────────────────────────────────

    pub fn create_coordinated_operation(
        &mut self,
        operation: &NewCoordinatedOperation,
    ) -> Result<CoordinatedOperation, BrokerError> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO coordinated_operations (
                 session_id, provider, repository, scope, effect, status,
                 authorization_reason, command_json, pid, created_at, updated_at,
                 host_operation_id, identity_provenance
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'prepared', ?6, ?7, ?8, ?9, ?9,
                       ?10, ?11)",
            params![
                operation.session_id,
                operation.provider.as_str(),
                operation.repository,
                operation.scope,
                operation.effect.as_str(),
                operation.authorization_reason,
                operation.command_json,
                operation.pid,
                now,
                operation.host_operation_id,
                operation.identity_provenance.as_str(),
            ],
        )?;
        let id = tx.last_insert_rowid();
        let payload = crate::events::operation_payload(
            id,
            operation.provider,
            &operation.repository,
            &operation.scope,
            operation.effect,
            OperationStatus::Prepared,
            None,
        );
        insert_event(
            &tx,
            now,
            "operation.prepared",
            Some(operation.session_id),
            Some(&payload),
        )?;
        tx.commit()?;
        self.coordinated_operation(id)?
            .ok_or(BrokerError::CoordinatedOperationNotFound(id))
    }

    pub fn coordinated_operation(
        &self,
        id: i64,
    ) -> Result<Option<CoordinatedOperation>, BrokerError> {
        self.conn
            .query_row(
                "SELECT id, session_id, provider, repository, scope, effect,
                        status, authorization_reason, command_json, pid,
                        exit_code, details_json,
                        created_at, updated_at, finished_at,
                        host_operation_id, identity_provenance
                 FROM coordinated_operations WHERE id = ?1",
                [id],
                coordinated_operation_from_row,
            )
            .optional()?
            .transpose()
    }

    pub fn coordinated_operations(&self) -> Result<Vec<CoordinatedOperation>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, provider, repository, scope, effect,
                    status, authorization_reason, command_json, pid,
                    exit_code, details_json,
                    created_at, updated_at, finished_at,
                    host_operation_id, identity_provenance
             FROM coordinated_operations ORDER BY id",
        )?;
        let rows = stmt.query_map([], coordinated_operation_from_row)?;
        let mut operations = Vec::new();
        for row in rows {
            operations.push(row??);
        }
        Ok(operations)
    }

    /// Query one stable newest-first page of coordinated-operation history.
    ///
    /// Every value is bound, while the SQL shape contains only broker-owned
    /// column predicates. `before_id` is exclusive so a caller can pass
    /// `next_before_id` directly without duplicates.
    pub fn operation_history(
        &self,
        query: &OperationHistoryQuery,
    ) -> Result<OperationHistoryPage, BrokerError> {
        if query.limit == 0 || query.limit > MAX_OPERATION_HISTORY_LIMIT {
            return Err(BrokerError::InvalidOperationHistoryLimit {
                limit: query.limit,
                maximum: MAX_OPERATION_HISTORY_LIMIT,
            });
        }

        let mut sql = String::from(
            "SELECT id, session_id, provider, repository, scope, effect,
                    status, authorization_reason, command_json, pid,
                    exit_code, details_json,
                    created_at, updated_at, finished_at,
                    host_operation_id, identity_provenance
             FROM coordinated_operations",
        );
        let mut clauses = Vec::new();
        let mut values = Vec::<rusqlite::types::Value>::new();
        if let Some(before_id) = query.before_id {
            clauses.push("id < ?");
            values.push(before_id.into());
        }
        if let Some(session_id) = query.session_id {
            clauses.push("session_id = ?");
            values.push(session_id.into());
        }
        if let Some(status) = query.status {
            clauses.push("status = ?");
            values.push(status.as_str().to_owned().into());
        }
        if let Some(repository) = &query.repository {
            clauses.push("repository = ?");
            values.push(repository.clone().into());
        }
        if let Some(provider) = query.provider {
            clauses.push("provider = ?");
            values.push(provider.as_str().to_owned().into());
        }
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        sql.push_str(" ORDER BY id DESC LIMIT ?");
        values.push((i64::from(query.limit) + 1).into());

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(values.iter()),
            coordinated_operation_from_row,
        )?;
        let mut operations = Vec::new();
        for row in rows {
            operations.push(row??);
        }
        let has_more = operations.len() > query.limit as usize;
        operations.truncate(query.limit as usize);
        let next_before_id = has_more
            .then(|| operations.last().map(|operation| operation.id))
            .flatten();
        Ok(OperationHistoryPage {
            operations,
            next_before_id,
        })
    }

    /// Most recent coordinated operations for a report, newest first.
    /// When a session is selected, unrelated operations stay out of the
    /// snapshot rather than broadening its diagnostic scope.
    pub(crate) fn recent_coordinated_operations(
        &self,
        limit: i64,
        session_id: Option<i64>,
    ) -> Result<Vec<CoordinatedOperation>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, provider, repository, scope, effect,
                    status, authorization_reason, command_json, pid,
                    exit_code, details_json,
                    created_at, updated_at, finished_at,
                    host_operation_id, identity_provenance
             FROM coordinated_operations
             WHERE (?2 IS NULL OR session_id = ?2)
             ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit, session_id], coordinated_operation_from_row)?;
        let mut operations = Vec::new();
        for row in rows {
            operations.push(row??);
        }
        Ok(operations)
    }

    pub fn unresolved_coordinated_operations(
        &self,
        repository: &str,
    ) -> Result<Vec<CoordinatedOperation>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, provider, repository, scope, effect,
                    status, authorization_reason, command_json, pid,
                    exit_code, details_json,
                    created_at, updated_at, finished_at,
                    host_operation_id, identity_provenance
             FROM coordinated_operations
             WHERE repository = ?1
               AND effect <> 'read'
               AND status IN ('prepared', 'running', 'outcome_unknown')
             ORDER BY id",
        )?;
        let rows = stmt.query_map([repository], coordinated_operation_from_row)?;
        let mut operations = Vec::new();
        for row in rows {
            operations.push(row??);
        }
        Ok(operations)
    }

    pub fn transition_coordinated_operation(
        &mut self,
        id: i64,
        status: OperationStatus,
        exit_code: Option<i64>,
        details_json: Option<&str>,
    ) -> Result<CoordinatedOperation, BrokerError> {
        let operation = self
            .coordinated_operation(id)?
            .ok_or(BrokerError::CoordinatedOperationNotFound(id))?;
        let now = now_ms();
        let finished_at = matches!(
            status,
            OperationStatus::Succeeded
                | OperationStatus::Failed
                | OperationStatus::OutcomeUnknown
                | OperationStatus::ReconciledSucceeded
                | OperationStatus::ReconciledFailed
        )
        .then_some(now);
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE coordinated_operations
             SET status = ?2, exit_code = ?3, details_json = ?4,
                 updated_at = ?5, finished_at = ?6
             WHERE id = ?1",
            params![
                id,
                status.as_str(),
                exit_code,
                details_json,
                now,
                finished_at,
            ],
        )?;
        let payload = crate::events::operation_payload(
            id,
            operation.provider,
            &operation.repository,
            &operation.scope,
            operation.effect,
            status,
            exit_code,
        );
        insert_event(
            &tx,
            now,
            &format!("operation.{}", status.as_str()),
            Some(operation.session_id),
            Some(&payload),
        )?;
        tx.commit()?;
        self.coordinated_operation(id)?
            .ok_or(BrokerError::CoordinatedOperationNotFound(id))
    }

    // ── non-blocking advisories ─────────────────────────────────────

    /// Persist one immutable advisory idempotently by producer identity.
    /// Reusing an identity with different data is refused rather than
    /// rewriting historical evidence.
    pub fn record_advisory(&mut self, advisory: &NewAdvisory) -> Result<Advisory, BrokerError> {
        let paths_json =
            serde_json::to_string(&advisory.paths).expect("serializing advisory paths cannot fail");
        let evidence_json = serde_json::to_string(&advisory.evidence)
            .expect("serializing advisory evidence cannot fail");
        let now = now_ms();
        let tx = self.conn.transaction()?;
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO advisories (
                 identity, session_id, severity, queue_entry_id, integration_sha,
                 paths_json, evidence_json, created_at, resolution_state
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'outstanding')",
            params![
                advisory.identity,
                advisory.session_id,
                advisory.severity.as_str(),
                advisory.queue_entry_id,
                advisory.integration_sha,
                paths_json,
                evidence_json,
                now,
            ],
        )?;
        let _ = inserted;
        tx.commit()?;

        let stored = self
            .advisory_by_identity(&advisory.identity)?
            .expect("insert or existing advisory identity must resolve");
        if stored.session_id != advisory.session_id
            || stored.severity != advisory.severity
            || stored.queue_entry_id != advisory.queue_entry_id
            || stored.integration_sha != advisory.integration_sha
            || stored.paths != advisory.paths
            || stored.evidence != advisory.evidence
        {
            return Err(BrokerError::AdvisoryIdentityConflict(
                advisory.identity.clone(),
            ));
        }
        Ok(stored)
    }

    /// Exact durable advisory lookup.
    pub fn advisory(&self, id: i64) -> Result<Option<Advisory>, BrokerError> {
        self.conn
            .query_row(
                &(ADVISORY_SELECT.to_owned() + " WHERE id = ?1"),
                [id],
                advisory_from_row,
            )
            .optional()?
            .transpose()
    }

    fn advisory_by_identity(&self, identity: &str) -> Result<Option<Advisory>, BrokerError> {
        self.conn
            .query_row(
                &(ADVISORY_SELECT.to_owned() + " WHERE identity = ?1"),
                [identity],
                advisory_from_row,
            )
            .optional()?
            .transpose()
    }

    /// Newest-first advisory inventory. The default operator view contains
    /// only outstanding rows; history remains available with `include_all`.
    pub fn advisories(&self, include_all: bool) -> Result<Vec<Advisory>, BrokerError> {
        let sql = if include_all {
            ADVISORY_SELECT.to_owned() + " ORDER BY id DESC"
        } else {
            ADVISORY_SELECT.to_owned() + " WHERE resolution_state = 'outstanding' ORDER BY id DESC"
        };
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], advisory_from_row)?;
        rows.map(|row| row?).collect()
    }

    /// Outstanding advisories for one session, oldest first so repeated
    /// command-boundary notices stay deterministic and preserve chronology.
    pub fn outstanding_advisories_for_session(
        &self,
        session_id: i64,
    ) -> Result<Vec<Advisory>, BrokerError> {
        let mut stmt = self.conn.prepare(&format!(
            "{ADVISORY_SELECT}
             WHERE session_id = ?1 AND resolution_state = 'outstanding'
             ORDER BY id"
        ))?;
        let rows = stmt.query_map([session_id], advisory_from_row)?;
        rows.map(|row| row?).collect()
    }

    /// Idempotently acknowledge one advisory without deleting its evidence.
    pub fn acknowledge_advisory(&mut self, id: i64) -> Result<Advisory, BrokerError> {
        let existing = self
            .advisory(id)?
            .ok_or(BrokerError::AdvisoryNotFound(id))?;
        if existing.resolution_state == AdvisoryResolutionState::Acknowledged {
            return Ok(existing);
        }

        let now = now_ms();
        let tx = self.conn.transaction()?;
        let updated = tx.execute(
            "UPDATE advisories
             SET resolution_state = 'acknowledged', acknowledged_at = ?2
             WHERE id = ?1 AND resolution_state = 'outstanding'",
            params![id, now],
        )?;
        let _ = updated;
        tx.commit()?;
        self.advisory(id)?.ok_or(BrokerError::AdvisoryNotFound(id))
    }

    // ── events ────────────────────────────────────────────────────────

    /// Append one event. Most mutations already emit their own event in
    /// the same transaction; this is for kinds with no store mutation
    /// (e.g. `lease.overlap`, `worktree.stale`).
    pub fn append_event(
        &mut self,
        kind: &str,
        session_id: Option<i64>,
        payload_json: Option<&str>,
    ) -> Result<i64, BrokerError> {
        let tx = self.conn.transaction()?;
        let id = insert_event(&tx, now_ms(), kind, session_id, payload_json)?;
        tx.commit()?;
        Ok(id)
    }

    /// Events with id > `after_id`, optionally filtered to kinds starting
    /// with `kind_prefix` (e.g. "merge." or the exact "lease.overlap"),
    /// oldest first, up to `limit`.
    pub fn events_after_filtered(
        &self,
        after_id: i64,
        limit: i64,
        kind_prefix: Option<&str>,
    ) -> Result<Vec<Event>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, schema_version, ts, kind, session_id, payload_json
             FROM events WHERE id > ?1 AND (?3 IS NULL OR kind LIKE ?3 || '%')
             ORDER BY id LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![after_id, limit, kind_prefix], |row| {
            Ok(Event {
                id: row.get(0)?,
                schema_version: row.get(1)?,
                ts: row.get(2)?,
                kind: row.get(3)?,
                session_id: row.get(4)?,
                payload_json: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Retention: delete events strictly older than `before_ts_ms`,
    /// returning the number removed. Event ids stay strictly increasing
    /// forever (AUTOINCREMENT never reuses rowids), so existing `--since`
    /// cursors remain valid after a prune. This is an explicit operator
    /// action — the log is append-only in normal operation.
    pub fn prune_events_before(&mut self, before_ts_ms: i64) -> Result<usize, BrokerError> {
        let removed = self
            .conn
            .execute("DELETE FROM events WHERE ts < ?1", [before_ts_ms])?;
        Ok(removed)
    }

    /// Events with id > `after_id`, oldest first, up to `limit`. This is
    /// the tail/replay cursor API (`events --follow` polls it).
    pub fn events_after(&self, after_id: i64, limit: i64) -> Result<Vec<Event>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, schema_version, ts, kind, session_id, payload_json
             FROM events WHERE id > ?1 ORDER BY id LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![after_id, limit], |row| {
            Ok(Event {
                id: row.get(0)?,
                schema_version: row.get(1)?,
                ts: row.get(2)?,
                kind: row.get(3)?,
                session_id: row.get(4)?,
                payload_json: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Most recent event rows for an offline report, newest first.
    pub(crate) fn recent_events(
        &self,
        limit: i64,
        session_id: Option<i64>,
    ) -> Result<Vec<Event>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, schema_version, ts, kind, session_id, payload_json
             FROM events
             WHERE (?2 IS NULL OR session_id = ?2)
             ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit, session_id], event_from_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Most recent gate events for report provenance, including cache hits
    /// (which intentionally do not duplicate rows in `gate_results`).
    pub(crate) fn recent_gate_events(
        &self,
        limit: i64,
        session_id: Option<i64>,
    ) -> Result<Vec<Event>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, schema_version, ts, kind, session_id, payload_json
             FROM events
             WHERE kind IN ('gate.pass', 'gate.fail', 'gate.cancelled', 'gate.error', 'gate.cached')
               AND (?2 IS NULL OR session_id = ?2)
             ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit, session_id], event_from_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Latest executed or cache-resolved gate activity for one session.
    pub fn latest_session_gate_event(&self, session_id: i64) -> Result<Option<Event>, BrokerError> {
        self.conn
            .query_row(
                "SELECT id, schema_version, ts, kind, session_id, payload_json
                 FROM events
                 WHERE session_id = ?1
                   AND kind IN ('gate.pass', 'gate.fail', 'gate.cancelled', 'gate.error', 'gate.cached')
                 ORDER BY id DESC LIMIT 1",
                [session_id],
                |row| {
                    Ok(Event {
                        id: row.get(0)?,
                        schema_version: row.get(1)?,
                        ts: row.get(2)?,
                        kind: row.get(3)?,
                        session_id: row.get(4)?,
                        payload_json: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(BrokerError::from)
    }

    /// Latest durable finish handoff for one session.
    pub fn latest_session_finished_event(
        &self,
        session_id: i64,
    ) -> Result<Option<Event>, BrokerError> {
        self.conn
            .query_row(
                "SELECT id, schema_version, ts, kind, session_id, payload_json
                 FROM events
                 WHERE session_id = ?1 AND kind = ?2
                 ORDER BY id DESC LIMIT 1",
                params![session_id, crate::events::SESSION_FINISHED],
                event_from_row,
            )
            .optional()
            .map_err(BrokerError::from)
    }

    /// Latest durable finish handoff across all sessions registered for
    /// exactly one worktree path, including cleaned sessions.
    pub fn latest_worktree_finished_event(
        &self,
        worktree_path: &str,
    ) -> Result<Option<Event>, BrokerError> {
        self.conn
            .query_row(
                "SELECT events.id, events.schema_version, events.ts, events.kind,
                        events.session_id, events.payload_json
                 FROM events
                 JOIN sessions ON sessions.id = events.session_id
                 WHERE sessions.worktree_path = ?1 AND events.kind = ?2
                 ORDER BY events.id DESC LIMIT 1",
                params![worktree_path, crate::events::SESSION_FINISHED],
                event_from_row,
            )
            .optional()
            .map_err(BrokerError::from)
    }
}

// ── row mapping helpers ──────────────────────────────────────────────

const SESSION_SELECT: &str = "SELECT id, worktree_path, branch, origin, status, task, \
     diff_base, adoption_base, adopted_head, accepted_session_head, \
     accepted_integration_commit, accepted_integration_tree, accepted_queue_entry_id, \
     accepted_at, repository_schema, deployment_state_digest, aethyme_version, \
     gate_definition_digest, repository_contract_backfilled, pid, command, log_path, \
     exit_code, created_at, updated_at, last_activity_at \
     FROM sessions";

const LEASE_SELECT: &str =
    "SELECT id, session_id, path, kind, created_at, expires_at, released_at FROM leases";

const GATE_RESULT_SELECT: &str = "SELECT id, gate_name, tree_hash, definition_hash, status, \
     failure_class, exit_code, duration_ms, log_path, session_id, created_at, wait_duration_ms, \
     first_output_ms, output_bytes FROM gate_results";

const MERGE_SELECT: &str = "SELECT id, session_id, head_commit, base_commit, status, \
     merged_tree, details_json, created_at, updated_at FROM merge_queue";

const PR_WATCH_SELECT: &str = "SELECT id, target_branch, pr_number, activity_fingerprint, \
     marker, last_dispatch_at, last_agent_session_id, updated_at FROM pr_watch_state";

const ADVISORY_SELECT: &str = "SELECT id, identity, session_id, severity, queue_entry_id, \
     integration_sha, paths_json, evidence_json, created_at, resolution_state, acknowledged_at \
     FROM advisories";

type RowResult<T> = Result<Result<T, BrokerError>, rusqlite::Error>;

fn event_from_row(row: &rusqlite::Row<'_>) -> Result<Event, rusqlite::Error> {
    Ok(Event {
        id: row.get(0)?,
        schema_version: row.get(1)?,
        ts: row.get(2)?,
        kind: row.get(3)?,
        session_id: row.get(4)?,
        payload_json: row.get(5)?,
    })
}

fn session_from_row(row: &rusqlite::Row<'_>) -> RowResult<Session> {
    let origin: String = row.get(3)?;
    let status: String = row.get(4)?;
    let repository_schema: Option<u32> = row.get(14)?;
    let deployment_state_digest: Option<String> = row.get(15)?;
    let aethyme_version: Option<String> = row.get(16)?;
    let gate_definition_digest: Option<String> = row.get(17)?;
    let repository_contract_backfilled: bool = row.get(18)?;
    let repository_contract = deployment_state_digest.zip(aethyme_version).map(
        |(deployment_state_digest, aethyme_version)| crate::RepositoryContract {
            repository_schema,
            deployment_state_digest,
            aethyme_version,
            gate_definition_digest,
            backfilled: repository_contract_backfilled,
        },
    );
    Ok((|| {
        Ok(Session {
            id: row.get(0)?,
            worktree_path: row.get(1)?,
            branch: row.get(2)?,
            origin: SessionOrigin::parse(&origin)?,
            status: SessionStatus::parse(&status)?,
            task: row.get(5)?,
            diff_base: row.get(6)?,
            adoption_base: row.get(7)?,
            adopted_head: row.get(8)?,
            accepted_session_head: row.get(9)?,
            accepted_integration_commit: row.get(10)?,
            accepted_integration_tree: row.get(11)?,
            accepted_queue_entry_id: row.get(12)?,
            accepted_at: row.get(13)?,
            repository_contract,
            pid: row.get(19)?,
            command: row.get(20)?,
            log_path: row.get(21)?,
            exit_code: row.get(22)?,
            created_at: row.get(23)?,
            updated_at: row.get(24)?,
            last_activity_at: row.get(25)?,
        })
    })())
}

fn lease_from_row(row: &rusqlite::Row<'_>) -> RowResult<Lease> {
    let kind: String = row.get(3)?;
    Ok((|| {
        Ok(Lease {
            id: row.get(0)?,
            session_id: row.get(1)?,
            path: row.get(2)?,
            kind: LeaseKind::parse(&kind)?,
            created_at: row.get(4)?,
            expires_at: row.get(5)?,
            released_at: row.get(6)?,
        })
    })())
}

fn gate_result_from_row(row: &rusqlite::Row<'_>) -> RowResult<GateResult> {
    let status: String = row.get(4)?;
    let failure_class: Option<String> = row.get(5)?;
    Ok((|| {
        Ok(GateResult {
            id: row.get(0)?,
            gate_name: row.get(1)?,
            tree_hash: row.get(2)?,
            definition_hash: row.get(3)?,
            status: GateStatus::parse(&status)?,
            failure_class: failure_class
                .as_deref()
                .map(GateFailureClass::parse)
                .transpose()?,
            exit_code: row.get(6)?,
            duration_ms: row.get(7)?,
            log_path: row.get(8)?,
            session_id: row.get(9)?,
            created_at: row.get(10)?,
            wait_duration_ms: row.get(11)?,
            first_output_ms: row.get(12)?,
            output_bytes: row.get(13)?,
        })
    })())
}

fn merge_from_row(row: &rusqlite::Row<'_>) -> RowResult<MergeQueueEntry> {
    let status: String = row.get(4)?;
    Ok((|| {
        Ok(MergeQueueEntry {
            id: row.get(0)?,
            session_id: row.get(1)?,
            head_commit: row.get(2)?,
            base_commit: row.get(3)?,
            status: MergeStatus::parse(&status)?,
            merged_tree: row.get(5)?,
            details_json: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
        })
    })())
}

fn pr_watch_from_row(row: &rusqlite::Row<'_>) -> RowResult<PrWatchState> {
    Ok(Ok(PrWatchState {
        id: row.get(0)?,
        target_branch: row.get(1)?,
        pr_number: row.get(2)?,
        activity_fingerprint: row.get(3)?,
        marker: row.get(4)?,
        last_dispatch_at: row.get(5)?,
        last_agent_session_id: row.get(6)?,
        updated_at: row.get(7)?,
    }))
}

fn coordinated_operation_from_row(row: &rusqlite::Row<'_>) -> RowResult<CoordinatedOperation> {
    let provider: String = row.get(2)?;
    let effect: String = row.get(5)?;
    let status: String = row.get(6)?;
    let identity_provenance: String = row.get(16)?;
    Ok((|| {
        Ok(CoordinatedOperation {
            id: row.get(0)?,
            session_id: row.get(1)?,
            provider: OperationProvider::parse(&provider)?,
            repository: row.get(3)?,
            scope: row.get(4)?,
            effect: OperationEffect::parse(&effect)?,
            status: OperationStatus::parse(&status)?,
            authorization_reason: row.get(7)?,
            command_json: row.get(8)?,
            pid: row.get(9)?,
            exit_code: row.get(10)?,
            details_json: row.get(11)?,
            created_at: row.get(12)?,
            updated_at: row.get(13)?,
            finished_at: row.get(14)?,
            host_operation_id: row.get(15)?,
            identity_provenance: OperationIdentityProvenance::parse(&identity_provenance)?,
        })
    })())
}

fn advisory_from_row(row: &rusqlite::Row<'_>) -> RowResult<Advisory> {
    let id = row.get(0)?;
    let severity: String = row.get(3)?;
    let paths_json: String = row.get(6)?;
    let evidence_json: String = row.get(7)?;
    let resolution_state: String = row.get(9)?;
    Ok((|| {
        Ok(Advisory {
            id,
            identity: row.get(1)?,
            session_id: row.get(2)?,
            severity: AdvisorySeverity::parse(&severity)?,
            queue_entry_id: row.get(4)?,
            integration_sha: row.get(5)?,
            paths: serde_json::from_str(&paths_json).map_err(|source| {
                BrokerError::InvalidAdvisoryJson {
                    id,
                    field: "paths_json",
                    source,
                }
            })?,
            evidence: serde_json::from_str(&evidence_json).map_err(|source| {
                BrokerError::InvalidAdvisoryJson {
                    id,
                    field: "evidence_json",
                    source,
                }
            })?,
            created_at: row.get(8)?,
            resolution_state: AdvisoryResolutionState::parse(&resolution_state)?,
            acknowledged_at: row.get(10)?,
        })
    })())
}

fn update_accepted_checkpoint(
    conn: &Connection,
    session_id: i64,
    session_head: &str,
    integration_commit: &str,
    integration_tree: &str,
    queue_entry_id: i64,
    accepted_at: i64,
) -> Result<(), BrokerError> {
    let updated = conn.execute(
        "UPDATE sessions
         SET accepted_session_head = ?2,
             accepted_integration_commit = ?3,
             accepted_integration_tree = ?4,
             accepted_queue_entry_id = ?5,
             accepted_at = ?6,
             updated_at = ?6
         WHERE id = ?1",
        params![
            session_id,
            session_head,
            integration_commit,
            integration_tree,
            queue_entry_id,
            accepted_at,
        ],
    )?;
    if updated != 1 {
        return Err(BrokerError::SessionNotFound(session_id));
    }
    Ok(())
}

fn insert_event(
    conn: &Connection,
    ts: i64,
    kind: &str,
    session_id: Option<i64>,
    payload_json: Option<&str>,
) -> Result<i64, BrokerError> {
    conn.execute(
        "INSERT INTO events (schema_version, ts, kind, session_id, payload_json)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![EVENTS_SCHEMA_VERSION, ts, kind, session_id, payload_json],
    )?;
    Ok(conn.last_insert_rowid())
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;

    #[test]
    fn readable_old_schema_is_migrated_only_in_a_temporary_snapshot() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".aethyme")).unwrap();
        let database = repo.path().join(crate::BROKER_DB_RELPATH);
        let source = Connection::open(&database).unwrap();
        source
            .execute_batch(&format!(
                "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);\n\
                 INSERT INTO meta (key, value) VALUES ('schema_version', '1');\n{}",
                crate::schema::MIGRATION_V1
            ))
            .unwrap();
        drop(source);

        let snapshot = BrokerStore::open_snapshot_in_repo(repo.path()).unwrap();
        assert!(snapshot.live_sessions().unwrap().is_empty());
        assert_eq!(
            schema::current_version(&snapshot.conn).unwrap(),
            crate::SCHEMA_VERSION
        );
        drop(snapshot);

        let source =
            Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        assert_eq!(schema::current_version(&source).unwrap(), 1);
        let adoption_base_exists = source
            .prepare("PRAGMA table_info(sessions)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .any(|column| column.unwrap() == "adoption_base");
        assert!(!adoption_base_exists);
    }
}

#[cfg(test)]
mod operation_history_tests {
    use super::*;

    fn store() -> BrokerStore {
        let conn = Connection::open_in_memory().unwrap();
        schema::migrate(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO sessions (
                 id, worktree_path, branch, origin, status,
                 created_at, updated_at, last_activity_at
             ) VALUES
                 (1, '/repo/one', 'agent/one', 'adopted', 'active', 1, 1, 1),
                 (2, '/repo/two', 'agent/two', 'adopted', 'active', 1, 1, 1);",
        )
        .unwrap();
        BrokerStore {
            conn,
            path: PathBuf::from(":memory:"),
            _snapshot_dir: None,
        }
    }

    fn operation(
        store: &mut BrokerStore,
        session_id: i64,
        provider: OperationProvider,
        repository: &str,
        status: OperationStatus,
    ) -> i64 {
        operation_with_identity(
            store,
            session_id,
            provider,
            repository,
            status,
            OperationIdentityProvenance::VerifiedCanonical,
        )
    }

    fn operation_with_identity(
        store: &mut BrokerStore,
        session_id: i64,
        provider: OperationProvider,
        repository: &str,
        status: OperationStatus,
        identity_provenance: OperationIdentityProvenance,
    ) -> i64 {
        let operation = store
            .create_coordinated_operation(&NewCoordinatedOperation {
                session_id,
                provider,
                repository: repository.into(),
                scope: "repository".into(),
                effect: OperationEffect::Write,
                authorization_reason: None,
                command_json: "[]".into(),
                pid: 1,
                host_operation_id: None,
                identity_provenance,
            })
            .unwrap();
        if status != OperationStatus::Prepared {
            store
                .transition_coordinated_operation(operation.id, status, None, None)
                .unwrap();
        }
        operation.id
    }

    fn ids(page: OperationHistoryPage) -> Vec<i64> {
        page.operations
            .into_iter()
            .map(|operation| operation.id)
            .collect()
    }

    #[test]
    fn operation_history_is_stably_paged_and_filters_every_selector() {
        let mut store = store();
        operation(
            &mut store,
            1,
            OperationProvider::Git,
            "github.com/owner/a",
            OperationStatus::Succeeded,
        );
        operation(
            &mut store,
            2,
            OperationProvider::Github,
            "github.com/owner/b",
            OperationStatus::Failed,
        );
        operation(
            &mut store,
            1,
            OperationProvider::Git,
            "github.com/owner/a",
            OperationStatus::Failed,
        );
        operation(
            &mut store,
            1,
            OperationProvider::Github,
            "github.com/owner/a",
            OperationStatus::Succeeded,
        );
        operation(
            &mut store,
            2,
            OperationProvider::Git,
            "github.com/owner/a",
            OperationStatus::Succeeded,
        );
        operation(
            &mut store,
            1,
            OperationProvider::Git,
            "github.com/owner/b",
            OperationStatus::Running,
        );

        let first = store
            .operation_history(&OperationHistoryQuery {
                limit: 2,
                ..OperationHistoryQuery::default()
            })
            .unwrap();
        assert_eq!(ids(first.clone()), vec![6, 5]);
        assert_eq!(first.next_before_id, Some(5));
        let second = store
            .operation_history(&OperationHistoryQuery {
                limit: 2,
                before_id: first.next_before_id,
                ..OperationHistoryQuery::default()
            })
            .unwrap();
        assert_eq!(ids(second.clone()), vec![4, 3]);
        assert_eq!(second.next_before_id, Some(3));
        let last = store
            .operation_history(&OperationHistoryQuery {
                limit: 2,
                before_id: second.next_before_id,
                ..OperationHistoryQuery::default()
            })
            .unwrap();
        assert_eq!(ids(last.clone()), vec![2, 1]);
        assert_eq!(last.next_before_id, None);

        let cases = [
            (
                OperationHistoryQuery {
                    session_id: Some(1),
                    ..OperationHistoryQuery::default()
                },
                vec![6, 4, 3, 1],
            ),
            (
                OperationHistoryQuery {
                    status: Some(OperationStatus::Failed),
                    ..OperationHistoryQuery::default()
                },
                vec![3, 2],
            ),
            (
                OperationHistoryQuery {
                    repository: Some("github.com/owner/a".into()),
                    ..OperationHistoryQuery::default()
                },
                vec![5, 4, 3, 1],
            ),
            (
                OperationHistoryQuery {
                    provider: Some(OperationProvider::Github),
                    ..OperationHistoryQuery::default()
                },
                vec![4, 2],
            ),
        ];
        for (query, expected) in cases {
            assert_eq!(ids(store.operation_history(&query).unwrap()), expected);
        }

        let combined = store
            .operation_history(&OperationHistoryQuery {
                session_id: Some(1),
                status: Some(OperationStatus::Succeeded),
                repository: Some("github.com/owner/a".into()),
                provider: Some(OperationProvider::Github),
                ..OperationHistoryQuery::default()
            })
            .unwrap();
        assert_eq!(ids(combined), vec![4]);
    }

    #[test]
    fn operation_history_refuses_unbounded_limits() {
        let store = store();
        for limit in [0, MAX_OPERATION_HISTORY_LIMIT + 1] {
            assert!(matches!(
                store.operation_history(&OperationHistoryQuery {
                    limit,
                    ..OperationHistoryQuery::default()
                }),
                Err(BrokerError::InvalidOperationHistoryLimit { .. })
            ));
        }
    }

    #[test]
    fn operation_history_filters_legacy_and_canonical_identity_rows_by_persisted_values() {
        let mut store = store();
        operation_with_identity(
            &mut store,
            1,
            OperationProvider::Git,
            "github.com/owner/repo",
            OperationStatus::Succeeded,
            OperationIdentityProvenance::LegacyUnverifiedIdentity,
        );
        operation_with_identity(
            &mut store,
            2,
            OperationProvider::Git,
            "github.com/owner/repo",
            OperationStatus::Succeeded,
            OperationIdentityProvenance::VerifiedCanonical,
        );

        let page = store
            .operation_history(&OperationHistoryQuery {
                status: Some(OperationStatus::Succeeded),
                repository: Some("github.com/owner/repo".into()),
                provider: Some(OperationProvider::Git),
                ..OperationHistoryQuery::default()
            })
            .unwrap();
        assert_eq!(ids(page.clone()), vec![2, 1]);
        assert_eq!(
            page.operations[0].identity_provenance,
            OperationIdentityProvenance::VerifiedCanonical
        );
        assert_eq!(
            page.operations[1].identity_provenance,
            OperationIdentityProvenance::LegacyUnverifiedIdentity
        );
    }
}
