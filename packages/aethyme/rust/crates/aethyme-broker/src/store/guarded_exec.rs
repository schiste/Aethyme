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

impl super::BrokerStore {
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
        self.conn
            .query_row(
                "SELECT receipt_id, session_id, phase, command_outcome, exit_code,
                        ownership_audit, process_group_cleanup, created_at_ms, updated_at_ms
                 FROM guarded_exec_receipts
                 WHERE session_id = ?1 AND receipt_id = ?2",
                params![session_id, receipt_id],
                guarded_exec_receipt_from_row,
            )
            .optional()
            .map_err(BrokerError::from)
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
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(BrokerError::from)
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
        assert!(store
            .guarded_exec_receipt(2, &receipt_id)
            .unwrap()
            .is_none());
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
        assert!(store
            .guarded_exec_receipt(1, oldest_terminal.as_deref().unwrap())
            .unwrap()
            .is_none());
        let unresolved = store.guarded_exec_receipt(1, &unresolved).unwrap().unwrap();
        assert_eq!(unresolved.phase, "outcome_unknown");
        assert_eq!(unresolved.process_group_cleanup, "unverified");
        assert_eq!(unresolved.process_revalidation, "not_revalidated");
        assert!(store.guarded_exec_receipts(2, 100).unwrap().is_empty());
    }
}
