use super::*;

impl BrokerStore {
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
                    host_operation_id, identity_provenance, agent_provenance_json
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

    /// The host operation is only known once the lock is held, but the record is
    /// created before queueing, so the link is attached rather than inserted.
    pub fn attach_host_operation(
        &mut self,
        id: i64,
        host_operation_id: &str,
    ) -> Result<(), BrokerError> {
        self.conn.execute(
            "UPDATE coordinated_operations SET host_operation_id = ?2, updated_at = ?3
             WHERE id = ?1",
            params![id, host_operation_id, now_ms()],
        )?;
        Ok(())
    }

    /// Unresolved write operations across every repository, oldest first.
    ///
    /// `unresolved_coordinated_operations` answers "what holds this one
    /// repository's lock", which is the question the lock itself asks. Status
    /// needs the other question -- "is anything queued anywhere" -- because a
    /// blocked caller cannot report on itself: it is parked inside a command
    /// that never returns, and the one wait notice it printed went to stderr
    /// before the hang (issue #147).
    pub fn pending_coordinated_operations(&self) -> Result<Vec<CoordinatedOperation>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, provider, repository, scope, effect,
                    status, authorization_reason, command_json, pid,
                    exit_code, details_json,
                    created_at, updated_at, finished_at,
                    host_operation_id, identity_provenance, agent_provenance_json
             FROM coordinated_operations
             WHERE effect <> 'read'
               AND status IN ('prepared', 'running', 'outcome_unknown')
             ORDER BY id",
        )?;
        let rows = stmt.query_map([], coordinated_operation_from_row)?;
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
                    host_operation_id, identity_provenance, agent_provenance_json
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

    /// Persist the child-owner witness without resurrecting a concurrently
    /// reconciled operation or overwriting heartbeat/progress fields.
    pub(crate) fn record_fetch_observation(
        &mut self,
        id: i64,
        journal: &str,
    ) -> Result<bool, BrokerError> {
        let changed = self.conn.execute(
            "UPDATE coordinated_operations SET details_json = json_set(COALESCE(details_json, '{}'),
                 '$.fetch_reconciliation', json(?2)), updated_at = ?3
             WHERE id = ?1 AND status = 'running'",
            params![id, journal, now_ms()],
        )?;
        Ok(changed == 1)
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
        // Completion evidence must survive timeout and crash transitions whose
        // new details contain only a reason. Preserve this protocol key only;
        // unrelated previous output is deliberately not carried forward.
        let mut details =
            details_json.and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok());
        if let Some(proof) = operation
            .details_json
            .as_deref()
            .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
            .and_then(|value| value.get("fetch_reconciliation").cloned())
        {
            let destination = details.get_or_insert_with(|| serde_json::json!({}));
            if let Some(object) = destination.as_object_mut() {
                object.entry("fetch_reconciliation").or_insert(proof);
            }
        }
        let preserved_details = details.map(|value| value.to_string());
        let details_json = preserved_details.as_deref().or(details_json);
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

    /// Attach inspection evidence only to the exact operation observed before
    /// the remote read. Inspection must never overwrite a concurrent operator
    /// reconciliation or a newer inspection.
    pub(crate) fn record_unknown_operation_inspection(
        &mut self,
        expected: &CoordinatedOperation,
        details_json: &str,
    ) -> Result<Option<CoordinatedOperation>, BrokerError> {
        self.record_unknown_operation_update(expected, details_json, None)
    }

    pub(crate) fn reconcile_observed_operation(
        &mut self,
        expected: &CoordinatedOperation,
        details_json: &str,
        status: OperationStatus,
        exit_code: Option<i64>,
    ) -> Result<Option<CoordinatedOperation>, BrokerError> {
        if !matches!(
            status,
            OperationStatus::ReconciledSucceeded | OperationStatus::ReconciledFailed
        ) {
            return Ok(None);
        }
        self.record_unknown_operation_update(expected, details_json, Some((status, exit_code)))
    }

    fn record_unknown_operation_update(
        &mut self,
        expected: &CoordinatedOperation,
        details_json: &str,
        resolution: Option<(OperationStatus, Option<i64>)>,
    ) -> Result<Option<CoordinatedOperation>, BrokerError> {
        if expected.status != OperationStatus::OutcomeUnknown {
            return Ok(None);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let read = |tx: &rusqlite::Transaction<'_>| {
            tx.query_row(
                "SELECT id, session_id, provider, repository, scope, effect,
                        status, authorization_reason, command_json, pid,
                        exit_code, details_json, created_at, updated_at, finished_at,
                        host_operation_id, identity_provenance, agent_provenance_json
                 FROM coordinated_operations WHERE id = ?1",
                [expected.id],
                coordinated_operation_from_row,
            )
            .optional()
        };
        let Some(current) = read(&tx)?.transpose()? else {
            return Ok(None);
        };
        // Compare every persisted field while holding the write lock, including
        // the exact original details string and host-operation identity.
        if serde_json::to_value(&current).expect("serializing an operation")
            != serde_json::to_value(expected).expect("serializing an operation")
        {
            return Ok(None);
        }
        let now = now_ms();
        let (status, exit_code) = resolution.unwrap_or((expected.status, expected.exit_code));
        let finished_at = if resolution.is_some() {
            Some(now)
        } else {
            expected.finished_at
        };
        tx.execute(
            "UPDATE coordinated_operations SET details_json = ?2, updated_at = ?3,
                 status = ?4, exit_code = ?5, finished_at = ?6 WHERE id = ?1",
            params![
                expected.id,
                details_json,
                now,
                status.as_str(),
                exit_code,
                finished_at
            ],
        )?;
        let payload = if resolution.is_some() {
            crate::events::fetch_reconciled_payload(
                expected.id,
                &expected.repository,
                &expected.scope,
                expected.effect,
                status,
                exit_code,
            )
        } else {
            crate::events::operation_payload(
                expected.id,
                expected.provider,
                &expected.repository,
                &expected.scope,
                expected.effect,
                status,
                exit_code,
            )
        };
        let event_kind = if resolution.is_some() {
            crate::events::OPERATION_FETCH_RECONCILED
        } else {
            crate::events::OPERATION_INSPECTED
        };
        insert_event(
            &tx,
            now,
            event_kind,
            Some(expected.session_id),
            Some(&payload),
        )?;
        let updated = read(&tx)?.transpose()?;
        tx.commit()?;
        Ok(updated)
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
        tx.execute(
            "INSERT OR IGNORE INTO advisories (
                 identity, audience, producer, session_id, severity,
                 queue_entry_id, integration_sha, paths_json, evidence_json,
                 created_at, resolution_state
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'outstanding')",
            params![
                advisory.identity,
                advisory.audience.as_str(),
                advisory.producer.as_str(),
                advisory.session_id,
                advisory.severity.as_str(),
                advisory.queue_entry_id,
                advisory.integration_sha,
                paths_json,
                evidence_json,
                now,
            ],
        )?;
        tx.commit()?;

        let stored = self
            .advisory_by_identity(&advisory.identity)?
            .expect("insert or existing advisory identity must resolve");
        if stored.audience != advisory.audience
            || stored.producer != advisory.producer
            || stored.session_id != advisory.session_id
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

    /// Persist the current bounded maintainer snapshot and advance only its
    /// explicit lifecycle states. Session-facing coordination advisories are
    /// never selected by this operation.
    pub(crate) fn sync_maintainer_recommendations(
        &mut self,
        snapshot: &crate::recommendations::RecommendationSnapshot,
    ) -> Result<(), BrokerError> {
        let now = now_ms();
        let current_identities = snapshot
            .recommendations
            .iter()
            .map(|recommendation| recommendation.identity.as_str())
            .collect::<BTreeSet<_>>();
        let saturated_kinds = snapshot
            .saturated_kinds
            .iter()
            .map(|kind| kind.as_str())
            .collect::<BTreeSet<_>>();
        let tx = self.conn.transaction()?;

        for recommendation in &snapshot.recommendations {
            let advisory = recommendation.to_new_advisory();
            let paths_json = serde_json::to_string(&advisory.paths)
                .expect("serializing maintainer recommendation paths cannot fail");
            let evidence_json = serde_json::to_string(&advisory.evidence)
                .expect("serializing maintainer recommendation evidence cannot fail");
            let existing = tx
                .query_row(
                    "SELECT audience, producer, resolution_state, evidence_json, suppressed_at
                     FROM advisories WHERE identity = ?1",
                    [&advisory.identity],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, Option<i64>>(4)?,
                        ))
                    },
                )
                .optional()?;
            match existing {
                None => {
                    tx.execute(
                        "INSERT INTO advisories (
                             identity, audience, producer, session_id, severity,
                             queue_entry_id, integration_sha, paths_json, evidence_json,
                             created_at, resolution_state
                         ) VALUES (?1, 'maintainer', ?2, NULL, ?3, NULL, NULL,
                                   ?4, ?5, ?6, 'outstanding')",
                        params![
                            advisory.identity,
                            advisory.producer.as_str(),
                            advisory.severity.as_str(),
                            paths_json,
                            evidence_json,
                            now,
                        ],
                    )?;
                }
                Some((audience, producer, resolution_state, previous_evidence, suppressed_at)) => {
                    if audience != crate::AdvisoryAudience::Maintainer.as_str()
                        || producer != advisory.producer.as_str()
                    {
                        return Err(BrokerError::AdvisoryIdentityConflict(
                            advisory.identity.clone(),
                        ));
                    }
                    let previous = if suppressed_at.is_some() {
                        AdvisoryResolutionState::Suppressed
                    } else {
                        AdvisoryResolutionState::parse(&resolution_state)?
                    };
                    let next = match previous {
                        AdvisoryResolutionState::Suppressed => AdvisoryResolutionState::Suppressed,
                        AdvisoryResolutionState::Acknowledged
                            if previous_evidence == evidence_json =>
                        {
                            AdvisoryResolutionState::Acknowledged
                        }
                        _ => AdvisoryResolutionState::Outstanding,
                    };
                    tx.execute(
                        "UPDATE advisories
                         SET severity = ?2, paths_json = ?3, evidence_json = ?4,
                             resolution_state = ?5,
                             acknowledged_at = CASE
                                 WHEN ?5 = 'acknowledged' THEN acknowledged_at ELSE NULL END,
                             suppressed_at = CASE
                                 WHEN ?6 = 1 THEN suppressed_at ELSE NULL END,
                             resolved_at = NULL, resolution_evidence = NULL
                         WHERE identity = ?1",
                        params![
                            advisory.identity,
                            advisory.severity.as_str(),
                            paths_json,
                            evidence_json,
                            if next == AdvisoryResolutionState::Suppressed {
                                AdvisoryResolutionState::Acknowledged.as_str()
                            } else {
                                next.as_str()
                            },
                            i64::from(next == AdvisoryResolutionState::Suppressed),
                        ],
                    )?;
                }
            }
        }

        let candidates = {
            let mut statement = tx.prepare(
                "SELECT id, identity, evidence_json
                 FROM advisories
                 WHERE audience = 'maintainer'
                   AND resolution_state IN ('outstanding', 'acknowledged')
                   AND suppressed_at IS NULL
                 ORDER BY id",
            )?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        for (id, identity, evidence_json) in candidates {
            if current_identities.contains(identity.as_str()) {
                continue;
            }
            let evidence = serde_json::from_str::<Vec<crate::AdvisoryEvidence>>(&evidence_json)
                .map_err(|source| BrokerError::InvalidAdvisoryJson {
                    id,
                    field: "evidence_json",
                    source,
                })?;
            let kind = evidence
                .iter()
                .find(|item| item.kind == "recommendation_kind")
                .map(|item| item.summary.as_str());
            if !kind.is_some_and(|kind| saturated_kinds.contains(kind)) {
                continue;
            }
            tx.execute(
                "UPDATE advisories
                 SET resolution_state = 'resolved', resolved_at = ?2,
                     resolution_evidence = 'bounded_clean_window'
                 WHERE id = ?1
                   AND resolution_state IN ('outstanding', 'acknowledged')",
                params![id, now],
            )?;
        }
        tx.commit()?;
        Ok(())
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

    pub(super) fn advisory_by_identity(
        &self,
        identity: &str,
    ) -> Result<Option<Advisory>, BrokerError> {
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

    /// Record content-free delivery correlation. Repeated displays update one
    /// row per advisory/surface, so ordinary command use cannot create an
    /// unbounded event stream. No task, arguments, paths, or evidence enter
    /// this table.
    pub fn record_advisories_shown(
        &mut self,
        advisories: &[Advisory],
        surface: crate::AdvisoryDeliverySurface,
    ) -> Result<(), BrokerError> {
        if advisories.is_empty() {
            return Ok(());
        }
        let now = now_ms();
        let tx = self.conn.transaction()?;
        for advisory in advisories {
            tx.execute(
                "INSERT INTO advisory_delivery_metrics (
                     advisory_id, session_id, surface, first_shown_at,
                     last_shown_at, show_count
                 ) VALUES (?1, ?2, ?3, ?4, ?4, 1)
                 ON CONFLICT(advisory_id, surface) DO UPDATE SET
                     last_shown_at = excluded.last_shown_at,
                     show_count = advisory_delivery_metrics.show_count + 1",
                params![advisory.id, advisory.session_id, surface.as_str(), now],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn advisory_delivery_metrics(
        &self,
    ) -> Result<Vec<crate::AdvisoryDeliveryMetric>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT advisory_id, session_id, surface, first_shown_at,
                    last_shown_at, show_count, acted_at, action
             FROM advisory_delivery_metrics
             ORDER BY advisory_id DESC, surface",
        )?;
        let rows = stmt.query_map([], advisory_delivery_metric_from_row)?;
        rows.map(|row| row?).collect()
    }

    pub fn advisory_delivery_summary(&self) -> Result<crate::AdvisoryDeliverySummary, BrokerError> {
        self.conn
            .query_row(
                "SELECT COUNT(DISTINCT advisory_id), COUNT(*),
                        COALESCE(SUM(show_count), 0),
                        COUNT(DISTINCT CASE WHEN acted_at IS NOT NULL THEN advisory_id END)
                 FROM advisory_delivery_metrics",
                [],
                |row| {
                    Ok(crate::AdvisoryDeliverySummary {
                        shown_advisories: row.get::<_, i64>(0)?.max(0) as usize,
                        surface_rows: row.get::<_, i64>(1)?.max(0) as usize,
                        total_shows: row.get::<_, i64>(2)?.max(0) as u64,
                        actioned_advisories: row.get::<_, i64>(3)?.max(0) as usize,
                    })
                },
            )
            .map_err(Into::into)
    }

    // ── authenticated external coordination events ─────────────────

    /// Insert one strict normalized event idempotently. The provider/event ID
    /// pair is immutable; a different digest is an identity conflict.
    pub(crate) fn record_external_event(
        &mut self,
        event: &NewExternalEventRecord,
    ) -> Result<(ExternalEventRecord, bool), BrokerError> {
        let tx = self.conn.transaction()?;
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO external_coordination_events (
                 provider, provider_event_id, event_type, repository,
                 target_branch, pr_number, commit_sha, occurred_at,
                 verification_method, verified_at, normalized_digest, status,
                 session_id, queue_entry_id, received_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                       ?13, ?14, ?15)",
            params![
                event.envelope.provider.as_str(),
                event.envelope.provider_event_id,
                event.envelope.event_type,
                event.envelope.repository,
                event.envelope.target_branch,
                event.envelope.pr_number,
                event.envelope.commit_sha,
                event.envelope.occurred_at,
                event.envelope.verified_source.method.as_str(),
                event.envelope.verified_source.verified_at,
                event.envelope.normalized_digest,
                event.status.as_str(),
                event.session_id,
                event.queue_entry_id,
                event.received_at,
            ],
        )?;
        if inserted > 0 {
            let payload = serde_json::json!({
                "external_event_id": tx.last_insert_rowid(),
                "event_type": event.envelope.event_type,
                "status": event.status.as_str(),
            })
            .to_string();
            insert_event(
                &tx,
                event.received_at,
                "external_event.ingested",
                event.session_id,
                Some(&payload),
            )?;
        }
        tx.commit()?;
        let stored = self
            .external_event_by_identity(
                event.envelope.provider.as_str(),
                &event.envelope.provider_event_id,
            )?
            .expect("inserted or existing external event identity must resolve");
        if stored.normalized_digest != event.envelope.normalized_digest {
            return Err(BrokerError::ExternalEventIdentityConflict {
                provider: event.envelope.provider.as_str().into(),
                event_id: event.envelope.provider_event_id.clone(),
            });
        }
        Ok((stored, inserted == 0))
    }

    pub fn external_event(&self, id: i64) -> Result<Option<ExternalEventRecord>, BrokerError> {
        self.conn
            .query_row(
                &(EXTERNAL_EVENT_SELECT.to_owned() + " WHERE id = ?1"),
                [id],
                external_event_from_row,
            )
            .optional()?
            .transpose()
    }

    pub(super) fn external_event_by_identity(
        &self,
        provider: &str,
        provider_event_id: &str,
    ) -> Result<Option<ExternalEventRecord>, BrokerError> {
        self.conn
            .query_row(
                &(EXTERNAL_EVENT_SELECT.to_owned()
                    + " WHERE provider = ?1 AND provider_event_id = ?2"),
                params![provider, provider_event_id],
                external_event_from_row,
            )
            .optional()?
            .transpose()
    }

    pub fn external_events(
        &self,
        include_all: bool,
    ) -> Result<Vec<ExternalEventRecord>, BrokerError> {
        let sql = if include_all {
            EXTERNAL_EVENT_SELECT.to_owned() + " ORDER BY id DESC LIMIT 500"
        } else {
            EXTERNAL_EVENT_SELECT.to_owned()
                + " WHERE status NOT IN ('advisory_created', 'ignored') ORDER BY id DESC LIMIT 500"
        };
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map([], external_event_from_row)?;
        rows.map(|row| row?).collect()
    }

    pub(crate) fn complete_external_event_advisory(
        &mut self,
        event_id: i64,
        advisory_id: i64,
    ) -> Result<(), BrokerError> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE external_coordination_events
             SET status = 'advisory_created', advisory_id = ?2
             WHERE id = ?1 AND status = 'pending_advisory'
               AND (advisory_id IS NULL OR advisory_id = ?2)",
            params![event_id, advisory_id],
        )?;
        let session_id = tx.query_row(
            "SELECT session_id FROM external_coordination_events WHERE id = ?1",
            [event_id],
            |row| row.get::<_, Option<i64>>(0),
        )?;
        let payload = serde_json::json!({
            "external_event_id": event_id,
            "advisory_id": advisory_id,
        })
        .to_string();
        insert_event(
            &tx,
            now,
            "external_event.advisory_created",
            session_id,
            Some(&payload),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn prepare_external_event_assignment(
        &mut self,
        event_id: i64,
        session_id: i64,
        reason_digest: &str,
        now: i64,
    ) -> Result<(), BrokerError> {
        let changed = self.conn.execute(
            "UPDATE external_coordination_events
             SET status = 'pending_advisory', session_id = ?2,
                 queue_entry_id = NULL, reconciled_at = ?3,
                 reconciliation_kind = 'assigned',
                 reconciliation_reason_digest = ?4
             WHERE id = ?1 AND status NOT IN ('advisory_created', 'ignored')",
            params![event_id, session_id, now, reason_digest],
        )?;
        if changed == 0 {
            return Err(BrokerError::ExternalEventNotFound(event_id));
        }
        Ok(())
    }

    pub(crate) fn ignore_external_event(
        &mut self,
        event_id: i64,
        reason_digest: &str,
        now: i64,
    ) -> Result<(), BrokerError> {
        let changed = self.conn.execute(
            "UPDATE external_coordination_events
             SET status = 'ignored', reconciled_at = ?2,
                 reconciliation_kind = 'ignored',
                 reconciliation_reason_digest = ?3
             WHERE id = ?1 AND status NOT IN ('advisory_created', 'ignored')",
            params![event_id, now, reason_digest],
        )?;
        if changed == 0 {
            return Err(BrokerError::ExternalEventNotFound(event_id));
        }
        Ok(())
    }

    pub(crate) fn external_event_ownership_candidates(
        &self,
        commit_sha: &str,
    ) -> Result<Vec<crate::ExternalEventOwnershipCandidate>, BrokerError> {
        let mut statement = self.conn.prepare(
            "SELECT id, NULL, 'adopted_head' FROM sessions WHERE adopted_head = ?1
             UNION ALL
             SELECT id, accepted_queue_entry_id, 'accepted_session_head'
               FROM sessions WHERE accepted_session_head = ?1
             UNION ALL
             SELECT id, accepted_queue_entry_id, 'accepted_integration_commit'
               FROM sessions WHERE accepted_integration_commit = ?1
             UNION ALL
             SELECT session_id, id, 'queue_head' FROM merge_queue WHERE head_commit = ?1
             UNION ALL
             SELECT q.session_id, x.queue_entry_id, 'promotion_commit'
               FROM entry_path_exposures x
               JOIN merge_queue q ON q.id = x.queue_entry_id
              WHERE x.promotion_sha = ?1
             ORDER BY 1, 2, 3",
        )?;
        let rows = statement.query_map([commit_sha], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let rows = rows.collect::<Result<Vec<_>, _>>()?;
        Ok(aggregate_ownership_candidates(rows))
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
        tx.execute(
            "UPDATE advisories
             SET resolution_state = 'acknowledged', acknowledged_at = ?2
             WHERE id = ?1 AND resolution_state = 'outstanding'",
            params![id, now],
        )?;
        tx.execute(
            "UPDATE advisory_delivery_metrics
             SET acted_at = COALESCE(acted_at, ?2), action = 'acknowledged'
             WHERE advisory_id = ?1",
            params![id, now],
        )?;
        tx.commit()?;
        self.advisory(id)?.ok_or(BrokerError::AdvisoryNotFound(id))
    }

    /// Suppress a maintainer recommendation without affecting session
    /// coordination delivery. Suppression remains in force across new samples.
    pub fn suppress_maintainer_advisory(&mut self, id: i64) -> Result<Advisory, BrokerError> {
        let existing = self
            .advisory(id)?
            .ok_or(BrokerError::AdvisoryNotFound(id))?;
        if existing.audience != crate::AdvisoryAudience::Maintainer {
            return Err(BrokerError::AdvisorySuppressionNotAllowed(id));
        }
        if existing.resolution_state == AdvisoryResolutionState::Suppressed {
            return Ok(existing);
        }
        let now = now_ms();
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE advisories
             SET resolution_state = 'acknowledged', suppressed_at = ?2,
                 acknowledged_at = NULL, resolved_at = NULL,
                 resolution_evidence = NULL
             WHERE id = ?1",
            params![id, now],
        )?;
        tx.commit()?;
        self.advisory(id)?.ok_or(BrokerError::AdvisoryNotFound(id))
    }

    // ── local session notes ─────────────────────────────────────────

    pub fn record_session_note(
        &mut self,
        sender_session_id: i64,
        recipient_session_id: i64,
        message: &str,
    ) -> Result<SessionNote, BrokerError> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO session_notes (
                 sender_session_id, recipient_session_id, message, created_at
             ) VALUES (?1, ?2, ?3, ?4)",
            params![sender_session_id, recipient_session_id, message, now],
        )?;
        let id = tx.last_insert_rowid();
        let payload = serde_json::json!({
            "note_id": id,
            "sender_session_id": sender_session_id,
            "recipient_session_id": recipient_session_id,
            "message_bytes": message.len(),
        })
        .to_string();
        insert_event(
            &tx,
            now,
            "session.note.sent",
            Some(recipient_session_id),
            Some(&payload),
        )?;
        tx.commit()?;
        self.session_note(id)?
            .ok_or(BrokerError::SessionNoteNotFound(id))
    }

    pub fn session_note(&self, id: i64) -> Result<Option<SessionNote>, BrokerError> {
        self.conn
            .query_row(
                &(SESSION_NOTE_SELECT.to_owned() + " WHERE id = ?1"),
                [id],
                session_note_from_row,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn session_notes(
        &self,
        recipient_session_id: i64,
    ) -> Result<Vec<SessionNote>, BrokerError> {
        let mut stmt = self.conn.prepare(&format!(
            "{SESSION_NOTE_SELECT} WHERE recipient_session_id = ?1 ORDER BY id DESC"
        ))?;
        let rows = stmt.query_map([recipient_session_id], session_note_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn unread_session_notes(
        &self,
        recipient_session_id: i64,
    ) -> Result<Vec<SessionNote>, BrokerError> {
        let mut stmt = self.conn.prepare(&format!(
            "{SESSION_NOTE_SELECT}
             WHERE recipient_session_id = ?1 AND acknowledged_at IS NULL
             ORDER BY id"
        ))?;
        let rows = stmt.query_map([recipient_session_id], session_note_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn acknowledge_session_note(&mut self, id: i64) -> Result<SessionNote, BrokerError> {
        let existing = self
            .session_note(id)?
            .ok_or(BrokerError::SessionNoteNotFound(id))?;
        if existing.acknowledged_at.is_some() {
            return Ok(existing);
        }
        let now = now_ms();
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE session_notes SET acknowledged_at = ?2
             WHERE id = ?1 AND acknowledged_at IS NULL",
            params![id, now],
        )?;
        let payload = serde_json::json!({
            "note_id": id,
            "sender_session_id": existing.sender_session_id,
            "recipient_session_id": existing.recipient_session_id,
        })
        .to_string();
        insert_event(
            &tx,
            now,
            "session.note.acknowledged",
            Some(existing.recipient_session_id),
            Some(&payload),
        )?;
        tx.commit()?;
        self.session_note(id)?
            .ok_or(BrokerError::SessionNoteNotFound(id))
    }

    // ── promoted entry path exposures ────────────────────────────────

    /// Exact durable exposure for one promoted queue entry.
    pub fn entry_path_exposure(
        &self,
        queue_entry_id: i64,
    ) -> Result<Option<EntryPathExposure>, BrokerError> {
        self.conn
            .query_row(
                &(ENTRY_PATH_EXPOSURE_SELECT.to_owned() + " WHERE queue_entry_id = ?1"),
                [queue_entry_id],
                entry_path_exposure_from_row,
            )
            .optional()?
            .transpose()
    }

    /// Idempotent compatibility backfill for an entry promoted before path
    /// exposure storage existed. Normal promotion uses the atomic write in
    /// [`Self::record_merge_promotion`] instead.
    pub(crate) fn backfill_entry_path_exposure(
        &mut self,
        queue_entry_id: i64,
        promotion_sha: &str,
        promoted_paths: &[String],
    ) -> Result<EntryPathExposure, BrokerError> {
        let mut paths = promoted_paths.to_vec();
        paths.sort();
        paths.dedup();
        let paths_json =
            serde_json::to_string(&paths).expect("serializing exposure paths cannot fail");
        self.conn.execute(
            "INSERT OR IGNORE INTO entry_path_exposures (
                 queue_entry_id, promotion_sha, paths_json, created_at, state
             ) VALUES (?1, ?2, ?3, ?4, 'outstanding')",
            params![queue_entry_id, promotion_sha, paths_json, now_ms()],
        )?;
        let stored = self
            .entry_path_exposure(queue_entry_id)?
            .expect("insert or existing exposure must resolve");
        if stored.promotion_sha != promotion_sha || stored.paths != paths {
            return Err(BrokerError::EntryExposureIdentityConflict(queue_entry_id));
        }
        Ok(stored)
    }

    /// Oldest-first outstanding exposures, suitable for deterministic
    /// containment checks against one verified remote tip.
    pub fn outstanding_entry_path_exposures(&self) -> Result<Vec<EntryPathExposure>, BrokerError> {
        let mut statement = self.conn.prepare(&format!(
            "{ENTRY_PATH_EXPOSURE_SELECT} WHERE state = 'outstanding' ORDER BY id"
        ))?;
        let rows = statement.query_map([], entry_path_exposure_from_row)?;
        rows.map(|row| row?).collect()
    }

    /// Resolve exact entry exposures. Advisory resolution is deliberately
    /// separate because a published promotion can still intersect a live
    /// session lease.
    pub(crate) fn resolve_entry_path_exposures(
        &mut self,
        queue_entry_ids: &[i64],
        kind: EntryExposureResolutionKind,
        resolution_sha: &str,
        evidence: &str,
    ) -> Result<Vec<EntryPathExposure>, BrokerError> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        let mut resolved_ids = Vec::new();
        for queue_entry_id in queue_entry_ids {
            let updated = tx.execute(
                "UPDATE entry_path_exposures
                 SET state = 'resolved', resolved_at = ?2, resolution_kind = ?3,
                     resolution_sha = ?4, resolution_evidence = ?5
                 WHERE queue_entry_id = ?1 AND state = 'outstanding'",
                params![queue_entry_id, now, kind.as_str(), resolution_sha, evidence],
            )?;
            if updated == 0 {
                continue;
            }
            resolved_ids.push(*queue_entry_id);
        }
        tx.commit()?;

        resolved_ids
            .into_iter()
            .filter_map(|queue_entry_id| self.entry_path_exposure(queue_entry_id).transpose())
            .collect()
    }

    /// Move one exact, reviewed publication exposure to an explicit terminal
    /// state. The identity and creation timestamp are rechecked so a plan
    /// cannot expire a row that has since been replaced or verified.
    pub(crate) fn expire_gc_publication_exposure(
        &mut self,
        candidate: &GcPublicationExposureExpiry,
    ) -> Result<bool, BrokerError> {
        let now = now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let expired = tx.execute(
            "UPDATE entry_path_exposures
             SET state = 'expired', resolved_at = ?4, resolution_kind = 'expired',
                 resolution_sha = NULL, resolution_evidence = ?5
             WHERE id = ?1 AND queue_entry_id = ?2 AND created_at = ?3
               AND state = 'outstanding'",
            params![
                candidate.exposure_id,
                candidate.queue_entry_id,
                candidate.created_at,
                now,
                candidate.reason,
            ],
        )?;
        if expired == 0 {
            let already_expired: bool = tx.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM entry_path_exposures
                     WHERE id = ?1 AND queue_entry_id = ?2
                       AND created_at = ?3 AND state = 'expired'
                 )",
                params![
                    candidate.exposure_id,
                    candidate.queue_entry_id,
                    candidate.created_at
                ],
                |row| row.get(0),
            )?;
            tx.commit()?;
            return Ok(already_expired);
        }
        let payload = serde_json::json!({
            "exposure_id": candidate.exposure_id,
            "queue_entry_id": candidate.queue_entry_id,
            "created_at": candidate.created_at,
            "age_days": candidate.age_days,
            "reason": candidate.reason,
        })
        .to_string();
        insert_event(&tx, now, "exposure.expired", None, Some(&payload))?;
        tx.commit()?;
        Ok(true)
    }

    /// Check one session's unreleased, unexpired lease rows at a single time.
    ///
    /// This intentionally does not filter by session status: finish calls the
    /// check after closing the session to verify that its close transaction
    /// released the leases.
    pub fn has_unreleased_unexpired_lease_for_session_at(
        &self,
        session_id: i64,
        now: i64,
    ) -> Result<bool, BrokerError> {
        self.conn
            .query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM leases
                     WHERE session_id = ?1
                       AND released_at IS NULL
                       AND (expires_at IS NULL OR expires_at > ?2)
                 )",
                params![session_id, now],
                |row| row.get(0),
            )
            .map_err(BrokerError::from)
    }

    /// Return only the unreleased, unexpired lease rows owned by `session_ids`
    /// at one caller-supplied snapshot time, regardless of session status.
    ///
    /// Cleanup uses this after confirming its candidate sessions are closed:
    /// the general active-lease view intentionally filters those sessions
    /// out, while a retained row must still be classified for unknown/stale
    /// holder evidence. Chunks stay below SQLite's older 999-variable limit.
    pub(crate) fn unreleased_unexpired_leases_for_sessions_at(
        &self,
        session_ids: &[i64],
        now: i64,
    ) -> Result<Vec<Lease>, BrokerError> {
        const SESSION_ID_CHUNK: usize = 900;

        let mut session_ids = session_ids.to_vec();
        session_ids.sort_unstable();
        session_ids.dedup();
        let mut leases = Vec::new();
        for session_ids in session_ids.chunks(SESSION_ID_CHUNK) {
            let placeholders = std::iter::repeat_n("?", session_ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            let now_parameter = session_ids.len() + 1;
            let sql = format!(
                "{LEASE_SELECT}
                 WHERE session_id IN ({placeholders})
                   AND released_at IS NULL
                   AND (expires_at IS NULL OR expires_at > ?{now_parameter})
                 ORDER BY id"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let parameters = session_ids.iter().copied().chain(std::iter::once(now));
            let rows = stmt.query_map(rusqlite::params_from_iter(parameters), lease_from_row)?;
            for row in rows {
                leases.push(row??);
            }
        }
        leases.sort_by_key(|lease| lease.id);
        Ok(leases)
    }

    /// Record exact-holder absence only if the binding is still current and
    /// no gone event has already been recorded for it. The immediate
    /// transaction makes the binding check and event append one conditional
    /// write; zero busy timeout lets maintenance defer instead of waiting on
    /// another writer.
    pub(crate) fn record_session_holder_gone_for_binding(
        &mut self,
        session_id: i64,
        binding_event_id: i64,
        holder: &crate::session_holder::AgentProcess,
    ) -> Result<bool, BrokerError> {
        self.conn.busy_timeout(std::time::Duration::ZERO)?;
        let result = (|| {
            let tx = self
                .conn
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let current_binding: Option<i64> = tx
                .query_row(
                    "SELECT id FROM events
                     WHERE session_id = ?1 AND kind = 'session.holder_bound'
                     ORDER BY id DESC LIMIT 1",
                    [session_id],
                    |row| row.get(0),
                )
                .optional()?;
            if current_binding != Some(binding_event_id) {
                tx.commit()?;
                return Ok(false);
            }
            let latest_gone: Option<(i64, Option<String>)> = tx
                .query_row(
                    "SELECT id, payload_json FROM events
                     WHERE session_id = ?1 AND kind = 'session.holder_gone'
                     ORDER BY id DESC LIMIT 1",
                    [session_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if latest_gone.is_some_and(|(id, payload)| {
                id > binding_event_id
                    && payload
                        .as_deref()
                        .and_then(|payload| {
                            serde_json::from_str::<crate::session_holder::AgentProcess>(payload)
                                .ok()
                        })
                        .is_some_and(|recorded| &recorded == holder)
            }) {
                tx.commit()?;
                return Ok(false);
            }
            let payload_json = crate::events::session_holder_gone_payload(holder);
            insert_event(
                &tx,
                now_ms(),
                "session.holder_gone",
                Some(session_id),
                Some(&payload_json),
            )?;
            tx.commit()?;
            Ok(true)
        })();
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
            .busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS))?;
        result
    }

    /// Resolve publication advisories only after their affected session no
    /// longer has a live lease overlapping the advisory paths. Acknowledged
    /// advisories still complete their lifecycle once the condition clears.
    pub(crate) fn resolve_entry_advisories_without_active_leases(
        &mut self,
        queue_entry_ids: &[i64],
        evidence: &str,
    ) -> Result<Vec<Advisory>, BrokerError> {
        let queue_entry_ids = queue_entry_ids
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        let active_leases = self.active_leases()?;
        let advisory_ids = self
            .advisories(true)?
            .into_iter()
            .filter(|advisory| advisory.resolution_state != AdvisoryResolutionState::Resolved)
            .filter(|advisory| {
                advisory
                    .queue_entry_id
                    .is_some_and(|entry_id| queue_entry_ids.contains(&entry_id))
            })
            .filter(|advisory| {
                !active_leases.iter().any(|lease| {
                    Some(lease.session_id) == advisory.session_id
                        && advisory
                            .paths
                            .iter()
                            .any(|path| crate::leases::paths_overlap(path, &lease.path))
                })
            })
            .map(|advisory| advisory.id)
            .collect::<Vec<_>>();
        let now = now_ms();
        let tx = self.conn.transaction()?;
        for advisory_id in &advisory_ids {
            tx.execute(
                "UPDATE advisories
                 SET resolution_state = 'resolved', resolved_at = ?2,
                     resolution_evidence = ?3
                 WHERE id = ?1 AND resolution_state IN ('outstanding', 'acknowledged')",
                params![advisory_id, now, evidence],
            )?;
            tx.execute(
                "UPDATE advisory_delivery_metrics
                 SET acted_at = COALESCE(acted_at, ?2), action = 'publication_resolved'
                 WHERE advisory_id = ?1",
                params![advisory_id, now],
            )?;
        }
        tx.commit()?;
        advisory_ids
            .into_iter()
            .filter_map(|id| self.advisory(id).transpose())
            .collect()
    }

    // ── pull request watches ────────────────────────────────────────

    pub fn insert_pull_request_watch(
        &mut self,
        watch: &NewPullRequestWatch,
    ) -> Result<PullRequestWatch, BrokerError> {
        let event_kinds_json = serde_json::to_string(&watch.event_kinds)
            .expect("serializing pull request event kinds cannot fail");
        let tx = self.conn.transaction()?;
        match tx.execute(
            "INSERT INTO pull_request_watches (
                 session_id, provider, canonical_repository, display_repository,
                 pr_number, target_branch, head_sha, is_draft, status,
                 event_kinds_json, poll_interval_seconds, cursor_digest,
                 next_poll_at, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'active', ?9, ?10, ?11, ?12, ?13, ?13)",
            params![
                watch.session_id,
                watch.provider,
                watch.canonical_repository,
                watch.display_repository,
                watch.pr_number,
                watch.target_branch,
                watch.head_sha,
                watch.is_draft,
                event_kinds_json,
                watch.poll_interval_seconds as i64,
                watch.cursor_digest,
                watch.now_ms + watch.poll_interval_seconds as i64 * 1_000,
                watch.now_ms,
            ],
        ) {
            Ok(_) => {}
            Err(error)
                if error.sqlite_error_code() == Some(rusqlite::ErrorCode::ConstraintViolation) =>
            {
                return Err(BrokerError::PullRequestWatchIdentityConflict {
                    repository: watch.canonical_repository.clone(),
                    pr_number: watch.pr_number,
                });
            }
            Err(error) => return Err(error.into()),
        }
        let id = tx.last_insert_rowid();
        for activity in &watch.baseline_activities {
            upsert_pull_request_activity(&tx, id, activity, watch.now_ms)?;
        }
        let payload = serde_json::json!({
            "watch_id": id,
            "repository": watch.canonical_repository,
            "pr_number": watch.pr_number,
            "provider": watch.provider,
        })
        .to_string();
        insert_event(
            &tx,
            watch.now_ms,
            "pr_watch.started",
            Some(watch.session_id),
            Some(&payload),
        )?;
        tx.commit()?;
        self.pull_request_watch(id)?
            .ok_or(BrokerError::InvalidEnumValue {
                field: "pull_request_watch.id",
                value: id.to_string(),
            })
    }

    /// Indexed lookup without scanning historical sessions or watches.
    pub(crate) fn live_pull_request_watch_for_identity(
        &self,
        canonical_repository: &str,
        pr_number: i64,
    ) -> Result<Option<PullRequestWatch>, BrokerError> {
        self.conn
            .query_row(
                &(PULL_REQUEST_WATCH_SELECT.to_owned()
                    + " WHERE provider = 'github' AND canonical_repository = ?1 \
                   AND pr_number = ?2 AND status IN ('active', 'paused')"),
                params![canonical_repository, pr_number],
                pull_request_watch_from_row,
            )
            .optional()?
            .transpose()
    }

    pub fn pull_request_watch(&self, id: i64) -> Result<Option<PullRequestWatch>, BrokerError> {
        self.conn
            .query_row(
                &(PULL_REQUEST_WATCH_SELECT.to_owned() + " WHERE id = ?1"),
                [id],
                pull_request_watch_from_row,
            )
            .optional()?
            .transpose()
    }

    pub fn pull_request_watches(
        &self,
        include_terminal: bool,
    ) -> Result<Vec<PullRequestWatch>, BrokerError> {
        let sql = if include_terminal {
            PULL_REQUEST_WATCH_SELECT.to_owned() + " ORDER BY id DESC"
        } else {
            PULL_REQUEST_WATCH_SELECT.to_owned()
                + " WHERE status IN ('active', 'paused') ORDER BY id DESC"
        };
        let mut statement = self.conn.prepare(&sql)?;
        statement
            .query_map([], pull_request_watch_from_row)?
            .map(|row| row?)
            .collect()
    }

    pub fn update_pull_request_watch_status(
        &mut self,
        id: i64,
        status: PullRequestWatchStatus,
        now: i64,
        last_error_code: Option<&str>,
    ) -> Result<PullRequestWatch, BrokerError> {
        let current = self
            .pull_request_watch(id)?
            .ok_or(BrokerError::InvalidEnumValue {
                field: "pull_request_watch.id",
                value: id.to_string(),
            })?;
        let next_poll_at = (status == PullRequestWatchStatus::Active)
            .then_some(now + current.poll_interval_seconds as i64 * 1_000);
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE pull_request_watches
             SET status = ?2, next_poll_at = ?3, last_error_code = ?4, updated_at = ?5
             WHERE id = ?1",
            params![id, status.as_str(), next_poll_at, last_error_code, now],
        )?;
        let payload = serde_json::json!({ "watch_id": id, "status": status }).to_string();
        insert_event(
            &tx,
            now,
            "pr_watch.status_changed",
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
}

#[cfg(test)]
mod finish_lease_query_tests {
    use super::*;

    fn new_session(name: &str, root: &std::path::Path) -> NewSession {
        NewSession {
            worktree_path: root
                .join(format!("{name}-worktree"))
                .to_string_lossy()
                .into_owned(),
            branch: format!("agent/finish-lease-{name}"),
            origin: SessionOrigin::Spawned,
            task: Some(name.to_owned()),
            diff_base: None,
            adoption_base: None,
            adopted_head: None,
            repository_contract: None,
            pid: None,
            command: None,
            log_path: None,
            agent_identity: None,
        }
    }

    #[test]
    fn post_close_lease_lookup_is_session_scoped_and_checks_closed_rows() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = BrokerStore::open(&temp.path().join("broker.db")).unwrap();
        let target = store
            .register_session(&new_session("target", temp.path()))
            .unwrap();
        let unrelated = store
            .register_session(&new_session("unrelated", temp.path()))
            .unwrap();

        store
            .set_session_status(target.id, SessionStatus::Closed, None)
            .unwrap();
        store
            .claim_lease(target.id, "unreleased-after-close", None)
            .unwrap();
        for index in 0..128 {
            store
                .claim_lease(unrelated.id, &format!("unrelated-lease-{index}"), None)
                .unwrap();
        }

        let now = now_ms();
        assert!(
            !store
                .active_leases_at(now)
                .unwrap()
                .iter()
                .any(|lease| lease.session_id == target.id),
            "active_leases intentionally excludes closed sessions"
        );
        assert!(
            store
                .has_unreleased_unexpired_lease_for_session_at(target.id, now)
                .unwrap(),
            "post-close verification must still see an unreleased target row"
        );
        let scoped = store
            .unreleased_unexpired_leases_for_sessions_at(&[target.id], now)
            .unwrap();
        assert_eq!(scoped.len(), 1, "unrelated lease history stays out");
        assert_eq!(scoped[0].session_id, target.id);
        assert_eq!(scoped[0].path, "unreleased-after-close");
        let mut chunked_ids = (-900..0).collect::<Vec<_>>();
        chunked_ids.push(target.id);
        let chunked = store
            .unreleased_unexpired_leases_for_sessions_at(&chunked_ids, now)
            .unwrap();
        assert_eq!(chunked.len(), 1, "the final ID chunk is queried");
        assert_eq!(chunked[0].session_id, target.id);
        let repeated_ids = vec![target.id; 901];
        assert_eq!(
            store
                .unreleased_unexpired_leases_for_sessions_at(&repeated_ids, now)
                .unwrap()
                .len(),
            1,
            "duplicate IDs across query chunks must not duplicate leases"
        );

        store
            .release_lease(target.id, "unreleased-after-close")
            .unwrap();
        assert!(
            !store
                .has_unreleased_unexpired_lease_for_session_at(target.id, now_ms())
                .unwrap(),
            "released target rows should no longer count"
        );
        assert!(
            store
                .unreleased_unexpired_leases_for_sessions_at(&[target.id], now_ms())
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .has_unreleased_unexpired_lease_for_session_at(unrelated.id, now_ms())
                .unwrap(),
            "unrelated active rows should be queried independently"
        );

        let expired = store
            .claim_lease(target.id, "expired-after-close", Some(1))
            .unwrap();
        assert!(
            !store
                .has_unreleased_unexpired_lease_for_session_at(
                    target.id,
                    expired.expires_at.unwrap(),
                )
                .unwrap(),
            "a row expiring at the query snapshot is no longer active"
        );
        assert!(
            store
                .unreleased_unexpired_leases_for_sessions_at(
                    &[target.id],
                    expired.expires_at.unwrap(),
                )
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn generic_path_release_still_releases_explicit_and_implicit_rows() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = BrokerStore::open(&temp.path().join("broker.db")).unwrap();
        let session = store
            .register_session(&new_session("path-wide-release", temp.path()))
            .unwrap();
        let path = "src/shared.rs";

        store.claim_lease(session.id, path, None).unwrap();
        store
            .set_implicit_leases(session.id, &[path.to_owned()])
            .unwrap();
        store.release_lease(session.id, path).unwrap();

        let leases = store.session_leases(session.id).unwrap();
        assert_eq!(leases.len(), 2);
        assert!(leases.iter().all(|lease| lease.released_at.is_some()));
    }
}
