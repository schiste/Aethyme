use super::*;

impl BrokerStore {
    pub(crate) fn record_pull_request_watch_poll(
        &mut self,
        id: i64,
        snapshot: &PullRequestSnapshot,
        cursor_digest: &str,
        status: PullRequestWatchStatus,
        now: i64,
    ) -> Result<PullRequestWatchPollStorageResult, BrokerError> {
        let current = self
            .pull_request_watch(id)?
            .ok_or(BrokerError::InvalidEnumValue {
                field: "pull_request_watch.id",
                value: id.to_string(),
            })?;
        let next_poll_at = (status == PullRequestWatchStatus::Active)
            .then_some(now + current.poll_interval_seconds as i64 * 1_000);
        let tx = self.conn.transaction()?;
        let mut new_activity_ids = Vec::new();
        let mut new_activity_identities = Vec::new();
        for activity in &snapshot.activities {
            let (activity_id, inserted) = upsert_pull_request_activity(&tx, id, activity, now)?;
            if inserted {
                new_activity_ids.push(activity_id);
                new_activity_identities.push(format!(
                    "{}:{}",
                    activity.kind.as_str(),
                    activity.provider_id
                ));
            }
        }
        new_activity_identities.sort();
        let batch_id = if new_activity_ids.is_empty() {
            None
        } else {
            let digest_input =
                serde_json::to_vec(&(id, snapshot.head_sha.as_str(), &new_activity_identities))
                    .expect("serializing batch identity cannot fail");
            let batch_digest = format!("{:x}", Sha256::digest(digest_input));
            tx.execute(
                "INSERT INTO pull_request_activity_batches (
                     watch_id, head_sha, digest, activity_count, status, created_at
                 ) VALUES (?1, ?2, ?3, ?4, 'pending', ?5)",
                params![
                    id,
                    snapshot.head_sha,
                    batch_digest,
                    new_activity_ids.len() as i64,
                    now,
                ],
            )?;
            let batch_id = tx.last_insert_rowid();
            for activity_id in &new_activity_ids {
                tx.execute(
                    "INSERT INTO pull_request_activity_batch_items (batch_id, activity_id)
                     VALUES (?1, ?2)",
                    params![batch_id, activity_id],
                )?;
            }
            tx.execute(
                "INSERT OR IGNORE INTO delivery_outbox (
                     subscription_id, batch_id, status, created_at, updated_at
                 )
                 SELECT id, ?1, 'pending', ?2, ?2
                 FROM delivery_subscriptions
                 WHERE watch_id = ?3 AND active = 1",
                params![batch_id, now, id],
            )?;
            Some(batch_id)
        };
        tx.execute(
            "UPDATE pull_request_watches
             SET target_branch = ?2, head_sha = ?3, is_draft = ?4,
                 status = ?5, cursor_digest = ?6, last_polled_at = ?7,
                 next_poll_at = ?8, last_error_code = NULL, updated_at = ?7
             WHERE id = ?1",
            params![
                id,
                snapshot.target_branch,
                snapshot.head_sha,
                snapshot.is_draft,
                status.as_str(),
                cursor_digest,
                now,
                next_poll_at,
            ],
        )?;
        // The provider's own open and merge instants, recorded on first sight.
        // Both are in the same transaction as the poll that observed them, so a
        // crash cannot leave a poll that saw a merge with nothing recording it.
        //
        // Written on *every* poll rather than only on transition, because the
        // first poll of a pull request that is already merged is the only poll
        // that can ever observe the opening, and a watch that starts late must
        // still recover the provider's `createdAt`.
        if let Some(created_at_ms) = snapshot.created_at_ms {
            upsert_pull_request_milestone_in_tx(
                &tx,
                &current.display_repository,
                snapshot.number,
                Some(current.session_id),
                Some(created_at_ms),
                None,
                now,
            )?;
        }
        if let Some(merged_at_ms) = snapshot.merged_at_ms {
            upsert_pull_request_milestone_in_tx(
                &tx,
                &current.display_repository,
                snapshot.number,
                Some(current.session_id),
                None,
                Some(merged_at_ms),
                now,
            )?;
        }
        let payload = serde_json::json!({
            "watch_id": id,
            "head_sha": snapshot.head_sha,
            "status": status,
            "activity_count": snapshot.activities.len(),
            "new_activity_count": new_activity_ids.len(),
            "batch_id": batch_id,
        })
        .to_string();
        insert_event(
            &tx,
            now,
            "pr_watch.polled",
            Some(current.session_id),
            Some(&payload),
        )?;
        tx.commit()?;
        let watch = self
            .pull_request_watch(id)?
            .ok_or(BrokerError::InvalidEnumValue {
                field: "pull_request_watch.id",
                value: id.to_string(),
            })?;
        let batch = batch_id
            .map(|batch_id| self.pull_request_activity_batch(batch_id))
            .transpose()?
            .flatten();
        Ok(PullRequestWatchPollStorageResult { watch, batch })
    }

    pub(crate) fn record_pull_request_watch_failure(
        &mut self,
        id: i64,
        error_code: &str,
        retry_at: i64,
        now: i64,
        attempted: bool,
    ) -> Result<PullRequestWatch, BrokerError> {
        let current = self
            .pull_request_watch(id)?
            .ok_or(BrokerError::InvalidEnumValue {
                field: "pull_request_watch.id",
                value: id.to_string(),
            })?;
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE pull_request_watches
             SET last_polled_at = CASE WHEN ?5 THEN ?2 ELSE last_polled_at END,
                 next_poll_at = ?3, last_error_code = ?4, updated_at = ?2
             WHERE id = ?1 AND status = 'active'",
            params![id, now, retry_at, error_code, attempted],
        )?;
        let payload = serde_json::json!({
            "watch_id": id,
            "error_code": error_code,
            "retry_at": retry_at,
        })
        .to_string();
        insert_event(
            &tx,
            now,
            if attempted {
                "pr_watch.poll_failed"
            } else {
                "pr_watch.poll_deferred"
            },
            Some(current.session_id),
            Some(&payload),
        )?;
        tx.commit()?;
        self.pull_request_watch(id)?
            .ok_or(BrokerError::InvalidEnumValue {
                field: "pull_request_watch.id",
                value: id.to_string(),
            })
    }

    pub fn pull_request_activity_batches(
        &self,
        watch_id: i64,
        include_acknowledged: bool,
    ) -> Result<Vec<PullRequestActivityBatch>, BrokerError> {
        let sql = if include_acknowledged {
            "SELECT id FROM pull_request_activity_batches WHERE watch_id = ?1 ORDER BY id"
        } else {
            "SELECT id FROM pull_request_activity_batches WHERE watch_id = ?1 AND status = 'pending' ORDER BY id"
        };
        let mut statement = self.conn.prepare(sql)?;
        let ids = statement
            .query_map([watch_id], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| {
                self.pull_request_activity_batch(id)?
                    .ok_or(BrokerError::PullRequestActivityBatchNotFound(id))
            })
            .collect()
    }

    pub fn pull_request_activity_batch(
        &self,
        batch_id: i64,
    ) -> Result<Option<PullRequestActivityBatch>, BrokerError> {
        let header = self
            .conn
            .query_row(
                "SELECT watch_id, head_sha, digest, status, ack_outcome,
                        ack_reason_digest, created_at, acknowledged_at
                 FROM pull_request_activity_batches WHERE id = ?1",
                [batch_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, Option<i64>>(7)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            watch_id,
            head_sha,
            digest,
            status,
            ack_outcome,
            ack_reason_digest,
            created_at,
            acknowledged_at,
        )) = header
        else {
            return Ok(None);
        };
        let mut statement = self.conn.prepare(
            "SELECT a.id, a.watch_id, a.kind, a.provider_id, a.author, a.state,
                    a.url, a.provider_updated_at, a.first_seen_at, a.last_seen_at
             FROM pull_request_activities a
             JOIN pull_request_activity_batch_items i ON i.activity_id = a.id
             WHERE i.batch_id = ?1 ORDER BY a.kind, a.provider_id",
        )?;
        let activities = statement
            .query_map([batch_id], pull_request_activity_from_row)?
            .map(|row| row?)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(PullRequestActivityBatch {
            id: batch_id,
            watch_id,
            head_sha,
            digest,
            activities,
            status: PullRequestBatchStatus::parse(&status)?,
            ack_outcome: ack_outcome
                .as_deref()
                .map(PullRequestBatchAckOutcome::parse)
                .transpose()?,
            ack_reason_digest,
            created_at,
            acknowledged_at,
        }))
    }

    pub fn acknowledge_pull_request_activity_batch(
        &mut self,
        batch_id: i64,
        outcome: PullRequestBatchAckOutcome,
        reason_digest: &str,
        now: i64,
    ) -> Result<PullRequestActivityBatch, BrokerError> {
        let current = self
            .pull_request_activity_batch(batch_id)?
            .ok_or(BrokerError::PullRequestActivityBatchNotFound(batch_id))?;
        if current.status == PullRequestBatchStatus::Acknowledged {
            if current.ack_outcome == Some(outcome)
                && current.ack_reason_digest.as_deref() == Some(reason_digest)
            {
                return Ok(current);
            }
            return Err(BrokerError::PullRequestActivityBatchAckConflict(batch_id));
        }
        let session_id = self
            .pull_request_watch(current.watch_id)?
            .map(|watch| watch.session_id);
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE pull_request_activity_batches
             SET status = 'acknowledged', ack_outcome = ?2,
                 ack_reason_digest = ?3, acknowledged_at = ?4
             WHERE id = ?1 AND status = 'pending'",
            params![batch_id, outcome.as_str(), reason_digest, now],
        )?;
        let payload = serde_json::json!({
            "batch_id": batch_id,
            "watch_id": current.watch_id,
            "outcome": outcome,
        })
        .to_string();
        insert_event(
            &tx,
            now,
            "pr_watch.batch_acknowledged",
            session_id,
            Some(&payload),
        )?;
        tx.commit()?;
        self.pull_request_activity_batch(batch_id)?
            .ok_or(BrokerError::PullRequestActivityBatchNotFound(batch_id))
    }

    // ── provider-neutral delivery outbox ─────────────────────────────

    pub fn subscribe_pull_request_delivery(
        &mut self,
        watch_id: i64,
        adapter: &str,
        target: &str,
        policy: DeliveryPolicy,
        now: i64,
    ) -> Result<DeliverySubscription, BrokerError> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO delivery_subscriptions (
                 watch_id, adapter, target, policy, active, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, 1, ?5, ?5)
             ON CONFLICT(watch_id, adapter, target) DO UPDATE SET
                 policy = excluded.policy, active = 1, updated_at = excluded.updated_at",
            params![watch_id, adapter, target, policy.as_str(), now],
        )?;
        let subscription_id = tx.query_row(
            "SELECT id FROM delivery_subscriptions
             WHERE watch_id = ?1 AND adapter = ?2 AND target = ?3",
            params![watch_id, adapter, target],
            |row| row.get::<_, i64>(0),
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO delivery_outbox (
                 subscription_id, batch_id, status, created_at, updated_at
             )
             SELECT ?1, id, 'pending', ?2, ?2
             FROM pull_request_activity_batches
             WHERE watch_id = ?3 AND status = 'pending'",
            params![subscription_id, now, watch_id],
        )?;
        let payload = serde_json::json!({
            "subscription_id": subscription_id,
            "watch_id": watch_id,
            "adapter": adapter,
            "policy": policy,
        })
        .to_string();
        let session_id = tx.query_row(
            "SELECT session_id FROM pull_request_watches WHERE id = ?1",
            [watch_id],
            |row| row.get::<_, i64>(0),
        )?;
        insert_event(
            &tx,
            now,
            "delivery.subscribed",
            Some(session_id),
            Some(&payload),
        )?;
        tx.commit()?;
        self.delivery_subscription(subscription_id)?
            .ok_or(BrokerError::InvalidEnumValue {
                field: "delivery_subscription.id",
                value: subscription_id.to_string(),
            })
    }

    pub fn delivery_subscription(
        &self,
        id: i64,
    ) -> Result<Option<DeliverySubscription>, BrokerError> {
        self.conn
            .query_row(
                &(DELIVERY_SUBSCRIPTION_SELECT.to_owned() + " WHERE id = ?1"),
                [id],
                delivery_subscription_from_row,
            )
            .optional()?
            .transpose()
    }

    pub fn delivery_outbox(
        &self,
        adapter: Option<&str>,
        include_terminal: bool,
    ) -> Result<Vec<DeliveryOutboxItem>, BrokerError> {
        let mut sql = DELIVERY_OUTBOX_SELECT.to_owned()
            + " JOIN delivery_subscriptions s ON s.id = o.subscription_id WHERE 1 = 1";
        if adapter.is_some() {
            sql.push_str(" AND s.adapter = ?1");
        }
        if !include_terminal {
            sql.push_str(" AND o.status IN ('pending', 'claimed')");
        }
        sql.push_str(" ORDER BY o.id");
        let mut statement = self.conn.prepare(&sql)?;
        let rows = if let Some(adapter) = adapter {
            statement.query_map([adapter], delivery_outbox_from_row)?
        } else {
            statement.query_map([], delivery_outbox_from_row)?
        };
        rows.map(|row| row?).collect()
    }

    pub fn claim_next_delivery(
        &mut self,
        adapter: &str,
        worker: &str,
        claim_seconds: u64,
        now: i64,
    ) -> Result<
        Option<(
            DeliveryOutboxItem,
            DeliverySubscription,
            PullRequestWatch,
            PullRequestActivityBatch,
        )>,
        BrokerError,
    > {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = tx
            .query_row(
                "SELECT o.id
                 FROM delivery_outbox o
                 JOIN delivery_subscriptions s ON s.id = o.subscription_id
                 WHERE s.adapter = ?1 AND s.active = 1
                   AND (
                        -- Never attempted: claimable at once.
                        (o.status = 'pending' AND o.attempt_count = 0)
                        -- Retried: hold it back, or `ORDER BY o.id` re-selects
                        -- the same row forever and starves every later
                        -- delivery for this adapter (#154). The target that
                        -- just declined will not have changed a millisecond
                        -- later, so an immediate retry cannot succeed anyway.
                        OR (o.status = 'pending' AND o.attempt_count > 0
                            AND o.updated_at + MIN(

                                    15000 * (1 << MIN(o.attempt_count - 1, 5)),
                                    300000
                                ) <= ?2)
                        -- An expired claim means the worker died, not that the
                        -- target refused, so it retries promptly.
                        OR (o.status = 'claimed' AND o.claim_expires_at <= ?2))
                 ORDER BY o.id LIMIT 1",
                params![adapter, now],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let Some(id) = id else {
            tx.commit()?;
            return Ok(None);
        };
        tx.execute(
            "UPDATE delivery_outbox
             SET status = 'claimed', generation = generation + 1,
                 claimed_by = ?2, claim_expires_at = ?3,
                 attempt_count = attempt_count + 1,
                 last_error_code = NULL, updated_at = ?4
             WHERE id = ?1",
            params![id, worker, now + claim_seconds as i64 * 1_000, now],
        )?;
        tx.commit()?;
        self.delivery_context(id).map(Some)
    }

    pub fn complete_delivery(
        &mut self,
        id: i64,
        worker: &str,
        generation: i64,
        completion: DeliveryCompletion,
        error_code: Option<&str>,
        now: i64,
    ) -> Result<DeliveryOutboxItem, BrokerError> {
        let (current, subscription, watch, _) = self.delivery_context(id)?;
        if current.status != DeliveryStatus::Claimed
            || current.claimed_by.as_deref() != Some(worker)
            || current.generation != generation
            || current.claim_expires_at.is_none_or(|expiry| expiry <= now)
        {
            return Err(BrokerError::DeliveryClaimChanged {
                id,
                worker: worker.into(),
                generation,
            });
        }
        let (status, delivered_at) = match completion {
            DeliveryCompletion::Delivered => (DeliveryStatus::Delivered, Some(now)),
            // The claim already counted this attempt, so an exhausted row is
            // dead-lettered here rather than handed back to the adapter that
            // has just failed to place it `MAX_DELIVERY_ATTEMPTS` times. The
            // caller's `Retry` stays advisory: only the broker can see how
            // long the row has been asking, so only the broker can stop it.
            DeliveryCompletion::Retry if current.attempt_count >= MAX_DELIVERY_ATTEMPTS => {
                (DeliveryStatus::Failed, None)
            }
            DeliveryCompletion::Retry => (DeliveryStatus::Pending, None),
            DeliveryCompletion::Failed => (DeliveryStatus::Failed, None),
        };
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = tx.execute(
            "UPDATE delivery_outbox
             SET status = ?2, claimed_by = NULL, claim_expires_at = NULL,
                 last_error_code = ?3, delivered_at = ?4, updated_at = ?5
             WHERE id = ?1 AND status = 'claimed' AND generation = ?6 AND claimed_by = ?7",
            params![
                id,
                status.as_str(),
                error_code,
                delivered_at,
                now,
                generation,
                worker,
            ],
        )?;
        if updated != 1 {
            return Err(BrokerError::DeliveryClaimChanged {
                id,
                worker: worker.into(),
                generation,
            });
        }
        let payload = serde_json::json!({
            "delivery_id": id,
            "subscription_id": subscription.id,
            "adapter": subscription.adapter,
            "status": status,
            "generation": generation,
        })
        .to_string();
        insert_event(
            &tx,
            now,
            &format!("delivery.{}", status.as_str()),
            Some(watch.session_id),
            Some(&payload),
        )?;
        tx.commit()?;
        self.delivery_outbox_item(id)?
            .ok_or(BrokerError::DeliveryOutboxNotFound(id))
    }

    /// The connection, for broker modules that own their own tables
    /// (repository watches, #606) rather than growing this file further.
    pub(crate) fn connection(&self) -> &Connection {
        &self.conn
    }

    pub(crate) fn connection_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    pub fn delivery_outbox_item(&self, id: i64) -> Result<Option<DeliveryOutboxItem>, BrokerError> {
        self.conn
            .query_row(
                &(DELIVERY_OUTBOX_SELECT.to_owned() + " WHERE o.id = ?1"),
                [id],
                delivery_outbox_from_row,
            )
            .optional()?
            .transpose()
    }

    pub(super) fn delivery_context(
        &self,
        id: i64,
    ) -> Result<
        (
            DeliveryOutboxItem,
            DeliverySubscription,
            PullRequestWatch,
            PullRequestActivityBatch,
        ),
        BrokerError,
    > {
        let item = self
            .delivery_outbox_item(id)?
            .ok_or(BrokerError::DeliveryOutboxNotFound(id))?;
        let subscription = self
            .delivery_subscription(item.subscription_id)?
            .ok_or(BrokerError::DeliveryOutboxNotFound(id))?;
        let watch = self
            .pull_request_watch(subscription.watch_id)?
            .ok_or(BrokerError::DeliveryOutboxNotFound(id))?;
        let batch = self
            .pull_request_activity_batch(item.batch_id)?
            .ok_or(BrokerError::DeliveryOutboxNotFound(id))?;
        Ok((item, subscription, watch, batch))
    }

    // ── events ────────────────────────────────────────────────────────

    /// Append one event. Most mutations already emit their own event in
    /// the same transaction; this is for kinds with no store mutation
    /// (e.g. `lease.overlap`, `worktree.stale`).
    /// Read a broker-scoped key from the `meta` table.
    ///
    /// Durable maintenance bookkeeping belongs here rather than in a runtime
    /// file: anything written under `.aethyme/` shows up as a dirty path to the
    /// checkout-cleanliness gates.
    pub fn meta_get(&self, key: &str) -> Result<Option<String>, BrokerError> {
        let value = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
                row.get::<_, String>(0)
            })
            .optional()?;
        Ok(value)
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<(), BrokerError> {
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [key, value],
        )?;
        Ok(())
    }

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

    // ── test-only fixtures ────────────────────────────────────────────
    //
    // These exist because the insights tests need scenarios the production
    // path cannot produce: an event stamped in the past, a session created at a
    // chosen instant. `append_event` is deliberately now-only — there is no
    // production reason to backdate an event, and adding a timestamp parameter
    // for tests would make backdating reachable from production. So the tests
    // write the row through the real API and correct it here, in one place,
    // rather than growing a general capability.

    /// Backdate one event row. `#[doc(hidden)]`: reachable from integration
    /// tests, not part of the broker's API.
    #[doc(hidden)]
    pub fn set_event_timestamp_for_test(&mut self, id: i64, ts: i64) {
        self.conn
            .execute("UPDATE events SET ts = ?2 WHERE id = ?1", params![id, ts])
            .expect("event row exists");
    }

    /// Backdate one session's `created_at`, so a funnel scenario's window is
    /// anchored where the test means it to be.
    #[doc(hidden)]
    pub fn set_session_created_at_for_test(&mut self, id: i64, created_at: i64) {
        self.conn
            .execute(
                "UPDATE sessions SET created_at = ?2 WHERE id = ?1",
                params![id, created_at],
            )
            .expect("session row exists");
    }

    /// How many activity intervals a session has, optionally only the open one.
    #[doc(hidden)]
    pub fn count_session_activity_for_test(&self, session_id: i64, open_only: bool) -> i64 {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM session_activity
                 WHERE session_id = ?1 AND (?2 = 0 OR ended_at IS NULL)",
                params![session_id, i64::from(open_only)],
                |row| row.get(0),
            )
            .expect("activity query")
    }

    /// Newest event timestamp in the log, or 0 when it is empty.
    #[doc(hidden)]
    pub fn newest_event_ts(&self) -> i64 {
        self.conn
            .query_row("SELECT COALESCE(MAX(ts), 0) FROM events", [], |row| {
                row.get(0)
            })
            .expect("events query")
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

    /// Exact database rows eligible under the retention cutoffs. Protection
    /// predicates live in SQL so planning and apply can share one definition.
    pub fn gc_row_candidates(
        &self,
        event_cutoff: i64,
        gate_cutoff: i64,
        queue_cutoff: i64,
    ) -> Result<Vec<crate::GcRowCandidate>, BrokerError> {
        use crate::{GcRowCandidate, GcRowKind};

        let mut candidates = Vec::new();
        let unresolved_advisories: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM advisories WHERE resolution_state != 'resolved'",
            [],
            |row| row.get(0),
        )?;
        let outstanding_exposures: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM entry_path_exposures WHERE state = 'outstanding'",
            [],
            |row| row.get(0),
        )?;
        let unresolved_operations: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM coordinated_operations
             WHERE status IN ('prepared', 'running', 'outcome_unknown')",
            [],
            |row| row.get(0),
        )?;
        let unresolved_queue: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM merge_queue
             WHERE status IN ('submitted', 'simulating', 'conflict', 'verified')",
            [],
            |row| row.get(0),
        )?;

        let mut events = self.conn.prepare(
            "SELECT e.id, e.ts,
                    length(e.kind) + COALESCE(length(e.payload_json), 0) + 32
             FROM events e
             WHERE e.ts < ?1
               AND NOT EXISTS (
                   SELECT 1 FROM sessions s
                   WHERE s.id = e.session_id AND s.cleanup_state = 'open'
               )
               AND (?2 = 0 OR e.kind NOT LIKE 'advisory.%')
               AND (?3 = 0 OR e.kind NOT LIKE 'exposure.%')
               AND (?4 = 0 OR e.kind NOT LIKE 'operation.%')
               AND (?5 = 0 OR e.kind NOT LIKE 'merge.%')
             ORDER BY e.id",
        )?;
        let rows = events.query_map(
            params![
                event_cutoff,
                unresolved_advisories,
                outstanding_exposures,
                unresolved_operations,
                unresolved_queue
            ],
            |row| {
                Ok(GcRowCandidate {
                    kind: GcRowKind::Event,
                    id: row.get(0)?,
                    recorded_at: row.get(1)?,
                    estimated_bytes: row.get::<_, i64>(2)?.max(0) as u64,
                    gate_log_path: None,
                })
            },
        )?;
        candidates.extend(rows.collect::<Result<Vec<_>, _>>()?);

        let mut gates = self.conn.prepare(
            "SELECT r.id, r.created_at,
                    length(r.gate_name) + length(r.tree_hash)
                      + COALESCE(length(r.definition_hash), 0)
                      + COALESCE(length(r.log_path), 0) + 96,
                    r.log_path
             FROM gate_results r
             WHERE r.created_at < ?1
               AND NOT EXISTS (
                   SELECT 1 FROM sessions s
                   WHERE s.id = r.session_id AND s.cleanup_state = 'open'
               )
             ORDER BY r.id",
        )?;
        let rows = gates.query_map([gate_cutoff], |row| {
            Ok(GcRowCandidate {
                kind: GcRowKind::GateResult,
                id: row.get(0)?,
                recorded_at: row.get(1)?,
                estimated_bytes: row.get::<_, i64>(2)?.max(0) as u64,
                gate_log_path: row.get(3)?,
            })
        })?;
        candidates.extend(rows.collect::<Result<Vec<_>, _>>()?);

        let mut queue = self.conn.prepare(
            "WITH eligible_queue AS (
            SELECT q.* FROM merge_queue q
            WHERE q.updated_at < ?1
              AND q.status IN ('promoted', 'externally_landed', 'rejected', 'superseded')
              AND NOT EXISTS (
                  SELECT 1 FROM sessions s
                  WHERE s.id = q.session_id AND s.cleanup_state = 'open'
              )
              AND NOT EXISTS (
                  SELECT 1 FROM sessions s
                  WHERE s.accepted_queue_entry_id = q.id
                    AND NOT EXISTS (
                        SELECT 1 FROM gc_checkpoint_pin_releases r
                        WHERE r.session_id = s.id
                          AND r.queue_entry_id = q.id
                    )
              )
              AND NOT EXISTS (
                  SELECT 1 FROM entry_path_exposures x
                  WHERE x.queue_entry_id = q.id AND x.state = 'outstanding'
              )
              AND NOT EXISTS (
                  SELECT 1 FROM advisories a
                  WHERE a.queue_entry_id = q.id AND a.resolution_state != 'resolved'
              )
              AND NOT EXISTS (
                  SELECT 1 FROM integration_reconciliation_intent_entries i
                  WHERE i.queue_entry_id = q.id
              )
            )
            SELECT 'merge_queue', q.id, q.updated_at,
                   length(q.head_commit) + length(q.base_commit)
                     + COALESCE(length(q.merged_tree), 0)
                     + COALESCE(length(q.details_json), 0) + 96
            FROM eligible_queue q
            UNION ALL
            SELECT 'advisory', a.id, q.updated_at,
                   length(a.identity) + length(a.paths_json) + length(a.evidence_json)
                     + COALESCE(length(a.resolution_evidence), 0) + 96
            FROM advisories a
            JOIN eligible_queue q ON q.id = a.queue_entry_id
            UNION ALL
            SELECT 'entry_exposure', x.id, q.updated_at,
                   length(x.promotion_sha) + length(x.paths_json)
                     + COALESCE(length(x.resolution_evidence), 0) + 96
            FROM entry_path_exposures x
            JOIN eligible_queue q ON q.id = x.queue_entry_id
            UNION ALL
            SELECT 'integration_reconciliation_entry', i.id, q.updated_at,
                   length(i.details_json) + length(i.old_merge_commit)
                     + COALESCE(length(i.upstream_landing), 0)
                     + COALESCE(length(i.replayed_commit), 0) + 64
            FROM integration_reconciliation_entries i
            JOIN eligible_queue q ON q.id = i.queue_entry_id
            ORDER BY 1, 2",
        )?;
        let rows = queue.query_map([queue_cutoff], |row| {
            let kind = match row.get_ref(0)?.as_str()? {
                "merge_queue" => GcRowKind::MergeQueue,
                "advisory" => GcRowKind::Advisory,
                "entry_exposure" => GcRowKind::EntryExposure,
                "integration_reconciliation_entry" => GcRowKind::IntegrationReconciliationEntry,
                value => {
                    return Err(rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        std::io::Error::other(format!("unknown GC row kind {value}")).into(),
                    ));
                }
            };
            Ok(GcRowCandidate {
                kind,
                id: row.get(1)?,
                recorded_at: row.get(2)?,
                estimated_bytes: row.get::<_, i64>(3)?.max(0) as u64,
                gate_log_path: None,
            })
        })?;
        candidates.extend(rows.collect::<Result<Vec<_>, _>>()?);
        candidates.sort_by_key(|row| (row.kind, row.id));
        Ok(candidates)
    }

    /// Closed sessions normally release this pin in their terminal
    /// transition. Rows returned here are legacy or crash-recovered pins that
    /// still need an explicit, reviewed GC plan before they can be released.
    pub fn gc_checkpoint_pin_candidates(&self) -> Result<Vec<GcCheckpointPinRelease>, BrokerError> {
        const REASON: &str = "accepted checkpoint pin remains after session close; releasing this broker pin does not remove committed work";
        let mut statement = self.conn.prepare(
            "SELECT s.id, s.accepted_queue_entry_id,
                    COALESCE(s.closed_at, s.updated_at),
                    COALESCE(
                        length(q.head_commit) + length(q.base_commit)
                          + COALESCE(length(q.merged_tree), 0)
                          + COALESCE(length(q.details_json), 0) + 96,
                        0
                    )
             FROM sessions s
             LEFT JOIN merge_queue q ON q.id = s.accepted_queue_entry_id
             WHERE s.status = 'cleaned'
               AND s.accepted_queue_entry_id IS NOT NULL
               AND NOT EXISTS (
                   SELECT 1 FROM gc_checkpoint_pin_releases r
                   WHERE r.session_id = s.id
                     AND r.queue_entry_id = s.accepted_queue_entry_id
               )
             ORDER BY COALESCE(s.closed_at, s.updated_at), s.id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(GcCheckpointPinRelease {
                session_id: row.get(0)?,
                queue_entry_id: row.get(1)?,
                recorded_at: row.get(2)?,
                estimated_bytes: row.get::<_, i64>(3)?.max(0) as u64,
                reason: REASON.into(),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Release one reviewed legacy checkpoint pin without touching the queue,
    /// session provenance, Git refs, or any committed worktree files.
    pub fn release_gc_checkpoint_pin(
        &mut self,
        candidate: &GcCheckpointPinRelease,
    ) -> Result<bool, BrokerError> {
        let now = now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO gc_checkpoint_pin_releases
                 (session_id, queue_entry_id, released_at)
             SELECT id, accepted_queue_entry_id, ?3
             FROM sessions
             WHERE id = ?1 AND status = 'cleaned'
               AND accepted_queue_entry_id = ?2",
            params![candidate.session_id, candidate.queue_entry_id, now],
        )?;
        if inserted == 0 {
            let already_released: bool = tx.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM gc_checkpoint_pin_releases
                     WHERE session_id = ?1 AND queue_entry_id = ?2
                 )",
                params![candidate.session_id, candidate.queue_entry_id],
                |row| row.get(0),
            )?;
            tx.commit()?;
            return Ok(already_released);
        }
        let payload = serde_json::json!({
            "session_id": candidate.session_id,
            "queue_entry_id": candidate.queue_entry_id,
            "committed_work_untouched": true,
            "reason": candidate.reason,
        })
        .to_string();
        insert_event(
            &tx,
            now,
            "gc.checkpoint_pin_released",
            Some(candidate.session_id),
            Some(&payload),
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub(crate) fn released_checkpoint_queue_entry(
        &self,
        session_id: i64,
    ) -> Result<Option<i64>, BrokerError> {
        self.conn
            .query_row(
                "SELECT queue_entry_id
                 FROM gc_checkpoint_pin_releases
                 WHERE session_id = ?1
                 ORDER BY released_at DESC, queue_entry_id DESC
                 LIMIT 1",
                [session_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    /// Delete one reviewed GC batch atomically. Child rows sort before their
    /// merge-queue parent; missing rows are accepted for crash-idempotent
    /// journal replay and primary keys are never reused.
    pub fn delete_gc_rows(&mut self, rows: &[crate::GcRowCandidate]) -> Result<usize, BrokerError> {
        let mut rows = rows.to_vec();
        rows.sort_by_key(|row| (row.kind, row.id));
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut removed = 0_usize;
        for row in rows {
            if row.kind == crate::GcRowKind::MergeQueue {
                tx.execute(
                    "UPDATE sessions
                     SET accepted_queue_entry_id = NULL
                     WHERE accepted_queue_entry_id = ?1
                       AND EXISTS (
                           SELECT 1 FROM gc_checkpoint_pin_releases r
                           WHERE r.session_id = sessions.id
                             AND r.queue_entry_id = ?1
                       )",
                    [row.id],
                )?;
            }
            if row.kind == crate::GcRowKind::GateResult {
                tx.execute(
                    "INSERT OR IGNORE INTO gate_result_gc_permits (gate_result_id) VALUES (?1)",
                    [row.id],
                )?;
            }
            let table = match row.kind {
                crate::GcRowKind::Event => "events",
                crate::GcRowKind::GateResult => "gate_results",
                crate::GcRowKind::Advisory => "advisories",
                crate::GcRowKind::EntryExposure => "entry_path_exposures",
                crate::GcRowKind::IntegrationReconciliationEntry => {
                    "integration_reconciliation_entries"
                }
                crate::GcRowKind::MergeQueue => "merge_queue",
            };
            removed += tx.execute(&format!("DELETE FROM {table} WHERE id = ?1"), [row.id])?;
            if row.kind == crate::GcRowKind::GateResult {
                tx.execute(
                    "DELETE FROM gate_result_gc_permits WHERE gate_result_id = ?1",
                    [row.id],
                )?;
            }
        }
        tx.commit()?;
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

    /// Events of one kind recorded at or after `since_ms`, newest first.
    /// Walks the `(kind, id)` index backwards, so it stays cheap on a large
    /// event table.
    pub(crate) fn recent_events_of_kind(
        &self,
        kind: &str,
        since_ms: i64,
        limit: i64,
    ) -> Result<Vec<Event>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, schema_version, ts, kind, session_id, payload_json
             FROM events
             WHERE kind = ?1 AND ts >= ?2
             ORDER BY id DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![kind, since_ms, limit], event_from_row)?;
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

    pub fn latest_session_graph_integrity_event(
        &self,
        session_id: i64,
    ) -> Result<Option<Event>, BrokerError> {
        self.conn
            .query_row(
                "SELECT id, schema_version, ts, kind, session_id, payload_json
                 FROM events
                 WHERE session_id = ?1 AND kind = ?2
                 ORDER BY id DESC LIMIT 1",
                params![session_id, crate::events::GRAPH_INTEGRITY_CHECKED],
                event_from_row,
            )
            .optional()
            .map_err(BrokerError::from)
    }

    /// Latest `session.holder_bound` event for one session: who holds it now.
    pub fn latest_session_holder_event(
        &self,
        session_id: i64,
    ) -> Result<Option<Event>, BrokerError> {
        self.conn
            .query_row(
                "SELECT id, schema_version, ts, kind, session_id, payload_json
                 FROM events
                 WHERE session_id = ?1 AND kind = ?2
                 ORDER BY id DESC LIMIT 1",
                params![session_id, crate::events::SESSION_HOLDER_BOUND],
                event_from_row,
            )
            .optional()
            .map_err(BrokerError::from)
    }

    /// Latest `session.holder_gone` event for one session.
    pub fn latest_session_holder_gone_event(
        &self,
        session_id: i64,
    ) -> Result<Option<Event>, BrokerError> {
        self.conn
            .query_row(
                "SELECT id, schema_version, ts, kind, session_id, payload_json
                 FROM events
                 WHERE session_id = ?1 AND kind = ?2
                 ORDER BY id DESC LIMIT 1",
                params![session_id, crate::events::SESSION_HOLDER_GONE],
                event_from_row,
            )
            .optional()
            .map_err(BrokerError::from)
    }

    /// Latest `session.install_recorded` event for one session: the build
    /// that was installed when the session started or was adopted.
    pub fn latest_session_install_event(
        &self,
        session_id: i64,
    ) -> Result<Option<Event>, BrokerError> {
        self.conn
            .query_row(
                "SELECT id, schema_version, ts, kind, session_id, payload_json
                 FROM events
                 WHERE session_id = ?1 AND kind = ?2
                 ORDER BY id DESC LIMIT 1",
                params![session_id, crate::events::SESSION_INSTALL_RECORDED],
                event_from_row,
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
