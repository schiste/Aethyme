use super::*;

impl BrokerStore {
    /// Mark conclusive failing gate results cleared without removing their
    /// historical rows. Cleared rows stop satisfying the active cache.
    pub(crate) fn clear_cached_test_failures(
        &mut self,
        gate_name: &str,
        tree_hash: &str,
        reason: &str,
    ) -> Result<Vec<i64>, BrokerError> {
        let now = now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ids = {
            let mut stmt = tx.prepare(
                "SELECT id FROM gate_results
                 WHERE gate_name = ?1 AND tree_hash = ?2
                   AND status = 'fail' AND failure_class = 'test_failure'
                   AND cleared_at IS NULL
                 ORDER BY id",
            )?;
            stmt.query_map(params![gate_name, tree_hash], |row| row.get::<_, i64>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        for id in &ids {
            tx.execute(
                "UPDATE gate_results
                 SET cleared_at = ?1, cleared_reason = ?2
                 WHERE id = ?3 AND cleared_at IS NULL",
                params![now, reason, id],
            )?;
        }
        tx.commit()?;
        Ok(ids)
    }

    /// Cache lookup: the most recent conclusive, uncleared result for this
    /// gate against this exact tree. Passes are conclusive; failures are only
    /// conclusive when classified as real test failures. Cancelled, error,
    /// infra-classified, legacy unclassified fail, and cleared rows never
    /// satisfy the cache.
    pub fn cached_gate_result(
        &self,
        gate_name: &str,
        tree_hash: &str,
    ) -> Result<Option<GateResult>, BrokerError> {
        let result = self
            .conn
            .query_row(
                &format!(
                    "{GATE_RESULT_SELECT}
                    WHERE gate_name = ?1 AND tree_hash = ?2 AND cleared_at IS NULL
                      AND NOT EXISTS (
                          SELECT 1 FROM gate_results cleared
                          WHERE cleared.gate_name = gate_results.gate_name
                            AND cleared.tree_hash = gate_results.tree_hash
                            AND cleared.cleared_at IS NOT NULL
                            AND cleared.id > gate_results.id
                      )
                      AND (status = 'pass'
                           OR (status = 'fail' AND failure_class = 'test_failure'))
                     ORDER BY id DESC LIMIT 1"
                ),
                params![gate_name, tree_hash],
                gate_result_from_row,
            )
            .optional()?;
        result.transpose()
    }

    /// Definition-bound cache lookup used by execution. Cleared results remain
    /// in gate history but cannot satisfy this lookup.
    pub fn cached_gate_result_for_definition(
        &self,
        gate_name: &str,
        tree_hash: &str,
        definition_hash: &str,
    ) -> Result<Option<GateResult>, BrokerError> {
        let result = self
            .conn
            .query_row(
                &format!(
                    "{GATE_RESULT_SELECT}
                    WHERE gate_name = ?1 AND tree_hash = ?2 AND definition_hash = ?3
                      AND cleared_at IS NULL
                      AND NOT EXISTS (
                          SELECT 1 FROM gate_results cleared
                          WHERE cleared.gate_name = gate_results.gate_name
                            AND cleared.tree_hash = gate_results.tree_hash
                            AND cleared.definition_hash = gate_results.definition_hash
                            AND cleared.cleared_at IS NOT NULL
                            AND cleared.id > gate_results.id
                      )
                      AND (
                            status = 'pass'
                            OR (status = 'fail' AND failure_class = 'test_failure')
                       )
                     ORDER BY id DESC LIMIT 1"
                ),
                params![gate_name, tree_hash, definition_hash],
                gate_result_from_row,
            )
            .optional()?;
        result.transpose()
    }

    /// Cache lookup bound to the exact execution profile that produced the
    /// verdict. Rows from before profile tracking (or runs whose profile could
    /// not be captured) cannot satisfy this lookup.
    pub fn cached_gate_result_for_execution_profile(
        &self,
        gate_name: &str,
        tree_hash: &str,
        definition_hash: &str,
        execution_profile_hash: &str,
    ) -> Result<Option<GateResult>, BrokerError> {
        let result = self
            .conn
            .query_row(
                &format!(
                    "{GATE_RESULT_SELECT}
                    WHERE gate_name = ?1 AND tree_hash = ?2 AND definition_hash = ?3
                      AND execution_profile_hash = ?4 AND cleared_at IS NULL
                      AND NOT EXISTS (
                          SELECT 1 FROM gate_results cleared
                          WHERE cleared.gate_name = gate_results.gate_name
                            AND cleared.tree_hash = gate_results.tree_hash
                            AND cleared.definition_hash = gate_results.definition_hash
                            AND cleared.execution_profile_hash = gate_results.execution_profile_hash
                            AND cleared.cleared_at IS NOT NULL
                            AND cleared.id > gate_results.id
                      )
                      AND (
                            status = 'pass'
                            OR (status = 'fail' AND failure_class = 'test_failure')
                       )
                     ORDER BY id DESC LIMIT 1"
                ),
                params![
                    gate_name,
                    tree_hash,
                    definition_hash,
                    execution_profile_hash
                ],
                gate_result_from_row,
            )
            .optional()?;
        result.transpose()
    }

    /// How many earlier runs of this gate definition on this tree timed out.
    ///
    /// A first timeout may be the host; a repeated one on an unchanged tree is
    /// evidence about the change, so the gate runner stops treating it as a
    /// host fault.
    pub fn gate_timeouts_for_tree(
        &self,
        gate_name: &str,
        tree_hash: &str,
        definition_hash: &str,
    ) -> Result<i64, BrokerError> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM gate_results
             WHERE gate_name = ?1 AND tree_hash = ?2 AND definition_hash = ?3
               AND failure_class = 'timeout'",
            params![gate_name, tree_hash, definition_hash],
            |row| row.get(0),
        )?)
    }

    /// Timeout history is profile-bound for the same reason as cache hits: a
    /// timeout in one toolchain must not classify the first timeout in a
    /// different toolchain as a verdict.
    pub fn gate_timeouts_for_execution_profile(
        &self,
        gate_name: &str,
        tree_hash: &str,
        definition_hash: &str,
        execution_profile_hash: Option<&str>,
    ) -> Result<i64, BrokerError> {
        let Some(execution_profile_hash) = execution_profile_hash else {
            // Unknown profiles cannot prove that a previous timeout came from
            // the same environment, so do not turn one into a code verdict.
            return Ok(0);
        };
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM gate_results
             WHERE gate_name = ?1 AND tree_hash = ?2 AND definition_hash = ?3
               AND failure_class = 'timeout'
               AND execution_profile_hash = ?4",
            params![
                gate_name,
                tree_hash,
                definition_hash,
                execution_profile_hash
            ],
            |row| row.get(0),
        )?)
    }

    /// Aggregate executed gate runs (pass/fail only): (gate, runs,
    /// total_duration_ms). For the metrics/kill-criterion report.
    pub fn gate_execution_totals(&self) -> Result<Vec<(String, i64, i64)>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT gate_name, COUNT(*), SUM(COALESCE(duration_ms, 0))
             FROM gate_results WHERE status IN ('pass', 'fail')
             GROUP BY gate_name ORDER BY gate_name",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    // ── lifecycle telemetry ───────────────────────────────────────────

    /// Record one activity signal for a session, opening or extending a period
    /// of attention.
    ///
    /// A signal within [`crate::insights::IDLE_GAP_MS`] of the previous one
    /// extends the open interval; a signal after the gap closes that interval
    /// at the last signal seen and opens a new one. Splitting on the gap is
    /// what makes active time different from wall-clock: without it, an
    /// overnight pause is indistinguishable from a long uninterrupted stretch
    /// of work and both come out the same length.
    ///
    /// Returns the id of the interval the signal landed in.
    pub fn record_session_activity(
        &mut self,
        session_id: i64,
        at_ms: i64,
    ) -> Result<i64, BrokerError> {
        let now = now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let open: Option<(i64, i64)> = tx
            .query_row(
                "SELECT id, last_signal_at FROM session_activity
                 WHERE session_id = ?1 AND ended_at IS NULL",
                params![session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        let id = match open {
            // Backwards clock, or a signal that predates the open interval:
            // extend the existing one rather than writing a negative gap.
            Some((id, last_signal_at))
                if at_ms.saturating_sub(last_signal_at) <= crate::insights::IDLE_GAP_MS =>
            {
                tx.execute(
                    "UPDATE session_activity
                     SET last_signal_at = MAX(last_signal_at, ?2), signals = signals + 1
                     WHERE id = ?1",
                    params![id, at_ms],
                )?;
                id
            }
            Some((id, last_signal_at)) => {
                // The gap closed the period: end it at the last signal rather
                // than at this one, so the interval measures the attention that
                // was actually recorded and not the silence that followed.
                tx.execute(
                    "UPDATE session_activity SET ended_at = ?2 WHERE id = ?1",
                    params![id, last_signal_at],
                )?;
                tx.execute(
                    "INSERT INTO session_activity (session_id, started_at, last_signal_at, source)
                     VALUES (?1, ?2, ?2, 'host_hook')",
                    params![session_id, at_ms],
                )?;
                tx.last_insert_rowid()
            }
            None => {
                tx.execute(
                    "INSERT INTO session_activity (session_id, started_at, last_signal_at, source)
                     VALUES (?1, ?2, ?2, 'host_hook')",
                    params![session_id, at_ms],
                )?;
                tx.last_insert_rowid()
            }
        };
        // A session's activity and its liveness are the same fact; keeping
        // them in one transaction is what stops `last_activity_at` from
        // disagreeing with the history it summarizes.
        tx.execute(
            "UPDATE sessions SET last_activity_at = MAX(last_activity_at, ?2), updated_at = ?3
             WHERE id = ?1",
            params![session_id, at_ms, now],
        )?;
        tx.commit()?;
        Ok(id)
    }

    /// Close whatever period of attention a session currently has open.
    ///
    /// Called when a session closes, so its final interval does not stay open
    /// forever and read as an unbounded duration. Sessions are never back-
    /// filled: an open interval on a closed session would otherwise be
    /// indistinguishable from one whose session is merely idle.
    pub fn close_session_activity(
        &mut self,
        session_id: i64,
        at_ms: i64,
    ) -> Result<usize, BrokerError> {
        close_open_activity_in_tx(&self.conn, session_id, at_ms)
    }

    /// Session rows for the insights funnel, newest first.
    ///
    /// A deliberately narrow projection rather than [`Session`]: the funnel
    /// needs identity, origin, and two timestamps, and pulling task text, work
    /// paths, commands, log paths, and agent identities into a reporting query
    /// would put every one of those fields one refactor away from a serialized
    /// report. `agent_identity` in particular is not selected — see
    /// [`crate::insights`].
    ///
    /// `in_flight` is derived rather than stored: a session is in flight when
    /// it is live and has queue work that has not reached a terminal status.
    pub fn insight_session_rows(&self) -> Result<Vec<crate::InsightSessionRow>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.origin, s.created_at, s.closed_at,
                    EXISTS(SELECT 1 FROM merge_queue q
                           WHERE q.session_id = s.id
                             AND q.status IN ('submitted','simulating','verified')) AS in_flight
             FROM sessions s
             ORDER BY s.created_at DESC",
        )?;
        // The origin string is parsed through the same `parse` the rest of the
        // store uses, and an unrecognized value is an error rather than a
        // default: a CHECK constraint makes it unreachable, and silently
        // reporting an adopted session as spawned would be a wrong answer
        // instead of a loud one.
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, i64>(4)? != 0,
            ))
        })?;
        let mut sessions = Vec::new();
        for row in rows {
            let (session_id, origin, created_at, closed_at, in_flight) = row?;
            sessions.push(crate::InsightSessionRow {
                session_id,
                origin: SessionOrigin::parse(&origin)?,
                created_at,
                closed_at,
                in_flight,
            });
        }
        Ok(sessions)
    }

    /// Gate rows for the insights report.
    /// Gate runs recorded at or after `since_ms` (0 reads all history), so the
    /// gate figures cover the same window as the funnel beside them.
    pub fn insight_gate_rows(
        &self,
        since_ms: i64,
    ) -> Result<Vec<crate::InsightGateRow>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT gate_name, status, duration_ms, wait_duration_ms, first_output_ms, failure_class
             FROM gate_results WHERE created_at >= ?1 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([since_ms], |row| {
            let status: String = row.get(1)?;
            Ok(crate::InsightGateRow {
                gate_name: row.get(0)?,
                status: match status.as_str() {
                    "pass" => "pass",
                    "fail" => "fail",
                    "cancelled" => "cancelled",
                    _ => "error",
                },
                duration_ms: row.get(2)?,
                wait_duration_ms: row.get(3)?,
                first_output_ms: row.get(4)?,
                failure_class: row.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Coordinated operation outcome counts at or after `since_ms` (0 reads
    /// all history), for the insights report.
    pub fn insight_operation_counts(&self, since_ms: i64) -> Result<(u64, u64, u64), BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT status, COUNT(*) FROM coordinated_operations
             WHERE status IN ('succeeded','failed','outcome_unknown') AND created_at >= ?1
             GROUP BY status",
        )?;
        let rows = stmt.query_map([since_ms], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut succeeded = 0;
        let mut failed = 0;
        let mut unknown = 0;
        for row in rows {
            let (status, count) = row?;
            match status.as_str() {
                "succeeded" => succeeded = count,
                "failed" => failed = count,
                "outcome_unknown" => unknown = count,
                _ => {}
            }
        }
        Ok((
            succeeded.max(0) as u64,
            failed.max(0) as u64,
            unknown.max(0) as u64,
        ))
    }

    /// Active milliseconds and signal count per session, in one query.
    ///
    /// An open interval is bounded by its own `last_signal_at` rather than by
    /// `now`: its duration is not yet known, and crediting it with the time
    /// since the last signal would make a session that has been idle for a day
    /// look like it was worked on for a day.
    pub fn session_activity_totals(&self) -> Result<BTreeMap<i64, (i64, u64)>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id,
                    SUM(CASE WHEN ended_at IS NULL
                             THEN MAX(last_signal_at - started_at, 0)
                             ELSE MAX(ended_at - started_at, 0) END),
                    SUM(signals)
             FROM session_activity
             GROUP BY session_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                (row.get::<_, i64>(1)?, row.get::<_, i64>(2)?),
            ))
        })?;
        let mut totals = BTreeMap::new();
        for row in rows {
            let (session_id, (active_ms, signals)) = row?;
            totals.insert(session_id, (active_ms, signals.max(0) as u64));
        }
        Ok(totals)
    }

    /// Every recorded pull request milestone, with its linked sessions.
    pub fn pull_request_milestones(&self) -> Result<Vec<PullRequestMilestoneRow>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT repository, pr_number, opened_at, merged_at
             FROM pull_request_milestones
             ORDER BY COALESCE(merged_at, opened_at, first_seen_at)",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<i64>>(3)?,
            ))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Sessions linked to one pull request.
    pub fn pull_request_sessions(
        &self,
        repository: &str,
        pr_number: i64,
    ) -> Result<Vec<i64>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id FROM pull_request_session_links
             WHERE repository = ?1 AND pr_number = ?2 ORDER BY session_id",
        )?;
        let rows = stmt.query_map(params![repository, pr_number], |row| row.get(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Record when a pull request opened, first sighting wins.
    ///
    /// Idempotent on the pull request, because several observers can report the
    /// same pull request and the earliest `opened_at` any of them saw is the
    /// best answer available — later polls see a pull request that already
    /// exists and cannot recover when it was created.
    pub fn record_pull_request_opened(
        &mut self,
        repository: &str,
        pr_number: i64,
        session_id: Option<i64>,
        opened_at: i64,
    ) -> Result<(), BrokerError> {
        let now = now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        upsert_pull_request_milestone_in_tx(
            &tx,
            repository,
            pr_number,
            session_id,
            Some(opened_at),
            None,
            now,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Record when a pull request merged.
    pub fn record_pull_request_merged(
        &mut self,
        repository: &str,
        pr_number: i64,
        session_id: Option<i64>,
        merged_at: i64,
    ) -> Result<(), BrokerError> {
        let now = now_ms();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        upsert_pull_request_milestone_in_tx(
            &tx,
            repository,
            pr_number,
            session_id,
            None,
            Some(merged_at),
            now,
        )?;
        tx.commit()?;
        Ok(())
    }

    // ── merge queue ───────────────────────────────────────────────────

    /// Submit a session head. Idempotent per (session, head): resubmitting
    /// the same commit returns the existing entry.
    pub fn submit(
        &mut self,
        session_id: i64,
        head_commit: &str,
        base_commit: &str,
    ) -> Result<MergeQueueEntry, BrokerError> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        let inserted = tx.execute(
            "INSERT INTO merge_queue (session_id, head_commit, base_commit, created_at,
                                      updated_at)
             VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT (session_id, head_commit) DO NOTHING",
            params![session_id, head_commit, base_commit, now],
        )?;
        if inserted > 0 {
            insert_event(
                &tx,
                now,
                "merge.submitted",
                Some(session_id),
                Some(&crate::events::merge_submitted_payload(head_commit)),
            )?;
        }
        tx.commit()?;
        let entry = self.conn.query_row(
            &format!("{MERGE_SELECT} WHERE session_id = ?1 AND head_commit = ?2"),
            params![session_id, head_commit],
            merge_from_row,
        )??;
        Ok(entry)
    }

    /// Transition a queue entry; emits `merge.<status>` in the same
    /// transaction and stores updated details/merged tree when given.
    pub fn set_merge_status(
        &mut self,
        entry_id: i64,
        status: MergeStatus,
        merged_tree: Option<&str>,
        details_json: Option<&str>,
    ) -> Result<(), BrokerError> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        let session_id: Option<i64> = tx
            .query_row(
                "SELECT session_id FROM merge_queue WHERE id = ?1",
                [entry_id],
                |row| row.get(0),
            )
            .optional()?;
        tx.execute(
            "UPDATE merge_queue
             SET status = ?2,
                 merged_tree = COALESCE(?3, merged_tree),
                 details_json = COALESCE(?4, details_json),
                 updated_at = ?5
             WHERE id = ?1",
            params![entry_id, status.as_str(), merged_tree, details_json, now],
        )?;
        insert_event(
            &tx,
            now,
            &format!("merge.{}", status.as_str()),
            session_id,
            details_json,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Mark one verified queue entry promoted and advance its session's
    /// accepted contribution checkpoint in the same SQLite transaction.
    pub fn record_merge_promotion(
        &mut self,
        entry_id: i64,
        integration_commit: &str,
        integration_ref: &str,
        promoted_paths: &[String],
        details_json: &str,
    ) -> Result<(), BrokerError> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        let entry = tx
            .query_row(
                "SELECT session_id, head_commit, merged_tree
                 FROM merge_queue WHERE id = ?1 AND status = 'verified'",
                [entry_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((session_id, session_head, Some(integration_tree))) = entry else {
            return Err(BrokerError::SessionNotFound(entry_id));
        };
        tx.execute(
            "UPDATE merge_queue
             SET status = 'promoted', details_json = ?2, updated_at = ?3
             WHERE id = ?1",
            params![entry_id, details_json, now],
        )?;
        let mut paths = promoted_paths.to_vec();
        paths.sort();
        paths.dedup();
        let paths_json =
            serde_json::to_string(&paths).expect("serializing exposure paths cannot fail");
        tx.execute(
            "INSERT INTO entry_path_exposures (
                 queue_entry_id, promotion_sha, paths_json, created_at, state
             ) VALUES (?1, ?2, ?3, ?4, 'outstanding')",
            params![entry_id, integration_commit, paths_json, now],
        )?;
        Self::record_promotion_representation(
            &tx,
            session_id,
            &session_head,
            integration_commit,
            integration_ref,
            &paths,
            now,
        )?;
        update_accepted_checkpoint(
            &tx,
            session_id,
            &session_head,
            integration_commit,
            &integration_tree,
            entry_id,
            now,
        )?;
        insert_event(
            &tx,
            now,
            "merge.promoted",
            Some(session_id),
            Some(details_json),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Claim an integration commit this session already produced, for an entry
    /// that is still mid-flight.
    ///
    /// A submit whose response was lost can leave the ref advanced while its row
    /// is `simulating`, and queue revalidation may then supersede that row -- so
    /// the commit ends up on integration with nothing claiming it (issue #135).
    /// Unlike [`Self::record_merge_promotion`] this accepts a row that is not
    /// `verified`, because the verification already happened in the attempt that
    /// produced the commit.
    pub fn record_recovered_promotion(
        &mut self,
        entry_id: i64,
        integration_commit: &str,
        integration_tree: &str,
        integration_ref: &str,
        promoted_paths: &[String],
        details_json: &str,
    ) -> Result<(), BrokerError> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        let entry = tx
            .query_row(
                "SELECT session_id, head_commit
                 FROM merge_queue
                 WHERE id = ?1 AND status IN ('simulating', 'verified', 'superseded')",
                [entry_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((session_id, session_head)) = entry else {
            return Err(BrokerError::SessionNotFound(entry_id));
        };
        tx.execute(
            "UPDATE merge_queue
             SET status = 'promoted', merged_tree = ?2, details_json = ?3, updated_at = ?4
             WHERE id = ?1",
            params![entry_id, integration_tree, details_json, now],
        )?;
        Self::record_promotion_representation(
            &tx,
            session_id,
            &session_head,
            integration_commit,
            integration_ref,
            promoted_paths,
            now,
        )?;
        update_accepted_checkpoint(
            &tx,
            session_id,
            &session_head,
            integration_commit,
            integration_tree,
            entry_id,
            now,
        )?;
        insert_event(
            &tx,
            now,
            "merge.promoted",
            Some(session_id),
            Some(details_json),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Record the exact rewritten landing produced by broker promotion.
    ///
    /// Promotion changes the commit identity even though the merged tree is
    /// the verified session contribution. The representation ledger preserves
    /// that relationship for cleanup, where ancestry alone cannot prove a
    /// single-parent rewrite landed.
    pub(super) fn record_promotion_representation(
        tx: &Transaction<'_>,
        session_id: i64,
        session_head: &str,
        integration_commit: &str,
        integration_ref: &str,
        promoted_paths: &[String],
        now: i64,
    ) -> Result<(), BrokerError> {
        let mut paths = promoted_paths.to_vec();
        paths.sort();
        paths.dedup();
        let paths_json =
            serde_json::to_string(&paths).expect("serializing representation paths cannot fail");
        let evidence = format!(
            "broker promotion landed session head {session_head} as {integration_commit} on {integration_ref}"
        );
        // A promotion records the local integration landing, which carries no
        // pull request. A row already naming a PR merge is the stronger claim:
        // the integration commit is not an ancestor of the default branch, so
        // overwriting one with the other turns a session that demonstrably
        // landed upstream into unproven provenance and blocks its cleanup. The
        // guard is on the UPDATE rather than the INSERT because the first
        // writer may legitimately be either lane.
        tx.execute(
            "INSERT INTO session_representations (
                 session_id, session_head, representing_commit, representing_ref,
                 discovery, pr_number, paths_json, evidence, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, ?8)
             ON CONFLICT(session_id, session_head) DO UPDATE SET
                 representing_commit = excluded.representing_commit,
                 representing_ref = excluded.representing_ref,
                 discovery = excluded.discovery,
                 pr_number = excluded.pr_number,
                 paths_json = excluded.paths_json,
                 evidence = excluded.evidence,
                 created_at = excluded.created_at
             WHERE session_representations.pr_number IS NULL",
            params![
                session_id,
                session_head,
                integration_commit,
                integration_ref,
                RepresentationDiscovery::MergeTime.as_str(),
                paths_json,
                evidence,
                now,
            ],
        )?;
        Ok(())
    }

    /// Mark one simulated queue entry superseded because normalized replay
    /// proved it content-empty, and advance its session's accepted
    /// contribution checkpoint in the same SQLite transaction.
    pub fn record_content_empty_supersession(
        &mut self,
        entry_id: i64,
        integration_commit: &str,
        integration_tree: &str,
        details_json: &str,
    ) -> Result<(), BrokerError> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        let entry = tx
            .query_row(
                "SELECT session_id, head_commit
                 FROM merge_queue WHERE id = ?1 AND status = 'simulating'",
                [entry_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((session_id, session_head)) = entry else {
            return Err(BrokerError::SessionNotFound(entry_id));
        };
        tx.execute(
            "UPDATE merge_queue
             SET status = 'superseded', merged_tree = ?2,
                 details_json = ?3, updated_at = ?4
             WHERE id = ?1",
            params![entry_id, integration_tree, details_json, now],
        )?;
        update_accepted_checkpoint(
            &tx,
            session_id,
            &session_head,
            integration_commit,
            integration_tree,
            entry_id,
            now,
        )?;
        insert_event(
            &tx,
            now,
            "merge.superseded",
            Some(session_id),
            Some(details_json),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn merge_queue(&self) -> Result<Vec<MergeQueueEntry>, BrokerError> {
        let mut stmt = self.conn.prepare(&format!("{MERGE_SELECT} ORDER BY id"))?;
        let rows = stmt.query_map([], merge_from_row)?;
        let mut entries = Vec::new();
        for row in rows {
            entries.push(row??);
        }
        Ok(entries)
    }

    /// One exact merge queue entry for read-only projections.
    pub fn merge_queue_entry(&self, entry_id: i64) -> Result<MergeQueueEntry, BrokerError> {
        self.conn
            .query_row(
                &format!("{MERGE_SELECT} WHERE id = ?1"),
                [entry_id],
                merge_from_row,
            )
            .optional()?
            .ok_or(BrokerError::SessionNotFound(entry_id))?
    }

    pub fn current_merge_queue(&self) -> Result<Vec<MergeQueueEntry>, BrokerError> {
        let mut stmt = self.conn.prepare(&format!(
            "{MERGE_SELECT}
             WHERE status IN ('submitted', 'simulating', 'conflict', 'verified')
             ORDER BY id"
        ))?;
        let rows = stmt.query_map([], merge_from_row)?;
        rows.map(|row| row?).collect()
    }

    /// The commit a promoted or externally-landed entry claims for this exact
    /// session and head, newest first, so the caller can check ancestry.
    ///
    /// `broker status` asks this once per live session. Selecting on
    /// `(session_id, head_commit, status)` in SQL rather than loading the whole
    /// queue and filtering in Rust keeps the rows read proportional to the
    /// answer rather than to the queue, which on a busy repository is hundreds
    /// of rows read once per session.
    pub fn latest_representation_for_session(
        &self,
        session_id: i64,
        head_commit: &str,
    ) -> Result<Option<String>, BrokerError> {
        let details = self
            .conn
            .query_row(
                "SELECT details_json FROM merge_queue
                 WHERE session_id = ?1
                   AND head_commit = ?2
                   AND status IN ('promoted', 'externally_landed')
                 ORDER BY id DESC
                 LIMIT 1",
                rusqlite::params![session_id, head_commit],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?;
        Ok(details.and_then(|json| crate::broker::details_string_value(json.as_deref(), "commit")))
    }

    pub fn latest_merge_queue_for_session(
        &self,
        session_id: i64,
    ) -> Result<Option<MergeQueueEntry>, BrokerError> {
        self.conn
            .query_row(
                &format!("{MERGE_SELECT} WHERE session_id = ?1 ORDER BY id DESC LIMIT 1"),
                [session_id],
                merge_from_row,
            )
            .optional()?
            .transpose()
    }

    /// The latest queue entry per session, for all of `session_ids` at once.
    ///
    /// `broker status` needs this for every live session, and doing it with
    /// [`Self::latest_merge_queue_for_session`] is one `SELECT ... ORDER BY id
    /// DESC LIMIT 1` per session on a connection with a 5s busy timeout — an
    /// N+1 that gets linearly slower exactly when many sessions are live, which
    /// is when status matters most.
    ///
    /// The subquery keeps the "latest per session" semantics without relying on
    /// SQLite's bare-columns-in-GROUP-BY behaviour, which is only defined for
    /// a bare column of an aggregate and is not something to depend on for a
    /// 14-column row. Order is not guaranteed; callers that care sort.
    pub fn latest_merge_queue_for_sessions(
        &self,
        session_ids: &[i64],
    ) -> Result<Vec<MergeQueueEntry>, BrokerError> {
        if session_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat_n("?", session_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "{MERGE_SELECT} WHERE id IN (\
               SELECT MAX(id) FROM merge_queue \
               WHERE session_id IN ({placeholders}) GROUP BY session_id)"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(session_ids), merge_from_row)?;
        let mut entries = Vec::new();
        for row in rows {
            entries.push(row??);
        }
        Ok(entries)
    }

    pub fn terminal_merge_queue_counts(
        &self,
    ) -> Result<Vec<crate::MergeQueueStatusCount>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT status, COUNT(*) FROM merge_queue
             WHERE status IN ('promoted', 'externally_landed', 'rejected', 'superseded')
             GROUP BY status",
        )?;
        let rows = stmt.query_map([], |row| {
            let value: String = row.get(0)?;
            let status = MergeStatus::parse(&value).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
            Ok(crate::MergeQueueStatusCount {
                status,
                count: row.get::<_, i64>(1)?.max(0) as usize,
            })
        })?;
        let mut counts = rows.collect::<Result<Vec<_>, _>>()?;
        counts.sort_by_key(|item| match item.status {
            MergeStatus::Promoted => 0,
            MergeStatus::ExternallyLanded => 1,
            MergeStatus::Rejected => 2,
            MergeStatus::Superseded => 3,
            _ => 4,
        });
        Ok(counts)
    }

    pub fn merge_queue_history_page(
        &self,
        limit: u32,
        before_id: Option<i64>,
    ) -> Result<crate::MergeQueueHistoryPage, BrokerError> {
        const MAX_LIMIT: u32 = 200;
        if !(1..=MAX_LIMIT).contains(&limit) {
            return Err(BrokerError::InvalidMergeQueueHistoryLimit {
                limit,
                maximum: MAX_LIMIT,
            });
        }
        let mut stmt = self.conn.prepare(&format!(
            "{MERGE_SELECT}
             WHERE status IN ('promoted', 'externally_landed', 'rejected', 'superseded')
               AND (?1 IS NULL OR id < ?1)
             ORDER BY id DESC LIMIT ?2"
        ))?;
        let rows = stmt.query_map(
            rusqlite::params![before_id, i64::from(limit) + 1],
            merge_from_row,
        )?;
        let mut entries = rows.map(|row| row?).collect::<Result<Vec<_>, _>>()?;
        let has_more = entries.len() > limit as usize;
        entries.truncate(limit as usize);
        let next_before_id = has_more
            .then(|| entries.last().map(|entry| entry.id))
            .flatten();
        Ok(crate::MergeQueueHistoryPage {
            schema_version: crate::MERGE_QUEUE_HISTORY_SCHEMA_VERSION,
            entries,
            terminal_counts: self.terminal_merge_queue_counts()?,
            next_before_id,
        })
    }

    /// Newest-first bounded input for repository-quality recommendation
    /// producers. Unlike the operator history page, this includes conflict
    /// rows and reads no task or command data.
    pub(crate) fn recent_merge_history(
        &self,
        limit: usize,
    ) -> Result<Vec<MergeQueueEntry>, BrokerError> {
        let limit = limit.clamp(1, crate::RECOMMENDATION_HISTORY_LIMIT);
        let mut stmt = self
            .conn
            .prepare(&format!("{MERGE_SELECT} ORDER BY id DESC LIMIT ?1"))?;
        let rows = stmt.query_map([limit as i64], merge_from_row)?;
        rows.map(|row| row?).collect()
    }

    pub(crate) fn recent_gate_results_for_recommendations(
        &self,
        limit: usize,
    ) -> Result<Vec<GateResult>, BrokerError> {
        let limit = limit.clamp(1, crate::RECOMMENDATION_GATE_HISTORY_LIMIT);
        let mut stmt = self.conn.prepare(&format!(
            "{GATE_RESULT_SELECT} WHERE cleared_at IS NULL ORDER BY id DESC LIMIT ?1"
        ))?;
        let rows = stmt.query_map([limit as i64], gate_result_from_row)?;
        rows.map(|row| row?).collect()
    }

    pub(crate) fn recent_leases_for_recommendations(
        &self,
        limit: usize,
    ) -> Result<Vec<Lease>, BrokerError> {
        let limit = limit.clamp(1, crate::RECOMMENDATION_HISTORY_LIMIT);
        let mut stmt = self
            .conn
            .prepare(&format!("{LEASE_SELECT} ORDER BY id DESC LIMIT ?1"))?;
        let rows = stmt.query_map([limit as i64], lease_from_row)?;
        rows.map(|row| row?).collect()
    }

    /// Move a reviewed session ownership checkpoint and journal the exact
    /// transition in one SQLite transaction. Git preservation happens before
    /// this call, so a database failure can only leave an extra safe ref.
    pub fn reanchor_session_checkpoint(
        &mut self,
        session_id: i64,
        expected_old: &str,
        new_checkpoint: &str,
        event_payload: &str,
    ) -> Result<(), BrokerError> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        let actual = tx
            .query_row(
                "SELECT accepted_session_head FROM sessions WHERE id = ?1",
                [session_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .ok_or(BrokerError::SessionNotFound(session_id))?;
        if actual.as_deref() != Some(expected_old) {
            return Err(BrokerError::SessionCheckpointChanged {
                session_id,
                expected: expected_old.to_string(),
                actual: actual.unwrap_or_else(|| "<missing>".into()),
            });
        }
        let updated = tx.execute(
            "UPDATE sessions
             SET accepted_session_head = ?2, diff_base = ?2,
                 updated_at = ?3, last_activity_at = ?3
             WHERE id = ?1 AND accepted_session_head = ?4",
            params![session_id, new_checkpoint, now, expected_old],
        )?;
        if updated != 1 {
            return Err(BrokerError::SessionCheckpointChanged {
                session_id,
                expected: expected_old.to_string(),
                actual: "<changed concurrently>".into(),
            });
        }
        insert_event(
            &tx,
            now,
            crate::events::SESSION_CHECKPOINT_REANCHORED,
            Some(session_id),
            Some(event_payload),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Advance the durable session baseline after a successful explicit
    /// rebase. Future repairs must only replay work created after this base.
    pub fn set_session_diff_base(
        &mut self,
        session_id: i64,
        diff_base: &str,
    ) -> Result<(), BrokerError> {
        self.conn.execute(
            "UPDATE sessions SET diff_base = ?2, updated_at = ?3 WHERE id = ?1",
            params![session_id, diff_base, now_ms()],
        )?;
        Ok(())
    }

    /// Persist the complete reconciliation plan before moving its Git ref.
    /// This is phase one of the crash-recoverable ref/database update.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn prepare_integration_reconciliation(
        &mut self,
        branch: &str,
        upstream_ref: &str,
        local_main: &str,
        old_integration: &str,
        upstream_commit: &str,
        new_integration: &str,
        plan_digest: &str,
        updates: &[ReconciliationQueueUpdate],
    ) -> Result<(), BrokerError> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO integration_reconciliation_intent
                (id, branch, upstream_ref, local_main_commit,
                 old_integration, upstream_commit, new_integration, plan_digest, created_at)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                branch,
                upstream_ref,
                local_main,
                old_integration,
                upstream_commit,
                new_integration,
                plan_digest,
                now,
            ],
        )?;
        for update in updates {
            tx.execute(
                "INSERT INTO integration_reconciliation_intent_entries
                    (queue_entry_id, status, merged_tree, details_json,
                     classification, old_merge_commit, upstream_landing,
                     replayed_commit)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    update.queue_entry_id,
                    update.status.as_str(),
                    update.merged_tree,
                    update.details_json,
                    update.classification,
                    update.old_merge_commit,
                    update.upstream_landing,
                    update.replayed_commit,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn prepared_integration_reconciliation(
        &self,
    ) -> Result<Option<PreparedIntegrationReconciliation>, BrokerError> {
        self.conn
            .query_row(
                "SELECT branch, upstream_ref, local_main_commit,
                        old_integration, upstream_commit, new_integration, plan_digest
                 FROM integration_reconciliation_intent WHERE id = 1",
                [],
                |row| {
                    Ok(PreparedIntegrationReconciliation {
                        branch: row.get(0)?,
                        upstream_ref: row.get(1)?,
                        local_main: row.get(2)?,
                        old_integration: row.get(3)?,
                        upstream_commit: row.get(4)?,
                        new_integration: row.get(5)?,
                        plan_digest: row.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Phase two: apply all queue rows, audit rows, and events and remove
    /// the durable intent in the same SQLite transaction.
    pub(crate) fn finalize_integration_reconciliation(&mut self) -> Result<(), BrokerError> {
        let now = now_ms();
        let tx = self.conn.transaction()?;
        let prepared = tx.query_row(
            "SELECT branch, upstream_ref, local_main_commit,
                    old_integration, upstream_commit, new_integration, plan_digest
             FROM integration_reconciliation_intent WHERE id = 1",
            [],
            |row| {
                Ok(PreparedIntegrationReconciliation {
                    branch: row.get(0)?,
                    upstream_ref: row.get(1)?,
                    local_main: row.get(2)?,
                    old_integration: row.get(3)?,
                    upstream_commit: row.get(4)?,
                    new_integration: row.get(5)?,
                    plan_digest: row.get(6)?,
                })
            },
        )?;
        let raw_updates = {
            let mut stmt = tx.prepare(
                "SELECT queue_entry_id, status, merged_tree, details_json,
                        classification, old_merge_commit, upstream_landing,
                        replayed_commit
                 FROM integration_reconciliation_intent_entries
                 ORDER BY queue_entry_id",
            )?;
            stmt.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
        };
        let updates = raw_updates
            .into_iter()
            .map(
                |(
                    queue_entry_id,
                    status,
                    merged_tree,
                    details_json,
                    classification,
                    old_merge_commit,
                    upstream_landing,
                    replayed_commit,
                )| {
                    Ok(ReconciliationQueueUpdate {
                        queue_entry_id,
                        status: MergeStatus::parse(&status)?,
                        merged_tree,
                        details_json,
                        classification,
                        old_merge_commit,
                        upstream_landing,
                        replayed_commit,
                    })
                },
            )
            .collect::<Result<Vec<_>, BrokerError>>()?;

        tx.execute(
            "INSERT INTO integration_reconciliations
                (upstream_ref, local_main_commit, old_integration,
                 upstream_commit, new_integration, plan_digest, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                prepared.upstream_ref,
                prepared.local_main,
                prepared.old_integration,
                prepared.upstream_commit,
                prepared.new_integration,
                prepared.plan_digest,
                now,
            ],
        )?;
        let reconciliation_id = tx.last_insert_rowid();
        for update in updates {
            let session_id: i64 = tx.query_row(
                "SELECT session_id FROM merge_queue WHERE id = ?1",
                [update.queue_entry_id],
                |row| row.get(0),
            )?;
            tx.execute(
                "UPDATE merge_queue
                 SET status = ?2,
                     merged_tree = COALESCE(?3, merged_tree),
                     details_json = ?4,
                     updated_at = ?5
                 WHERE id = ?1",
                params![
                    update.queue_entry_id,
                    update.status.as_str(),
                    update.merged_tree,
                    update.details_json,
                    now,
                ],
            )?;
            if update.status == MergeStatus::ExternallyLanded {
                insert_event(
                    &tx,
                    now,
                    "merge.externally_landed",
                    Some(session_id),
                    Some(&update.details_json),
                )?;
                let resolution_sha = update
                    .upstream_landing
                    .as_deref()
                    .unwrap_or(&prepared.upstream_commit);
                let resolution_evidence = format!(
                    "integration reconciliation classified queue entry {} as {} against {} at {}",
                    update.queue_entry_id,
                    update.classification,
                    prepared.upstream_ref,
                    prepared.upstream_commit
                );
                let resolved = tx.execute(
                    "UPDATE entry_path_exposures
                     SET state = 'resolved', resolved_at = ?2,
                         resolution_kind = 'external_reconciliation',
                         resolution_sha = ?3, resolution_evidence = ?4
                     WHERE queue_entry_id = ?1 AND state = 'outstanding'",
                    params![
                        update.queue_entry_id,
                        now,
                        resolution_sha,
                        resolution_evidence
                    ],
                )?;
                if resolved > 0 {
                    tx.execute(
                        "UPDATE advisories
                         SET resolution_state = 'resolved', resolved_at = ?2,
                             resolution_evidence = ?3
                         WHERE queue_entry_id = ?1 AND resolution_state = 'outstanding'",
                        params![update.queue_entry_id, now, resolution_evidence],
                    )?;
                    tx.execute(
                        "UPDATE advisory_delivery_metrics
                         SET acted_at = COALESCE(acted_at, ?2),
                             action = 'publication_resolved'
                         WHERE advisory_id IN (
                             SELECT id FROM advisories WHERE queue_entry_id = ?1
                         )",
                        params![update.queue_entry_id, now],
                    )?;
                }
            } else if update.status == MergeStatus::Promoted
                && let Some(replayed_commit) = update.replayed_commit.as_deref()
            {
                // A reconciliation rebase can change commit identity without
                // ending publication exposure. Retarget the same entry-level
                // record and leave its state outstanding.
                tx.execute(
                    "UPDATE entry_path_exposures
                     SET promotion_sha = ?2
                     WHERE queue_entry_id = ?1 AND state = 'outstanding'",
                    params![update.queue_entry_id, replayed_commit],
                )?;
            }
            tx.execute(
                "INSERT INTO integration_reconciliation_entries
                    (reconciliation_id, queue_entry_id, classification,
                     old_merge_commit, upstream_landing, replayed_commit,
                     details_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    reconciliation_id,
                    update.queue_entry_id,
                    update.classification,
                    update.old_merge_commit,
                    update.upstream_landing,
                    update.replayed_commit,
                    update.details_json,
                ],
            )?;
        }
        tx.execute("DELETE FROM integration_reconciliation_intent_entries", [])?;
        tx.execute(
            "DELETE FROM integration_reconciliation_intent WHERE id = 1",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn abort_integration_reconciliation(&mut self) -> Result<(), BrokerError> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM integration_reconciliation_intent_entries", [])?;
        tx.execute(
            "DELETE FROM integration_reconciliation_intent WHERE id = 1",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    // ── Session representation ───────────────────────────────────────

    /// The recorded representation for one session head, if any.
    ///
    /// Keyed by head rather than by session: a session that commits further
    /// work after its pull request merged is not represented by that merge, and
    /// must not inherit its record.
    pub fn session_representation(
        &self,
        session_id: i64,
        session_head: &str,
    ) -> Result<Option<SessionRepresentation>, BrokerError> {
        self.conn
            .query_row(
                &format!(
                    "{SESSION_REPRESENTATION_SELECT} \
                     WHERE session_id = ?1 AND session_head = ?2"
                ),
                params![session_id, session_head],
                session_representation_from_row,
            )
            .optional()?
            .transpose()
    }

    /// Record that a session head is represented on the default branch.
    ///
    /// Idempotent on `(session_id, session_head)`: recording the same landing
    /// twice is a no-op rather than an error, so a retried merge hook or a
    /// re-run scan does not fail.
    pub fn record_session_representation(
        &mut self,
        record: &NewSessionRepresentation,
    ) -> Result<SessionRepresentation, BrokerError> {
        self.conn.execute(
            "INSERT INTO session_representations (
                 session_id, session_head, representing_commit, representing_ref,
                 discovery, pr_number, paths_json, evidence, created_at
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(session_id, session_head) DO UPDATE SET
                 representing_commit = excluded.representing_commit,
                 representing_ref = excluded.representing_ref,
                 discovery = excluded.discovery,
                 pr_number = excluded.pr_number,
                 paths_json = excluded.paths_json,
                 evidence = excluded.evidence",
            params![
                record.session_id,
                record.session_head,
                record.representing_commit,
                record.representing_ref,
                record.discovery.as_str(),
                record.pr_number,
                record.paths_json,
                record.evidence,
                now_ms(),
            ],
        )?;
        Ok(self
            .session_representation(record.session_id, &record.session_head)?
            .expect("representation just written"))
    }

    // ── PR watch state ───────────────────────────────────────────────

    /// Fetch the durable cursor for one PR follow-up watch.
    pub fn pr_watch_state(
        &self,
        target_branch: &str,
        pr_number: i64,
    ) -> Result<Option<PrWatchState>, BrokerError> {
        self.conn
            .query_row(
                &format!("{PR_WATCH_SELECT} WHERE target_branch = ?1 AND pr_number = ?2"),
                params![target_branch, pr_number],
                pr_watch_from_row,
            )
            .optional()?
            .transpose()
    }

    /// Insert or update one PR follow-up cursor.
    pub fn upsert_pr_watch_state(
        &mut self,
        state: &NewPrWatchState,
    ) -> Result<PrWatchState, BrokerError> {
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO pr_watch_state (
                 target_branch, pr_number, activity_fingerprint, marker,
                 last_dispatch_at, last_agent_session_id, updated_at
             )
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(target_branch, pr_number) DO UPDATE SET
                 activity_fingerprint = excluded.activity_fingerprint,
                 marker = excluded.marker,
                 last_dispatch_at = excluded.last_dispatch_at,
                 last_agent_session_id = excluded.last_agent_session_id,
                 updated_at = excluded.updated_at",
            params![
                state.target_branch,
                state.pr_number,
                state.activity_fingerprint,
                state.marker,
                state.last_dispatch_at,
                state.last_agent_session_id,
                now,
            ],
        )?;
        Ok(self
            .pr_watch_state(&state.target_branch, state.pr_number)?
            .expect("upserted pr_watch_state row should be readable"))
    }
}
