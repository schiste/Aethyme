use super::*;

impl BrokerStore {
    /// Open (creating and migrating if needed) the broker database for a
    /// repository root: `<repo>/.aethyme/broker.db`, or wherever
    /// [`crate::BROKER_DB_ENV`] points.
    pub fn open_in_repo(repo_root: &Path) -> Result<Self, BrokerError> {
        Self::open(&crate::broker_db_path(repo_root)?)
    }

    /// Open the current broker schema without creating, migrating, or
    /// reconciling any persisted state. Compatibility diagnostics use this
    /// path so an observational command cannot become the write that upgrades
    /// storage or refreshes a session.
    pub fn open_snapshot_in_repo(repo_root: &Path) -> Result<Self, BrokerError> {
        Self::open_snapshot_at(&crate::broker_db_path(repo_root)?)
    }

    /// Open an exact broker database path read-only, without applying the
    /// process-wide repository-database override. Host storage inventory uses
    /// this when joining several repositories' ledgers: one test or embedding
    /// override must not make every owner appear to share the same database.
    pub fn open_snapshot_at(path: &Path) -> Result<Self, BrokerError> {
        let path = path.to_path_buf();
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
        if found > crate::SCHEMA_VERSION && schema::newer_schema_is_compatible(&conn, found)? {
            conn.pragma_update(None, "query_only", true)?;
            return Ok(Self {
                conn,
                path,
                _snapshot_dir: None,
            });
        }
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

    /// Open the repository database only when its persisted schema is already
    /// current. Unlike [`Self::open_snapshot_in_repo`], this path never creates
    /// an in-memory database or a migrated temporary copy. Readiness uses it to
    /// keep the absence and age of broker state observable facts.
    pub(crate) fn open_current_read_only_in_repo(repo_root: &Path) -> Result<Self, BrokerError> {
        let path = crate::broker_db_path(repo_root)?;
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS))?;
        let found = schema::current_version(&conn)?;
        if found != crate::SCHEMA_VERSION {
            return Err(BrokerError::SnapshotSchemaMismatch {
                found,
                minimum: crate::SCHEMA_VERSION,
                maximum: crate::SCHEMA_VERSION,
            });
        }
        conn.pragma_update(None, "query_only", true)?;
        Ok(Self {
            conn,
            path,
            _snapshot_dir: None,
        })
    }

    /// Open a repository's broker database for a best-effort write, or decline.
    ///
    /// `Ok(None)` means there is nothing this binary may safely write to: no
    /// database exists yet, or the one that does is at a different schema
    /// version. Neither is an error. The caller is telemetry, and telemetry
    /// must never be the write that brings a repository's broker state into
    /// existence or moves it forward.
    ///
    /// #163: a test binary's working directory is its crate directory inside a
    /// real checkout, so the post-command metric hook resolved the developer's
    /// live database through `main_root()` and called `migrate` on it. On a
    /// branch that added a migration, `cargo test --workspace` moved the shared
    /// database ahead of every installed binary on the machine -- silently, and
    /// before the branch merged. Declining is the fix rather than redirecting,
    /// because the redirect ([`crate::BROKER_DB_ENV`]) only helps a harness that
    /// remembers to set it, and the developer in the bug report had not.
    pub fn open_current_in_repo(repo_root: &Path) -> Result<Option<Self>, BrokerError> {
        let path = crate::broker_db_path(repo_root)?;
        if !path.is_file() {
            return Ok(None);
        }
        // No `SQLITE_OPEN_CREATE`: an absent database stays absent even if it
        // is deleted between the check above and this open.
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        if schema::current_version(&conn)? != crate::SCHEMA_VERSION {
            return Ok(None);
        }
        Ok(Some(Self {
            conn,
            path,
            _snapshot_dir: None,
        }))
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

    pub(super) fn open_once(db_path: &Path) -> Result<Self, BrokerError> {
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
        self.register_session_with_leases(new, &[])
    }

    /// Register a session and materialize its planned explicit leases in one
    /// immediate transaction. Directory overlap is rechecked after the write
    /// lock is acquired, so concurrent planners cannot both succeed.
    pub fn register_session_with_leases(
        &mut self,
        new: &NewSession,
        planned_paths: &[String],
    ) -> Result<Session, BrokerError> {
        self.register_session_with_context_and_leases(
            false,
            new,
            &SessionContext::default(),
            planned_paths,
        )
    }

    pub fn register_session_with_context_and_leases(
        &mut self,
        verify_only: bool,
        new: &NewSession,
        context: &SessionContext,
        planned_paths: &[String],
    ) -> Result<Session, BrokerError> {
        let now = now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_planned_lease_conflicts(&tx, None, planned_paths, now, verify_only)?;
        let id = insert_session(&tx, new, context, now)?;
        insert_session_context_event(&tx, id, context, now)?;
        insert_planned_explicit_leases(&tx, id, planned_paths, now)?;
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

    /// The most recent session closed with its worktree kept at
    /// `worktree_path`, the predecessor a re-adoption succeeds (issue #294).
    pub fn closed_session_for_worktree(
        &self,
        worktree_path: &str,
    ) -> Result<Option<Session>, BrokerError> {
        self.conn
            .query_row(
                &format!(
                    "{SESSION_SELECT} WHERE worktree_path = ?1 AND cleanup_state = 'closed'
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
        agent_identity: Option<&str>,
    ) -> Result<Session, BrokerError> {
        self.reuse_session_with_leases(id, task, diff_base, agent_identity, &[])
    }

    pub fn reuse_session_with_leases(
        &mut self,
        id: i64,
        task: Option<&str>,
        diff_base: Option<&str>,
        agent_identity: Option<&str>,
        planned_paths: &[String],
    ) -> Result<Session, BrokerError> {
        self.reuse_session_with_context_and_leases(
            false,
            id,
            task,
            diff_base,
            agent_identity,
            &SessionContext::default(),
            planned_paths,
        )
    }

    // `verify_only` decides whether planned leases may be refused; see
    // `validate_planned_lease_conflicts`.
    #[allow(clippy::too_many_arguments)]
    pub fn reuse_session_with_context_and_leases(
        &mut self,
        verify_only: bool,
        id: i64,
        task: Option<&str>,
        diff_base: Option<&str>,
        agent_identity: Option<&str>,
        context: &SessionContext,
        planned_paths: &[String],
    ) -> Result<Session, BrokerError> {
        let now = now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_planned_lease_conflicts(&tx, Some(id), planned_paths, now, verify_only)?;
        let changed = tx.execute(
            "UPDATE sessions SET task = COALESCE(?2, task), diff_base = COALESCE(?3, diff_base),
                                 agent_identity = COALESCE(?4, agent_identity),
                                 repository_name = COALESCE(?5, repository_name),
                                 tab_name = CASE WHEN short_name IS NULL
                                                 THEN COALESCE(?6, tab_name)
                                                 ELSE tab_name END,
                                 ai_provider = COALESCE(?7, ai_provider),
                                 short_name = COALESCE(?8, short_name),
                                 status = 'active', last_activity_at = ?9, updated_at = ?9
             WHERE id = ?1 AND status != 'cleaned'",
            params![
                id,
                task,
                diff_base,
                agent_identity,
                context.repository_name,
                context.tab_name,
                context.ai_provider,
                context.short_name,
                now,
            ],
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
        insert_session_context_event(&tx, id, context, now)?;
        insert_planned_explicit_leases(&tx, id, planned_paths, now)?;
        tx.commit()?;
        self.session(id)
    }

    /// Atomically close one stale session, register its replacement, and
    /// claim the reviewed path plan. The replaced session's leases are
    /// excluded from conflict checks because they are deleted in this same
    /// transaction; every other session remains authoritative.
    pub fn replace_session_with_leases(
        &mut self,
        replaced_id: i64,
        new: &NewSession,
        planned_paths: &[String],
    ) -> Result<Session, BrokerError> {
        self.replace_session_with_context_and_leases(
            false,
            replaced_id,
            new,
            &SessionContext::default(),
            planned_paths,
        )
    }

    pub fn replace_session_with_context_and_leases(
        &mut self,
        verify_only: bool,
        replaced_id: i64,
        new: &NewSession,
        context: &SessionContext,
        planned_paths: &[String],
    ) -> Result<Session, BrokerError> {
        let now = now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        validate_planned_lease_conflicts(&tx, Some(replaced_id), planned_paths, now, verify_only)?;
        let changed = tx.execute(
            "UPDATE sessions
             SET status = 'cleaned', cleanup_state = 'cleaned', closed_at = ?2,
                 cleanup_completed_at = ?2, updated_at = ?2
             WHERE id = ?1 AND status != 'cleaned'",
            params![replaced_id, now],
        )?;
        if changed == 0 {
            return Err(BrokerError::SessionNotFound(replaced_id));
        }
        release_checkpoint_pin_in_tx(&tx, replaced_id, now)?;
        tx.execute("DELETE FROM leases WHERE session_id = ?1", [replaced_id])?;
        tx.execute(
            "DELETE FROM session_foreign_files WHERE session_id = ?1",
            [replaced_id],
        )?;
        insert_event(&tx, now, "session.cleaned", Some(replaced_id), None)?;
        let id = insert_session(&tx, new, context, now)?;
        insert_session_context_event(&tx, id, context, now)?;
        insert_planned_explicit_leases(&tx, id, planned_paths, now)?;
        tx.commit()?;
        self.session(id)
    }

    /// Record host-facing session identity without changing ownership or
    /// liveness. Values supplied by a later Chau7 snapshot replace stale
    /// values; omitted values preserve what registration already knew.
    pub fn update_session_context(
        &mut self,
        id: i64,
        context: &SessionContext,
    ) -> Result<Session, BrokerError> {
        if context.is_empty() {
            return self.session(id);
        }
        let now = now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE sessions
             SET repository_name = COALESCE(?2, repository_name),
                 tab_name = CASE WHEN short_name IS NULL
                                 THEN COALESCE(?3, tab_name) ELSE tab_name END,
                 ai_provider = COALESCE(?4, ai_provider),
                 short_name = COALESCE(?5, short_name),
                 updated_at = ?6
             WHERE id = ?1",
            params![
                id,
                context.repository_name,
                context.tab_name,
                context.ai_provider,
                context.short_name,
                now,
            ],
        )?;
        if changed == 0 {
            return Err(BrokerError::SessionNotFound(id));
        }
        insert_session_context_event(&tx, id, context, now)?;
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

    /// All sessions already closed in broker state, oldest first.
    ///
    /// Cleanup planning uses this separate query so normal live-session
    /// surfaces remain bounded to identities that can still act.
    pub fn cleaned_sessions(&self) -> Result<Vec<Session>, BrokerError> {
        let mut stmt = self.conn.prepare(&format!(
            "{SESSION_SELECT} WHERE status = 'cleaned' ORDER BY id"
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
        // `closed_at` records the first close and is never moved: cleanup
        // after a state-only close, or a second close, must not overwrite it.
        // Retention and the closed-worktree grace period are measured from
        // it, and overwriting it once stamped 97 sessions' close times with
        // the minute a bulk cleanup ran.
        let (stored_status, cleanup_state, closed_at, cleanup_completed_at) = match status {
            SessionStatus::Closed => ("cleaned", "closed", Some(now), None),
            SessionStatus::Cleaned => ("cleaned", "cleaned", Some(now), Some(now)),
            _ => (status.as_str(), "open", None, None),
        };
        let changed = tx.execute(
            "UPDATE sessions SET status = ?2, exit_code = COALESCE(?3, exit_code),
                                 updated_at = ?4, cleanup_state = ?5,
                                 closed_at = COALESCE(closed_at, ?6),
                                 cleanup_completed_at = ?7
             WHERE id = ?1",
            params![
                id,
                stored_status,
                exit_code,
                now,
                cleanup_state,
                closed_at,
                cleanup_completed_at
            ],
        )?;
        if changed == 0 {
            return Err(BrokerError::SessionNotFound(id));
        }
        // A closed session has no more working time ahead of it, so its open
        // period of attention ends now. Left open, it would keep accruing
        // nothing while `session_activity_totals` reported an interval that
        // never resolves.
        if matches!(status, SessionStatus::Closed | SessionStatus::Cleaned) {
            close_open_activity_in_tx(&tx, id, now)?;
        }
        // `cleaned` is terminal (reuse_session excludes it), so the
        // session's leases can never matter again — drop them in the same
        // transaction. Without this every cleaned session leaves its last
        // implicit-lease snapshot behind forever (722 orphaned rows for
        // ~25 sessions observed in the 2026-07-17 dogfood database).
        if status.is_closed() {
            release_checkpoint_pin_in_tx(&tx, id, now)?;
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
            "UPDATE sessions
             SET status = 'cleaned', cleanup_state = 'closed', closed_at = ?2,
                 cleanup_completed_at = NULL, updated_at = ?2
             WHERE id = ?1 AND cleanup_state = 'open'",
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
        release_checkpoint_pin_in_tx(&tx, id, now)?;
        release_session_leases_in_tx(&tx, id, now, "finish")?;
        tx.execute(
            "DELETE FROM session_foreign_files WHERE session_id = ?1",
            [id],
        )?;
        insert_event(
            &tx,
            now,
            &format!("session.{}", SessionStatus::Closed.as_str()),
            Some(id),
            None,
        )?;
        close_open_activity_in_tx(&tx, id, now)?;
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

    /// Durably close a session and journal that physical cleanup is about to
    /// begin. The final redacted handoff is appended separately after the Git
    /// outcome is known, so a crash leaves an explicit recoverable boundary.
    pub fn begin_finish_cleanup(
        &mut self,
        id: i64,
        cleanup_payload: &str,
    ) -> Result<(), BrokerError> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        let state: Option<String> = tx
            .query_row(
                "SELECT cleanup_state FROM sessions WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(state) = state else {
            return Err(BrokerError::SessionNotFound(id));
        };
        if state == "cleaned" {
            return Ok(());
        }
        if state == "open" {
            tx.execute(
                "UPDATE sessions
                 SET status = 'cleaned', cleanup_state = 'closed', closed_at = ?2,
                     cleanup_completed_at = NULL, updated_at = ?2
                 WHERE id = ?1",
                params![id, now],
            )?;
            release_checkpoint_pin_in_tx(&tx, id, now)?;
            release_session_leases_in_tx(&tx, id, now, "finish")?;
            tx.execute(
                "DELETE FROM session_foreign_files WHERE session_id = ?1",
                [id],
            )?;
            insert_event(&tx, now, "session.closed", Some(id), None)?;
        }
        insert_event(
            &tx,
            now,
            crate::events::SESSION_FINISH_CLEANUP_STARTED,
            Some(id),
            Some(cleanup_payload),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn record_finished_handoff(
        &mut self,
        id: i64,
        handoff_payload: &str,
    ) -> Result<(), BrokerError> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?1)",
            [id],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(BrokerError::SessionNotFound(id));
        }
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

    /// Release `session_id`'s leases on exactly `path` for `reason`, audited
    /// like a finish release (lease id and generation on the record). Rows
    /// stay, marked released. Returns the released lease ids.
    pub fn release_lease_for(
        &mut self,
        session_id: i64,
        path: &str,
        reason: &str,
    ) -> Result<Vec<i64>, BrokerError> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        let ids = record_lease_releases_in_tx(&tx, session_id, Some(path), now, reason)?;
        tx.execute(
            "UPDATE leases SET released_at = ?3
             WHERE session_id = ?1 AND path = ?2 AND released_at IS NULL",
            params![session_id, path, now],
        )?;
        tx.commit()?;
        Ok(ids)
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
    /// Record the targets a session says it will work on.
    ///
    /// Idempotent per (session, kind, value): re-recording a target keeps the
    /// first row and upgrades its operation when the new one is stated, so a
    /// derived `unknown` never overwrites an operator's declaration.
    pub fn record_session_scopes(
        &mut self,
        session_id: i64,
        scopes: &[(
            crate::ScopeKind,
            String,
            crate::ScopeOperation,
            crate::ScopeSource,
        )],
    ) -> Result<usize, BrokerError> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        let mut written = 0usize;
        for (kind, value, operation, source) in scopes {
            written += tx.execute(
                "INSERT INTO session_scopes
                     (session_id, kind, value, operation, source, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT (session_id, kind, value) DO UPDATE SET
                     operation = CASE
                         WHEN excluded.operation = 'unknown' THEN session_scopes.operation
                         ELSE excluded.operation
                     END,
                     source = CASE
                         WHEN excluded.operation = 'unknown' THEN session_scopes.source
                         ELSE excluded.source
                     END,
                     released_at = NULL",
                rusqlite::params![
                    session_id,
                    kind.as_str(),
                    value,
                    operation.as_str(),
                    source.as_str(),
                    now
                ],
            )?;
        }
        tx.commit()?;
        Ok(written)
    }

    /// Declared scopes for every session that is still live.
    ///
    /// Scoped to live sessions by the same allow-list `active_leases` uses.
    /// A finished session's declaration is history, not a claim on anything,
    /// and an allow-list keeps a status added later out until someone decides
    /// it belongs -- a deny-list would silently admit it.
    pub fn active_session_scopes(&self) -> Result<Vec<crate::SessionScope>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.session_id, s.kind, s.value, s.operation, s.source,
                    s.created_at, s.released_at
               FROM session_scopes s
               JOIN sessions ON sessions.id = s.session_id
              WHERE s.released_at IS NULL
                AND sessions.status IN ('active', 'idle', 'stale')
              ORDER BY s.kind, s.value, s.session_id",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(crate::SessionScope {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    kind: crate::ScopeKind::parse(&row.get::<_, String>(2)?)
                        .unwrap_or(crate::ScopeKind::Symbol),
                    value: row.get(3)?,
                    operation: crate::ScopeOperation::parse(&row.get::<_, String>(4)?)
                        .unwrap_or(crate::ScopeOperation::Unknown),
                    source: crate::ScopeSource::parse(&row.get::<_, String>(5)?)
                        .unwrap_or(crate::ScopeSource::Derived),
                    created_at: row.get(6)?,
                    released_at: row.get(7)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Open ownership claims whose holder is still a live session, by name.
    ///
    /// The same allow-list as [`Self::active_session_scopes`]: a finished
    /// session's claim is history and stops counting when the session closes,
    /// without a write on the finish path.
    pub fn active_ownership_claims(&self) -> Result<Vec<crate::OwnershipClaim>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT c.id, c.name, c.session_id, c.purpose, c.claimed_at,
                    c.taken_over_from
               FROM ownership_claims c
               JOIN sessions ON sessions.id = c.session_id
              WHERE c.released_at IS NULL
                AND sessions.status IN ('active', 'idle', 'stale')
              ORDER BY c.name",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(crate::OwnershipClaim {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    session_id: row.get(2)?,
                    purpose: row.get(3)?,
                    claimed_at: row.get(4)?,
                    taken_over_from: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Record `session_id` as the holder of `name`.
    ///
    /// In one transaction: close the open row of a holder that is no longer
    /// live, or of `replacing` (a takeover the caller already judged safe),
    /// then insert. A live holder the caller did not name survives the update,
    /// so the insert hits the one-open-claim index and the claim fails rather
    /// than silently taking a name someone else just took.
    pub fn take_ownership_claim(
        &mut self,
        name: &str,
        session_id: i64,
        purpose: &str,
        replacing: Option<i64>,
        at_ms: i64,
    ) -> Result<bool, BrokerError> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE ownership_claims
                SET released_at = ?2,
                    released_reason = CASE WHEN session_id = ?3 THEN 'taken_over'
                                           ELSE 'holder_closed' END
              WHERE name = ?1 AND released_at IS NULL AND session_id != ?4
                AND (session_id = ?3 OR session_id NOT IN
                     (SELECT id FROM sessions WHERE status IN ('active', 'idle', 'stale')))",
            rusqlite::params![name, at_ms, replacing, session_id],
        )?;
        let updated = tx.execute(
            "UPDATE ownership_claims SET purpose = ?3
              WHERE name = ?1 AND session_id = ?2 AND released_at IS NULL",
            rusqlite::params![name, session_id, purpose],
        )?;
        let inserted = if updated == 0 {
            match tx.execute(
                "INSERT INTO ownership_claims
                     (name, session_id, purpose, claimed_at, taken_over_from)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![name, session_id, purpose, at_ms, replacing],
            ) {
                Ok(_) => true,
                Err(rusqlite::Error::SqliteFailure(error, _))
                    if error.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    return Ok(false);
                }
                Err(error) => return Err(error.into()),
            }
        } else {
            false
        };
        tx.commit()?;
        Ok(inserted || updated > 0)
    }

    /// Close `session_id`'s open claim on `name`; false when it held none.
    pub fn release_ownership_claim(
        &mut self,
        name: &str,
        session_id: i64,
        at_ms: i64,
    ) -> Result<bool, BrokerError> {
        let changed = self.conn.execute(
            "UPDATE ownership_claims SET released_at = ?3, released_reason = 'released'
              WHERE name = ?1 AND session_id = ?2 AND released_at IS NULL",
            rusqlite::params![name, session_id, at_ms],
        )?;
        Ok(changed > 0)
    }

    /// The most recent coordinated operation a session ran: when it last
    /// changed and the reason it was authorized with. "Last activity" for a
    /// release driver is its last push, merge or tag, which hook-driven
    /// activity does not always see.
    pub fn latest_coordinated_operation(
        &self,
        session_id: i64,
    ) -> Result<Option<(i64, Option<String>)>, BrokerError> {
        let row = self
            .conn
            .query_row(
                "SELECT updated_at, authorization_reason FROM coordinated_operations
                  WHERE session_id = ?1 ORDER BY id DESC LIMIT 1",
                [session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(row)
    }

    pub fn active_leases(&self) -> Result<Vec<Lease>, BrokerError> {
        self.active_leases_at(now_ms())
    }

    /// Live leases at one caller-supplied snapshot time. Read-only exports use
    /// this so lease state and conflict classification share one time boundary.
    pub fn active_leases_at(&self, now: i64) -> Result<Vec<Lease>, BrokerError> {
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
    /// The machine environment is left unrecorded (NULL): use
    /// [`Self::record_gate_result_with_environment`] for an executed run.
    pub fn record_gate_result(&mut self, result: &NewGateResult) -> Result<i64, BrokerError> {
        self.record_gate_result_with_environment(result, &GateEnvironment::default())
    }

    /// Record a gate run together with the machine conditions it ran under.
    pub fn record_gate_result_with_environment(
        &mut self,
        result: &NewGateResult,
        environment: &GateEnvironment,
    ) -> Result<i64, BrokerError> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO gate_results (gate_name, tree_hash, definition_hash, status,
                                       failure_class, exit_code, duration_ms, log_path,
                                       session_id, created_at, wait_duration_ms,
                                       first_output_ms, output_bytes, load_avg_1m_start,
                                       load_avg_1m_end, cpu_count, free_disk_bytes_start)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                     ?16, ?17)",
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
                environment.load_avg_1m_start,
                environment.load_avg_1m_end,
                environment.cpu_count,
                environment.free_disk_bytes_start,
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
}
