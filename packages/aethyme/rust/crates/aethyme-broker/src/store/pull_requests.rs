use super::*;

const ARTIFACT_SWEEP_NAMESPACE_REGISTRY_KEY: &str = "gc.artifact_sweep.v2.context_registry";
const ARTIFACT_SWEEP_NAMESPACE_REGISTRY_MAX: usize = 64;
const ARTIFACT_SWEEP_NAMESPACE_REGISTRY_MAX_BYTES: usize = 8 * 1024;

fn append_gc_id_filter(
    sql: &mut String,
    column: &str,
    ids: Option<&[i64]>,
    next_parameter: &mut usize,
    values: &mut Vec<rusqlite::types::Value>,
) {
    let Some(ids) = ids else {
        return;
    };
    if ids.is_empty() {
        sql.push_str(" AND 0");
        return;
    }

    sql.push_str(" AND ");
    sql.push_str(column);
    sql.push_str(" IN (");
    for (index, id) in ids.iter().enumerate() {
        if index > 0 {
            sql.push_str(", ");
        }
        sql.push('?');
        sql.push_str(&next_parameter.to_string());
        *next_parameter += 1;
        values.push(rusqlite::types::Value::Integer(*id));
    }
    sql.push(')');
}

fn gc_row_table(kind: crate::GcRowKind) -> (&'static str, &'static str) {
    match kind {
        crate::GcRowKind::Event => ("events", "event"),
        crate::GcRowKind::GateResult => ("gate_results", "gate_result"),
        crate::GcRowKind::Advisory => ("advisories", "advisory"),
        crate::GcRowKind::EntryExposure => ("entry_path_exposures", "entry_exposure"),
        crate::GcRowKind::IntegrationReconciliationEntry => (
            "integration_reconciliation_entries",
            "integration_reconciliation_entry",
        ),
        crate::GcRowKind::MergeQueue => ("merge_queue", "merge_queue"),
    }
}

fn update_gc_fingerprint_bytes(digest: &mut sha2::Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
}

fn gc_row_content_fingerprint(
    kind: crate::GcRowKind,
    column_names: &[String],
    row: &rusqlite::Row<'_>,
) -> Result<String, rusqlite::Error> {
    use rusqlite::types::ValueRef;
    use sha2::Digest;

    let mut digest = sha2::Sha256::new();
    digest.update(b"aethyme-gc-row/v1\0");
    update_gc_fingerprint_bytes(&mut digest, gc_row_table(kind).1.as_bytes());
    digest.update((column_names.len() as u64).to_be_bytes());
    for (index, name) in column_names.iter().enumerate() {
        update_gc_fingerprint_bytes(&mut digest, name.as_bytes());
        match row.get_ref(index)? {
            ValueRef::Null => digest.update([0]),
            ValueRef::Integer(value) => {
                digest.update([1]);
                digest.update(value.to_be_bytes());
            }
            ValueRef::Real(value) => {
                digest.update([2]);
                digest.update(value.to_bits().to_be_bytes());
            }
            ValueRef::Text(value) => {
                digest.update([3]);
                update_gc_fingerprint_bytes(&mut digest, value);
            }
            ValueRef::Blob(value) => {
                digest.update([4]);
                update_gc_fingerprint_bytes(&mut digest, value);
            }
        }
    }
    Ok(format!("sha256:{:x}", digest.finalize()))
}

fn attach_gc_row_content_fingerprints(
    conn: &rusqlite::Connection,
    candidates: &mut [crate::GcRowCandidate],
) -> Result<(), BrokerError> {
    candidates.sort_by_key(|candidate| (candidate.kind, candidate.id));
    let mut start = 0;
    while start < candidates.len() {
        let kind = candidates[start].kind;
        let mut end = start + 1;
        while end < candidates.len() && candidates[end].kind == kind {
            end += 1;
        }
        let kind_candidates = &mut candidates[start..end];
        start = end;
        if kind_candidates.is_empty() {
            continue;
        }

        let (table, _) = gc_row_table(kind);
        let placeholders = (1..=crate::error::GC_ROW_DELETE_BATCH_SIZE)
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut statement = conn.prepare(&format!(
            "SELECT * FROM {table} WHERE id IN ({placeholders}) ORDER BY id"
        ))?;
        let column_names = statement
            .column_names()
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let id_column = column_names
            .iter()
            .position(|name| name == "id")
            .ok_or_else(|| BrokerError::GcRowEligibilityChanged {
                kind: table.into(),
                id: kind_candidates[0].id,
            })?;
        for chunk in kind_candidates.chunks_mut(crate::error::GC_ROW_DELETE_BATCH_SIZE) {
            let mut parameters =
                vec![rusqlite::types::Value::Null; crate::error::GC_ROW_DELETE_BATCH_SIZE];
            for (index, candidate) in chunk.iter().enumerate() {
                parameters[index] = rusqlite::types::Value::Integer(candidate.id);
            }
            let mut rows = statement
                .query_map(rusqlite::params_from_iter(parameters.iter()), |row| {
                    let id = row.get::<_, i64>(id_column)?;
                    let fingerprint = gc_row_content_fingerprint(kind, &column_names, row)?;
                    Ok((id, fingerprint))
                })?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter();
            for candidate in chunk {
                candidate.content_fingerprint = Some(match rows.next() {
                    Some((id, fingerprint)) if id == candidate.id => fingerprint,
                    _ => {
                        return Err(BrokerError::GcRowEligibilityChanged {
                            kind: table.into(),
                            id: candidate.id,
                        });
                    }
                });
            }
            if let Some((id, _)) = rows.next() {
                return Err(BrokerError::GcRowEligibilityChanged {
                    kind: table.into(),
                    id,
                });
            }
        }
    }
    debug_assert_eq!(start, candidates.len());
    Ok(())
}

fn gc_event_blocker_needs(kinds: impl IntoIterator<Item = String>) -> [bool; 4] {
    let prefixes = ["advisory.", "exposure.", "operation.", "merge."];
    let mut needed = [false; 4];
    for kind in kinds {
        for (index, prefix) in prefixes.iter().enumerate() {
            needed[index] |= kind
                .get(..prefix.len())
                .is_some_and(|value| value.eq_ignore_ascii_case(prefix));
        }
    }
    needed
}

fn gc_event_blocker_needs_for_ids(
    conn: &rusqlite::Connection,
    event_ids: Option<&[i64]>,
) -> Result<[bool; 4], BrokerError> {
    let Some(event_ids) = event_ids else {
        return Ok([true; 4]);
    };
    if event_ids.is_empty() {
        return Ok([false; 4]);
    }

    let mut sql = String::from("SELECT kind FROM events WHERE 1 = 1");
    let mut values = Vec::new();
    let mut next_parameter = 1;
    append_gc_id_filter(
        &mut sql,
        "id",
        Some(event_ids),
        &mut next_parameter,
        &mut values,
    );
    let mut statement = conn.prepare(&sql)?;
    let kinds = statement
        .query_map(rusqlite::params_from_iter(values.iter()), |row| {
            row.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(gc_event_blocker_needs(kinds))
}

#[cfg(test)]
mod gc_event_blocker_needs_tests {
    use super::gc_event_blocker_needs_for_ids;

    #[test]
    fn bounded_batches_probe_only_blockers_for_current_event_prefixes() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE events (id INTEGER PRIMARY KEY, kind TEXT NOT NULL);
             INSERT INTO events (id, kind) VALUES
                 (1, 'gate.pass'),
                 (2, 'Advisory.fixture'),
                 (3, 'exposure.fixture'),
                 (4, 'operation.fixture'),
                 (5, 'merge.fixture');",
        )
        .unwrap();

        assert_eq!(
            gc_event_blocker_needs_for_ids(&conn, Some(&[1])).unwrap(),
            [false; 4]
        );
        assert_eq!(
            gc_event_blocker_needs_for_ids(&conn, Some(&[])).unwrap(),
            [false; 4]
        );
        assert_eq!(
            gc_event_blocker_needs_for_ids(&conn, Some(&[2, 5])).unwrap(),
            [true, false, false, true]
        );
        assert_eq!(
            gc_event_blocker_needs_for_ids(&conn, Some(&[2, 3, 4, 5])).unwrap(),
            [true; 4]
        );
        assert_eq!(
            gc_event_blocker_needs_for_ids(&conn, Some(&[999])).unwrap(),
            [false; 4]
        );
        assert_eq!(
            gc_event_blocker_needs_for_ids(&conn, None).unwrap(),
            [true; 4]
        );
    }
}

impl BrokerStore {
    /// Atomically register a bounded artifact-sweep namespace. IDs remain in
    /// the registry once created so another context's durable cursor or
    /// interrupted-path queue is never evicted to make room for a new one.
    pub(crate) fn register_artifact_sweep_namespace(
        &mut self,
        namespace: &str,
    ) -> Result<crate::broker::ArtifactSweepNamespaceRegistration, BrokerError> {
        let valid_id = namespace.len() == 64
            && namespace
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if !valid_id {
            return Ok(crate::broker::ArtifactSweepNamespaceRegistration::InvalidRegistry);
        }

        let transaction = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored = transaction
            .query_row(
                "SELECT typeof(value), length(CAST(value AS BLOB)),
                        CASE WHEN typeof(value) = 'text'
                                  AND length(CAST(value AS BLOB)) <= ?2
                             THEN CAST(value AS BLOB)
                             ELSE NULL
                        END
                 FROM meta WHERE key = ?1",
                params![
                    ARTIFACT_SWEEP_NAMESPACE_REGISTRY_KEY,
                    ARTIFACT_SWEEP_NAMESPACE_REGISTRY_MAX_BYTES as i64,
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, Option<Vec<u8>>>(2)?,
                    ))
                },
            )
            .optional()?;
        let mut namespaces = match stored {
            None => Vec::new(),
            Some((kind, Some(length), Some(bytes)))
                if kind == "text"
                    && length >= 0
                    && length as usize <= ARTIFACT_SWEEP_NAMESPACE_REGISTRY_MAX_BYTES =>
            {
                let Ok(value) = std::str::from_utf8(&bytes) else {
                    return Ok(crate::broker::ArtifactSweepNamespaceRegistration::InvalidRegistry);
                };
                match serde_json::from_str::<Vec<String>>(value) {
                    Ok(namespaces) => namespaces,
                    Err(_) => {
                        return Ok(
                            crate::broker::ArtifactSweepNamespaceRegistration::InvalidRegistry,
                        );
                    }
                }
            }
            Some(_) => {
                return Ok(crate::broker::ArtifactSweepNamespaceRegistration::InvalidRegistry);
            }
        };
        if namespaces.len() > ARTIFACT_SWEEP_NAMESPACE_REGISTRY_MAX
            || namespaces.iter().any(|value| {
                value.len() != 64
                    || !value
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            || namespaces.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Ok(crate::broker::ArtifactSweepNamespaceRegistration::InvalidRegistry);
        }
        if namespaces
            .binary_search_by(|value| value.as_str().cmp(namespace))
            .is_ok()
        {
            return Ok(crate::broker::ArtifactSweepNamespaceRegistration::Registered);
        }
        if namespaces.len() == ARTIFACT_SWEEP_NAMESPACE_REGISTRY_MAX {
            return Ok(crate::broker::ArtifactSweepNamespaceRegistration::AtCapacity);
        }
        namespaces.push(namespace.to_string());
        namespaces.sort_unstable();
        let encoded =
            serde_json::to_string(&namespaces).map_err(|error| BrokerError::InvalidEnumValue {
                field: "artifact_sweep_namespace_registry",
                value: error.to_string(),
            })?;
        if encoded.len() > ARTIFACT_SWEEP_NAMESPACE_REGISTRY_MAX_BYTES {
            return Ok(crate::broker::ArtifactSweepNamespaceRegistration::InvalidRegistry);
        }
        transaction.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![ARTIFACT_SWEEP_NAMESPACE_REGISTRY_KEY, encoded],
        )?;
        transaction.commit()?;
        Ok(crate::broker::ArtifactSweepNamespaceRegistration::Registered)
    }

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
                &format!(
                    "SELECT o.id
                     FROM delivery_outbox o
                     JOIN delivery_subscriptions s ON s.id = o.subscription_id
                     WHERE s.adapter = ?1 AND s.active = 1 AND {}
                     ORDER BY o.id LIMIT 1",
                    crate::outbox::CLAIMABLE_PREDICATE_SQL
                ),
                params![adapter, now],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let Some(id) = id else {
            tx.commit()?;
            return Ok(None);
        };
        let claim_update =
            crate::outbox::claim_update_statement(crate::outbox::OutboxTable::PullRequest);
        tx.execute(
            &claim_update,
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
        if !crate::outbox::claim_is_current(
            current.status,
            current.claimed_by.as_deref(),
            current.generation,
            current.claim_expires_at,
            worker,
            generation,
            now,
        ) {
            return Err(BrokerError::DeliveryClaimChanged {
                id,
                worker: worker.into(),
                generation,
            });
        }
        let (status, delivered_at) =
            crate::outbox::completion_state(completion, current.attempt_count, now);
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let completion_update =
            crate::outbox::completion_update_statement(crate::outbox::OutboxTable::PullRequest);
        let updated = tx.execute(
            &completion_update,
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

    /// Compare the current durable value and replace it atomically. A missing
    /// expected value inserts only when the key is still absent.
    pub fn meta_compare_and_set(
        &self,
        key: &str,
        expected: Option<&str>,
        value: &str,
    ) -> Result<bool, BrokerError> {
        let changed = match expected {
            Some(expected) => self.conn.execute(
                "UPDATE meta SET value = ?1 WHERE key = ?2 AND value = ?3",
                rusqlite::params![value, key, expected],
            )?,
            None => self.conn.execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO NOTHING",
                rusqlite::params![key, value],
            )?,
        };
        Ok(changed == 1)
    }

    /// Compare and update the bounded retirement cursor without waiting for a
    /// competing SQLite writer. A busy result is a deferral; restore the
    /// connection's normal timeout on both success and error.
    pub(crate) fn meta_compare_and_set_for_session_retirement(
        &mut self,
        key: &str,
        expected: Option<&str>,
        value: &str,
    ) -> Result<bool, BrokerError> {
        self.conn.busy_timeout(std::time::Duration::ZERO)?;
        let result = self.meta_compare_and_set(key, expected, value);
        let result = match result {
            Err(BrokerError::Sqlite(error))
                if matches!(
                    error.sqlite_error_code(),
                    Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
                ) =>
            {
                Err(BrokerError::SessionRetirementDatabaseBusy)
            }
            result => result,
        };
        self.conn
            .busy_timeout(std::time::Duration::from_millis(super::BUSY_TIMEOUT_MS))?;
        result
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

    /// Lease release request events, oldest first.
    ///
    /// This query is limited to the four event kinds interpreted by the
    /// release-request consumer. Keep `events_after_filtered` for callers that
    /// need arbitrary event-kind prefixes.
    pub(crate) fn lease_release_events(&self) -> Result<Vec<Event>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, schema_version, ts, kind, session_id, payload_json
             FROM events
             WHERE kind IN (?1, ?2, ?3, ?4)
             ORDER BY id",
        )?;
        let rows = stmt.query_map(
            params![
                crate::events::LEASE_RELEASE_REQUESTED,
                crate::events::LEASE_RELEASE_ACKED,
                crate::events::LEASE_RELEASE_DECLINED,
                crate::events::LEASE_RELEASE_GRANTED,
            ],
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
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Lease releases by one holder in a request's event window.
    /// Callers scope these reads to involved sessions and request ids, avoiding
    /// a repository-wide scan of release history on each status read.
    pub(crate) fn lease_release_events_for_holder_since(
        &self,
        holder_session_id: i64,
        after_event_id: i64,
        before: Option<(i64, i64)>,
    ) -> Result<Vec<Event>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, schema_version, ts, kind, session_id, payload_json
             FROM events
             WHERE kind = ?1 AND session_id = ?2 AND id > ?3
               AND (?4 IS NULL OR id < ?4 OR ts <= ?5)
             ORDER BY id",
        )?;
        let before_event_id = before.map(|(event_id, _)| event_id);
        let before_ts = before.map(|(_, ts)| ts);
        let rows = stmt.query_map(
            params![
                crate::events::LEASE_RELEASED,
                holder_session_id,
                after_event_id,
                before_event_id,
                before_ts
            ],
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
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Atomically release the holder's currently active overlapping leases
    /// and record an acknowledgement or stale-holder grant. Rechecking the
    /// request and requester inside the write transaction orders this against
    /// requester closure and competing outcomes.
    pub(crate) fn resolve_lease_release_request(
        &mut self,
        request_id: i64,
        holder_session_id: i64,
        outcome_kind: &str,
        outcome_reason: Option<&str>,
    ) -> Result<bool, BrokerError> {
        if !matches!(
            outcome_kind,
            crate::events::LEASE_RELEASE_ACKED | crate::events::LEASE_RELEASE_GRANTED
        ) {
            return Ok(false);
        }

        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let request = tx
            .query_row(
                "SELECT ts, session_id, payload_json FROM events
                 WHERE id = ?1 AND kind = ?2",
                params![request_id, crate::events::LEASE_RELEASE_REQUESTED],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((requested_at, requester_event_session_id, Some(payload))) = request else {
            return Ok(false);
        };
        let Ok(payload) = serde_json::from_str::<serde_json::Value>(&payload) else {
            return Ok(false);
        };
        let Some(path) = payload.get("path").and_then(serde_json::Value::as_str) else {
            return Ok(false);
        };
        let Some(requester_session_id) = payload
            .get("requester_session_id")
            .and_then(serde_json::Value::as_i64)
        else {
            return Ok(false);
        };
        let Some(requested_holder_id) = payload
            .get("holder_session_id")
            .and_then(serde_json::Value::as_i64)
        else {
            return Ok(false);
        };
        if requester_event_session_id != Some(requester_session_id)
            || requested_holder_id != holder_session_id
        {
            return Ok(false);
        }

        let has_outcome = {
            let mut stmt = tx.prepare(
                "SELECT payload_json FROM events
                 WHERE id > ?1 AND kind IN (?2, ?3, ?4)
                 ORDER BY id",
            )?;
            let rows = stmt.query_map(
                params![
                    request_id,
                    crate::events::LEASE_RELEASE_ACKED,
                    crate::events::LEASE_RELEASE_DECLINED,
                    crate::events::LEASE_RELEASE_GRANTED,
                ],
                |row| row.get::<_, Option<String>>(0),
            )?;
            let mut found = false;
            for row in rows {
                if row?
                    .and_then(|payload| serde_json::from_str::<serde_json::Value>(&payload).ok())
                    .and_then(|payload| payload.get("request_id").and_then(|id| id.as_i64()))
                    == Some(request_id)
                {
                    found = true;
                    break;
                }
            }
            found
        };
        if has_outcome {
            return Ok(false);
        }

        let requester_state = tx
            .query_row(
                "SELECT status, closed_at, updated_at FROM sessions WHERE id = ?1",
                [requester_session_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((status, closed_at, updated_at)) = requester_state else {
            return Ok(false);
        };
        if matches!(status.as_str(), "closed" | "cleaned")
            && closed_at.unwrap_or(updated_at) >= requested_at
        {
            return Ok(false);
        }

        let now = now_ms();
        let active_paths = {
            let mut stmt = tx.prepare(
                "SELECT l.path FROM leases l
                 JOIN sessions s ON s.id = l.session_id
                 WHERE l.session_id = ?1 AND l.released_at IS NULL
                   AND (l.expires_at IS NULL OR l.expires_at > ?2)
                   AND s.status IN ('active', 'idle', 'stale')
                 ORDER BY l.id",
            )?;
            let rows = stmt.query_map(params![holder_session_id, now], |row| {
                row.get::<_, String>(0)
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let active_paths = active_paths
            .into_iter()
            .filter(|lease_path| crate::leases::paths_overlap(path, lease_path))
            .collect::<std::collections::BTreeSet<_>>();
        if active_paths.is_empty() {
            return Ok(false);
        }

        let release_reason = if outcome_kind == crate::events::LEASE_RELEASE_GRANTED {
            "request_granted"
        } else {
            "request_acked"
        };
        for lease_path in active_paths {
            record_lease_releases_in_tx(
                &tx,
                holder_session_id,
                Some(&lease_path),
                None,
                now,
                release_reason,
                false,
            )?;
            tx.execute(
                "UPDATE leases SET released_at = ?3
                 WHERE session_id = ?1 AND path = ?2 AND released_at IS NULL
                   AND (expires_at IS NULL OR expires_at > ?4)",
                params![holder_session_id, lease_path, now, now],
            )?;
        }
        let outcome_payload =
            crate::lease_requests::outcome_payload(request_id, path, outcome_reason);
        insert_event(
            &tx,
            now,
            outcome_kind,
            Some(holder_session_id),
            Some(&outcome_payload),
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Session terminal events after a release request. These resolve
    /// requests from the requester side in event-id order.
    pub(crate) fn lease_request_closure_events_for_session_since(
        &self,
        session_id: i64,
        after_event_id: i64,
    ) -> Result<Vec<Event>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, schema_version, ts, kind, session_id, payload_json
             FROM events
             WHERE session_id = ?1 AND id > ?2 AND kind IN (?3, ?4, ?5, ?6)
             ORDER BY id",
        )?;
        let rows = stmt.query_map(
            params![
                session_id,
                after_event_id,
                "session.closed",
                "session.cleaned",
                "session.abandoned",
                crate::events::SESSION_FINISHED
            ],
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
        )?;
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
        Self::gc_row_candidates_on(
            &self.conn,
            event_cutoff,
            gate_cutoff,
            queue_cutoff,
            None,
            true,
        )
    }

    /// The row inventory used by health summaries. Health reports counts and
    /// estimates but cannot authorize deletion, so it does not read every
    /// candidate row back in full merely to compute apply-time fingerprints.
    pub(crate) fn gc_row_candidates_for_health(
        &self,
        event_cutoff: i64,
        gate_cutoff: i64,
        queue_cutoff: i64,
    ) -> Result<Vec<crate::GcRowCandidate>, BrokerError> {
        Self::gc_row_candidates_on(
            &self.conn,
            event_cutoff,
            gate_cutoff,
            queue_cutoff,
            None,
            false,
        )
    }

    fn gc_row_candidates_on(
        conn: &rusqlite::Connection,
        event_cutoff: i64,
        gate_cutoff: i64,
        queue_cutoff: i64,
        only: Option<&[crate::GcRowCandidate]>,
        attach_fingerprints: bool,
    ) -> Result<Vec<crate::GcRowCandidate>, BrokerError> {
        use crate::{GcRowCandidate, GcRowKind};

        let event_ids = only.map(|rows| {
            rows.iter()
                .filter(|row| row.kind == GcRowKind::Event)
                .map(|row| row.id)
                .collect::<Vec<_>>()
        });
        let gate_ids = only.map(|rows| {
            rows.iter()
                .filter(|row| row.kind == GcRowKind::GateResult)
                .map(|row| row.id)
                .collect::<Vec<_>>()
        });
        let queue_ids = only.map(|rows| {
            rows.iter()
                .filter(|row| row.kind == GcRowKind::MergeQueue)
                .map(|row| row.id)
                .collect::<Vec<_>>()
        });
        let advisory_ids = only.map(|rows| {
            rows.iter()
                .filter(|row| row.kind == GcRowKind::Advisory)
                .map(|row| row.id)
                .collect::<Vec<_>>()
        });
        let exposure_ids = only.map(|rows| {
            rows.iter()
                .filter(|row| row.kind == GcRowKind::EntryExposure)
                .map(|row| row.id)
                .collect::<Vec<_>>()
        });
        let reconciliation_ids = only.map(|rows| {
            rows.iter()
                .filter(|row| row.kind == GcRowKind::IntegrationReconciliationEntry)
                .map(|row| row.id)
                .collect::<Vec<_>>()
        });

        let mut candidates = Vec::new();
        let blocker_needs = gc_event_blocker_needs_for_ids(conn, event_ids.as_deref())?;
        let unresolved_advisories = blocker_needs[0]
            && conn.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM advisories WHERE resolution_state != 'resolved'
                 )",
                [],
                |row| row.get::<_, bool>(0),
            )?;
        let outstanding_exposures = blocker_needs[1]
            && conn.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM entry_path_exposures WHERE state = 'outstanding'
                 )",
                [],
                |row| row.get::<_, bool>(0),
            )?;
        let unresolved_operations = blocker_needs[2]
            && conn.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM coordinated_operations
                     WHERE status IN ('prepared', 'running', 'outcome_unknown')
                 )",
                [],
                |row| row.get::<_, bool>(0),
            )?;
        let unresolved_queue = blocker_needs[3]
            && conn.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM merge_queue
                     WHERE status IN ('submitted', 'simulating', 'conflict', 'verified')
                 )",
                [],
                |row| row.get::<_, bool>(0),
            )?;

        let mut event_sql = String::from(
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
               AND (?5 = 0 OR e.kind NOT LIKE 'merge.%')",
        );
        let mut event_parameters = vec![
            rusqlite::types::Value::Integer(event_cutoff),
            rusqlite::types::Value::Integer(i64::from(u8::from(unresolved_advisories))),
            rusqlite::types::Value::Integer(i64::from(u8::from(outstanding_exposures))),
            rusqlite::types::Value::Integer(i64::from(u8::from(unresolved_operations))),
            rusqlite::types::Value::Integer(i64::from(u8::from(unresolved_queue))),
        ];
        let mut next_parameter = 6;
        append_gc_id_filter(
            &mut event_sql,
            "e.id",
            event_ids.as_deref(),
            &mut next_parameter,
            &mut event_parameters,
        );
        event_sql.push_str(" ORDER BY e.id");
        let mut events = conn.prepare(&event_sql)?;
        let rows =
            events.query_map(rusqlite::params_from_iter(event_parameters.iter()), |row| {
                Ok(GcRowCandidate {
                    kind: GcRowKind::Event,
                    id: row.get(0)?,
                    recorded_at: row.get(1)?,
                    estimated_bytes: row.get::<_, i64>(2)?.max(0) as u64,
                    gate_log_path: None,
                    content_fingerprint: None,
                })
            })?;
        candidates.extend(rows.collect::<Result<Vec<_>, _>>()?);

        let mut gate_sql = String::from(
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
               )",
        );
        let mut gate_parameters = vec![rusqlite::types::Value::Integer(gate_cutoff)];
        let mut next_parameter = 2;
        append_gc_id_filter(
            &mut gate_sql,
            "r.id",
            gate_ids.as_deref(),
            &mut next_parameter,
            &mut gate_parameters,
        );
        gate_sql.push_str(" ORDER BY r.id");
        let mut gates = conn.prepare(&gate_sql)?;
        let rows = gates.query_map(rusqlite::params_from_iter(gate_parameters.iter()), |row| {
            Ok(GcRowCandidate {
                kind: GcRowKind::GateResult,
                id: row.get(0)?,
                recorded_at: row.get(1)?,
                estimated_bytes: row.get::<_, i64>(2)?.max(0) as u64,
                gate_log_path: row.get(3)?,
                content_fingerprint: None,
            })
        })?;
        candidates.extend(rows.collect::<Result<Vec<_>, _>>()?);

        let eligible_queue_ids = if only.is_some() {
            let mut query = String::from(
                "SELECT queue_entry_id FROM advisories WHERE 1 = 1
                 UNION
                 SELECT queue_entry_id FROM entry_path_exposures WHERE 1 = 1
                 UNION
                 SELECT queue_entry_id FROM integration_reconciliation_entries WHERE 1 = 1",
            );
            let mut values = Vec::new();
            let mut next_parameter = 1;
            let mut filter = String::new();
            append_gc_id_filter(
                &mut filter,
                "id",
                advisory_ids.as_deref(),
                &mut next_parameter,
                &mut values,
            );
            let anchor = "SELECT queue_entry_id FROM advisories WHERE 1 = 1";
            let position = query.find(anchor).expect("advisory parent lookup") + anchor.len();
            query.insert_str(position, &filter);

            filter.clear();
            append_gc_id_filter(
                &mut filter,
                "id",
                exposure_ids.as_deref(),
                &mut next_parameter,
                &mut values,
            );
            let anchor = "SELECT queue_entry_id FROM entry_path_exposures WHERE 1 = 1";
            let position = query.find(anchor).expect("exposure parent lookup") + anchor.len();
            query.insert_str(position, &filter);

            filter.clear();
            append_gc_id_filter(
                &mut filter,
                "id",
                reconciliation_ids.as_deref(),
                &mut next_parameter,
                &mut values,
            );
            let anchor =
                "SELECT queue_entry_id FROM integration_reconciliation_entries WHERE 1 = 1";
            let position = query.find(anchor).expect("reconciliation parent lookup") + anchor.len();
            query.insert_str(position, &filter);

            let mut statement = conn.prepare(&query)?;
            let ids = statement
                .query_map(rusqlite::params_from_iter(values.iter()), |row| {
                    row.get::<_, i64>(0)
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let mut ids = ids;
            ids.extend(queue_ids.as_deref().unwrap_or_default().iter().copied());
            ids.sort_unstable();
            ids.dedup();
            Some(ids)
        } else {
            None
        };

        let mut queue_sql = String::from(
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
            WHERE 1 = 1
            UNION ALL
            SELECT 'advisory', a.id, q.updated_at,
                   length(a.identity) + length(a.paths_json) + length(a.evidence_json)
                     + COALESCE(length(a.resolution_evidence), 0) + 96
            FROM advisories a
            JOIN eligible_queue q ON q.id = a.queue_entry_id
            WHERE 1 = 1
            UNION ALL
            SELECT 'entry_exposure', x.id, q.updated_at,
                   length(x.promotion_sha) + length(x.paths_json)
                     + COALESCE(length(x.resolution_evidence), 0) + 96
            FROM entry_path_exposures x
            JOIN eligible_queue q ON q.id = x.queue_entry_id
            WHERE 1 = 1
            UNION ALL
            SELECT 'integration_reconciliation_entry', i.id, q.updated_at,
                   length(i.details_json) + length(i.old_merge_commit)
                     + COALESCE(length(i.upstream_landing), 0)
                     + COALESCE(length(i.replayed_commit), 0) + 64
            FROM integration_reconciliation_entries i
            JOIN eligible_queue q ON q.id = i.queue_entry_id
            WHERE 1 = 1",
        );
        let mut queue_parameters = vec![rusqlite::types::Value::Integer(queue_cutoff)];
        let mut next_parameter = 2;
        let mut filter = String::new();
        append_gc_id_filter(
            &mut filter,
            "q.id",
            eligible_queue_ids.as_deref(),
            &mut next_parameter,
            &mut queue_parameters,
        );
        let anchor = "WHERE q.updated_at < ?1";
        let position = queue_sql.find(anchor).expect("eligible queue id anchor") + anchor.len();
        queue_sql.insert_str(position, &filter);

        filter.clear();
        append_gc_id_filter(
            &mut filter,
            "q.id",
            queue_ids.as_deref(),
            &mut next_parameter,
            &mut queue_parameters,
        );
        let anchor = "FROM eligible_queue q\n            WHERE 1 = 1";
        let position = queue_sql.find(anchor).expect("queue parent filter anchor") + anchor.len();
        queue_sql.insert_str(position, &filter);

        filter.clear();
        append_gc_id_filter(
            &mut filter,
            "a.id",
            advisory_ids.as_deref(),
            &mut next_parameter,
            &mut queue_parameters,
        );
        let anchor = "FROM advisories a\n            JOIN eligible_queue q ON q.id = a.queue_entry_id\n            WHERE 1 = 1";
        let position = queue_sql.find(anchor).expect("advisory filter anchor") + anchor.len();
        queue_sql.insert_str(position, &filter);

        filter.clear();
        append_gc_id_filter(
            &mut filter,
            "x.id",
            exposure_ids.as_deref(),
            &mut next_parameter,
            &mut queue_parameters,
        );
        let anchor = "FROM entry_path_exposures x\n            JOIN eligible_queue q ON q.id = x.queue_entry_id\n            WHERE 1 = 1";
        let position = queue_sql.find(anchor).expect("exposure filter anchor") + anchor.len();
        queue_sql.insert_str(position, &filter);

        filter.clear();
        append_gc_id_filter(
            &mut filter,
            "i.id",
            reconciliation_ids.as_deref(),
            &mut next_parameter,
            &mut queue_parameters,
        );
        let anchor = "FROM integration_reconciliation_entries i\n            JOIN eligible_queue q ON q.id = i.queue_entry_id\n            WHERE 1 = 1";
        let position = queue_sql
            .find(anchor)
            .expect("reconciliation filter anchor")
            + anchor.len();
        queue_sql.insert_str(position, &filter);

        queue_sql.push_str(" ORDER BY 1, 2");
        let mut queue = conn.prepare(&queue_sql)?;
        let rows = queue.query_map(rusqlite::params_from_iter(queue_parameters.iter()), |row| {
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
                content_fingerprint: None,
            })
        })?;
        candidates.extend(rows.collect::<Result<Vec<_>, _>>()?);
        if attach_fingerprints {
            attach_gc_row_content_fingerprints(conn, &mut candidates)?;
        }
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

    /// Delete one reviewed GC batch atomically after rechecking its current
    /// retention eligibility and complete candidate row contents in the same
    /// transaction. Missing rows are accepted for crash-idempotent journal
    /// replay; a present row without a matching content fingerprint fails closed.
    pub fn delete_gc_rows(
        &mut self,
        rows: &[crate::GcRowCandidate],
        event_cutoff: i64,
        gate_cutoff: i64,
        queue_cutoff: i64,
    ) -> Result<usize, BrokerError> {
        if rows.len() > crate::error::GC_ROW_DELETE_BATCH_SIZE {
            return Err(BrokerError::GcRowBatchTooLarge {
                requested: rows.len(),
                max: crate::error::GC_ROW_DELETE_BATCH_SIZE,
            });
        }
        let mut rows = rows.to_vec();
        rows.sort_by_key(|row| (row.kind, row.id));
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        let eligible = Self::gc_row_candidates_on(
            &tx,
            event_cutoff,
            gate_cutoff,
            queue_cutoff,
            Some(&rows),
            true,
        )?;
        let eligible_by_id = eligible
            .into_iter()
            .map(|candidate| ((candidate.kind, candidate.id), candidate))
            .collect::<BTreeMap<_, _>>();

        // Validate the complete batch before the first delete or checkpoint
        // pin release. A changed protection predicate must leave every row
        // untouched so the journal can be reviewed again.
        for row in &rows {
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
            match eligible_by_id.get(&(row.kind, row.id)) {
                Some(current) if current == row => {}
                Some(_) => {
                    return Err(BrokerError::GcRowEligibilityChanged {
                        kind: table.into(),
                        id: row.id,
                    });
                }
                None => {
                    let exists: bool = tx.query_row(
                        &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE id = ?1)"),
                        [row.id],
                        |record| record.get(0),
                    )?;
                    if exists {
                        return Err(BrokerError::GcRowEligibilityChanged {
                            kind: table.into(),
                            id: row.id,
                        });
                    }
                }
            }
        }

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

#[cfg(test)]
mod artifact_sweep_namespace_tests {
    use super::*;
    use crate::broker::ArtifactSweepNamespaceRegistration;
    use std::sync::{Arc, Barrier};

    fn namespace(value: u64) -> String {
        format!("{value:064x}")
    }

    #[test]
    fn registry_cap_is_atomic_and_existing_namespaces_are_never_evicted() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("broker.db");
        drop(BrokerStore::open(&database).unwrap());

        let retained = namespace(0);
        {
            let mut store = BrokerStore::open(&database).unwrap();
            for value in 0..ARTIFACT_SWEEP_NAMESPACE_REGISTRY_MAX - 1 {
                assert_eq!(
                    store
                        .register_artifact_sweep_namespace(&namespace(value as u64))
                        .unwrap(),
                    ArtifactSweepNamespaceRegistration::Registered
                );
            }
            store
                .meta_set(
                    &format!("gc.artifact_sweep.v2.{retained}.cursor_session_id"),
                    "17",
                )
                .unwrap();
        }

        let barrier = Arc::new(Barrier::new(3));
        let contenders = [namespace(1_000), namespace(1_001)];
        let handles = contenders.map(|candidate| {
            let database = database.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                let mut store = BrokerStore::open(&database).unwrap();
                store.register_artifact_sweep_namespace(&candidate).unwrap()
            })
        });
        barrier.wait();
        let outcomes = handles.map(|handle| handle.join().unwrap());
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == ArtifactSweepNamespaceRegistration::Registered)
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == ArtifactSweepNamespaceRegistration::AtCapacity)
                .count(),
            1
        );

        let mut store = BrokerStore::open(&database).unwrap();
        let registry: Vec<String> = serde_json::from_str(
            &store
                .meta_get(ARTIFACT_SWEEP_NAMESPACE_REGISTRY_KEY)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(registry.len(), ARTIFACT_SWEEP_NAMESPACE_REGISTRY_MAX);
        assert!(registry.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(
            store.register_artifact_sweep_namespace(&retained).unwrap(),
            ArtifactSweepNamespaceRegistration::Registered
        );
        assert_eq!(
            store
                .meta_get(&format!(
                    "gc.artifact_sweep.v2.{retained}.cursor_session_id"
                ))
                .unwrap()
                .as_deref(),
            Some("17")
        );
    }

    #[test]
    fn meta_compare_and_set_rejects_a_second_handle_with_a_stale_value() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("broker.db");
        drop(BrokerStore::open(&database).unwrap());
        let first = BrokerStore::open(&database).unwrap();
        let second = BrokerStore::open(&database).unwrap();
        let key = "auto_cleanup.cursor";

        let first_read = first.meta_get(key).unwrap();
        let second_read = second.meta_get(key).unwrap();
        assert_eq!(first_read, None);
        assert_eq!(second_read, first_read);

        assert!(
            first
                .meta_compare_and_set(key, first_read.as_deref(), "first")
                .unwrap()
        );
        assert!(
            !second
                .meta_compare_and_set(key, second_read.as_deref(), "stale-second")
                .unwrap()
        );
        assert_eq!(first.meta_get(key).unwrap().as_deref(), Some("first"));

        assert!(
            second
                .meta_compare_and_set(key, Some("first"), "second")
                .unwrap()
        );
        assert!(
            !first
                .meta_compare_and_set(key, Some("first"), "stale-third")
                .unwrap()
        );
        assert_eq!(second.meta_get(key).unwrap().as_deref(), Some("second"));
    }

    #[test]
    fn malformed_or_oversized_registry_is_preserved_and_refuses_registration() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("broker.db");
        let mut store = BrokerStore::open(&database).unwrap();

        for invalid in [
            "not-json".to_string(),
            "x".repeat(ARTIFACT_SWEEP_NAMESPACE_REGISTRY_MAX_BYTES + 1),
            serde_json::json!([namespace(2), namespace(1)]).to_string(),
            serde_json::json!([namespace(1), namespace(1)]).to_string(),
            serde_json::json!(["A".repeat(64)]).to_string(),
            serde_json::json!(["too-short"]).to_string(),
        ] {
            store
                .meta_set(ARTIFACT_SWEEP_NAMESPACE_REGISTRY_KEY, &invalid)
                .unwrap();
            assert_eq!(
                store
                    .register_artifact_sweep_namespace(&namespace(3))
                    .unwrap(),
                ArtifactSweepNamespaceRegistration::InvalidRegistry
            );
            assert_eq!(
                store
                    .meta_get(ARTIFACT_SWEEP_NAMESPACE_REGISTRY_KEY)
                    .unwrap()
                    .as_deref(),
                Some(invalid.as_str())
            );
        }
        let valid_registry = serde_json::json!([namespace(3)]).to_string();
        store
            .meta_set(ARTIFACT_SWEEP_NAMESPACE_REGISTRY_KEY, &valid_registry)
            .unwrap();
        assert_eq!(
            store
                .register_artifact_sweep_namespace("invalid-namespace")
                .unwrap(),
            ArtifactSweepNamespaceRegistration::InvalidRegistry
        );
        assert_eq!(
            store
                .meta_get(ARTIFACT_SWEEP_NAMESPACE_REGISTRY_KEY)
                .unwrap()
                .as_deref(),
            Some(valid_registry.as_str())
        );

        let blob = vec![0xff; 16];
        store
            .conn
            .execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![
                    ARTIFACT_SWEEP_NAMESPACE_REGISTRY_KEY,
                    rusqlite::types::Value::Blob(blob.clone()),
                ],
            )
            .unwrap();
        assert_eq!(
            store
                .register_artifact_sweep_namespace(&namespace(3))
                .unwrap(),
            ArtifactSweepNamespaceRegistration::InvalidRegistry
        );
        let value_type: String = store
            .conn
            .query_row(
                "SELECT typeof(value) FROM meta WHERE key = ?1",
                [ARTIFACT_SWEEP_NAMESPACE_REGISTRY_KEY],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(value_type, "blob");
        let raw_value: rusqlite::types::Value = store
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                [ARTIFACT_SWEEP_NAMESPACE_REGISTRY_KEY],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(raw_value, rusqlite::types::Value::Blob(blob));
    }
}
