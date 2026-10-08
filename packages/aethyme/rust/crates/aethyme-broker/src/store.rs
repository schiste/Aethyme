//! Typed store over `.aethyme/broker.db`.
//!
//! All SQL in the crate lives here and in `schema.rs`. Callers (CLI, TUI,
//! tests) go through these methods only — that is the API-first contract.
//!
//! One `BrokerStore` wraps one connection and is intended per-process
//! (CLI invocations are short-lived). Cross-process safety comes from
//! SQLite WAL + a 5s busy timeout; nothing here assumes in-process locks.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use sha2::{Digest, Sha256};

use crate::delivery::{
    DELIVERY_OUTBOX_SCHEMA_VERSION, DeliveryCompletion, DeliveryOutboxItem, DeliveryPolicy,
    DeliveryStatus, DeliverySubscription, MAX_DELIVERY_ATTEMPTS,
};
use crate::error::BrokerError;
use crate::external_events::{
    ExternalEventRecord, ExternalEventStatus, NewExternalEventRecord,
    aggregate_ownership_candidates,
};
use crate::pr_watch::{
    NewPullRequestWatch, PULL_REQUEST_WATCH_SCHEMA_VERSION, PullRequestActivity,
    PullRequestActivityBatch, PullRequestActivityKind, PullRequestActivityMetadata,
    PullRequestBatchAckOutcome, PullRequestBatchStatus, PullRequestSnapshot, PullRequestWatch,
    PullRequestWatchPollStorageResult, PullRequestWatchStatus,
};
use crate::retention::{GcCheckpointPinRelease, GcPublicationExposureExpiry};
use crate::review::{NewReviewLifecycle, ReviewLifecycle, ReviewLifecycleState};
use crate::review_ledger::{ReviewRequest, ReviewRequestState, ReviewVerdict, ReviewerIdentity};
use crate::review_trigger::ReviewTrigger;
use crate::schema::{self, EVENTS_SCHEMA_VERSION};
use crate::types::{
    Advisory, AdvisoryResolutionState, AdvisorySeverity, CoordinatedOperation,
    EntryExposureResolutionKind, EntryExposureState, EntryPathExposure, Event, GateDef,
    GateEnvironment, GateFailureClass, GateResult, GateStatus, Lease, LeaseKind,
    MAX_OPERATION_HISTORY_LIMIT, MergeQueueEntry, MergeStatus, NewAdvisory,
    NewCoordinatedOperation, NewGateResult, NewPrWatchState, NewSession, OperationEffect,
    OperationHistoryPage, OperationHistoryQuery, OperationIdentityProvenance, OperationProvider,
    OperationStatus, PrWatchState, Session, SessionCleanupState, SessionContext, SessionNote,
    SessionOrigin, SessionStatus,
};
use crate::types::{NewSessionRepresentation, RepresentationDiscovery, SessionRepresentation};

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

// ── row mapping helpers ──────────────────────────────────────────────

fn insert_session(
    tx: &Transaction<'_>,
    new: &NewSession,
    context: &SessionContext,
    now: i64,
) -> Result<i64, BrokerError> {
    let contract = new.repository_contract.as_ref();
    let inserted = tx.execute(
        "INSERT INTO sessions (worktree_path, branch, origin, status, task, diff_base,
                               adoption_base, adopted_head, repository_schema,
                               deployment_state_digest, aethyme_version,
                               gate_definition_digest, repository_contract_backfilled,
                               pid, command, log_path, agent_identity, repository_name,
                               tab_name, ai_provider, short_name, created_at,
                               updated_at, last_activity_at)
         VALUES (?1, ?2, ?3, 'active', ?4, ?5, COALESCE(?6, ?5),
                 COALESCE(?7, ?6, ?5), ?8, ?9, ?10, ?11, ?12, ?13,
                 ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?21, ?21)",
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
            new.agent_identity,
            context.repository_name,
            context.tab_name,
            context.ai_provider,
            context.short_name,
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
        tx,
        now,
        crate::events::SESSION_REGISTERED,
        Some(id),
        Some(&crate::events::session_registered_payload(
            new.origin.as_str(),
            &new.branch,
            &new.worktree_path,
        )),
    )?;
    Ok(id)
}

fn insert_session_context_event(
    tx: &Transaction<'_>,
    session_id: i64,
    context: &SessionContext,
    now: i64,
) -> Result<(), BrokerError> {
    if context.is_empty() {
        return Ok(());
    }
    insert_event(
        tx,
        now,
        crate::events::SESSION_CONTEXT_UPDATED,
        Some(session_id),
        Some(&crate::events::session_context_updated_payload(context)),
    )?;
    Ok(())
}

/// Recheck planned leases inside the registering transaction so two
/// concurrent planners cannot both claim one path. Mirrors
/// `LeaseRefusalPolicy::refuses` against the stored status: only an explicit
/// lease of an active session refuses, and nothing refuses under verify-only.
fn validate_planned_lease_conflicts(
    tx: &Transaction<'_>,
    owner_session_id: Option<i64>,
    planned_paths: &[String],
    now: i64,
    verify_only: bool,
) -> Result<(), BrokerError> {
    if verify_only {
        return Ok(());
    }
    let leases = {
        let mut stmt = tx.prepare(&format!(
            "{LEASE_SELECT}
             WHERE released_at IS NULL
               AND (expires_at IS NULL OR expires_at > ?1)
               AND kind = 'explicit'
               AND session_id IN (SELECT id FROM sessions WHERE status = 'active')
             ORDER BY session_id, path, kind"
        ))?;
        let rows = stmt.query_map([now], lease_from_row)?;
        let mut leases = Vec::new();
        for row in rows {
            leases.push(row??);
        }
        leases
    };
    for path in planned_paths {
        if let Some(blocker) = leases.iter().find(|lease| {
            Some(lease.session_id) != owner_session_id
                && crate::leases::paths_overlap(path, &lease.path)
        }) {
            let (blocker_worktree, blocker_status): (String, String) = tx.query_row(
                "SELECT worktree_path, status FROM sessions WHERE id = ?1",
                [blocker.session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let blocker_status_value = SessionStatus::parse(&blocker_status)?;
            let remediation = crate::leases::planned_lease_next_actions(
                &blocker_worktree,
                blocker.session_id,
                blocker_status_value,
                path,
            )
            .join("\n  ");
            return Err(BrokerError::PlannedLeaseConflict(Box::new(
                crate::error::PlannedLeaseConflict {
                    path: path.clone(),
                    blocker_session_id: blocker.session_id,
                    blocker_path: blocker.path.clone(),
                    blocker_kind: blocker.kind.as_str().to_string(),
                    blocker_status,
                    blocker_worktree,
                    remediation,
                },
            )));
        }
    }
    Ok(())
}

fn insert_planned_explicit_leases(
    tx: &Transaction<'_>,
    session_id: i64,
    planned_paths: &[String],
    now: i64,
) -> Result<(), BrokerError> {
    for path in planned_paths {
        tx.execute(
            "INSERT INTO leases (session_id, path, kind, created_at, expires_at)
             VALUES (?1, ?2, 'explicit', ?3, NULL)
             ON CONFLICT (session_id, path, kind)
             DO UPDATE SET created_at = excluded.created_at,
                           expires_at = NULL,
                           released_at = NULL
             WHERE leases.released_at IS NOT NULL
                OR (leases.expires_at IS NOT NULL AND leases.expires_at <= ?3)",
            params![session_id, path, now],
        )?;
        insert_event(
            tx,
            now,
            crate::events::LEASE_CLAIMED,
            Some(session_id),
            Some(&crate::events::lease_path_payload(path)),
        )?;
    }
    Ok(())
}

fn upsert_pull_request_activity(
    tx: &Transaction<'_>,
    watch_id: i64,
    activity: &PullRequestActivityMetadata,
    now: i64,
) -> Result<(i64, bool), BrokerError> {
    let inserted = tx.execute(
        "INSERT OR IGNORE INTO pull_request_activities (
             watch_id, kind, provider_id, author, state, url,
             provider_updated_at, first_seen_at, last_seen_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
        params![
            watch_id,
            activity.kind.as_str(),
            activity.provider_id,
            activity.author,
            activity.state,
            activity.url,
            activity.updated_at,
            now,
        ],
    )? == 1;
    tx.execute(
        "UPDATE pull_request_activities
         SET author = ?4, state = ?5, url = ?6,
             provider_updated_at = ?7, last_seen_at = ?8
         WHERE watch_id = ?1 AND kind = ?2 AND provider_id = ?3",
        params![
            watch_id,
            activity.kind.as_str(),
            activity.provider_id,
            activity.author,
            activity.state,
            activity.url,
            activity.updated_at,
            now,
        ],
    )?;
    let id = tx.query_row(
        "SELECT id FROM pull_request_activities
         WHERE watch_id = ?1 AND kind = ?2 AND provider_id = ?3",
        params![watch_id, activity.kind.as_str(), activity.provider_id],
        |row| row.get(0),
    )?;
    Ok((id, inserted))
}

const SESSION_SELECT: &str = "SELECT id, worktree_path, branch, origin, status, task, \
     diff_base, adoption_base, adopted_head, accepted_session_head, \
     accepted_integration_commit, accepted_integration_tree, accepted_queue_entry_id, \
     accepted_at, repository_schema, deployment_state_digest, aethyme_version, \
     gate_definition_digest, repository_contract_backfilled, pid, command, log_path, \
     exit_code, created_at, updated_at, last_activity_at, cleanup_state, closed_at, \
     cleanup_completed_at, agent_identity, repository_name, tab_name, ai_provider, short_name \
     FROM sessions";

/// Record one `lease.released` event per lease `session_id` still holds
/// (optionally only those on `path`), naming its generation and `reason`,
/// and return their ids. The one audit path every reasoned release shares:
/// a terminal finish (#358) and an acknowledged or granted release request
/// (#359).
fn record_lease_releases_in_tx(
    tx: &rusqlite::Transaction<'_>,
    session_id: i64,
    path: Option<&str>,
    now: i64,
    reason: &str,
) -> Result<Vec<i64>, BrokerError> {
    let held: Vec<(i64, String, i64)> = {
        let mut stmt = tx.prepare(
            "SELECT id, path, created_at FROM leases
             WHERE session_id = ?1 AND released_at IS NULL
               AND (expires_at IS NULL OR expires_at > ?2)
               AND (?3 IS NULL OR path = ?3)
             ORDER BY id",
        )?;
        stmt.query_map(params![session_id, now, path], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<Result<_, _>>()?
    };
    let mut ids = Vec::with_capacity(held.len());
    for (lease_id, path, created_at) in held {
        insert_event(
            tx,
            now,
            crate::events::LEASE_RELEASED,
            Some(session_id),
            Some(&crate::events::lease_released_payload(
                &path, reason, lease_id, created_at,
            )),
        )?;
        ids.push(lease_id);
    }
    Ok(ids)
}

/// Release every lease `session_id` still holds, inside a terminal
/// transition's transaction: audited by [`record_lease_releases_in_tx`],
/// then the rows go. Scoped to this session, so a newer generation of the
/// same path held by another session is untouched.
fn release_session_leases_in_tx(
    tx: &rusqlite::Transaction<'_>,
    session_id: i64,
    now: i64,
    reason: &str,
) -> Result<(), BrokerError> {
    record_lease_releases_in_tx(tx, session_id, None, now, reason)?;
    tx.execute("DELETE FROM leases WHERE session_id = ?1", [session_id])?;
    Ok(())
}

const LEASE_SELECT: &str =
    "SELECT id, session_id, path, kind, created_at, expires_at, released_at FROM leases";

const GATE_RESULT_SELECT: &str = "SELECT id, gate_name, tree_hash, definition_hash, status, \
     failure_class, exit_code, duration_ms, log_path, session_id, created_at, wait_duration_ms, \
     first_output_ms, output_bytes, load_avg_1m_start, load_avg_1m_end, cpu_count, \
     free_disk_bytes_start, cleared_at, cleared_reason FROM gate_results";

const MERGE_SELECT: &str = "SELECT id, session_id, head_commit, base_commit, status, \
     merged_tree, details_json, created_at, updated_at FROM merge_queue";

const SESSION_REPRESENTATION_SELECT: &str = "SELECT id, session_id, session_head, \
     representing_commit, representing_ref, discovery, pr_number, paths_json, evidence, \
     created_at FROM session_representations";

const PR_WATCH_SELECT: &str = "SELECT id, target_branch, pr_number, activity_fingerprint, \
     marker, last_dispatch_at, last_agent_session_id, updated_at FROM pr_watch_state";

const PULL_REQUEST_WATCH_SELECT: &str = "SELECT id, session_id, provider, canonical_repository, \
     display_repository, pr_number, target_branch, head_sha, is_draft, status, event_kinds_json, \
     poll_interval_seconds, cursor_digest, last_polled_at, next_poll_at, last_error_code, \
     created_at, updated_at FROM pull_request_watches";

const DELIVERY_SUBSCRIPTION_SELECT: &str = "SELECT id, watch_id, adapter, target, policy, active, \
     created_at, updated_at FROM delivery_subscriptions";

const DELIVERY_OUTBOX_SELECT: &str = "SELECT o.id, o.subscription_id, o.batch_id, o.status, \
     o.generation, o.claimed_by, o.claim_expires_at, o.attempt_count, o.last_error_code, \
     o.delivered_at, o.created_at, o.updated_at FROM delivery_outbox o";

const ADVISORY_SELECT: &str = "SELECT id, identity, session_id, severity, queue_entry_id, \
     integration_sha, paths_json, evidence_json, created_at, resolution_state, acknowledged_at, \
     suppressed_at, resolved_at, resolution_evidence, audience, producer \
     FROM advisories";

const SESSION_NOTE_SELECT: &str = "SELECT id, sender_session_id, recipient_session_id, message, \
     created_at, acknowledged_at FROM session_notes";

const ENTRY_PATH_EXPOSURE_SELECT: &str = "SELECT id, queue_entry_id, promotion_sha, paths_json, \
     created_at, state, resolved_at, resolution_kind, resolution_sha, resolution_evidence \
     FROM entry_path_exposures";

const EXTERNAL_EVENT_SELECT: &str = "SELECT id, provider, provider_event_id, event_type, \
     repository, target_branch, pr_number, commit_sha, occurred_at, verification_method, \
     verified_at, normalized_digest, status, session_id, queue_entry_id, advisory_id, \
     received_at, reconciled_at, reconciliation_kind, reconciliation_reason_digest \
     FROM external_coordination_events";

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
    let stored_status: String = row.get(4)?;
    let cleanup_state: String = row.get(26)?;
    let cleanup_state = SessionCleanupState::parse(&cleanup_state);
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
        let cleanup_state = cleanup_state?;
        Ok(Session {
            id: row.get(0)?,
            worktree_path: row.get(1)?,
            branch: row.get(2)?,
            origin: SessionOrigin::parse(&origin)?,
            status: match cleanup_state {
                SessionCleanupState::Closed => SessionStatus::Closed,
                SessionCleanupState::Cleaned => SessionStatus::Cleaned,
                SessionCleanupState::Open => SessionStatus::parse(&stored_status)?,
            },
            cleanup_state,
            closed_at: row.get(27)?,
            cleanup_completed_at: row.get(28)?,
            agent_identity: row.get(29)?,
            repository_name: row.get(30)?,
            tab_name: row.get(31)?,
            ai_provider: row.get(32)?,
            short_name: row.get(33)?,
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
            cleared_at: row.get(18)?,
            cleared_reason: row.get(19)?,
            environment: GateEnvironment {
                load_avg_1m_start: row.get(14)?,
                load_avg_1m_end: row.get(15)?,
                cpu_count: row.get(16)?,
                free_disk_bytes_start: row.get(17)?,
                free_disk_bytes_end: None,
            },
        })
    })())
}

#[cfg(test)]
mod gate_result_clear_metadata_tests {
    use super::*;

    #[test]
    fn gate_result_readback_exposes_clear_time_and_reason() {
        let conn = Connection::open_in_memory().unwrap();
        crate::schema::migrate(&conn).unwrap();
        conn.execute(
            "INSERT INTO gate_results (
                 gate_name, tree_hash, definition_hash, status, failure_class,
                 created_at, cleared_at, cleared_reason
             ) VALUES ('unit', 'tree', 'definition', 'fail', 'test_failure', 1, 2, 'operator review')",
            [],
        )
        .unwrap();

        let result = conn
            .query_row(
                &format!("{GATE_RESULT_SELECT} WHERE id = 1"),
                [],
                gate_result_from_row,
            )
            .unwrap()
            .unwrap();
        assert_eq!(result.cleared_at, Some(2));
        assert_eq!(result.cleared_reason.as_deref(), Some("operator review"));
    }

    #[test]
    fn garbage_collection_can_purge_failed_gate_results_without_opening_delete_to_old_writers() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = BrokerStore::open_in_repo(directory.path()).unwrap();
        store
            .conn
            .execute(
                "INSERT INTO gate_results (
                     gate_name, tree_hash, definition_hash, status, failure_class, created_at
                 ) VALUES ('unit', 'tree', 'definition', 'fail', 'test_failure', 1)",
                [],
            )
            .unwrap();
        let id = store.conn.last_insert_rowid();
        let candidate = crate::GcRowCandidate {
            kind: crate::GcRowKind::GateResult,
            id,
            recorded_at: 1,
            estimated_bytes: 1,
            gate_log_path: None,
        };

        assert!(
            store
                .conn
                .execute("DELETE FROM gate_results WHERE id = ?1", [id])
                .is_err(),
            "ordinary old-writer deletes remain fenced"
        );
        assert_eq!(store.delete_gc_rows(&[candidate]).unwrap(), 1);
        let remaining: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM gate_results WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0);
        let permits: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM gate_result_gc_permits", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(permits, 0);
    }

    #[test]
    fn a_cleared_latest_failure_prevents_falling_back_to_an_older_pass() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = BrokerStore::open_in_repo(directory.path()).unwrap();
        store
            .conn
            .execute(
                "INSERT INTO gate_results (
                     gate_name, tree_hash, definition_hash, status, created_at
                 ) VALUES ('unit', 'tree', 'definition', 'pass', 1)",
                [],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO gate_results (
                     gate_name, tree_hash, definition_hash, status,
                     failure_class, created_at
                 ) VALUES ('unit', 'tree', 'definition', 'fail', 'test_failure', 2)",
                [],
            )
            .unwrap();

        assert_eq!(
            store
                .clear_cached_test_failures("unit", "tree", "operator review")
                .unwrap(),
            vec![2]
        );
        assert!(
            store
                .cached_gate_result_for_definition("unit", "tree", "definition")
                .unwrap()
                .is_none(),
            "clearing the newest failure must invalidate earlier cached proof"
        );
        assert!(
            store.cached_gate_result("unit", "tree").unwrap().is_none(),
            "the definition-agnostic lookup must also require a fresh run"
        );
    }
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

fn session_representation_from_row(row: &rusqlite::Row<'_>) -> RowResult<SessionRepresentation> {
    let raw: String = row.get(5)?;
    let Some(discovery) = RepresentationDiscovery::parse(&raw) else {
        return Ok(Err(BrokerError::InvalidRepresentationDiscovery(raw)));
    };
    Ok(Ok(SessionRepresentation {
        id: row.get(0)?,
        session_id: row.get(1)?,
        session_head: row.get(2)?,
        representing_commit: row.get(3)?,
        representing_ref: row.get(4)?,
        discovery,
        pr_number: row.get(6)?,
        paths_json: row.get(7)?,
        evidence: row.get(8)?,
        created_at: row.get(9)?,
    }))
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

fn pull_request_watch_from_row(row: &rusqlite::Row<'_>) -> RowResult<PullRequestWatch> {
    let status = PullRequestWatchStatus::parse(row.get(9)?)?;
    let event_kinds_json: String = row.get(10)?;
    let event_kinds = match serde_json::from_str(&event_kinds_json) {
        Ok(value) => value,
        Err(source) => {
            return Ok(Err(BrokerError::InvalidEnumValue {
                field: "pull_request_watches.event_kinds_json",
                value: source.to_string(),
            }));
        }
    };
    Ok(Ok(PullRequestWatch {
        schema_version: PULL_REQUEST_WATCH_SCHEMA_VERSION,
        id: row.get(0)?,
        session_id: row.get(1)?,
        provider: row.get(2)?,
        canonical_repository: row.get(3)?,
        display_repository: row.get(4)?,
        pr_number: row.get(5)?,
        target_branch: row.get(6)?,
        head_sha: row.get(7)?,
        is_draft: row.get(8)?,
        status,
        event_kinds,
        poll_interval_seconds: row.get::<_, i64>(11)? as u64,
        cursor_digest: row.get(12)?,
        last_polled_at: row.get(13)?,
        next_poll_at: row.get(14)?,
        last_error_code: row.get(15)?,
        created_at: row.get(16)?,
        updated_at: row.get(17)?,
    }))
}

fn pull_request_activity_from_row(row: &rusqlite::Row<'_>) -> RowResult<PullRequestActivity> {
    let kind: String = row.get(2)?;
    Ok((|| {
        Ok(PullRequestActivity {
            id: row.get(0)?,
            watch_id: row.get(1)?,
            metadata: PullRequestActivityMetadata {
                kind: PullRequestActivityKind::parse(&kind)?,
                provider_id: row.get(3)?,
                author: row.get(4)?,
                state: row.get(5)?,
                url: row.get(6)?,
                updated_at: row.get(7)?,
            },
            first_seen_at: row.get(8)?,
            last_seen_at: row.get(9)?,
        })
    })())
}

fn delivery_subscription_from_row(row: &rusqlite::Row<'_>) -> RowResult<DeliverySubscription> {
    let policy: String = row.get(4)?;
    Ok((|| {
        Ok(DeliverySubscription {
            id: row.get(0)?,
            watch_id: row.get(1)?,
            adapter: row.get(2)?,
            target: row.get(3)?,
            policy: DeliveryPolicy::parse(&policy)?,
            active: row.get(5)?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        })
    })())
}

fn delivery_outbox_from_row(row: &rusqlite::Row<'_>) -> RowResult<DeliveryOutboxItem> {
    let status: String = row.get(3)?;
    Ok((|| {
        Ok(DeliveryOutboxItem {
            schema_version: DELIVERY_OUTBOX_SCHEMA_VERSION,
            id: row.get(0)?,
            subscription_id: row.get(1)?,
            batch_id: row.get(2)?,
            status: DeliveryStatus::parse(&status)?,
            generation: row.get(4)?,
            claimed_by: row.get(5)?,
            claim_expires_at: row.get(6)?,
            attempt_count: row.get(7)?,
            last_error_code: row.get(8)?,
            delivered_at: row.get(9)?,
            created_at: row.get(10)?,
            updated_at: row.get(11)?,
        })
    })())
}

const REVIEW_REQUEST_SELECT: &str = "SELECT id, repository, pr_number, review_type, head_commit,
            requested_for_commit, base_commit, trigger, backend, state, detail,
            requested_at, completed_at, completed_for_commit, verdict,
            reviewer_provider, reviewer_model, updated_at
     FROM review_requests";

fn review_request_from_row(row: &rusqlite::Row<'_>) -> RowResult<ReviewRequest> {
    let trigger: Option<String> = row.get(7)?;
    let state: String = row.get(9)?;
    let verdict: Option<String> = row.get(14)?;
    let reviewer_provider: Option<String> = row.get(15)?;
    let reviewer_model: Option<String> = row.get(16)?;
    Ok((|| {
        Ok(ReviewRequest {
            id: row.get(0)?,
            repository: row.get(1)?,
            pr_number: row.get(2)?,
            review_type: row.get(3)?,
            head_commit: row.get(4)?,
            requested_for_commit: row.get(5)?,
            base_commit: row.get(6)?,
            trigger: match trigger.as_deref() {
                None => None,
                Some(value) => Some(ReviewTrigger::parse(value).ok_or_else(|| {
                    BrokerError::InvalidEnumValue {
                        field: "review_requests.trigger",
                        value: value.to_string(),
                    }
                })?),
            },
            backend: row.get(8)?,
            state: ReviewRequestState::parse(&state).ok_or_else(|| {
                BrokerError::InvalidEnumValue {
                    field: "review_requests.state",
                    value: state.clone(),
                }
            })?,
            detail: row.get(10)?,
            requested_at: row.get(11)?,
            completed_at: row.get(12)?,
            completed_for_commit: row.get(13)?,
            verdict: match verdict.as_deref() {
                None => None,
                Some(value) => Some(ReviewVerdict::parse(value).ok_or_else(|| {
                    BrokerError::InvalidEnumValue {
                        field: "review_requests.verdict",
                        value: value.to_string(),
                    }
                })?),
            },
            reviewer: reviewer_provider.map(|provider| ReviewerIdentity {
                provider,
                model: reviewer_model,
            }),
            updated_at: row.get(17)?,
        })
    })())
}

const REVIEW_LIFECYCLE_SELECT: &str =
    "SELECT id, session_id, queue_entry_id, repository, target_branch,
            pr_number, commit_sha, state, generation, evidence_digest,
            unlock_operation_id, active, abandoned_at, abandon_reason_digest,
            created_at, updated_at
     FROM review_lifecycles";

fn review_lifecycle_from_row(row: &rusqlite::Row<'_>) -> RowResult<ReviewLifecycle> {
    let state: String = row.get(7)?;
    Ok((|| {
        Ok(ReviewLifecycle {
            id: row.get(0)?,
            session_id: row.get(1)?,
            queue_entry_id: row.get(2)?,
            repository: row.get(3)?,
            target_branch: row.get(4)?,
            pr_number: row.get(5)?,
            commit_sha: row.get(6)?,
            state: ReviewLifecycleState::parse(&state)?,
            generation: row.get(8)?,
            evidence_digest: row.get(9)?,
            unlock_operation_id: row.get(10)?,
            active: row.get::<_, i64>(11)? != 0,
            abandoned_at: row.get(12)?,
            abandon_reason_digest: row.get(13)?,
            created_at: row.get(14)?,
            updated_at: row.get(15)?,
        })
    })())
}

fn coordinated_operation_from_row(row: &rusqlite::Row<'_>) -> RowResult<CoordinatedOperation> {
    let provider: String = row.get(2)?;
    let effect: String = row.get(5)?;
    let status: String = row.get(6)?;
    let identity_provenance: String = row.get(16)?;
    let agent_provenance: Option<serde_json::Value> = row
        .get::<_, Option<String>>(17)?
        .map(|json| serde_json::from_str(&json))
        .transpose()
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                17,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?;
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
            agent_provenance,
        })
    })())
}

fn advisory_from_row(row: &rusqlite::Row<'_>) -> RowResult<Advisory> {
    let id = row.get(0)?;
    let severity: String = row.get(3)?;
    let paths_json: String = row.get(6)?;
    let evidence_json: String = row.get(7)?;
    let resolution_state: String = row.get(9)?;
    let suppressed_at: Option<i64> = row.get(11)?;
    let audience: String = row.get(14)?;
    let producer: String = row.get(15)?;
    Ok((|| {
        Ok(Advisory {
            id,
            identity: row.get(1)?,
            audience: crate::AdvisoryAudience::parse(&audience)?,
            producer: crate::AdvisoryProducer::parse(&producer)?,
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
            resolution_state: if suppressed_at.is_some() {
                AdvisoryResolutionState::Suppressed
            } else {
                AdvisoryResolutionState::parse(&resolution_state)?
            },
            acknowledged_at: row.get(10)?,
            suppressed_at,
            resolved_at: row.get(12)?,
            resolution_evidence: row.get(13)?,
        })
    })())
}

fn advisory_delivery_metric_from_row(
    row: &rusqlite::Row<'_>,
) -> RowResult<crate::AdvisoryDeliveryMetric> {
    let surface: String = row.get(2)?;
    let action: Option<String> = row.get(7)?;
    Ok((|| {
        Ok(crate::AdvisoryDeliveryMetric {
            advisory_id: row.get(0)?,
            session_id: row.get(1)?,
            surface: crate::AdvisoryDeliverySurface::parse(&surface)?,
            first_shown_at: row.get(3)?,
            last_shown_at: row.get(4)?,
            show_count: row.get::<_, i64>(5)?.max(0) as u64,
            acted_at: row.get(6)?,
            action: action
                .as_deref()
                .map(crate::AdvisoryAction::parse)
                .transpose()?,
        })
    })())
}

fn session_note_from_row(row: &rusqlite::Row<'_>) -> Result<SessionNote, rusqlite::Error> {
    Ok(SessionNote {
        id: row.get(0)?,
        sender_session_id: row.get(1)?,
        recipient_session_id: row.get(2)?,
        message: row.get(3)?,
        created_at: row.get(4)?,
        acknowledged_at: row.get(5)?,
    })
}

fn entry_path_exposure_from_row(row: &rusqlite::Row<'_>) -> RowResult<EntryPathExposure> {
    let id = row.get(0)?;
    let paths_json: String = row.get(3)?;
    let state: String = row.get(5)?;
    let resolution_kind: Option<String> = row.get(7)?;
    Ok((|| {
        Ok(EntryPathExposure {
            id,
            queue_entry_id: row.get(1)?,
            promotion_sha: row.get(2)?,
            paths: serde_json::from_str(&paths_json).map_err(|source| {
                BrokerError::InvalidEntryExposureJson {
                    id,
                    field: "paths_json",
                    source,
                }
            })?,
            created_at: row.get(4)?,
            state: EntryExposureState::parse(&state)?,
            resolved_at: row.get(6)?,
            resolution_kind: resolution_kind
                .as_deref()
                .map(EntryExposureResolutionKind::parse)
                .transpose()?,
            resolution_sha: row.get(8)?,
            resolution_evidence: row.get(9)?,
        })
    })())
}

fn external_event_from_row(row: &rusqlite::Row<'_>) -> RowResult<ExternalEventRecord> {
    let provider = row.get::<_, String>(1)?;
    let verification_method = row.get::<_, String>(9)?;
    let status = row.get::<_, String>(12)?;
    Ok((|| {
        Ok(ExternalEventRecord {
            id: row.get(0)?,
            provider: crate::ExternalEventProvider::parse(&provider)?,
            provider_event_id: row.get(2)?,
            event_type: row.get(3)?,
            repository: row.get(4)?,
            target_branch: row.get(5)?,
            pr_number: row.get(6)?,
            commit_sha: row.get(7)?,
            occurred_at: row.get(8)?,
            verification_method: crate::ExternalVerificationMethod::parse(&verification_method)?,
            verified_at: row.get(10)?,
            normalized_digest: row.get(11)?,
            status: ExternalEventStatus::parse(&status)?,
            session_id: row.get(13)?,
            queue_entry_id: row.get(14)?,
            advisory_id: row.get(15)?,
            received_at: row.get(16)?,
            reconciled_at: row.get(17)?,
            reconciliation_kind: row.get(18)?,
            reconciliation_reason_digest: row.get(19)?,
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

/// Record a pull request's own open and merge instants.
///
/// `MIN(COALESCE(existing, new), new)` rather than an unconditional overwrite:
/// the provider reports the same instant on every poll, but a watch that
/// started late can still recover the original `createdAt`, so a later poll must
/// never be able to make a milestone *later* than one already recorded. An
/// absent column is filled from the first poll that supplies it and then left
/// alone.
fn upsert_pull_request_milestone_in_tx(
    tx: &Transaction<'_>,
    repository: &str,
    pr_number: i64,
    session_id: Option<i64>,
    opened_at: Option<i64>,
    merged_at: Option<i64>,
    now: i64,
) -> Result<(), BrokerError> {
    let first_seen_at = match (opened_at, merged_at) {
        (Some(opened), Some(merged)) => opened.min(merged),
        (Some(opened), None) => opened,
        (None, Some(merged)) => merged,
        (None, None) => now,
    };
    // `opened_at` and `merged_at` are updated only when this observation
    // supplies one. A merge poll carries `mergedAt` but not `createdAt`, and
    // SQLite's `MIN(a, NULL)` is NULL — so a single combined upsert would erase
    // the opening that an earlier poll recorded, which is exactly the field the
    // insights report needs in order to have a lifetime at all. Two guarded
    // updates keep each column first-seen-wins and never cleared.
    tx.execute(
        "INSERT INTO pull_request_milestones (
             repository, pr_number, session_id, opened_at, merged_at,
             first_seen_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT (repository, pr_number) DO UPDATE SET
             session_id = COALESCE(session_id, ?3),
             first_seen_at = MIN(first_seen_at, ?6),
             updated_at = ?7",
        params![
            repository,
            pr_number,
            session_id,
            opened_at,
            merged_at,
            first_seen_at,
            now
        ],
    )?;
    if opened_at.is_some() {
        tx.execute(
            "UPDATE pull_request_milestones
             SET opened_at = MIN(COALESCE(opened_at, ?3), ?3)
             WHERE repository = ?1 AND pr_number = ?2",
            params![repository, pr_number, opened_at],
        )?;
    }
    if merged_at.is_some() {
        tx.execute(
            "UPDATE pull_request_milestones
             SET merged_at = MIN(COALESCE(merged_at, ?3), ?3)
             WHERE repository = ?1 AND pr_number = ?2",
            params![repository, pr_number, merged_at],
        )?;
    }
    if let Some(session_id) = session_id {
        tx.execute(
            "INSERT INTO pull_request_session_links
                 (repository, pr_number, session_id, linked_at, link_source)
             VALUES (?1, ?2, ?3, ?4, 'watch')
             ON CONFLICT DO NOTHING",
            params![repository, pr_number, session_id, now],
        )?;
    }
    Ok(())
}

/// `(repository, pr_number, opened_at, merged_at)` for one pull request, as
/// [`BrokerStore::pull_request_milestones`] returns it.
pub type PullRequestMilestoneRow = (String, i64, Option<i64>, Option<i64>);

/// End a session's open period of attention, if it has one.
///
/// Called from every path that closes a session, inside that path's
/// transaction, so a close and the end of its activity interval commit together.
///
/// A close within [`crate::insights::IDLE_GAP_MS`] of the last signal is the
/// agent finishing its own work, so the period runs to the close. A later close
/// is housekeeping — cleanup, the sweep, abandonment, often a day on — and the
/// period ends at its last signal, by the same rule that splits a period on a
/// long silence. A last signal after the close (a clock that disagreed) also
/// ends the period at that signal, never with a negative duration.
fn close_open_activity_in_tx(
    conn: &Connection,
    session_id: i64,
    at_ms: i64,
) -> Result<usize, BrokerError> {
    Ok(conn.execute(
        "UPDATE session_activity
         SET ended_at = CASE
                 WHEN ?2 >= last_signal_at AND ?2 - last_signal_at <= ?3 THEN ?2
                 ELSE last_signal_at
             END,
             source = 'close'
         WHERE session_id = ?1 AND ended_at IS NULL",
        params![session_id, at_ms, crate::insights::IDLE_GAP_MS],
    )?)
}

fn release_checkpoint_pin_in_tx(
    conn: &Connection,
    session_id: i64,
    released_at: i64,
) -> Result<bool, BrokerError> {
    let Some(queue_entry_id) = conn
        .query_row(
            "SELECT accepted_queue_entry_id
             FROM sessions
             WHERE id = ?1 AND status = 'cleaned'
               AND accepted_queue_entry_id IS NOT NULL",
            [session_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
    else {
        return Ok(false);
    };
    let inserted = conn.execute(
        "INSERT OR IGNORE INTO gc_checkpoint_pin_releases
             (session_id, queue_entry_id, released_at)
         VALUES (?1, ?2, ?3)",
        params![session_id, queue_entry_id, released_at],
    )?;
    if inserted > 0 {
        let payload = serde_json::json!({
            "session_id": session_id,
            "queue_entry_id": queue_entry_id,
            "committed_work_untouched": true,
            "automatic_terminal_transition": true,
        })
        .to_string();
        insert_event(
            conn,
            released_at,
            "gc.checkpoint_pin_released",
            Some(session_id),
            Some(&payload),
        )?;
    }
    Ok(inserted > 0)
}

pub(crate) fn insert_event(
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

        // The legacy fixture owns this database; an outer gate's
        // AETHYME_BROKER_DB override must not redirect the snapshot.
        let snapshot = BrokerStore::open_snapshot_at(&database).unwrap();
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

#[cfg(test)]
mod review_ledger_tests {
    use super::*;

    fn store() -> BrokerStore {
        let conn = Connection::open_in_memory().unwrap();
        schema::migrate(&conn).unwrap();
        BrokerStore {
            conn,
            path: PathBuf::from(":memory:"),
            _snapshot_dir: None,
        }
    }

    /// The `created` flag, not the row, is what the executor acts on: it is how
    /// a re-run after a crash tells "I am the one who asked for this" from "it
    /// was already asked for". Losing that distinction is the difference
    /// between a missed review and two reviewers on one pull request.
    #[test]
    fn recording_the_same_head_twice_reports_the_second_as_not_created() {
        let mut store = store();
        let (first, created) = store
            .record_review_request("o/r", 7, "security", "abc", None, "chau7", 100)
            .unwrap();
        assert!(created);
        let (second, created_again) = store
            .record_review_request("o/r", 7, "security", "abc", None, "chau7", 200)
            .unwrap();
        assert!(!created_again);
        assert_eq!(first.id, second.id);
        assert_eq!(
            second.requested_at,
            Some(100),
            "the first request keeps its time"
        );
    }

    /// A new head is a new review. The router asks again when the change it was
    /// reviewing has moved, and the ledger must not read that as a duplicate.
    #[test]
    fn a_new_head_is_a_new_request() {
        let mut store = store();
        let (first, _) = store
            .record_review_request("o/r", 7, "security", "abc", None, "chau7", 100)
            .unwrap();
        let (second, created) = store
            .record_review_request("o/r", 7, "security", "def", None, "chau7", 200)
            .unwrap();
        assert!(created);
        assert_ne!(first.id, second.id);
        assert_eq!(store.review_requests_for_pr("o/r", 7).unwrap().len(), 2);
    }

    #[test]
    fn a_state_change_is_readable_and_stamped() {
        let mut store = store();
        let (request, _) = store
            .record_review_request("o/r", 7, "security", "abc", None, "chau7", 100)
            .unwrap();
        assert_eq!(request.state, ReviewRequestState::Requested);
        let updated = store
            .set_review_request_state(request.id, ReviewRequestState::Failed, Some("no tab"), 300)
            .unwrap();
        assert_eq!(updated.state, ReviewRequestState::Failed);
        assert_eq!(updated.detail.as_deref(), Some("no tab"));
        assert_eq!(updated.updated_at, 300);
        assert_eq!(updated.requested_at, Some(100));
        let reread = store.review_request(request.id).unwrap().unwrap();
        assert_eq!(reread.state, ReviewRequestState::Failed);
    }

    #[test]
    fn completion_keeps_request_and_provider_facts_separate() {
        let mut store = store();
        let (request, _) = store
            .record_review_request_with_trigger(
                "o/r",
                7,
                "security",
                "requested-commit",
                Some("base-commit"),
                "chau7",
                Some(ReviewTrigger::AdditionalCommit),
                100,
            )
            .unwrap();

        let completed = store
            .complete_review_request(
                request.id,
                "completed-commit",
                ReviewVerdict::ChangesRequested,
                "github",
                Some("reviewer-model"),
                Some("found a blocking issue"),
                200,
            )
            .unwrap();
        assert_eq!(completed.head_commit, "requested-commit");
        assert_eq!(
            completed.requested_for_commit.as_deref(),
            Some("requested-commit")
        );
        assert_eq!(completed.trigger, Some(ReviewTrigger::AdditionalCommit));
        assert_eq!(completed.requested_at, Some(100));
        assert_eq!(completed.completed_at, Some(200));
        assert_eq!(
            completed.completed_for_commit.as_deref(),
            Some("completed-commit")
        );
        assert_eq!(completed.verdict, Some(ReviewVerdict::ChangesRequested));
        assert_eq!(
            completed.reviewer,
            Some(ReviewerIdentity {
                provider: "github".into(),
                model: Some("reviewer-model".into()),
            })
        );
        assert_eq!(completed.detail.as_deref(), Some("found a blocking issue"));
    }

    #[test]
    fn an_unsolicited_completion_is_a_first_class_row() {
        let mut store = store();
        let (completion, created) = store
            .record_unsolicited_review_completion(
                "o/r",
                7,
                "security",
                "provider-commit",
                ReviewVerdict::Pass,
                "github",
                Some("reviewer-model"),
                Some("no findings"),
                200,
            )
            .unwrap();
        assert!(created);
        assert_eq!(completion.head_commit, "provider-commit");
        assert_eq!(completion.requested_for_commit, None);
        assert_eq!(completion.requested_at, None);
        assert_eq!(completion.trigger, Some(ReviewTrigger::Unsolicited));
        assert_eq!(completion.completed_at, Some(200));
        assert_eq!(
            completion.completed_for_commit.as_deref(),
            Some("provider-commit")
        );
        assert_eq!(completion.verdict, Some(ReviewVerdict::Pass));
        assert_eq!(completion.backend, "unsolicited");
        assert_eq!(
            completion
                .reviewer
                .as_ref()
                .map(|reviewer| reviewer.provider.as_str()),
            Some("github")
        );

        let (updated, duplicate) = store
            .record_unsolicited_review_completion(
                "o/r",
                7,
                "security",
                "provider-commit",
                ReviewVerdict::Commented,
                "github",
                None,
                None,
                300,
            )
            .unwrap();
        assert!(!duplicate);
        assert_eq!(updated.id, completion.id);
        assert_eq!(updated.verdict, Some(ReviewVerdict::Commented));
        assert_eq!(updated.completed_at, Some(300));
        assert_eq!(updated.reviewer.unwrap().model, None);
    }

    #[test]
    fn a_late_request_reconciles_an_unsolicited_completion_without_overwriting_it() {
        let mut store = store();
        let (completion, _) = store
            .record_unsolicited_review_completion(
                "o/r",
                7,
                "security",
                "same-commit",
                ReviewVerdict::Pass,
                "github",
                Some("reviewer-model"),
                Some("no findings"),
                200,
            )
            .unwrap();
        let (request, created) = store
            .record_review_request_with_trigger(
                "o/r",
                7,
                "security",
                "same-commit",
                None,
                "provider_comment",
                Some(ReviewTrigger::Scheduled),
                300,
            )
            .unwrap();
        assert!(!created);
        assert_eq!(request.id, completion.id);
        assert_eq!(request.requested_for_commit.as_deref(), Some("same-commit"));
        assert_eq!(request.requested_at, Some(300));
        assert_eq!(request.trigger, Some(ReviewTrigger::Scheduled));
        assert_eq!(request.backend, "provider_comment");
        assert_eq!(request.completed_at, Some(200));
        assert_eq!(request.verdict, Some(ReviewVerdict::Pass));
        assert_eq!(
            request
                .reviewer
                .as_ref()
                .and_then(|reviewer| reviewer.model.as_deref()),
            Some("reviewer-model")
        );
    }

    /// The dimension most worth waiving is usually the one with no row at all
    /// -- a provider that refused on quota, a dispatch that never happened. If
    /// waiving needed an existing request it would be unavailable exactly
    /// there, which is where #172 leaves the operator stuck.
    #[test]
    fn a_dimension_nobody_requested_can_still_be_waived() {
        let mut store = store();
        assert!(store.review_requests_for_pr("o/r", 7).unwrap().is_empty());

        let waived = store
            .waive_review_request("o/r", 7, "security", "abc", "waived by Ada: hotfix", 100)
            .unwrap();
        assert_eq!(waived.state, ReviewRequestState::Waived);
        assert_eq!(waived.detail.as_deref(), Some("waived by Ada: hotfix"));
        assert_eq!(
            waived.backend, "waiver",
            "no backend performed this, and naming one would credit a reviewer \
             who never looked"
        );
        assert_eq!(store.review_requests_for_pr("o/r", 7).unwrap().len(), 1);
    }

    /// One line per review per head, as everywhere else in this table. A second
    /// row would break the unique index, and duplicating the dimension in the
    /// ledger would make spend count it twice.
    #[test]
    fn waiving_a_requested_review_reuses_its_row() {
        let mut store = store();
        let (request, _) = store
            .record_review_request("o/r", 7, "security", "abc", None, "chau7", 100)
            .unwrap();

        let waived = store
            .waive_review_request("o/r", 7, "security", "abc", "waived by Ada: hotfix", 300)
            .unwrap();
        assert_eq!(waived.id, request.id);
        assert_eq!(waived.state, ReviewRequestState::Waived);
        assert_eq!(
            waived.requested_at,
            Some(100),
            "when the review was asked for is history, not something a waiver rewrites"
        );
        assert_eq!(waived.updated_at, 300);
        assert_eq!(store.review_requests_for_pr("o/r", 7).unwrap().len(), 1);
    }

    /// The scope claim, at the storage layer: waiving one dimension at one head
    /// leaves every other row exactly as it was.
    #[test]
    fn waiving_one_dimension_leaves_the_others_untouched() {
        let mut store = store();
        store
            .record_review_request("o/r", 7, "security", "abc", None, "chau7", 100)
            .unwrap();
        store
            .record_review_request("o/r", 7, "code", "abc", None, "chau7", 100)
            .unwrap();
        store
            .record_review_request("o/r", 7, "code", "def", None, "chau7", 100)
            .unwrap();

        store
            .waive_review_request("o/r", 7, "code", "abc", "waived by Ada: hotfix", 300)
            .unwrap();

        let states: Vec<(String, String, ReviewRequestState)> = store
            .review_requests_for_pr("o/r", 7)
            .unwrap()
            .into_iter()
            .map(|row| (row.review_type, row.head_commit, row.state))
            .collect();
        assert_eq!(
            states,
            vec![
                (
                    "security".to_string(),
                    "abc".to_string(),
                    ReviewRequestState::Requested
                ),
                (
                    "code".to_string(),
                    "abc".to_string(),
                    ReviewRequestState::Waived
                ),
                (
                    "code".to_string(),
                    "def".to_string(),
                    ReviewRequestState::Requested
                ),
            ]
        );
    }

    /// One pull request's ledger is one pull request's ledger. The scheduler
    /// reads spend per pull request, so a query that leaked a neighbour's rows
    /// would silently stop asking for reviews.
    #[test]
    fn requests_are_scoped_to_their_pull_request_and_repository() {
        let mut store = store();
        store
            .record_review_request("o/r", 7, "security", "abc", None, "chau7", 100)
            .unwrap();
        store
            .record_review_request("o/r", 8, "security", "abc", None, "chau7", 100)
            .unwrap();
        store
            .record_review_request("o/other", 7, "security", "abc", None, "chau7", 100)
            .unwrap();
        assert_eq!(store.review_requests_for_pr("o/r", 7).unwrap().len(), 1);
    }

    /// The unique index makes a row permanent for its head, so without revival
    /// one failed `gh` call would settle a dimension forever: the next tick
    /// would find the row, count it as spend, and skip. Reviving reuses the row
    /// rather than inserting beside it, so the ledger keeps one line per review.
    #[test]
    fn a_review_nobody_was_asked_for_can_be_asked_for_again() {
        let mut store = store();
        let (first, _) = store
            .record_review_request("o/r", 7, "security", "abc", None, "chau7", 100)
            .unwrap();
        store
            .set_review_request_state(
                first.id,
                ReviewRequestState::Abandoned,
                Some("the mention did not post"),
                150,
            )
            .unwrap();

        let (revived, created) = store
            .record_review_request("o/r", 7, "security", "abc", None, "provider_comment", 200)
            .unwrap();
        assert!(
            created,
            "reviving is the executor asking for the review, so it must read as created"
        );
        assert_eq!(revived.id, first.id, "one line per review, not two");
        assert_eq!(revived.state, ReviewRequestState::Requested);
        assert_eq!(
            revived.detail, None,
            "the old failure is not the new attempt"
        );
        assert_eq!(
            revived.backend, "provider_comment",
            "policy may have moved since the attempt"
        );
        assert_eq!(revived.requested_at, Some(200));
        assert_eq!(store.review_requests_for_pr("o/r", 7).unwrap().len(), 1);
    }

    /// Every other terminal state is an answer. Asking again would either
    /// duplicate work already done or re-run something that already produced no
    /// verdict for this exact head.
    #[test]
    fn a_settled_review_is_not_revived() {
        for settled in [
            ReviewRequestState::Satisfied,
            ReviewRequestState::Failed,
            ReviewRequestState::Recorded,
        ] {
            let mut store = store();
            let (first, _) = store
                .record_review_request("o/r", 7, "security", "abc", None, "chau7", 100)
                .unwrap();
            if settled == ReviewRequestState::Satisfied {
                store
                    .complete_review_request(
                        first.id,
                        "abc",
                        ReviewVerdict::Pass,
                        "test-provider",
                        Some("test-model"),
                        None,
                        150,
                    )
                    .unwrap();
            } else {
                store
                    .set_review_request_state(first.id, settled, None, 150)
                    .unwrap();
            }
            let (again, created) = store
                .record_review_request("o/r", 7, "security", "abc", None, "chau7", 200)
                .unwrap();
            assert!(!created, "{settled:?} is an answer, not a retry");
            assert_eq!(again.state, settled);
        }
    }

    /// `max_concurrent` is a per-repository budget. Reading it from one pull
    /// request's rows multiplies the cap by the number of open pull requests,
    /// which is the exact failure a cap exists to prevent.
    #[test]
    fn in_flight_is_counted_across_the_repository() {
        let mut store = store();
        store
            .record_review_request("o/r", 7, "security", "abc", None, "chau7", 100)
            .unwrap();
        store
            .record_review_request("o/r", 8, "security", "def", None, "chau7", 100)
            .unwrap();
        store
            .record_review_request("o/other", 9, "security", "ghi", None, "chau7", 100)
            .unwrap();
        let (settled, _) = store
            .record_review_request("o/r", 10, "security", "jkl", None, "chau7", 100)
            .unwrap();
        store
            .complete_review_request(
                settled.id,
                "jkl",
                ReviewVerdict::Pass,
                "test-provider",
                Some("test-model"),
                None,
                200,
            )
            .unwrap();

        let open = store.review_requests_in_flight("o/r").unwrap();
        assert_eq!(
            open.len(),
            2,
            "both open pull requests in this repository hold a slot, the neighbouring repository holds none, and a settled review holds nothing"
        );
        assert!(open.iter().all(|row| row.repository == "o/r"));
    }

    /// A reviewer reporting back names the review, not the row. Newest wins so
    /// a report that arrives now lands on what was most recently asked for;
    /// naming a head is how a late report about a superseded commit lands on
    /// the row it is actually about.
    #[test]
    fn the_latest_request_is_the_one_a_reviewer_reports_against() {
        let mut store = store();
        let (old, _) = store
            .record_review_request("o/r", 7, "security", "abc", None, "chau7", 100)
            .unwrap();
        let (current, _) = store
            .record_review_request("o/r", 7, "security", "def", None, "chau7", 200)
            .unwrap();

        let latest = store
            .latest_review_request("o/r", 7, "security", None)
            .unwrap()
            .unwrap();
        assert_eq!(latest.id, current.id);
        let named = store
            .latest_review_request("o/r", 7, "security", Some("abc"))
            .unwrap()
            .unwrap();
        assert_eq!(named.id, old.id);
        assert!(
            store
                .latest_review_request("o/r", 7, "performance", None)
                .unwrap()
                .is_none(),
            "a review nobody requested has no row to report against"
        );
    }

    /// An unsolicited completion has no request timestamp, but it is still a
    /// newer review fact when its completion arrived after the last request.
    /// The default reporting path must not attach a later provider result to an
    /// older requested row merely because that row has a non-null timestamp.
    #[test]
    fn the_latest_review_fact_includes_unsolicited_completions() {
        let mut store = store();
        store
            .record_review_request("o/r", 7, "security", "requested", None, "chau7", 100)
            .unwrap();
        store
            .record_unsolicited_review_completion(
                "o/r",
                7,
                "security",
                "completed",
                ReviewVerdict::Pass,
                "github",
                None,
                None,
                200,
            )
            .unwrap();

        let latest = store
            .latest_review_request("o/r", 7, "security", None)
            .unwrap()
            .unwrap();
        assert_eq!(latest.head_commit, "completed");
        assert_eq!(latest.requested_at, None);
        assert_eq!(latest.completed_at, Some(200));
    }
}

mod coordination;
mod open;
mod pull_requests;
mod queue;
mod review;
