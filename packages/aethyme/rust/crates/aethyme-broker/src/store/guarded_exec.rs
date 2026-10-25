//! Durable, observational receipts for `broker exec`.
//!
//! A receipt is deliberately not process authority: it stores no PID, argv,
//! environment, or output, and unresolved rows are never used to signal,
//! clean up, or transition a session.

use rusqlite::{OptionalExtension as _, TransactionBehavior, params};

use crate::error::BrokerError;

const MAX_RECEIPT_LIST_LIMIT: u32 = 100;
const RETAIN_TERMINAL_RECEIPTS_PER_SESSION: i64 = 100;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct GuardedExecReceipt {
    pub helper_observation: Option<HelperObservation>,
    pub receipt_id: String,
    pub session_id: i64,
    pub phase: String,
    pub command_outcome: String,
    pub exit_code: Option<i32>,
    pub ownership_audit: String,
    pub process_group_cleanup: String,
    /// A row left in `starting`, `running`, or `outcome_unknown` is not
    /// evidence that a process is still alive or has been cleaned up.
    pub process_revalidation: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// Observation of the directly supervised child only. This never completes
/// the caller's ownership audit or proves that descendants were cleaned up.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct HelperObservation {
    pub direct_child_outcome: String,
    pub exit_code: Option<i32>,
    pub observed_at_ms: i64,
    pub process_group_cleanup: String,
    pub ownership_audit: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HelperObservationTarget {
    database: std::path::PathBuf,
    device: u64,
    inode: u64,
    session_id: i64,
    receipt_id: String,
    created_at_ms: i64,
}

impl HelperObservationTarget {
    #[cfg(unix)]
    pub(crate) fn record(&self, outcome: &str, exit_code: Option<i32>) -> std::io::Result<()> {
        use std::os::unix::fs::MetadataExt as _;
        if !matches!(outcome, "succeeded" | "failed" | "cancelled")
            || (outcome == "succeeded" && exit_code != Some(0))
            || (outcome == "failed" && exit_code == Some(0))
            || !self.database.is_absolute()
            || self.receipt_id.len() != 32
            || !self.receipt_id.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(std::io::Error::other("invalid helper observation target"));
        }
        let check_identity = || -> std::io::Result<()> {
            let metadata = std::fs::symlink_metadata(&self.database)?;
            if !metadata.is_file() || metadata.dev() != self.device || metadata.ino() != self.inode
            {
                return Err(std::io::Error::other(
                    "helper observation database identity changed",
                ));
            }
            Ok(())
        };
        check_identity()?;
        // Fresh connection in the exec'd helper: never reuse a parent's SQLite
        // connection and never create or migrate a database from this path.
        let mut conn = rusqlite::Connection::open_with_flags(
            &self.database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
                | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(std::io::Error::other)?;
        conn.busy_timeout(std::time::Duration::from_millis(250))
            .map_err(std::io::Error::other)?;
        conn.pragma_update(None, "foreign_keys", true)
            .map_err(std::io::Error::other)?;
        check_identity()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(std::io::Error::other)?;
        let binding: Option<i64> = tx
            .query_row(
                "SELECT created_at_ms FROM guarded_exec_receipts
             WHERE receipt_id = ?1 AND session_id = ?2 AND phase != 'not_started'",
                params![self.receipt_id, self.session_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(std::io::Error::other)?;
        if binding != Some(self.created_at_ms) {
            return Err(std::io::Error::other(
                "helper observation receipt binding changed",
            ));
        }
        tx.execute(
            "INSERT OR IGNORE INTO guarded_exec_helper_observations
             (receipt_id, session_id, receipt_created_at_ms, direct_child_outcome, exit_code, observed_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![self.receipt_id, self.session_id, self.created_at_ms, outcome, exit_code,
                    super::now_ms().max(self.created_at_ms)],
        ).map_err(std::io::Error::other)?;
        // Bound diagnostics independently of unresolved parent receipts.
        tx.execute(
            "DELETE FROM guarded_exec_helper_observations WHERE session_id = ?1 AND receipt_id IN (
                 SELECT receipt_id FROM guarded_exec_helper_observations WHERE session_id = ?1
                 ORDER BY observed_at_ms DESC, receipt_id DESC LIMIT -1 OFFSET 100)",
            [self.session_id],
        )
        .map_err(std::io::Error::other)?;
        tx.commit().map_err(std::io::Error::other)
    }
}

impl super::BrokerStore {
    pub(crate) fn helper_observation_target(
        &self,
        session_id: i64,
        receipt_id: &str,
    ) -> Result<Option<HelperObservationTarget>, BrokerError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            if self.path == std::path::Path::new(":memory:") {
                return Ok(None);
            }
            let database = self.path.canonicalize().map_err(|source| BrokerError::Io {
                path: self.path.clone(),
                source,
            })?;
            let metadata =
                std::fs::symlink_metadata(&database).map_err(|source| BrokerError::Io {
                    path: database.clone(),
                    source,
                })?;
            let created_at_ms = self.conn.query_row(
                "SELECT created_at_ms FROM guarded_exec_receipts WHERE receipt_id = ?1 AND session_id = ?2",
                params![receipt_id, session_id], |row| row.get(0),
            )?;
            Ok(Some(HelperObservationTarget {
                database,
                device: metadata.dev(),
                inode: metadata.ino(),
                session_id,
                receipt_id: receipt_id.into(),
                created_at_ms,
            }))
        }
        #[cfg(not(unix))]
        {
            let _unused = (session_id, receipt_id);
            Ok(None)
        }
    }

    fn load_helper_observation(&self, receipt: &mut GuardedExecReceipt) -> Result<(), BrokerError> {
        receipt.helper_observation = self
            .conn
            .query_row(
                "SELECT direct_child_outcome, exit_code, observed_at_ms
             FROM guarded_exec_helper_observations
             WHERE receipt_id = ?1 AND session_id = ?2 AND receipt_created_at_ms = ?3",
                params![
                    receipt.receipt_id,
                    receipt.session_id,
                    receipt.created_at_ms
                ],
                |row| {
                    Ok(HelperObservation {
                        direct_child_outcome: row.get(0)?,
                        exit_code: row.get(1)?,
                        observed_at_ms: row.get(2)?,
                        process_group_cleanup: "unknown".into(),
                        ownership_audit: "not_run_by_helper".into(),
                    })
                },
            )
            .optional()?;
        Ok(())
    }

    /// Commit the pre-dispatch record. A failure here must stop the command
    /// before any child is spawned.
    pub(crate) fn begin_guarded_exec_receipt(
        &mut self,
        session_id: i64,
    ) -> Result<String, BrokerError> {
        let now = super::now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        prune_terminal_guarded_exec_receipts(&tx, session_id)?;
        let previous_created_at: i64 = tx.query_row(
            "SELECT COALESCE(MAX(created_at_ms), 0)
             FROM guarded_exec_receipts WHERE session_id = ?1",
            [session_id],
            |row| row.get(0),
        )?;
        let created_at = now.max(previous_created_at.saturating_add(1));
        let receipt_id = tx.query_row(
            "INSERT INTO guarded_exec_receipts (
                 receipt_id, session_id, phase, command_outcome, exit_code,
                 ownership_audit, process_group_cleanup, created_at_ms, updated_at_ms
             ) VALUES (
                 lower(hex(randomblob(16))), ?1, 'starting', 'unknown', NULL,
                 'not_run', 'unknown', ?2, ?2
             )
             RETURNING receipt_id",
            params![session_id, created_at],
            |row| row.get(0),
        )?;
        tx.commit()?;
        Ok(receipt_id)
    }

    pub(crate) fn mark_guarded_exec_running(
        &mut self,
        session_id: i64,
        receipt_id: &str,
    ) -> Result<(), BrokerError> {
        self.transition_guarded_exec_receipt(
            "UPDATE guarded_exec_receipts
             SET phase = 'running', updated_at_ms = max(updated_at_ms, ?1)
             WHERE session_id = ?2 AND receipt_id = ?3 AND phase = 'starting'",
            params![super::now_ms(), session_id, receipt_id],
        )
    }

    pub(crate) fn mark_guarded_exec_not_started(
        &mut self,
        session_id: i64,
        receipt_id: &str,
    ) -> Result<(), BrokerError> {
        self.transition_guarded_exec_receipt(
            "UPDATE guarded_exec_receipts
             SET phase = 'not_started', command_outcome = 'not_started',
                 exit_code = NULL, ownership_audit = 'not_run',
                 process_group_cleanup = 'not_applicable',
                 updated_at_ms = max(updated_at_ms, ?1)
             WHERE session_id = ?2 AND receipt_id = ?3 AND phase = 'starting'",
            params![super::now_ms(), session_id, receipt_id],
        )?;
        self.prune_terminal_guarded_exec_receipts(session_id)
    }

    pub(crate) fn complete_guarded_exec_command(
        &mut self,
        session_id: i64,
        receipt_id: &str,
        command_outcome: &str,
        exit_code: Option<i32>,
        process_group_cleanup: &str,
    ) -> Result<(), BrokerError> {
        debug_assert!(matches!(
            command_outcome,
            "succeeded" | "failed" | "timed_out"
        ));
        debug_assert!(matches!(
            process_group_cleanup,
            "verified" | "unverified" | "unknown"
        ));
        self.transition_guarded_exec_receipt(
            "UPDATE guarded_exec_receipts
             SET phase = 'completed', command_outcome = ?1, exit_code = ?2,
                 ownership_audit = 'unknown', process_group_cleanup = ?3,
                 updated_at_ms = max(updated_at_ms, ?4)
             WHERE session_id = ?5 AND receipt_id = ?6
               AND phase IN ('starting', 'running')",
            params![
                command_outcome,
                exit_code,
                process_group_cleanup,
                super::now_ms(),
                session_id,
                receipt_id
            ],
        )?;
        self.prune_terminal_guarded_exec_receipts(session_id)
    }

    pub(crate) fn mark_guarded_exec_outcome_unknown(
        &mut self,
        session_id: i64,
        receipt_id: &str,
        process_group_cleanup: &str,
    ) -> Result<(), BrokerError> {
        debug_assert!(matches!(
            process_group_cleanup,
            "verified" | "unverified" | "unknown"
        ));
        self.transition_guarded_exec_receipt(
            "UPDATE guarded_exec_receipts
             SET phase = 'outcome_unknown', command_outcome = 'unknown',
                 ownership_audit = 'unknown', process_group_cleanup = ?1,
                 updated_at_ms = max(updated_at_ms, ?2)
             WHERE session_id = ?3 AND receipt_id = ?4
               AND phase IN ('starting', 'running')",
            params![
                process_group_cleanup,
                super::now_ms(),
                session_id,
                receipt_id
            ],
        )?;
        if process_group_cleanup == "verified" {
            self.prune_terminal_guarded_exec_receipts(session_id)?;
        }
        Ok(())
    }

    pub(crate) fn finish_guarded_exec_audit(
        &mut self,
        session_id: i64,
        receipt_id: &str,
        ownership_audit: &str,
    ) -> Result<(), BrokerError> {
        debug_assert!(matches!(ownership_audit, "passed" | "failed"));
        self.transition_guarded_exec_receipt(
            "UPDATE guarded_exec_receipts
             SET ownership_audit = ?1, updated_at_ms = max(updated_at_ms, ?2)
             WHERE session_id = ?3 AND receipt_id = ?4
               AND phase IN ('completed', 'outcome_unknown')",
            params![ownership_audit, super::now_ms(), session_id, receipt_id],
        )
    }

    fn transition_guarded_exec_receipt(
        &mut self,
        sql: &str,
        values: impl rusqlite::Params,
    ) -> Result<(), BrokerError> {
        if self.conn.execute(sql, values)? != 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows.into());
        }
        Ok(())
    }

    fn prune_terminal_guarded_exec_receipts(&mut self, session_id: i64) -> Result<(), BrokerError> {
        prune_terminal_guarded_exec_receipts(&self.conn, session_id)
    }

    pub(crate) fn guarded_exec_receipt(
        &self,
        session_id: i64,
        receipt_id: &str,
    ) -> Result<Option<GuardedExecReceipt>, BrokerError> {
        let mut receipt = self
            .conn
            .query_row(
                "SELECT receipt_id, session_id, phase, command_outcome, exit_code,
                        ownership_audit, process_group_cleanup, created_at_ms, updated_at_ms
                 FROM guarded_exec_receipts
                 WHERE session_id = ?1 AND receipt_id = ?2",
                params![session_id, receipt_id],
                guarded_exec_receipt_from_row,
            )
            .optional()
            .map_err(BrokerError::from)?;
        if let Some(receipt) = receipt.as_mut() {
            self.load_helper_observation(receipt)?;
        }
        Ok(receipt)
    }

    pub(crate) fn guarded_exec_receipts(
        &self,
        session_id: i64,
        limit: u32,
    ) -> Result<Vec<GuardedExecReceipt>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT receipt_id, session_id, phase, command_outcome, exit_code,
                    ownership_audit, process_group_cleanup, created_at_ms, updated_at_ms
             FROM guarded_exec_receipts
             WHERE session_id = ?1
             ORDER BY created_at_ms DESC, receipt_id DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![session_id, limit.clamp(1, MAX_RECEIPT_LIST_LIMIT)],
            guarded_exec_receipt_from_row,
        )?;
        let mut receipts = rows.collect::<Result<Vec<_>, _>>()?;
        for receipt in &mut receipts {
            self.load_helper_observation(receipt)?;
        }
        Ok(receipts)
    }
}

fn prune_terminal_guarded_exec_receipts(
    connection: &rusqlite::Connection,
    session_id: i64,
) -> Result<(), BrokerError> {
    connection.execute(
        "DELETE FROM guarded_exec_receipts
         WHERE session_id = ?1
           AND receipt_id IN (
               SELECT receipt_id FROM guarded_exec_receipts
               WHERE session_id = ?1
                 AND (
                     phase IN ('not_started', 'completed')
                     OR (
                         phase = 'outcome_unknown'
                         AND process_group_cleanup = 'verified'
                     )
                 )
               ORDER BY created_at_ms DESC, receipt_id DESC
               LIMIT -1 OFFSET ?2
           )",
        params![session_id, RETAIN_TERMINAL_RECEIPTS_PER_SESSION],
    )?;
    Ok(())
}

fn guarded_exec_receipt_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GuardedExecReceipt> {
    let phase: String = row.get(2)?;
    let command_outcome: String = row.get(3)?;
    let process_group_cleanup: String = row.get(6)?;
    let process_revalidation =
        if matches!(phase.as_str(), "starting" | "running" | "outcome_unknown")
            || matches!(process_group_cleanup.as_str(), "unverified" | "unknown")
        {
            "not_revalidated"
        } else {
            "not_applicable"
        };
    Ok(GuardedExecReceipt {
        helper_observation: None,
        receipt_id: row.get(0)?,
        session_id: row.get(1)?,
        phase,
        command_outcome,
        exit_code: row.get(4)?,
        ownership_audit: row.get(5)?,
        process_group_cleanup,
        process_revalidation: process_revalidation.into(),
        created_at_ms: row.get(7)?,
        updated_at_ms: row.get(8)?,
    })
}

#[cfg(all(test, unix))]
pub(crate) fn helper_observation_fixture() -> (
    tempfile::TempDir,
    super::BrokerStore,
    String,
    HelperObservationTarget,
) {
    let owner = tempfile::tempdir().unwrap();
    let path = owner.path().join("broker.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    crate::schema::migrate(&conn).unwrap();
    conn.execute_batch(
        "PRAGMA foreign_keys = ON; INSERT INTO sessions (
        id, worktree_path, branch, origin, status, created_at, updated_at, last_activity_at
    ) VALUES (1, '/synthetic', 'agent/synthetic', 'adopted', 'active', 1, 1, 1);",
    )
    .unwrap();
    let mut store = super::BrokerStore {
        conn,
        path,
        _snapshot_dir: None,
        binding_scope: None,
    };
    let receipt = store.begin_guarded_exec_receipt(1).unwrap();
    store.mark_guarded_exec_running(1, &receipt).unwrap();
    let target = store
        .helper_observation_target(1, &receipt)
        .unwrap()
        .unwrap();
    (owner, store, receipt, target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> super::super::BrokerStore {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::schema::migrate(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO sessions (
                 id, worktree_path, branch, origin, status,
                 created_at, updated_at, last_activity_at
             ) VALUES
                 (1, '/repo/one', 'agent/one', 'adopted', 'active', 1, 1, 1),
                 (2, '/repo/two', 'agent/two', 'adopted', 'active', 1, 1, 1);",
        )
        .unwrap();
        super::super::BrokerStore {
            conn,
            path: std::path::PathBuf::from(":memory:"),
            _snapshot_dir: None,
            binding_scope: None,
        }
    }

    #[cfg(unix)]
    #[test]
    fn helper_observations_are_immutable_and_do_not_complete_parent_audit() {
        let (_owner, store, receipt, target) = helper_observation_fixture();
        target.record("failed", Some(7)).unwrap();
        target.record("succeeded", Some(0)).unwrap();
        let row = store.guarded_exec_receipt(1, &receipt).unwrap().unwrap();
        assert_eq!(row.phase, "running");
        assert_eq!(row.command_outcome, "unknown");
        assert_eq!(row.ownership_audit, "not_run");
        assert_eq!(row.process_group_cleanup, "unknown");
        let encoded = serde_json::to_value(&row).unwrap();
        assert_eq!(
            encoded["helper_observation"]["direct_child_outcome"],
            "failed"
        );
        assert_eq!(encoded["helper_observation"]["exit_code"], 7);
        assert_eq!(
            encoded["helper_observation"]["process_group_cleanup"],
            "unknown"
        );
        assert_eq!(
            encoded["helper_observation"]["ownership_audit"],
            "not_run_by_helper"
        );
        let observed = row.helper_observation.unwrap();
        assert_eq!(observed.direct_child_outcome, "failed");
        assert_eq!(observed.exit_code, Some(7));
        assert_eq!(observed.process_group_cleanup, "unknown");
        assert!(store.guarded_exec_receipt(2, &receipt).unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn helper_observation_refuses_changed_binding_and_database_identity() {
        let (owner, store, receipt, target) = helper_observation_fixture();
        let mut wrong = target.clone();
        wrong.created_at_ms += 1;
        assert!(
            wrong
                .record("succeeded", Some(0))
                .unwrap_err()
                .to_string()
                .contains("binding changed")
        );
        wrong = target.clone();
        wrong.session_id = 2;
        assert!(
            wrong
                .record("succeeded", Some(0))
                .unwrap_err()
                .to_string()
                .contains("binding changed")
        );
        assert!(
            store
                .guarded_exec_receipt(1, &receipt)
                .unwrap()
                .unwrap()
                .helper_observation
                .is_none()
        );
        std::fs::rename(&target.database, owner.path().join("retained.db")).unwrap();
        std::fs::write(&target.database, b"replacement").unwrap();
        assert!(
            target
                .record("succeeded", Some(0))
                .unwrap_err()
                .to_string()
                .contains("identity changed")
        );
        assert_eq!(std::fs::read(&target.database).unwrap(), b"replacement");
        std::fs::remove_file(&target.database).unwrap();
        assert!(target.record("succeeded", Some(0)).is_err());
        assert!(
            !target.database.exists(),
            "observation must not create a database"
        );
    }

    #[cfg(unix)]
    #[test]
    fn helper_observation_lock_contention_retains_unknown() {
        let (_owner, store, receipt, target) = helper_observation_fixture();
        store.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert!(target.record("cancelled", None).is_err());
        store.conn.execute_batch("ROLLBACK").unwrap();
        assert!(
            store
                .guarded_exec_receipt(1, &receipt)
                .unwrap()
                .unwrap()
                .helper_observation
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn helper_observations_have_an_independent_bounded_retention_budget() {
        let (_owner, mut store, first, target) = helper_observation_fixture();
        target.record("failed", Some(1)).unwrap();
        let mut newest = first.clone();
        for _ in 0..104 {
            newest = store.begin_guarded_exec_receipt(1).unwrap();
            store
                .helper_observation_target(1, &newest)
                .unwrap()
                .unwrap()
                .record("cancelled", None)
                .unwrap();
        }
        let count: i64 = store
            .conn
            .query_row(
                "SELECT count(*) FROM guarded_exec_helper_observations",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 100);
        assert!(
            store
                .guarded_exec_receipt(1, &first)
                .unwrap()
                .unwrap()
                .helper_observation
                .is_none()
        );
        assert!(
            store
                .guarded_exec_receipt(1, &newest)
                .unwrap()
                .unwrap()
                .helper_observation
                .is_some()
        );
        store
            .conn
            .execute(
                "DELETE FROM guarded_exec_receipts WHERE receipt_id = ?1",
                [newest],
            )
            .unwrap();
        let count: i64 = store
            .conn
            .query_row(
                "SELECT count(*) FROM guarded_exec_helper_observations",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 99);
    }

    #[test]
    fn receipts_preserve_dispatch_and_audit_outcomes_per_session() {
        let mut store = store();
        let receipt_id = store.begin_guarded_exec_receipt(1).unwrap();
        let starting = store.guarded_exec_receipt(1, &receipt_id).unwrap().unwrap();
        assert_eq!(starting.phase, "starting");
        assert_eq!(starting.command_outcome, "unknown");
        assert_eq!(starting.process_revalidation, "not_revalidated");

        store.mark_guarded_exec_running(1, &receipt_id).unwrap();
        store
            .complete_guarded_exec_command(1, &receipt_id, "failed", Some(3), "verified")
            .unwrap();
        store
            .finish_guarded_exec_audit(1, &receipt_id, "passed")
            .unwrap();

        let completed = store.guarded_exec_receipt(1, &receipt_id).unwrap().unwrap();
        assert_eq!(completed.phase, "completed");
        assert_eq!(completed.command_outcome, "failed");
        assert_eq!(completed.exit_code, Some(3));
        assert_eq!(completed.ownership_audit, "passed");
        assert_eq!(completed.process_group_cleanup, "verified");
        assert_eq!(completed.process_revalidation, "not_applicable");
        assert!(
            store
                .guarded_exec_receipt(2, &receipt_id)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn terminal_receipts_are_bounded_without_pruning_unverified_outcomes() {
        let mut store = store();
        let unresolved = store.begin_guarded_exec_receipt(1).unwrap();
        store.mark_guarded_exec_running(1, &unresolved).unwrap();
        store
            .mark_guarded_exec_outcome_unknown(1, &unresolved, "unverified")
            .unwrap();

        let mut oldest_terminal = None;
        for index in 0..=RETAIN_TERMINAL_RECEIPTS_PER_SESSION {
            let receipt_id = store.begin_guarded_exec_receipt(1).unwrap();
            if index == 0 {
                oldest_terminal = Some(receipt_id.clone());
            }
            store.mark_guarded_exec_not_started(1, &receipt_id).unwrap();
        }

        let terminal_count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM guarded_exec_receipts
                 WHERE session_id = 1 AND phase = 'not_started'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(terminal_count, RETAIN_TERMINAL_RECEIPTS_PER_SESSION);
        assert!(
            store
                .guarded_exec_receipt(1, oldest_terminal.as_deref().unwrap())
                .unwrap()
                .is_none()
        );
        let unresolved = store.guarded_exec_receipt(1, &unresolved).unwrap().unwrap();
        assert_eq!(unresolved.phase, "outcome_unknown");
        assert_eq!(unresolved.process_group_cleanup, "unverified");
        assert_eq!(unresolved.process_revalidation, "not_revalidated");
        assert!(store.guarded_exec_receipts(2, 100).unwrap().is_empty());
    }
}
