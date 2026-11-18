//! Storage for explicit session stacks and the remote session-branch cleanup
//! ledger (schema v66). The decisions live in `crate::session_stack` and
//! `crate::remote_branch_cleanup`; this module only reads and writes rows.

use rusqlite::{OptionalExtension as _, params};

use super::BrokerStore;
use crate::error::BrokerError;
use crate::session_stack::{ParentState, SessionStack};

const STACK_SELECT: &str = "SELECT session_id, parent_session_id, parent_branch, parent_tip, \
     base_branch, parent_state, last_parent_tip, parent_pr_number, parent_merged_head, \
     parent_merge_commit, retargeted_at, created_at, updated_at FROM session_stacks";

fn stack_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionStack> {
    let state: String = row.get(5)?;
    Ok(SessionStack {
        session_id: row.get(0)?,
        parent_session_id: row.get(1)?,
        parent_branch: row.get(2)?,
        parent_tip: row.get(3)?,
        base_branch: row.get(4)?,
        // The column's CHECK admits only known states; anything else is read
        // as `open`, the state in which nothing is retargeted automatically.
        parent_state: ParentState::parse(&state).unwrap_or(ParentState::Open),
        last_parent_tip: row.get(6)?,
        parent_pr_number: row.get(7)?,
        parent_merged_head: row.get(8)?,
        parent_merge_commit: row.get(9)?,
        retargeted_at: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

/// One decision of the remote session-branch cleanup lane, as recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteBranchDecisionRow<'a> {
    pub session_id: i64,
    pub branch: &'a str,
    /// `deleted`, `absent`, `kept` or `failed`.
    pub outcome: &'a str,
    pub reason: &'a str,
    pub pr_number: Option<i64>,
    pub expected_oid: Option<&'a str>,
}

impl BrokerStore {
    /// Record that `session_id` was started on top of `parent_session_id`.
    pub(crate) fn insert_session_stack(
        &mut self,
        session_id: i64,
        parent_session_id: i64,
        parent_branch: &str,
        parent_tip: &str,
    ) -> Result<SessionStack, BrokerError> {
        let now = crate::clock::epoch_ms();
        self.conn.execute(
            "INSERT INTO session_stacks (session_id, parent_session_id, parent_branch,
                 parent_tip, base_branch, parent_state, last_parent_tip, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?3, 'open', ?4, ?5, ?5)",
            params![
                session_id,
                parent_session_id,
                parent_branch,
                parent_tip,
                now
            ],
        )?;
        self.session_stack(session_id)?
            .ok_or(BrokerError::SessionNotFound(session_id))
    }

    /// The stack record of one session, if it was started on top of another.
    pub(crate) fn session_stack(
        &self,
        session_id: i64,
    ) -> Result<Option<SessionStack>, BrokerError> {
        Ok(self
            .conn
            .query_row(
                &format!("{STACK_SELECT} WHERE session_id = ?1"),
                [session_id],
                stack_from_row,
            )
            .optional()?)
    }

    /// Sessions started on top of `parent_session_id`, oldest first.
    pub(crate) fn session_stack_children(
        &self,
        parent_session_id: i64,
    ) -> Result<Vec<SessionStack>, BrokerError> {
        let mut stmt = self.conn.prepare(&format!(
            "{STACK_SELECT} WHERE parent_session_id = ?1 ORDER BY session_id"
        ))?;
        let rows = stmt.query_map([parent_session_id], stack_from_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Stack records whose child session is not closed, oldest first.
    pub(crate) fn open_session_stacks(&self) -> Result<Vec<SessionStack>, BrokerError> {
        let mut stmt = self.conn.prepare(&format!(
            "{STACK_SELECT} WHERE session_id IN (
                 SELECT id FROM sessions WHERE cleanup_state = 'open')
             ORDER BY session_id"
        ))?;
        let rows = stmt.query_map([], stack_from_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Sessions stacked on, or started from, `branch` that are not cleaned:
    /// the same state filter as "every session cleaned", so a closed child
    /// that can still be restored keeps protecting its parent branch.
    pub(crate) fn open_sessions_stacked_on_branch(
        &self,
        branch: &str,
    ) -> Result<Vec<i64>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT st.session_id FROM session_stacks st
             JOIN sessions s ON s.id = st.session_id
             WHERE (st.parent_branch = ?1 OR st.base_branch = ?1)
               AND s.cleanup_state <> 'cleaned'
             ORDER BY st.session_id",
        )?;
        let rows = stmt.query_map([branch], |row| row.get(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Remember the parent tip a child was last told about.
    pub(crate) fn set_session_stack_parent_tip_seen(
        &mut self,
        session_id: i64,
        tip: &str,
    ) -> Result<(), BrokerError> {
        self.conn.execute(
            "UPDATE session_stacks SET last_parent_tip = ?2, updated_at = ?3
             WHERE session_id = ?1",
            params![session_id, tip, crate::clock::epoch_ms()],
        )?;
        Ok(())
    }

    /// Record what became of the parent's pull request.
    pub(crate) fn set_session_stack_parent_state(
        &mut self,
        session_id: i64,
        state: ParentState,
        pr_number: Option<i64>,
        merged_head: Option<&str>,
        merge_commit: Option<&str>,
    ) -> Result<(), BrokerError> {
        self.conn.execute(
            "UPDATE session_stacks
             SET parent_state = ?2,
                 parent_pr_number = COALESCE(?3, parent_pr_number),
                 parent_merged_head = COALESCE(?4, parent_merged_head),
                 parent_merge_commit = COALESCE(?5, parent_merge_commit),
                 updated_at = ?6
             WHERE session_id = ?1",
            params![
                session_id,
                state.as_str(),
                pr_number,
                merged_head,
                merge_commit,
                crate::clock::epoch_ms()
            ],
        )?;
        Ok(())
    }

    /// Move the branch a child's pull request targets and `sync` follows.
    pub(crate) fn set_session_stack_base(
        &mut self,
        session_id: i64,
        base_branch: &str,
    ) -> Result<(), BrokerError> {
        let now = crate::clock::epoch_ms();
        self.conn.execute(
            "UPDATE session_stacks SET base_branch = ?2, retargeted_at = ?3, updated_at = ?3
             WHERE session_id = ?1",
            params![session_id, base_branch, now],
        )?;
        Ok(())
    }

    /// Cleaned sessions on `agent/*` branches whose remote branch has no
    /// final decision yet (never decided, kept or failed), least recently
    /// decided first, at most `limit`, one row per branch.
    pub(crate) fn remote_branch_cleanup_candidates(
        &self,
        limit: usize,
    ) -> Result<Vec<(i64, String)>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT MAX(s.id), s.branch FROM sessions s
             LEFT JOIN remote_branch_cleanup r ON r.session_id = s.id
             WHERE s.cleanup_state = 'cleaned' AND s.branch LIKE 'agent/%'
               AND (r.session_id IS NULL OR r.outcome IN ('kept', 'failed'))
             GROUP BY s.branch
             ORDER BY MIN(COALESCE(r.decided_at, 0)), MAX(s.id)
             LIMIT ?1",
        )?;
        let rows = stmt.query_map([i64::try_from(limit).unwrap_or(i64::MAX)], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// How many distinct cleaned `agent/*` branches still await a final
    /// decision: the backlog the cleanup lane works through.
    pub(crate) fn remote_branch_cleanup_backlog(&self) -> Result<u64, BrokerError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(DISTINCT s.branch) FROM sessions s
             LEFT JOIN remote_branch_cleanup r ON r.session_id = s.id
             WHERE s.cleanup_state = 'cleaned' AND s.branch LIKE 'agent/%'
               AND (r.session_id IS NULL OR r.outcome IN ('kept', 'failed'))",
            [],
            |row| row.get(0),
        )?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    /// Whether every session that ever worked on `branch` is cleaned.
    pub(crate) fn every_session_on_branch_cleaned(
        &self,
        branch: &str,
    ) -> Result<bool, BrokerError> {
        let open: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM sessions WHERE branch = ?1 AND cleanup_state <> 'cleaned'",
            [branch],
            |row| row.get(0),
        )?;
        Ok(open == 0)
    }

    /// Record one decision, counting attempts per session.
    pub(crate) fn record_remote_branch_decision(
        &mut self,
        decision: &RemoteBranchDecisionRow<'_>,
    ) -> Result<(), BrokerError> {
        self.conn.execute(
            "INSERT INTO remote_branch_cleanup (session_id, branch, outcome, reason, pr_number,
                 expected_oid, attempts, decided_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7)
             ON CONFLICT (session_id) DO UPDATE SET
                 branch = excluded.branch,
                 outcome = excluded.outcome,
                 reason = excluded.reason,
                 pr_number = excluded.pr_number,
                 expected_oid = excluded.expected_oid,
                 attempts = remote_branch_cleanup.attempts + 1,
                 decided_at = excluded.decided_at",
            params![
                decision.session_id,
                decision.branch,
                decision.outcome,
                decision.reason,
                decision.pr_number,
                decision.expected_oid,
                crate::clock::epoch_ms()
            ],
        )?;
        Ok(())
    }

    /// `(outcome, reason, count)` over every recorded decision.
    pub(crate) fn remote_branch_decision_counts(
        &self,
    ) -> Result<Vec<(String, String, u64)>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT outcome, reason, COUNT(*) FROM remote_branch_cleanup
             GROUP BY outcome, reason ORDER BY outcome, reason",
        )?;
        let rows = stmt.query_map([], |row| {
            let count: i64 = row.get(2)?;
            Ok((row.get(0)?, row.get(1)?, u64::try_from(count).unwrap_or(0)))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, BrokerStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = BrokerStore::open(&dir.path().join("broker.db")).unwrap();
        (dir, store)
    }

    fn insert_session(store: &BrokerStore, id: i64, branch: &str, status: &str, cleanup: &str) {
        store
            .conn
            .execute(
                "INSERT INTO sessions (id, worktree_path, branch, origin, status, cleanup_state,
                     created_at, updated_at, last_activity_at)
                 VALUES (?1, ?2, ?3, 'spawned', ?4, ?5, 0, 0, 0)",
                params![id, format!("/w/{id}"), branch, status, cleanup],
            )
            .unwrap();
    }

    #[test]
    fn a_stack_round_trips_and_moves_its_base() {
        let (_dir, mut store) = store();
        insert_session(&store, 1, "agent/parent", "active", "open");
        insert_session(&store, 2, "agent/child", "active", "open");
        let stack = store
            .insert_session_stack(2, 1, "agent/parent", "abc")
            .unwrap();
        assert_eq!(stack.base_branch, "agent/parent");
        assert_eq!(stack.parent_state, ParentState::Open);
        assert_eq!(stack.last_parent_tip.as_deref(), Some("abc"));
        assert_eq!(store.session_stack_children(1).unwrap().len(), 1);
        assert_eq!(
            store
                .open_sessions_stacked_on_branch("agent/parent")
                .unwrap(),
            vec![2]
        );
        store
            .set_session_stack_parent_state(2, ParentState::Merged, Some(7), Some("h"), Some("m"))
            .unwrap();
        store.set_session_stack_base(2, "main").unwrap();
        let stack = store.session_stack(2).unwrap().unwrap();
        assert_eq!(stack.parent_state, ParentState::Merged);
        assert_eq!(stack.base_branch, "main");
        assert_eq!(stack.parent_pr_number, Some(7));
        assert!(stack.retargeted_at.is_some());
        // Still started from the parent branch, so it keeps protecting it.
        assert_eq!(
            store
                .open_sessions_stacked_on_branch("agent/parent")
                .unwrap(),
            vec![2]
        );
        assert_eq!(store.open_session_stacks().unwrap().len(), 1);
    }

    #[test]
    fn a_closed_child_still_protects_its_parent_branch_until_it_is_cleaned() {
        let (_dir, mut store) = store();
        insert_session(&store, 1, "agent/parent", "cleaned", "cleaned");
        insert_session(&store, 2, "agent/child", "exited", "closed");
        insert_session(&store, 3, "agent/gone", "cleaned", "cleaned");
        store
            .insert_session_stack(2, 1, "agent/parent", "abc")
            .unwrap();
        store
            .insert_session_stack(3, 1, "agent/parent", "abc")
            .unwrap();
        // A closed child can be restored, so it keeps the parent branch; a
        // cleaned one does not.
        assert_eq!(
            store
                .open_sessions_stacked_on_branch("agent/parent")
                .unwrap(),
            vec![2]
        );
        // Reconcile and status still only follow open children.
        assert!(store.open_session_stacks().unwrap().is_empty());
    }

    #[test]
    fn cleanup_candidates_are_cleaned_agent_branches_without_a_final_decision() {
        let (_dir, mut store) = store();
        insert_session(&store, 1, "agent/done", "cleaned", "cleaned");
        insert_session(&store, 2, "agent/open", "active", "open");
        insert_session(&store, 3, "feature/x", "cleaned", "cleaned");
        insert_session(&store, 4, "agent/deleted", "cleaned", "cleaned");
        insert_session(&store, 5, "agent/kept", "cleaned", "cleaned");
        store
            .record_remote_branch_decision(&RemoteBranchDecisionRow {
                session_id: 4,
                branch: "agent/deleted",
                outcome: "deleted",
                reason: "merged",
                pr_number: Some(1),
                expected_oid: Some("abc"),
            })
            .unwrap();
        store
            .record_remote_branch_decision(&RemoteBranchDecisionRow {
                session_id: 5,
                branch: "agent/kept",
                outcome: "kept",
                reason: "remote_tip_moved",
                pr_number: None,
                expected_oid: None,
            })
            .unwrap();
        let candidates = store.remote_branch_cleanup_candidates(10).unwrap();
        // Never-decided first, then the least recently kept.
        assert_eq!(
            candidates,
            vec![(1, "agent/done".to_string()), (5, "agent/kept".to_string())]
        );
        assert_eq!(store.remote_branch_cleanup_backlog().unwrap(), 2);
        assert!(store.every_session_on_branch_cleaned("agent/done").unwrap());
        assert!(!store.every_session_on_branch_cleaned("agent/open").unwrap());
        store
            .record_remote_branch_decision(&RemoteBranchDecisionRow {
                session_id: 5,
                branch: "agent/kept",
                outcome: "deleted",
                reason: "merged",
                pr_number: Some(2),
                expected_oid: Some("def"),
            })
            .unwrap();
        let counts = store.remote_branch_decision_counts().unwrap();
        assert_eq!(counts, vec![("deleted".into(), "merged".into(), 2)]);
        let attempts: i64 = store
            .conn
            .query_row(
                "SELECT attempts FROM remote_branch_cleanup WHERE session_id = 5",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(attempts, 2);
    }
}
