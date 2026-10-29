//! Resumable, revision-bound inventory of session checkout paths.
//!
//! This cache is only a discovery index. Callers must re-read the current
//! session cohort, ownership witness, lifecycle state, and filesystem evidence
//! before any cleanup mutation.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{BrokerStore, SESSION_SELECT, session_from_row};
use crate::error::BrokerError;
use crate::types::Session;

pub(crate) const CLEANUP_COHORT_NAMESPACE: &str = "session-worktrees";
pub(crate) const CHECKOUT_CLEANUP_RECONCILE_COOLDOWN_MS: i64 = 10 * 60 * 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CleanupCohortScanState {
    pub(crate) revision: i64,
    pub(crate) generation: i64,
    /// Identifies the most recent complete filesystem alias refresh lap.
    /// Unlike `generation`, this advances without resetting the group cursor.
    pub(crate) alias_refresh_generation: i64,
    pub(crate) progress_sequence: i64,
    pub(crate) high_water_session_id: i64,
    pub(crate) after_session_id: i64,
    pub(crate) complete: bool,
    pub(crate) progress_at_ms: i64,
    pub(crate) unresolved_rows: i64,
    pub(crate) pending_dirty_rows: i64,
}

#[derive(Debug)]
pub(crate) struct CleanupCohortScanPage {
    pub(crate) state: CleanupCohortScanState,
    pub(crate) sessions: Vec<Session>,
    pub(crate) dirty_sessions: Vec<CleanupCohortDirtySession>,
}

#[derive(Debug)]
pub(crate) struct CleanupCohortDirtySession {
    pub(crate) session_id: i64,
    pub(crate) revision: i64,
    pub(crate) session: Option<Session>,
}

#[derive(Debug)]
pub(crate) struct CleanupCohortDirtyResolvedRow {
    pub(crate) session_id: i64,
    pub(crate) revision: i64,
    pub(crate) row: Option<CleanupCohortResolvedRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CleanupCohortResolvedRow {
    pub(crate) session_id: i64,
    pub(crate) raw_path: String,
    pub(crate) canonical_path: String,
    pub(crate) git_dir: Option<String>,
    pub(crate) resolution_error: Option<String>,
    pub(crate) cleanup_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CleanupCohortIndexedRow {
    pub(crate) session_id: i64,
    pub(crate) raw_path: String,
    pub(crate) canonical_path: String,
    pub(crate) git_dir: Option<String>,
    pub(crate) resolution_error: Option<String>,
    pub(crate) cleanup_state: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CleanupCohortPersistOutcome {
    Stored,
    Changed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CheckoutCleanupRequest {
    pub(crate) requested_at_ms: i64,
    pub(crate) not_before_ms: i64,
    pub(crate) completed_at_ms: Option<i64>,
    pub(crate) compact_requested_at_ms: Option<i64>,
    pub(crate) compact_retry_after_ms: Option<i64>,
    pub(crate) compact_completed_at_ms: Option<i64>,
}

/// Insert the per-session outbox row in the same transaction as the closed
/// state transition. A repeated close preserves an existing pending request
/// and does not resurrect one that was already completed.
pub(crate) fn enqueue_checkout_cleanup_in_tx(
    tx: &rusqlite::Transaction<'_>,
    session_id: i64,
    closed_at_ms: i64,
    grace_hours: u32,
    compact_checkout: bool,
) -> Result<usize, BrokerError> {
    enqueue_checkout_cleanup_in_tx_at(
        tx,
        session_id,
        closed_at_ms,
        closed_at_ms,
        grace_hours,
        compact_checkout,
    )
}

fn enqueue_checkout_cleanup_in_tx_at(
    tx: &rusqlite::Transaction<'_>,
    session_id: i64,
    closed_at_ms: i64,
    requested_at_ms: i64,
    grace_hours: u32,
    compact_checkout: bool,
) -> Result<usize, BrokerError> {
    let not_before_ms =
        closed_at_ms.saturating_add(i64::from(grace_hours).saturating_mul(3_600_000));
    Ok(tx.execute(
        "INSERT INTO closed_checkout_cleanup_requests (
             session_id, requested_at_ms, not_before_ms, completed_at_ms,
             last_progress_at_ms, compact_requested_at_ms,
             compact_retry_after_ms, compact_completed_at_ms
         ) SELECT ?1, ?2, ?3, NULL, ?2, ?4, NULL, NULL
           FROM sessions WHERE id = ?1 AND cleanup_state = 'closed'
         ON CONFLICT(session_id) DO UPDATE SET
             requested_at_ms = MIN(requested_at_ms, excluded.requested_at_ms),
             not_before_ms = MAX(
                 closed_checkout_cleanup_requests.not_before_ms,
                 excluded.not_before_ms
             ),
             last_progress_at_ms = MAX(last_progress_at_ms, excluded.last_progress_at_ms),
             compact_requested_at_ms = CASE
                 WHEN excluded.compact_requested_at_ms IS NOT NULL
                      AND closed_checkout_cleanup_requests.compact_completed_at_ms IS NULL
                 THEN COALESCE(
                     closed_checkout_cleanup_requests.compact_requested_at_ms,
                     excluded.compact_requested_at_ms
                 )
                 ELSE closed_checkout_cleanup_requests.compact_requested_at_ms
             END,
             compact_retry_after_ms = CASE
                 WHEN excluded.compact_requested_at_ms IS NOT NULL
                      AND closed_checkout_cleanup_requests.compact_completed_at_ms IS NULL
                 THEN NULL
                 ELSE closed_checkout_cleanup_requests.compact_retry_after_ms
             END
         WHERE closed_checkout_cleanup_requests.completed_at_ms IS NULL",
        params![
            session_id,
            requested_at_ms,
            not_before_ms,
            compact_checkout.then_some(requested_at_ms)
        ],
    )?)
}

fn latest_session_finish_marked_kept(
    conn: &rusqlite::Connection,
    session_id: i64,
) -> Result<bool, BrokerError> {
    let payload = conn
        .query_row(
            "SELECT payload_json FROM events
             WHERE session_id = ?1 AND kind = ?2
             ORDER BY id DESC LIMIT 1",
            params![session_id, crate::events::SESSION_FINISHED],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(payload
        .as_deref()
        .and_then(|payload| serde_json::from_str::<serde_json::Value>(payload).ok())
        .and_then(|payload| {
            payload
                .pointer("/cleanup/kept")
                .and_then(serde_json::Value::as_bool)
        })
        .unwrap_or(false))
}

impl BrokerStore {
    /// Begin one fresh pass over every registered session path. An unfinished
    /// lap resumes across worker invocations; a completed lap is rewound only
    /// when a new cleanup pass explicitly asks for fresh filesystem coverage.
    pub(crate) fn begin_cleanup_cohort_alias_refresh(
        &mut self,
        namespace: &str,
        now_ms: i64,
    ) -> Result<CleanupCohortScanState, BrokerError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let revision: i64 = tx.query_row(
            "SELECT revision FROM session_cleanup_revision WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        let high_water: i64 =
            tx.query_row("SELECT COALESCE(MAX(id), 0) FROM sessions", [], |row| {
                row.get(0)
            })?;
        let dirty_count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM cleanup_cohort_dirty_sessions",
            [],
            |row| row.get(0),
        )?;
        let prior = tx
            .query_row(
                "SELECT revision, generation, alias_refresh_generation,
                        progress_sequence, high_water_session_id, after_session_id,
                        complete, progress_at_ms
                 FROM cleanup_cohort_scans WHERE namespace = ?1",
                [namespace],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, bool>(6)?,
                        row.get::<_, i64>(7)?,
                    ))
                },
            )
            .optional()?;
        let state = match prior {
            Some((
                scan_revision,
                generation,
                alias_generation,
                progress_sequence,
                old_high_water,
                after,
                complete,
                progress,
            )) if !complete || dirty_count > 0 => {
                let effective_high_water = old_high_water.max(high_water);
                if effective_high_water != old_high_water {
                    tx.execute(
                        "UPDATE cleanup_cohort_scans SET high_water_session_id = ?2
                             WHERE namespace = ?1",
                        params![namespace, effective_high_water],
                    )?;
                }
                CleanupCohortScanState {
                    revision: scan_revision,
                    generation,
                    alias_refresh_generation: alias_generation,
                    progress_sequence,
                    high_water_session_id: effective_high_water,
                    after_session_id: after,
                    complete: false,
                    progress_at_ms: progress,
                    unresolved_rows: 0,
                    pending_dirty_rows: dirty_count,
                }
            }
            Some((_, generation, alias_generation, progress_sequence, _, _, _, progress_at_ms)) => {
                let next_alias_generation = alias_generation.saturating_add(1).max(1);
                let complete = high_water == 0 && dirty_count == 0;
                tx.execute(
                    "UPDATE cleanup_cohort_scans SET revision = ?2,
                         alias_refresh_generation = ?3,
                         high_water_session_id = ?4, after_session_id = 0,
                         complete = ?5
                     WHERE namespace = ?1",
                    params![
                        namespace,
                        revision,
                        next_alias_generation,
                        high_water,
                        complete
                    ],
                )?;
                CleanupCohortScanState {
                    revision,
                    generation,
                    alias_refresh_generation: next_alias_generation,
                    progress_sequence,
                    high_water_session_id: high_water,
                    after_session_id: 0,
                    complete,
                    progress_at_ms,
                    unresolved_rows: 0,
                    pending_dirty_rows: dirty_count,
                }
            }
            None => {
                let complete = high_water == 0 && dirty_count == 0;
                tx.execute(
                    "INSERT INTO cleanup_cohort_scans (
                         namespace, revision, generation, alias_refresh_generation,
                         high_water_session_id, after_session_id, complete, progress_at_ms
                     ) VALUES (?1, ?2, 1, 1, ?3, 0, ?4, ?5)",
                    params![namespace, revision, high_water, complete, now_ms],
                )?;
                CleanupCohortScanState {
                    revision,
                    generation: 1,
                    alias_refresh_generation: 1,
                    progress_sequence: 0,
                    high_water_session_id: high_water,
                    after_session_id: 0,
                    complete,
                    progress_at_ms: now_ms,
                    unresolved_rows: 0,
                    pending_dirty_rows: dirty_count,
                }
            }
        };
        let unresolved_rows: i64 = tx.query_row(
            "SELECT COUNT(*) FROM cleanup_cohort_scan_rows
             WHERE namespace = ?1 AND resolution_error IS NOT NULL",
            [namespace],
            |row| row.get(0),
        )?;
        tx.commit()?;
        Ok(CleanupCohortScanState {
            unresolved_rows,
            ..state
        })
    }

    pub(crate) fn pending_checkout_cleanup_request(
        &self,
        session_id: i64,
    ) -> Result<Option<CheckoutCleanupRequest>, BrokerError> {
        self.conn
            .query_row(
                "SELECT requested_at_ms, not_before_ms, completed_at_ms,
                                compact_requested_at_ms, compact_retry_after_ms,
                                compact_completed_at_ms
                 FROM closed_checkout_cleanup_requests WHERE session_id = ?1",
                [session_id],
                |row| {
                    Ok(CheckoutCleanupRequest {
                        requested_at_ms: row.get(0)?,
                        not_before_ms: row.get(1)?,
                        completed_at_ms: row.get(2)?,
                        compact_requested_at_ms: row.get(3)?,
                        compact_retry_after_ms: row.get(4)?,
                        compact_completed_at_ms: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(BrokerError::from)
    }

    /// Find a bounded page of already-terminal sessions whose physical
    /// checkout removal may have committed just before the outbox settlement
    /// crashed. The caller must verify the recorded path is absent before
    /// completing either obligation.
    pub(crate) fn pending_terminal_checkout_cleanup_ids(
        &self,
        now_ms: i64,
        after_session_id: i64,
        limit: usize,
    ) -> Result<Vec<i64>, BrokerError> {
        let mut statement = self.conn.prepare(
            "SELECT r.session_id
             FROM closed_checkout_cleanup_requests r
             JOIN sessions s ON s.id = r.session_id
             WHERE r.session_id > ?1
               AND s.status = 'cleaned' AND s.cleanup_state = 'cleaned'
               AND (r.completed_at_ms IS NULL OR
                    (r.compact_requested_at_ms IS NOT NULL
                     AND r.compact_completed_at_ms IS NULL))
               AND CASE
                   WHEN r.last_progress_at_ms <= r.requested_at_ms
                   THEN r.requested_at_ms
                   ELSE r.last_progress_at_ms + ?2
               END <= ?3
             ORDER BY r.session_id LIMIT ?4",
        )?;
        let rows = statement.query_map(
            params![
                after_session_id,
                CHECKOUT_CLEANUP_RECONCILE_COOLDOWN_MS,
                now_ms,
                limit.clamp(1, 128) as i64
            ],
            |row| row.get::<_, i64>(0),
        )?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(BrokerError::from)
    }

    /// Avoid immediately retrying an inconsistent terminal record whose
    /// checkout path still exists or could not be inspected. This does not
    /// settle an obligation or change its original grace timestamp.
    pub(crate) fn defer_terminal_checkout_cleanup_reconciliation(
        &mut self,
        session_ids: &[i64],
        attempted_at_ms: i64,
    ) -> Result<usize, BrokerError> {
        let mut updated = 0usize;
        for session_id in session_ids {
            updated += self.conn.execute(
                "UPDATE closed_checkout_cleanup_requests
                 SET last_progress_at_ms = MAX(?2, requested_at_ms + 1)
                 WHERE session_id = ?1
                   AND (completed_at_ms IS NULL OR
                        (compact_requested_at_ms IS NOT NULL
                         AND compact_completed_at_ms IS NULL))
                   AND EXISTS (
                       SELECT 1 FROM sessions s
                       WHERE s.id = ?1 AND s.status = 'cleaned'
                         AND s.cleanup_state = 'cleaned'
                   )",
                params![session_id, attempted_at_ms],
            )?;
        }
        Ok(updated)
    }

    pub(crate) fn next_pending_checkout_cleanup_at(&self) -> Result<Option<i64>, BrokerError> {
        self.next_pending_checkout_cleanup_at_with_compaction(false)
    }

    pub(crate) fn next_pending_checkout_cleanup_at_with_compaction(
        &self,
        include_compaction: bool,
    ) -> Result<Option<i64>, BrokerError> {
        self.conn
            .query_row(
                "SELECT MIN(next_at) FROM (
                     SELECT CASE
                         WHEN s.status = 'cleaned' AND s.cleanup_state = 'cleaned'
                         THEN CASE
                             WHEN r.last_progress_at_ms <= r.requested_at_ms
                             THEN r.requested_at_ms
                             ELSE r.last_progress_at_ms + ?2
                         END
                         ELSE r.not_before_ms
                     END AS next_at
                     FROM closed_checkout_cleanup_requests r
                     JOIN sessions s ON s.id = r.session_id
                     WHERE r.completed_at_ms IS NULL
                       AND s.origin = 'spawned'
                       AND s.status IN ('closed', 'cleaned')
                       AND s.cleanup_state IN ('closed', 'cleaned')
                     UNION ALL
                     SELECT CASE
                         WHEN s.status = 'cleaned' AND s.cleanup_state = 'cleaned'
                         THEN CASE
                             WHEN r.last_progress_at_ms <= r.requested_at_ms
                             THEN r.requested_at_ms
                             ELSE r.last_progress_at_ms + ?2
                         END
                         ELSE COALESCE(r.compact_retry_after_ms, r.compact_requested_at_ms)
                     END AS next_at
                     FROM closed_checkout_cleanup_requests r
                     JOIN sessions s ON s.id = r.session_id
                     WHERE ?1
                       AND r.compact_requested_at_ms IS NOT NULL
                       AND r.compact_completed_at_ms IS NULL
                       AND s.origin = 'spawned'
                       AND s.status IN ('closed', 'cleaned')
                       AND s.cleanup_state IN ('closed', 'cleaned')
                       AND EXISTS (
                           SELECT 1 FROM spawned_worktree_instances w
                           WHERE w.session_id = s.id
                       )
                 )",
                params![include_compaction, CHECKOUT_CLEANUP_RECONCILE_COOLDOWN_MS],
                |row| row.get(0),
            )
            .map_err(BrokerError::from)
    }

    pub(crate) fn next_pending_checkout_compaction_at(&self) -> Result<Option<i64>, BrokerError> {
        self.conn
            .query_row(
                "SELECT MIN(CASE
                     WHEN s.status = 'cleaned' AND s.cleanup_state = 'cleaned'
                     THEN CASE
                         WHEN r.last_progress_at_ms <= r.requested_at_ms
                         THEN r.requested_at_ms
                         ELSE r.last_progress_at_ms + ?1
                     END
                     ELSE COALESCE(r.compact_retry_after_ms, r.compact_requested_at_ms)
                 END)
                 FROM closed_checkout_cleanup_requests r
                 JOIN sessions s ON s.id = r.session_id
                 WHERE r.compact_requested_at_ms IS NOT NULL
                   AND r.compact_completed_at_ms IS NULL
                   AND s.origin = 'spawned'
                   AND s.status IN ('closed', 'cleaned')
                   AND s.cleanup_state IN ('closed', 'cleaned')
                   AND EXISTS (
                       SELECT 1 FROM spawned_worktree_instances w
                       WHERE w.session_id = s.id
                   )",
                [CHECKOUT_CLEANUP_RECONCILE_COOLDOWN_MS],
                |row| row.get(0),
            )
            .map_err(BrokerError::from)
    }

    /// Add compact requests for a bounded set of currently retained Spawned
    /// rows. The caller supplies IDs from the keyset path index; this method
    /// rechecks the authoritative session rows in the same write transaction.
    pub(crate) fn request_closed_spawned_checkout_compactions(
        &mut self,
        session_ids: &[i64],
        grace_hours: u32,
        requested_at_ms: i64,
    ) -> Result<usize, BrokerError> {
        if session_ids.is_empty() {
            return Ok(0);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut scheduled = 0usize;
        for session_id in session_ids.iter().copied().collect::<BTreeSet<_>>() {
            let row = tx
                .query_row(
                    "SELECT origin, status, cleanup_state, closed_at,
                            EXISTS (
                                SELECT 1 FROM spawned_worktree_instances w
                                WHERE w.session_id = sessions.id
                            )
                     FROM sessions WHERE id = ?1",
                    [session_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, Option<i64>>(3)?,
                            row.get::<_, bool>(4)?,
                        ))
                    },
                )
                .optional()?;
            let Some((origin, status, cleanup_state, Some(closed_at_ms), has_witness)) = row else {
                continue;
            };
            if origin != "spawned"
                || !matches!(status.as_str(), "closed" | "cleaned")
                || cleanup_state != "closed"
                || !has_witness
                || latest_session_finish_marked_kept(&tx, session_id)?
            {
                continue;
            }
            let not_before_ms =
                closed_at_ms.saturating_add(i64::from(grace_hours).saturating_mul(3_600_000));
            scheduled += tx.execute(
                "INSERT INTO closed_checkout_cleanup_requests (
                     session_id, requested_at_ms, not_before_ms, completed_at_ms,
                     last_progress_at_ms, compact_requested_at_ms,
                     compact_retry_after_ms, compact_completed_at_ms
                 ) VALUES (?1, ?2, ?3, NULL, ?2, ?2, NULL, NULL)
                 ON CONFLICT(session_id) DO UPDATE SET
                     requested_at_ms = MIN(
                         closed_checkout_cleanup_requests.requested_at_ms,
                         excluded.requested_at_ms
                     ),
                     not_before_ms = MAX(
                         closed_checkout_cleanup_requests.not_before_ms,
                         excluded.not_before_ms
                     ),
                     compact_requested_at_ms = COALESCE(
                         closed_checkout_cleanup_requests.compact_requested_at_ms,
                         excluded.compact_requested_at_ms
                     ),
                     compact_retry_after_ms = CASE
                         WHEN closed_checkout_cleanup_requests.compact_requested_at_ms IS NULL
                         THEN NULL
                         ELSE closed_checkout_cleanup_requests.compact_retry_after_ms
                     END
                 WHERE closed_checkout_cleanup_requests.compact_completed_at_ms IS NULL",
                params![session_id, requested_at_ms, not_before_ms],
            )?;
        }
        tx.commit()?;
        Ok(scheduled)
    }

    /// Backfill durable full-removal obligations for a bounded page of
    /// retained Spawned checkouts. This covers old runtimes that closed a
    /// session before transactional outbox insertion existed. The caller
    /// supplies IDs from the rotating cohort index; this transaction
    /// revalidates origin, terminal state and original close timestamp.
    pub(crate) fn request_closed_spawned_checkout_cleanups(
        &mut self,
        session_ids: &[i64],
        grace_hours: u32,
        requested_at_ms: i64,
    ) -> Result<usize, BrokerError> {
        if session_ids.is_empty() {
            return Ok(0);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut scheduled = 0usize;
        for session_id in session_ids.iter().copied().collect::<BTreeSet<_>>() {
            let row = tx
                .query_row(
                    "SELECT origin, status, cleanup_state, closed_at
                     FROM sessions WHERE id = ?1",
                    [session_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, Option<i64>>(3)?,
                        ))
                    },
                )
                .optional()?;
            let Some((origin, status, cleanup_state, Some(closed_at_ms))) = row else {
                continue;
            };
            if origin != "spawned"
                || !matches!(status.as_str(), "closed" | "cleaned")
                || cleanup_state != "closed"
            {
                continue;
            }
            if latest_session_finish_marked_kept(&tx, session_id)? {
                continue;
            }
            scheduled += enqueue_checkout_cleanup_in_tx_at(
                &tx,
                session_id,
                closed_at_ms,
                requested_at_ms,
                grace_hours,
                false,
            )?;
        }
        tx.commit()?;
        Ok(scheduled)
    }

    pub(crate) fn due_checkout_cleanup_compactions(
        &self,
        now_ms: i64,
        limit: usize,
    ) -> Result<Vec<i64>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT r.session_id FROM closed_checkout_cleanup_requests r
             JOIN sessions s ON s.id = r.session_id
             WHERE r.compact_requested_at_ms IS NOT NULL
               AND r.compact_completed_at_ms IS NULL
               AND COALESCE(r.compact_retry_after_ms, r.compact_requested_at_ms) <= ?1
               AND s.origin = 'spawned'
               AND s.status IN ('closed', 'cleaned')
               AND s.cleanup_state = 'closed'
               AND EXISTS (
                   SELECT 1 FROM spawned_worktree_instances w WHERE w.session_id = s.id
               )
             ORDER BY COALESCE(r.compact_retry_after_ms, r.compact_requested_at_ms), r.session_id
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![now_ms, limit.clamp(1, 128) as i64], |row| {
            row.get::<_, i64>(0)
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(BrokerError::from)
    }

    pub(crate) fn defer_checkout_cleanup_compactions(
        &mut self,
        session_ids: &[i64],
        retry_after_ms: i64,
        now_ms: i64,
    ) -> Result<usize, BrokerError> {
        let mut updated = 0usize;
        for session_id in session_ids {
            updated += self.conn.execute(
                "UPDATE closed_checkout_cleanup_requests
                 SET compact_retry_after_ms = ?2, last_progress_at_ms = ?3
                 WHERE session_id = ?1 AND compact_requested_at_ms IS NOT NULL
                   AND compact_completed_at_ms IS NULL",
                params![session_id, retry_after_ms, now_ms],
            )?;
        }
        Ok(updated)
    }

    /// A verified cleanup operation removed the checkout and the caller has
    /// recorded its terminal session transition. Both the full checkout and
    /// compact obligations are then satisfied, so neither lane should
    /// schedule a later no-op wake. The terminal-state predicate prevents a
    /// delayed removal result from completing a newer reactivated obligation.
    pub(crate) fn complete_checkout_cleanup_requests_after_removal(
        &mut self,
        session_ids: &[i64],
        now_ms: i64,
    ) -> Result<usize, BrokerError> {
        let mut updated = 0usize;
        for session_id in session_ids {
            updated += self.conn.execute(
                "UPDATE closed_checkout_cleanup_requests
                 SET completed_at_ms = COALESCE(completed_at_ms, ?2),
                     compact_completed_at_ms = COALESCE(compact_completed_at_ms, ?2),
                     compact_retry_after_ms = NULL,
                     last_progress_at_ms = ?2
                 WHERE session_id = ?1
                   AND EXISTS (
                       SELECT 1 FROM sessions s
                       WHERE s.id = ?1 AND s.status = 'cleaned'
                         AND s.cleanup_state = 'cleaned'
                   )",
                params![session_id, now_ms],
            )?;
        }
        Ok(updated)
    }

    /// Start or resume one bounded page of the session-path inventory. The
    /// revision and page are read in one write transaction, so the returned
    /// rows share the generation stored in the checkpoint.
    pub(crate) fn cleanup_cohort_scan_page(
        &mut self,
        namespace: &str,
        page_size: usize,
        now_ms: i64,
    ) -> Result<CleanupCohortScanPage, BrokerError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let revision: i64 = tx.query_row(
            "SELECT revision FROM session_cleanup_revision WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        let current_high_water: i64 =
            tx.query_row("SELECT COALESCE(MAX(id), 0) FROM sessions", [], |row| {
                row.get(0)
            })?;
        let dirty_count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM cleanup_cohort_dirty_sessions",
            [],
            |row| row.get(0),
        )?;
        let prior = tx
            .query_row(
                "SELECT revision, generation, alias_refresh_generation, progress_sequence,
                        high_water_session_id, after_session_id, complete, progress_at_ms
                 FROM cleanup_cohort_scans WHERE namespace = ?1",
                [namespace],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, bool>(6)?,
                        row.get::<_, i64>(7)?,
                    ))
                },
            )
            .optional()?;
        let (
            scan_revision,
            generation,
            alias_refresh_generation,
            mut progress_sequence,
            high_water,
            mut after_session_id,
            mut stored_complete,
            mut progress_at_ms,
        ) = match prior {
            Some((
                scan_revision,
                generation,
                alias_refresh_generation,
                progress_sequence,
                old_high_water,
                after,
                complete,
                progress_at_ms,
            )) if (scan_revision == revision && old_high_water == current_high_water)
                || dirty_count > 0 =>
            {
                let high_water = old_high_water.max(current_high_water);
                if high_water != old_high_water {
                    tx.execute(
                        "UPDATE cleanup_cohort_scans
                             SET high_water_session_id = ?2
                             WHERE namespace = ?1",
                        params![namespace, high_water],
                    )?;
                }
                (
                    scan_revision,
                    generation,
                    alias_refresh_generation,
                    progress_sequence,
                    high_water,
                    after,
                    complete,
                    progress_at_ms,
                )
            }
            Some((_, generation, alias_generation, progress_sequence, _, _, _, progress_at_ms)) => {
                tx.execute(
                    "DELETE FROM cleanup_cohort_scans WHERE namespace = ?1",
                    [namespace],
                )?;
                let generation = generation.saturating_add(1).max(1);
                let alias_refresh_generation = alias_generation.saturating_add(1).max(1);
                let complete = current_high_water == 0 && dirty_count == 0;
                tx.execute(
                    "INSERT INTO cleanup_cohort_scans (
                             namespace, revision, generation, alias_refresh_generation,
                             progress_sequence, high_water_session_id, after_session_id,
                             complete, progress_at_ms
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?8)",
                    params![
                        namespace,
                        revision,
                        generation,
                        alias_refresh_generation,
                        progress_sequence,
                        current_high_water,
                        complete,
                        progress_at_ms
                    ],
                )?;
                (
                    revision,
                    generation,
                    alias_refresh_generation,
                    progress_sequence,
                    current_high_water,
                    0,
                    complete,
                    progress_at_ms,
                )
            }
            None => {
                let generation = 1;
                let alias_refresh_generation = 1;
                let progress_sequence = 0;
                let complete = current_high_water == 0 && dirty_count == 0;
                tx.execute(
                    "INSERT INTO cleanup_cohort_scans (
                             namespace, revision, generation, alias_refresh_generation,
                             high_water_session_id, after_session_id, complete, progress_at_ms
                         ) VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7)",
                    params![
                        namespace,
                        revision,
                        generation,
                        alias_refresh_generation,
                        current_high_water,
                        complete,
                        now_ms
                    ],
                )?;
                (
                    revision,
                    generation,
                    alias_refresh_generation,
                    progress_sequence,
                    current_high_water,
                    0,
                    complete,
                    now_ms,
                )
            }
        };

        let mut dirty_sessions = Vec::new();
        {
            let mut stmt = tx.prepare(
                "SELECT session_id, revision FROM cleanup_cohort_dirty_sessions
                 ORDER BY session_id LIMIT ?1",
            )?;
            let rows = stmt.query_map([page_size.clamp(1, 32) as i64], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (session_id, dirty_revision) = row?;
                let query = format!("{} WHERE id = ?1", SESSION_SELECT);
                let session: Option<Session> = tx
                    .query_row(&query, [session_id], session_from_row)
                    .optional()?
                    .transpose()?;
                dirty_sessions.push(CleanupCohortDirtySession {
                    session_id,
                    revision: dirty_revision,
                    session,
                });
            }
        }

        let mut sessions = Vec::new();
        if dirty_sessions.is_empty() && !stored_complete {
            let mut stmt = tx.prepare(&format!(
                "{} WHERE id > ?1 AND id <= ?2 ORDER BY id LIMIT ?3",
                SESSION_SELECT
            ))?;
            let rows = stmt.query_map(
                params![after_session_id, high_water, page_size.clamp(1, 32) as i64],
                session_from_row,
            )?;
            for row in rows {
                sessions.push(row??);
            }
            if sessions.is_empty() {
                // Deleted IDs can leave gaps in the bounded range. With no
                // dirty rows left, an empty page proves there is no remaining
                // session row to index.
                after_session_id = high_water;
                stored_complete = true;
                progress_at_ms = now_ms;
                progress_sequence = progress_sequence.saturating_add(1);
                tx.execute(
                    "UPDATE cleanup_cohort_scans SET revision = ?2,
                         after_session_id = ?3, complete = 1, progress_at_ms = ?4,
                         progress_sequence = progress_sequence + 1
                     WHERE namespace = ?1",
                    params![namespace, revision, after_session_id, now_ms],
                )?;
            }
        }
        let unresolved_rows: i64 = tx.query_row(
            "SELECT COUNT(*) FROM cleanup_cohort_scan_rows
             WHERE namespace = ?1 AND resolution_error IS NOT NULL",
            [namespace],
            |row| row.get(0),
        )?;
        tx.commit()?;

        let pending_dirty_rows = dirty_count;
        let complete = stored_complete
            && dirty_sessions.is_empty()
            && scan_revision == revision
            && high_water == current_high_water;

        Ok(CleanupCohortScanPage {
            state: CleanupCohortScanState {
                revision,
                generation,
                alias_refresh_generation,
                progress_sequence,
                high_water_session_id: high_water,
                after_session_id,
                complete,
                progress_at_ms,
                unresolved_rows,
                pending_dirty_rows,
            },
            sessions,
            dirty_sessions,
        })
    }

    /// Commit one resolved page only if it still belongs to the current
    /// authority revision and checkpoint cursor. A mismatch never exposes a
    /// partial index; the next page call resets to the current generation.
    pub(crate) fn persist_cleanup_cohort_scan_page(
        &mut self,
        namespace: &str,
        revision: i64,
        generation: i64,
        alias_refresh_generation: i64,
        expected_after: i64,
        rows: &[CleanupCohortResolvedRow],
        complete: bool,
        now_ms: i64,
    ) -> Result<CleanupCohortPersistOutcome, BrokerError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current_revision: i64 = tx.query_row(
            "SELECT revision FROM session_cleanup_revision WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        let current = tx
            .query_row(
                "SELECT revision, generation, alias_refresh_generation, high_water_session_id,
                        after_session_id, complete
                 FROM cleanup_cohort_scans WHERE namespace = ?1",
                [namespace],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, bool>(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            scan_revision,
            scan_generation,
            current_alias_refresh_generation,
            high_water,
            after,
            already_complete,
        )) = current
        else {
            tx.commit()?;
            return Ok(CleanupCohortPersistOutcome::Changed);
        };
        if scan_revision != revision
            || scan_generation != generation
            || current_alias_refresh_generation != alias_refresh_generation
            || after != expected_after
            || already_complete
        {
            tx.commit()?;
            return Ok(CleanupCohortPersistOutcome::Changed);
        }

        let mut previous = expected_after;
        for row in rows {
            if row.session_id <= previous || row.session_id > high_water {
                return Err(BrokerError::InvalidSchemaMetadata(
                    "cleanup cohort page session ids are not ordered within its high-water mark"
                        .into(),
                ));
            }
            previous = row.session_id;
            replace_cleanup_cohort_row(&tx, namespace, row)?;
        }
        let new_after = rows.last().map_or(high_water, |row| row.session_id);
        if complete && new_after < high_water {
            return Err(BrokerError::InvalidSchemaMetadata(
                "cleanup cohort scan cannot complete before its high-water mark".into(),
            ));
        }
        let pending_dirty_rows: i64 = tx.query_row(
            "SELECT COUNT(*) FROM cleanup_cohort_dirty_sessions",
            [],
            |row| row.get(0),
        )?;
        let current_high_water: i64 =
            tx.query_row("SELECT COALESCE(MAX(id), 0) FROM sessions", [], |row| {
                row.get(0)
            })?;
        let complete = complete && pending_dirty_rows == 0 && current_revision == revision;
        tx.execute(
            "UPDATE cleanup_cohort_scans
             SET after_session_id = ?2, high_water_session_id = ?3,
                 complete = ?4, progress_at_ms = ?5,
                 progress_sequence = progress_sequence + 1
             WHERE namespace = ?1 AND generation = ?6
               AND alias_refresh_generation = ?7 AND after_session_id = ?8",
            params![
                namespace,
                new_after,
                high_water.max(current_high_water),
                complete,
                now_ms,
                generation,
                alias_refresh_generation,
                expected_after
            ],
        )?;
        tx.commit()?;
        Ok(CleanupCohortPersistOutcome::Stored)
    }

    pub(crate) fn persist_cleanup_cohort_dirty_page(
        &mut self,
        namespace: &str,
        generation: i64,
        alias_refresh_generation: i64,
        items: &[CleanupCohortDirtyResolvedRow],
        now_ms: i64,
    ) -> Result<CleanupCohortPersistOutcome, BrokerError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let scan = tx
            .query_row(
                "SELECT revision, generation, alias_refresh_generation, high_water_session_id,
                        after_session_id, complete
                 FROM cleanup_cohort_scans WHERE namespace = ?1",
                [namespace],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, bool>(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            scan_revision,
            scan_generation,
            current_alias_refresh_generation,
            old_high_water,
            old_after,
            was_complete,
        )) = scan
        else {
            tx.commit()?;
            return Ok(CleanupCohortPersistOutcome::Changed);
        };
        if scan_generation != generation
            || current_alias_refresh_generation != alias_refresh_generation
        {
            tx.commit()?;
            return Ok(CleanupCohortPersistOutcome::Changed);
        }

        let mut affected_paths = BTreeSet::<String>::new();
        let mut applied_items = 0i64;
        for item in items {
            let current_dirty_revision = tx
                .query_row(
                    "SELECT revision FROM cleanup_cohort_dirty_sessions
                     WHERE session_id = ?1",
                    [item.session_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            if current_dirty_revision != Some(item.revision) {
                continue;
            }
            applied_items += 1;
            if item
                .row
                .as_ref()
                .is_some_and(|row| row.session_id != item.session_id)
            {
                return Err(BrokerError::InvalidSchemaMetadata(
                    "dirty cleanup cohort row has a mismatched session id".into(),
                ));
            }
            if let Some(old_path) = remove_cleanup_cohort_row(&tx, namespace, item.session_id)? {
                affected_paths.insert(old_path);
            }
            if let Some(row) = &item.row {
                affected_paths.insert(row.canonical_path.clone());
                insert_cleanup_cohort_row(&tx, namespace, row)?;
            }
            tx.execute(
                "DELETE FROM cleanup_cohort_dirty_sessions
                 WHERE session_id = ?1 AND revision = ?2",
                params![item.session_id, item.revision],
            )?;
        }
        for path in &affected_paths {
            clear_cleanup_cohort_group(&tx, namespace, path)?;
        }
        for path in &affected_paths {
            recompute_cleanup_cohort_group(&tx, namespace, path)?;
        }

        let current_revision: i64 = tx.query_row(
            "SELECT revision FROM session_cleanup_revision WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        let current_high_water: i64 =
            tx.query_row("SELECT COALESCE(MAX(id), 0) FROM sessions", [], |row| {
                row.get(0)
            })?;
        let pending_dirty_rows: i64 = tx.query_row(
            "SELECT COUNT(*) FROM cleanup_cohort_dirty_sessions",
            [],
            |row| row.get(0),
        )?;
        let can_complete = pending_dirty_rows == 0 && (was_complete || old_after >= old_high_water);
        let (after_session_id, high_water, complete, revision) = if pending_dirty_rows == 0 {
            if can_complete {
                (
                    current_high_water,
                    current_high_water,
                    true,
                    current_revision,
                )
            } else {
                (
                    old_after,
                    old_high_water.max(current_high_water),
                    false,
                    current_revision,
                )
            }
        } else {
            (
                old_after,
                old_high_water.max(current_high_water),
                false,
                scan_revision,
            )
        };
        tx.execute(
            "UPDATE cleanup_cohort_scans
             SET revision = ?2, high_water_session_id = ?3,
                 after_session_id = ?4, complete = ?5,
                 progress_at_ms = CASE WHEN ?9 > 0 THEN ?6 ELSE progress_at_ms END,
                 progress_sequence = progress_sequence + (?9 > 0)
             WHERE namespace = ?1 AND generation = ?7
               AND alias_refresh_generation = ?8",
            params![
                namespace,
                revision,
                high_water,
                after_session_id,
                complete,
                now_ms,
                generation,
                alias_refresh_generation,
                applied_items
            ],
        )?;
        tx.commit()?;
        Ok(CleanupCohortPersistOutcome::Stored)
    }

    pub(crate) fn cleanup_cohort_scan_state(
        &self,
        namespace: &str,
    ) -> Result<Option<CleanupCohortScanState>, BrokerError> {
        let state = self
            .conn
            .query_row(
                "SELECT s.revision, s.generation, s.alias_refresh_generation,
                        s.progress_sequence,
                        s.high_water_session_id,
                        s.after_session_id, s.complete, s.progress_at_ms,
                        (SELECT revision FROM session_cleanup_revision WHERE id = 1),
                        (SELECT COALESCE(MAX(id), 0) FROM sessions),
                        (SELECT COUNT(*) FROM cleanup_cohort_dirty_sessions)
                 FROM cleanup_cohort_scans s WHERE s.namespace = ?1",
                [namespace],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, bool>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, i64>(8)?,
                        row.get::<_, i64>(9)?,
                        row.get::<_, i64>(10)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            scan_revision,
            generation,
            alias_refresh_generation,
            progress_sequence,
            high_water,
            after,
            stored_complete,
            progress_at_ms,
            revision,
            current_high_water,
            pending_dirty_rows,
        )) = state
        else {
            return Ok(None);
        };
        let unresolved_rows = self.conn.query_row(
            "SELECT COUNT(*) FROM cleanup_cohort_scan_rows
             WHERE namespace = ?1 AND resolution_error IS NOT NULL",
            [namespace],
            |row| row.get(0),
        )?;
        Ok(Some(CleanupCohortScanState {
            revision: scan_revision,
            generation,
            alias_refresh_generation,
            progress_sequence,
            high_water_session_id: high_water,
            after_session_id: after,
            complete: stored_complete
                && scan_revision == revision
                && high_water == current_high_water
                && pending_dirty_rows == 0,
            progress_at_ms,
            unresolved_rows,
            pending_dirty_rows,
        }))
    }

    pub(crate) fn cleanup_witness_revision(&self) -> Result<i64, BrokerError> {
        Ok(self.conn.query_row(
            "SELECT witness_revision FROM session_cleanup_revision WHERE id = 1",
            [],
            |row| row.get(0),
        )?)
    }

    pub(crate) fn cleanup_authority_revisions(&self) -> Result<(i64, i64), BrokerError> {
        Ok(self.conn.query_row(
            "SELECT revision, witness_revision FROM session_cleanup_revision WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?)
    }

    pub(crate) fn cleanup_cohort_group_page(
        &self,
        namespace: &str,
        after_session_id: i64,
        high_water_session_id: i64,
        limit: usize,
    ) -> Result<Vec<(String, i64)>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT canonical_path, first_session_id FROM cleanup_cohort_scan_groups
             WHERE namespace = ?1 AND first_session_id > ?2 AND first_session_id <= ?3
             ORDER BY first_session_id LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![
                namespace,
                after_session_id,
                high_water_session_id,
                limit.clamp(1, 32) as i64,
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(BrokerError::from)
    }

    pub(crate) fn cleanup_cohort_indexed_rows_page(
        &self,
        namespace: &str,
        after_session_id: i64,
        limit: usize,
    ) -> Result<Vec<CleanupCohortIndexedRow>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, raw_path, canonical_path, git_dir,
                    resolution_error, cleanup_state
             FROM cleanup_cohort_scan_rows
             WHERE namespace = ?1 AND session_id > ?2
             ORDER BY session_id LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![namespace, after_session_id, limit.clamp(1, 32) as i64],
            |row| {
                Ok(CleanupCohortIndexedRow {
                    session_id: row.get(0)?,
                    raw_path: row.get(1)?,
                    canonical_path: row.get(2)?,
                    git_dir: row.get(3)?,
                    resolution_error: row.get(4)?,
                    cleanup_state: row.get(5)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(BrokerError::from)
    }

    /// Invalidate cached filesystem resolution for selected session rows. The
    /// authority revision and dirty markers are updated together so the next
    /// scan resolves these rows before exposing a complete cohort again.
    pub(crate) fn mark_cleanup_cohort_rows_dirty(
        &mut self,
        namespace: &str,
        session_ids: &[i64],
    ) -> Result<usize, BrokerError> {
        if session_ids.is_empty() {
            return Ok(0);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut ids = BTreeSet::new();
        for session_id in session_ids.iter().copied() {
            let eligible = tx.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM cleanup_cohort_scan_rows r
                     JOIN sessions s ON s.id = r.session_id
                     WHERE r.namespace = ?1 AND r.session_id = ?2
                 ) AND NOT EXISTS(
                     SELECT 1 FROM cleanup_cohort_dirty_sessions d
                     WHERE d.session_id = ?2
                 )",
                params![namespace, session_id],
                |row| row.get::<_, bool>(0),
            )?;
            if eligible {
                ids.insert(session_id);
            }
        }
        if ids.is_empty() {
            tx.commit()?;
            return Ok(0);
        }
        tx.execute(
            "UPDATE session_cleanup_revision SET revision = revision + 1 WHERE id = 1",
            [],
        )?;
        let revision: i64 = tx.query_row(
            "SELECT revision FROM session_cleanup_revision WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        for session_id in &ids {
            tx.execute(
                "INSERT OR REPLACE INTO cleanup_cohort_dirty_sessions(session_id, revision)
                 VALUES (?1, ?2)",
                params![session_id, revision],
            )?;
        }
        tx.commit()?;
        Ok(ids.len())
    }

    pub(crate) fn cleanup_cohort_group_max_session_id(
        &self,
        namespace: &str,
    ) -> Result<i64, BrokerError> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(MAX(first_session_id), 0)
             FROM cleanup_cohort_scan_groups WHERE namespace = ?1",
            [namespace],
            |row| row.get(0),
        )?)
    }

    pub(crate) fn cleanup_cohort_group_count(
        &self,
        namespace: &str,
        after_session_id: i64,
        through_session_id: i64,
    ) -> Result<usize, BrokerError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM cleanup_cohort_scan_groups
             WHERE namespace = ?1 AND first_session_id > ?2 AND first_session_id <= ?3",
            params![namespace, after_session_id, through_session_id],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as usize)
    }

    pub(crate) fn cleanup_cohort_sessions_for_path(
        &self,
        namespace: &str,
        canonical_path: &str,
    ) -> Result<(Vec<Session>, bool), BrokerError> {
        let mut stmt = self.conn.prepare(&format!(
            "{} WHERE id IN (
                 SELECT session_id FROM cleanup_cohort_scan_rows
                 WHERE namespace = ?1 AND canonical_path = ?2 AND cleanup_state = 'closed'
             ) ORDER BY id LIMIT 129",
            SESSION_SELECT
        ))?;
        let rows = stmt.query_map(params![namespace, canonical_path], session_from_row)?;
        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row??);
        }
        let truncated = sessions.len() > 128;
        if truncated {
            sessions.pop();
        }
        Ok((sessions, truncated))
    }

    /// Return every cached row whose raw or canonical path can overlap one of
    /// the supplied candidate path ancestors. The caller must reject the
    /// snapshot if any row has unresolved path evidence.
    pub(crate) fn cleanup_cohort_alias_rows(
        &self,
        namespace: &str,
        path_ancestors: &[String],
        candidate_paths: &[String],
        git_dir: Option<&str>,
    ) -> Result<Vec<CleanupCohortIndexedRow>, BrokerError> {
        let mut rows = BTreeMap::<i64, CleanupCohortIndexedRow>::new();
        if !path_ancestors.is_empty() || !candidate_paths.is_empty() {
            let mut bind_values = Vec::<rusqlite::types::Value>::new();
            bind_values.push(namespace.to_owned().into());
            bind_values.extend(path_ancestors.iter().cloned().map(Into::into));
            let ancestor_placeholders = (2..=path_ancestors.len() + 1)
                .map(|index| format!("?{index}"))
                .collect::<Vec<_>>()
                .join(", ");
            let paths_start = path_ancestors.len() + 2;
            bind_values.extend(candidate_paths.iter().cloned().map(Into::into));
            let path_placeholders = (paths_start..paths_start + candidate_paths.len())
                .map(|index| format!("?{index}"))
                .collect::<Vec<_>>()
                .join(", ");
            let mut clauses = Vec::new();
            if !path_ancestors.is_empty() {
                clauses.push(format!(
                    "r.raw_path IN ({ancestor_placeholders}) OR r.canonical_path IN ({ancestor_placeholders})"
                ));
            }
            if !candidate_paths.is_empty() {
                clauses.push(format!(
                    "r.session_id IN (
                         SELECT session_id FROM cleanup_cohort_scan_path_ancestors
                         WHERE namespace = ?1 AND ancestor_path IN ({path_placeholders})
                     )"
                ));
            }
            let sql = format!(
                "SELECT DISTINCT r.session_id, r.raw_path, r.canonical_path, r.git_dir,
                                r.resolution_error, r.cleanup_state
                 FROM cleanup_cohort_scan_rows r
                 WHERE r.namespace = ?1 AND ({})",
                clauses.join(" OR ")
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let values = stmt.query_map(rusqlite::params_from_iter(bind_values), |row| {
                Ok(CleanupCohortIndexedRow {
                    session_id: row.get(0)?,
                    raw_path: row.get(1)?,
                    canonical_path: row.get(2)?,
                    git_dir: row.get(3)?,
                    resolution_error: row.get(4)?,
                    cleanup_state: row.get(5)?,
                })
            })?;
            for row in values {
                let row = row?;
                rows.insert(row.session_id, row);
            }
        }
        if let Some(git_dir) = git_dir {
            let mut stmt = self.conn.prepare(
                "SELECT session_id, raw_path, canonical_path, git_dir,
                        resolution_error, cleanup_state
                 FROM cleanup_cohort_scan_rows
                 WHERE namespace = ?1 AND git_dir = ?2",
            )?;
            let values = stmt.query_map(params![namespace, git_dir], |row| {
                Ok(CleanupCohortIndexedRow {
                    session_id: row.get(0)?,
                    raw_path: row.get(1)?,
                    canonical_path: row.get(2)?,
                    git_dir: row.get(3)?,
                    resolution_error: row.get(4)?,
                    cleanup_state: row.get(5)?,
                })
            })?;
            for row in values {
                let row = row?;
                rows.insert(row.session_id, row);
            }
        }
        Ok(rows.into_values().collect())
    }
}

fn remove_cleanup_cohort_row(
    tx: &rusqlite::Transaction<'_>,
    namespace: &str,
    session_id: i64,
) -> Result<Option<String>, BrokerError> {
    let canonical_path = tx
        .query_row(
            "SELECT canonical_path FROM cleanup_cohort_scan_rows
             WHERE namespace = ?1 AND session_id = ?2",
            params![namespace, session_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    tx.execute(
        "DELETE FROM cleanup_cohort_scan_path_ancestors
         WHERE namespace = ?1 AND session_id = ?2",
        params![namespace, session_id],
    )?;
    tx.execute(
        "DELETE FROM cleanup_cohort_scan_rows
         WHERE namespace = ?1 AND session_id = ?2",
        params![namespace, session_id],
    )?;
    Ok(canonical_path)
}

fn insert_cleanup_cohort_row(
    tx: &rusqlite::Transaction<'_>,
    namespace: &str,
    row: &CleanupCohortResolvedRow,
) -> Result<(), BrokerError> {
    tx.execute(
        "INSERT INTO cleanup_cohort_scan_rows (
             namespace, session_id, raw_path, canonical_path, git_dir,
             resolution_error, cleanup_state
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            namespace,
            row.session_id,
            row.raw_path,
            row.canonical_path,
            row.git_dir,
            row.resolution_error,
            row.cleanup_state,
        ],
    )?;
    for path in [&row.raw_path, &row.canonical_path] {
        for ancestor in Path::new(path).ancestors() {
            let Some(ancestor) = ancestor.to_str() else {
                continue;
            };
            tx.execute(
                "INSERT OR IGNORE INTO cleanup_cohort_scan_path_ancestors (
                     namespace, ancestor_path, session_id
                 ) VALUES (?1, ?2, ?3)",
                params![namespace, ancestor, row.session_id],
            )?;
        }
    }
    Ok(())
}

fn replace_cleanup_cohort_row(
    tx: &rusqlite::Transaction<'_>,
    namespace: &str,
    row: &CleanupCohortResolvedRow,
) -> Result<(), BrokerError> {
    let mut paths = BTreeSet::new();
    if let Some(path) = remove_cleanup_cohort_row(tx, namespace, row.session_id)? {
        paths.insert(path);
    }
    paths.insert(row.canonical_path.clone());
    insert_cleanup_cohort_row(tx, namespace, row)?;
    for path in &paths {
        clear_cleanup_cohort_group(tx, namespace, path)?;
    }
    for path in &paths {
        recompute_cleanup_cohort_group(tx, namespace, path)?;
    }
    Ok(())
}

fn clear_cleanup_cohort_group(
    tx: &rusqlite::Transaction<'_>,
    namespace: &str,
    canonical_path: &str,
) -> Result<(), BrokerError> {
    tx.execute(
        "DELETE FROM cleanup_cohort_scan_groups
         WHERE namespace = ?1 AND canonical_path = ?2",
        params![namespace, canonical_path],
    )?;
    Ok(())
}

fn recompute_cleanup_cohort_group(
    tx: &rusqlite::Transaction<'_>,
    namespace: &str,
    canonical_path: &str,
) -> Result<(), BrokerError> {
    let first_session_id: Option<i64> = tx.query_row(
        "SELECT MIN(session_id) FROM cleanup_cohort_scan_rows
         WHERE namespace = ?1 AND canonical_path = ?2 AND cleanup_state = 'closed'",
        params![namespace, canonical_path],
        |row| row.get(0),
    )?;
    if let Some(first_session_id) = first_session_id {
        tx.execute(
            "INSERT INTO cleanup_cohort_scan_groups (
                 namespace, canonical_path, first_session_id
             ) VALUES (?1, ?2, ?3)",
            params![namespace, canonical_path, first_session_id],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn fixture_store() -> (tempfile::TempDir, BrokerStore) {
        let repository = tempfile::tempdir().unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.name", "Cleanup cohort test"],
            vec!["config", "user.email", "cleanup-cohort@example.invalid"],
            vec!["commit", "--allow-empty", "-qm", "fixture"],
        ] {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(repository.path())
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let store = BrokerStore::open_in_repo(repository.path()).unwrap();
        (repository, store)
    }

    fn insert_session(store: &mut BrokerStore, name: &str) -> i64 {
        store
            .conn
            .execute(
                "INSERT INTO sessions (
                     worktree_path, branch, origin, status, created_at,
                     updated_at, last_activity_at
                 ) VALUES (?1, ?2, 'spawned', 'active', 1, 1, 1)",
                params![
                    format!("/tmp/aethyme-cleanup-cohort-{name}"),
                    format!("agent/{name}")
                ],
            )
            .unwrap();
        store.conn.last_insert_rowid()
    }

    fn resolved_row(session: &Session) -> CleanupCohortResolvedRow {
        CleanupCohortResolvedRow {
            session_id: session.id,
            raw_path: session.worktree_path.clone(),
            canonical_path: session.worktree_path.clone(),
            git_dir: None,
            resolution_error: None,
            cleanup_state: session.cleanup_state.as_str().to_owned(),
        }
    }

    #[test]
    fn paged_discovery_finishes_under_heartbeat_and_witness_churn() {
        let (_repository, mut store) = fixture_store();
        for index in 0..900 {
            insert_session(&mut store, &format!("legacy-{index}"));
        }
        // These rows predate the cache, so the initial build must discover
        // the complete history instead of treating the trigger queue as the
        // history itself.
        store
            .conn
            .execute("DELETE FROM cleanup_cohort_dirty_sessions", [])
            .unwrap();

        let namespace = CLEANUP_COHORT_NAMESPACE;
        let mut generation = None;
        let mut next_id = 0usize;
        let mut scanned_pages = 0usize;
        let mut complete = false;
        for step in 0..256 {
            let page = store.cleanup_cohort_scan_page(namespace, 17, step).unwrap();
            generation.get_or_insert(page.state.generation);
            assert_eq!(Some(page.state.generation), generation);
            if !page.dirty_sessions.is_empty() {
                let rows = page
                    .dirty_sessions
                    .iter()
                    .map(|dirty| CleanupCohortDirtyResolvedRow {
                        session_id: dirty.session_id,
                        revision: dirty.revision,
                        row: dirty.session.as_ref().map(resolved_row),
                    })
                    .collect::<Vec<_>>();
                store
                    .persist_cleanup_cohort_dirty_page(
                        namespace,
                        page.state.generation,
                        page.state.alias_refresh_generation,
                        &rows,
                        step,
                    )
                    .unwrap();
                continue;
            }
            if page.sessions.is_empty() {
                complete = page.state.complete;
                break;
            }

            let revision_before_heartbeats = store.cleanup_authority_revisions().unwrap().0;
            for session in &page.sessions {
                let next_status = if session.status.as_str() == "active" {
                    "idle"
                } else {
                    "active"
                };
                store
                    .conn
                    .execute(
                        "UPDATE sessions SET status = ?2, updated_at = updated_at + 1,
                             last_activity_at = last_activity_at + 1 WHERE id = ?1",
                        params![session.id, next_status],
                    )
                    .unwrap();
            }
            assert_eq!(
                revision_before_heartbeats,
                store.cleanup_authority_revisions().unwrap().0,
                "live status and heartbeat churn must not restart checkout discovery"
            );

            let rows = page.sessions.iter().map(resolved_row).collect::<Vec<_>>();
            let reached_high_water = rows
                .last()
                .is_some_and(|row| row.session_id >= page.state.high_water_session_id);
            store
                .persist_cleanup_cohort_scan_page(
                    namespace,
                    page.state.revision,
                    page.state.generation,
                    page.state.alias_refresh_generation,
                    page.state.after_session_id,
                    &rows,
                    reached_high_water,
                    step,
                )
                .unwrap();
            scanned_pages += 1;

            if scanned_pages % 5 == 0 && next_id < 12 {
                let before = store.cleanup_authority_revisions().unwrap();
                let id = insert_session(&mut store, &format!("concurrent-{next_id}"));
                store
                    .conn
                    .execute(
                        "INSERT INTO spawned_worktree_instances (session_id, witness_json)
                         VALUES (?1, '{}')",
                        [id],
                    )
                    .unwrap();
                let after = store.cleanup_authority_revisions().unwrap();
                assert_eq!(before.0 + 1, after.0, "new session is one dirty path row");
                assert_eq!(
                    before.1 + 1,
                    after.1,
                    "witness insertion has its own generation and does not restart path indexing"
                );
                next_id += 1;
            }
        }

        assert!(
            complete,
            "the incremental scanner must finish its finite high-water lap"
        );
        assert_eq!(
            Some(1),
            generation,
            "concurrent starts must not reset the generation"
        );
        let indexed: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM cleanup_cohort_scan_rows WHERE namespace = ?1",
                [namespace],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(indexed, 900 + next_id as i64);
        assert_eq!(
            0,
            store
                .cleanup_cohort_scan_state(namespace)
                .unwrap()
                .unwrap()
                .pending_dirty_rows
        );

        let changed_id: i64 = store
            .conn
            .query_row("SELECT MIN(id) FROM sessions", [], |row| row.get(0))
            .unwrap();
        let changed_path = format!("/tmp/aethyme-cleanup-cohort-path-change-{changed_id}");
        store
            .conn
            .execute(
                "UPDATE sessions SET worktree_path = ?2 WHERE id = ?1",
                params![changed_id, changed_path],
            )
            .unwrap();
        assert!(
            !store
                .cleanup_cohort_scan_state(namespace)
                .unwrap()
                .unwrap()
                .complete,
            "a path mutation makes the prior complete cache unavailable"
        );
        let dirty_page = store.cleanup_cohort_scan_page(namespace, 17, 300).unwrap();
        assert_eq!(dirty_page.dirty_sessions.len(), 1);
        let dirty_rows = dirty_page
            .dirty_sessions
            .iter()
            .map(|dirty| CleanupCohortDirtyResolvedRow {
                session_id: dirty.session_id,
                revision: dirty.revision,
                row: dirty.session.as_ref().map(resolved_row),
            })
            .collect::<Vec<_>>();
        store
            .persist_cleanup_cohort_dirty_page(
                namespace,
                dirty_page.state.generation,
                dirty_page.state.alias_refresh_generation,
                &dirty_rows,
                301,
            )
            .unwrap();
        let indexed_path: String = store
            .conn
            .query_row(
                "SELECT canonical_path FROM cleanup_cohort_scan_rows
                 WHERE namespace = ?1 AND session_id = ?2",
                params![namespace, changed_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(indexed_path, changed_path);
        assert!(
            store
                .cleanup_cohort_scan_state(namespace)
                .unwrap()
                .unwrap()
                .complete
        );
    }

    #[test]
    fn compact_outbox_due_is_separate_from_full_checkout_grace() {
        let (_repository, mut store) = fixture_store();
        let id = insert_session(&mut store, "compact-outbox");
        store
            .conn
            .execute(
                "UPDATE sessions SET status = 'cleaned', cleanup_state = 'closed', closed_at = 1000
                 WHERE id = ?1",
                [id],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO spawned_worktree_instances(session_id, witness_json)
                 VALUES (?1, '{}')",
                [id],
            )
            .unwrap();
        let tx = store.conn.transaction().unwrap();
        enqueue_checkout_cleanup_in_tx(&tx, id, 1000, 360, true).unwrap();
        tx.commit().unwrap();

        let full_due = 1000 + 360 * 3_600_000;
        let request = store.pending_checkout_cleanup_request(id).unwrap().unwrap();
        assert_eq!(request.not_before_ms, full_due);
        assert_eq!(request.compact_requested_at_ms, Some(1000));
        assert_eq!(
            store
                .next_pending_checkout_cleanup_at_with_compaction(true)
                .unwrap(),
            Some(1000)
        );
        assert_eq!(
            store
                .next_pending_checkout_cleanup_at_with_compaction(false)
                .unwrap(),
            Some(full_due),
            "disabled compaction ignores old immediate compact requests"
        );
        assert_eq!(
            store.next_pending_checkout_compaction_at().unwrap(),
            Some(1000)
        );

        let tx = store.conn.transaction().unwrap();
        enqueue_checkout_cleanup_in_tx(&tx, id, 1000, 24, false).unwrap();
        tx.commit().unwrap();
        let repeated = store.pending_checkout_cleanup_request(id).unwrap().unwrap();
        assert_eq!(
            repeated.not_before_ms, full_due,
            "a repeated close cannot shorten the full-removal grace"
        );
        assert_eq!(repeated.compact_requested_at_ms, Some(1000));

        store
            .defer_checkout_cleanup_compactions(&[id], 2000, 1500)
            .unwrap();
        assert_eq!(
            store
                .request_closed_spawned_checkout_compactions(&[id], 360, 1800)
                .unwrap(),
            1,
            "legacy reconciliation is idempotent for an already queued row"
        );
        assert_eq!(
            store
                .pending_checkout_cleanup_request(id)
                .unwrap()
                .unwrap()
                .compact_retry_after_ms,
            Some(2000),
            "reconciliation must not erase the bounded retry cooldown"
        );
        assert_eq!(
            store
                .next_pending_checkout_cleanup_at_with_compaction(true)
                .unwrap(),
            Some(2000)
        );
        assert_eq!(
            store
                .next_pending_checkout_cleanup_at_with_compaction(false)
                .unwrap(),
            Some(full_due)
        );
        assert_eq!(
            store
                .next_pending_checkout_cleanup_at_with_compaction(false)
                .unwrap(),
            Some(full_due)
        );
    }

    #[test]
    fn compact_removal_completes_both_independent_outbox_lanes() {
        let (_repository, mut store) = fixture_store();
        let id = insert_session(&mut store, "compact-removal");
        store
            .conn
            .execute(
                "UPDATE sessions SET status = 'cleaned', cleanup_state = 'closed', closed_at = 1000
                 WHERE id = ?1",
                [id],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO spawned_worktree_instances(session_id, witness_json)
                 VALUES (?1, '{}')",
                [id],
            )
            .unwrap();

        assert_eq!(
            store
                .request_closed_spawned_checkout_compactions(&[id], 360, 1000)
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .next_pending_checkout_cleanup_at_with_compaction(true)
                .unwrap(),
            Some(1000)
        );
        assert_eq!(
            store
                .next_pending_checkout_cleanup_at_with_compaction(false)
                .unwrap(),
            Some(1000 + 360 * 3_600_000)
        );

        store
            .conn
            .execute(
                "UPDATE sessions SET status = 'active', cleanup_state = 'open' WHERE id = ?1",
                [id],
            )
            .unwrap();
        assert_eq!(
            store
                .complete_checkout_cleanup_requests_after_removal(&[id], 1500)
                .unwrap(),
            0,
            "a delayed removal result cannot complete a reactivated session's obligation"
        );
        store
            .conn
            .execute(
                "UPDATE sessions SET status = 'cleaned', cleanup_state = 'cleaned' WHERE id = ?1",
                [id],
            )
            .unwrap();

        assert_eq!(
            store
                .pending_terminal_checkout_cleanup_ids(2000, 0, 10)
                .unwrap(),
            vec![id],
            "a crash between terminal session update and outbox settlement is recoverable"
        );
        assert_eq!(
            store
                .next_pending_checkout_cleanup_at_with_compaction(true)
                .unwrap(),
            Some(1000)
        );
        store
            .defer_terminal_checkout_cleanup_reconciliation(&[id], 2000)
            .unwrap();
        assert!(
            store
                .pending_terminal_checkout_cleanup_ids(2000, 0, 10)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store
                .pending_terminal_checkout_cleanup_ids(
                    2001 + CHECKOUT_CLEANUP_RECONCILE_COOLDOWN_MS,
                    0,
                    10
                )
                .unwrap(),
            vec![id]
        );

        store
            .complete_checkout_cleanup_requests_after_removal(&[id], 2000)
            .unwrap();
        assert_eq!(
            store
                .next_pending_checkout_cleanup_at_with_compaction(true)
                .unwrap(),
            None
        );
        assert_eq!(
            store
                .next_pending_checkout_cleanup_at_with_compaction(false)
                .unwrap(),
            None,
            "an early verified checkout removal must not create a 15-day no-op wake"
        );
    }

    #[test]
    fn bounded_legacy_closed_checkout_backfill_uses_original_close_time() {
        let (_repository, mut store) = fixture_store();
        let closed_id = insert_session(&mut store, "legacy-backfill-closed");
        let active_id = insert_session(&mut store, "legacy-backfill-active");
        let kept_id = insert_session(&mut store, "legacy-backfill-kept");
        store
            .conn
            .execute(
                "UPDATE sessions SET status = 'cleaned', cleanup_state = 'closed', closed_at = 2000
                 WHERE id = ?1",
                [closed_id],
            )
            .unwrap();
        store
            .conn
            .execute(
                "UPDATE sessions SET status = 'closed', cleanup_state = 'closed', closed_at = 3000
                 WHERE id = ?1",
                [kept_id],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO events (schema_version, ts, kind, session_id, payload_json)
                 VALUES (?1, 3000, ?2, ?3, '{\"cleanup\":{\"kept\":true}}')",
                params![
                    crate::schema::EVENTS_SCHEMA_VERSION,
                    crate::events::SESSION_FINISHED,
                    kept_id
                ],
            )
            .unwrap();

        assert_eq!(
            store
                .request_closed_spawned_checkout_cleanups(
                    &[closed_id, active_id, kept_id],
                    360,
                    9000
                )
                .unwrap(),
            1
        );
        let request = store
            .pending_checkout_cleanup_request(closed_id)
            .unwrap()
            .unwrap();
        assert_eq!(request.requested_at_ms, 9000);
        assert_eq!(request.not_before_ms, 2000 + 360 * 3_600_000);
        assert!(
            store
                .pending_checkout_cleanup_request(active_id)
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .pending_checkout_cleanup_request(kept_id)
                .unwrap()
                .is_none()
        );

        store
            .request_closed_spawned_checkout_cleanups(&[closed_id], 24, 10_000)
            .unwrap();
        let repeated = store
            .pending_checkout_cleanup_request(closed_id)
            .unwrap()
            .unwrap();
        assert_eq!(repeated.requested_at_ms, 9000);
        assert_eq!(
            repeated.not_before_ms,
            2000 + 360 * 3_600_000,
            "reconciliation keeps the original due time and cannot shorten its grace"
        );
    }

    #[test]
    fn terminal_outbox_reconciliation_is_keyset_bounded_and_cooldown_aware() {
        let (_repository, mut store) = fixture_store();
        let first = insert_session(&mut store, "terminal-reconcile-first");
        let second = insert_session(&mut store, "terminal-reconcile-second");
        for id in [first, second] {
            store
                .conn
                .execute(
                    "UPDATE sessions SET status = 'cleaned', cleanup_state = 'closed', closed_at = 500
                     WHERE id = ?1",
                    [id],
                )
                .unwrap();
            store
                .request_closed_spawned_checkout_cleanups(&[id], 360, 500)
                .unwrap();
            store
                .conn
                .execute(
                    "UPDATE sessions SET cleanup_state = 'cleaned' WHERE id = ?1",
                    [id],
                )
                .unwrap();
        }
        assert_eq!(
            store
                .pending_terminal_checkout_cleanup_ids(500, 0, 1)
                .unwrap(),
            vec![first]
        );
        assert_eq!(
            store
                .pending_terminal_checkout_cleanup_ids(500, first, 1)
                .unwrap(),
            vec![second]
        );
        store
            .defer_terminal_checkout_cleanup_reconciliation(&[first], 800)
            .unwrap();
        assert_eq!(
            store
                .pending_terminal_checkout_cleanup_ids(800, 0, 10)
                .unwrap(),
            vec![second],
            "a terminal path that still exists is retried after a bounded cooldown"
        );
        assert_eq!(
            store.next_pending_checkout_cleanup_at().unwrap(),
            Some(500),
            "the second terminal record remains due until its own reconciliation"
        );
        store
            .defer_terminal_checkout_cleanup_reconciliation(&[second], 800)
            .unwrap();
        assert_eq!(
            store.next_pending_checkout_cleanup_at().unwrap(),
            Some(800 + CHECKOUT_CLEANUP_RECONCILE_COOLDOWN_MS)
        );
    }
}
