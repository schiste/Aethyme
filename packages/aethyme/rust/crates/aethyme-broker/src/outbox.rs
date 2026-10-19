//! Shared mechanics for the pull-request and repository delivery outboxes.
//!
//! The tables and envelopes remain independent. Claim eligibility, retry
//! scheduling, generation fencing, and terminal transitions are policy shared
//! by both storage paths.

use crate::{DeliveryCompletion, DeliveryStatus};

/// How many times one delivery may be attempted before a retry dead-letters it.
///
/// A claim counts as an attempt. Retry delay grows 15s, 30s, 60s, 120s, 240s,
/// then holds at 300s; this cap bounds a repeatedly unavailable recipient.
pub(crate) const MAX_DELIVERY_ATTEMPTS: i64 = 20;

/// SQL predicate shared by both `claim_next` queries. A first attempt is due
/// immediately, a retry observes bounded exponential backoff, and an expired
/// worker claim can be recovered promptly.
pub(crate) const CLAIMABLE_PREDICATE_SQL: &str = r#"(
    (o.status = 'pending' AND o.attempt_count = 0)
    OR (o.status = 'pending' AND o.attempt_count > 0
        AND o.updated_at + MIN(15000 * (1 << MIN(o.attempt_count - 1, 5)), 300000) <= ?2)
    OR (o.status = 'claimed' AND o.claim_expires_at <= ?2)
)"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutboxTable {
    PullRequest,
    Repository,
}

impl OutboxTable {
    fn sql_name(self) -> &'static str {
        match self {
            Self::PullRequest => "delivery_outbox",
            Self::Repository => "repository_delivery_outbox",
        }
    }
}

/// The tables have separate rows and envelopes but apply one claim mutation.
pub(crate) fn claim_update_statement(table: OutboxTable) -> String {
    format!(
        "UPDATE {}\n         SET status = 'claimed', generation = generation + 1,\n             claimed_by = ?2, claim_expires_at = ?3,\n             attempt_count = attempt_count + 1,\n             last_error_code = NULL, updated_at = ?4\n         WHERE id = ?1",
        table.sql_name()
    )
}

/// One generation-fenced completion update is shared by both outboxes.
pub(crate) fn completion_update_statement(table: OutboxTable) -> String {
    format!(
        "UPDATE {}\n         SET status = ?2, claimed_by = NULL, claim_expires_at = NULL,\n             last_error_code = ?3, delivered_at = ?4, updated_at = ?5\n         WHERE id = ?1 AND status = 'claimed' AND generation = ?6 AND claimed_by = ?7",
        table.sql_name()
    )
}

/// Whether a worker still owns the exact, unexpired generation it claimed.
pub(crate) fn claim_is_current(
    status: DeliveryStatus,
    claimed_by: Option<&str>,
    current_generation: i64,
    claim_expires_at: Option<i64>,
    worker: &str,
    generation: i64,
    now_ms: i64,
) -> bool {
    status == DeliveryStatus::Claimed
        && claimed_by == Some(worker)
        && current_generation == generation
        && claim_expires_at.is_some_and(|expires_at| expires_at > now_ms)
}

/// The terminal/pending state after one fenced completion, including the
/// attempt cap that dead-letters a delivery after its final retry.
pub(crate) fn completion_state(
    completion: DeliveryCompletion,
    attempt_count: i64,
    now_ms: i64,
) -> (DeliveryStatus, Option<i64>) {
    match completion {
        DeliveryCompletion::Delivered => (DeliveryStatus::Delivered, Some(now_ms)),
        DeliveryCompletion::Retry if attempt_count >= MAX_DELIVERY_ATTEMPTS => {
            (DeliveryStatus::Failed, None)
        }
        DeliveryCompletion::Retry => (DeliveryStatus::Pending, None),
        DeliveryCompletion::Failed => (DeliveryStatus::Failed, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_requires_the_current_unexpired_claim() {
        let current = |worker, generation, expires_at| {
            claim_is_current(
                DeliveryStatus::Claimed,
                Some(worker),
                generation,
                Some(expires_at),
                "worker-a",
                7,
                100,
            )
        };
        assert!(current("worker-a", 7, 101));
        assert!(!current("worker-b", 7, 101));
        assert!(!current("worker-a", 8, 101));
        assert!(!current("worker-a", 7, 100));
        assert!(!claim_is_current(
            DeliveryStatus::Pending,
            Some("worker-a"),
            7,
            Some(101),
            "worker-a",
            7,
            100,
        ));
    }

    #[test]
    fn retries_dead_letter_at_the_shared_attempt_limit() {
        assert_eq!(
            completion_state(DeliveryCompletion::Retry, MAX_DELIVERY_ATTEMPTS - 1, 42),
            (DeliveryStatus::Pending, None)
        );
        assert_eq!(
            completion_state(DeliveryCompletion::Retry, MAX_DELIVERY_ATTEMPTS, 42),
            (DeliveryStatus::Failed, None)
        );
        assert_eq!(
            completion_state(DeliveryCompletion::Delivered, MAX_DELIVERY_ATTEMPTS, 42),
            (DeliveryStatus::Delivered, Some(42))
        );
        assert_eq!(
            completion_state(DeliveryCompletion::Failed, 1, 42),
            (DeliveryStatus::Failed, None)
        );
    }

    #[test]
    fn claim_schedule_and_fenced_updates_are_shared_by_both_tables() {
        assert!(
            CLAIMABLE_PREDICATE_SQL
                .contains("MIN(15000 * (1 << MIN(o.attempt_count - 1, 5)), 300000)")
        );
        assert!(CLAIMABLE_PREDICATE_SQL.contains("o.claim_expires_at <= ?2"));
        for table in [OutboxTable::PullRequest, OutboxTable::Repository] {
            assert!(claim_update_statement(table).contains("generation = generation + 1"));
            assert!(
                completion_update_statement(table).contains("generation = ?6 AND claimed_by = ?7")
            );
        }
    }
}
