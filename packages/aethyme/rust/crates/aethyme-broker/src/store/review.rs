use super::*;

impl BrokerStore {
    // ── review lifecycle ─────────────────────────────────────────────

    pub(crate) fn create_review_lifecycle(
        &mut self,
        lifecycle: &NewReviewLifecycle,
        now: i64,
    ) -> Result<(ReviewLifecycle, bool), BrokerError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = tx
            .query_row(
                &format!("{REVIEW_LIFECYCLE_SELECT} WHERE session_id = ?1 AND active = 1"),
                [lifecycle.session_id],
                review_lifecycle_from_row,
            )
            .optional()?
            .transpose()?;
        if let Some(existing) = existing {
            let same = existing.repository == lifecycle.repository
                && existing.target_branch == lifecycle.target_branch
                && existing.pr_number == lifecycle.pr_number
                && existing.commit_sha == lifecycle.commit_sha;
            if !same {
                return Err(BrokerError::ReviewLifecycleIdentityConflict);
            }
            tx.execute(
                "INSERT INTO pr_watch_state (
                     target_branch, pr_number, activity_fingerprint, marker,
                     last_agent_session_id, updated_at
                 ) VALUES (?1, ?2, ?3, 'review_lifecycle', ?4, ?5)
                 ON CONFLICT(target_branch, pr_number) DO UPDATE SET
                     activity_fingerprint = excluded.activity_fingerprint,
                     marker = excluded.marker,
                     last_agent_session_id = excluded.last_agent_session_id,
                     updated_at = excluded.updated_at",
                params![
                    lifecycle.target_branch,
                    lifecycle.pr_number,
                    lifecycle.evidence_digest,
                    lifecycle.session_id,
                    now,
                ],
            )?;
            tx.commit()?;
            return Ok((existing, false));
        }
        let existing_pr = tx
            .query_row(
                &format!(
                    "{REVIEW_LIFECYCLE_SELECT} WHERE repository = ?1 AND pr_number = ?2 AND active = 1"
                ),
                params![lifecycle.repository, lifecycle.pr_number],
                review_lifecycle_from_row,
            )
            .optional()?
            .transpose()?;
        if let Some(existing) = existing_pr {
            return Err(BrokerError::ReviewLifecyclePrOwned {
                repository: existing.repository,
                pr_number: existing.pr_number,
                session_id: existing.session_id,
            });
        }
        tx.execute(
            "INSERT INTO review_lifecycles (
                 session_id, repository, target_branch, pr_number, commit_sha,
                 state, generation, evidence_digest, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'draft_opened', 0, ?6, ?7, ?7)",
            params![
                lifecycle.session_id,
                lifecycle.repository,
                lifecycle.target_branch,
                lifecycle.pr_number,
                lifecycle.commit_sha,
                lifecycle.evidence_digest,
                now,
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO pr_watch_state (
                 target_branch, pr_number, activity_fingerprint, marker,
                 last_agent_session_id, updated_at
             ) VALUES (?1, ?2, ?3, 'review_lifecycle', ?4, ?5)
             ON CONFLICT(target_branch, pr_number) DO UPDATE SET
                 activity_fingerprint = excluded.activity_fingerprint,
                 marker = excluded.marker,
                 last_agent_session_id = excluded.last_agent_session_id,
                 updated_at = excluded.updated_at",
            params![
                lifecycle.target_branch,
                lifecycle.pr_number,
                lifecycle.evidence_digest,
                lifecycle.session_id,
                now,
            ],
        )?;
        tx.execute(
            "INSERT INTO review_lifecycle_transitions (
                 lifecycle_id, from_state, to_state, commit_sha,
                 evidence_digest, created_at
             ) VALUES (?1, NULL, 'draft_opened', ?2, ?3, ?4)",
            params![id, lifecycle.commit_sha, lifecycle.evidence_digest, now],
        )?;
        tx.commit()?;
        Ok((
            self.review_lifecycle_by_id(id)?
                .expect("inserted review lifecycle should be readable"),
            true,
        ))
    }

    pub(crate) fn reassign_review_lifecycle(
        &mut self,
        lifecycle_id: i64,
        from_session_id: i64,
        to_session_id: i64,
        reason_digest: &str,
        now: i64,
    ) -> Result<ReviewLifecycle, BrokerError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let lifecycle = tx.query_row(
            &format!("{REVIEW_LIFECYCLE_SELECT} WHERE id = ?1 AND active = 1"),
            [lifecycle_id],
            review_lifecycle_from_row,
        )??;
        let changed = tx.execute(
            "UPDATE review_lifecycles
             SET session_id = ?2, generation = generation + 1, updated_at = ?3
             WHERE id = ?1 AND session_id = ?4 AND active = 1",
            params![lifecycle_id, to_session_id, now, from_session_id],
        )?;
        if changed != 1 {
            return Err(BrokerError::ReviewLifecycleIdentityConflict);
        }
        tx.execute(
            "UPDATE pr_watch_state
             SET last_agent_session_id = ?1, updated_at = ?2
             WHERE target_branch = ?3 AND pr_number = ?4",
            params![
                to_session_id,
                now,
                lifecycle.target_branch,
                lifecycle.pr_number
            ],
        )?;
        insert_event(
            &tx,
            now,
            crate::events::REVIEW_LIFECYCLE_REASSIGNED,
            Some(to_session_id),
            Some(&crate::events::review_lifecycle_reassigned_payload(
                lifecycle_id,
                from_session_id,
                to_session_id,
                &lifecycle.repository,
                lifecycle.pr_number,
                reason_digest,
            )),
        )?;
        tx.commit()?;
        self.review_lifecycle_by_id(lifecycle_id)?
            .ok_or(BrokerError::ReviewLifecycleNotFound(to_session_id))
    }

    pub(crate) fn abandon_review_lifecycle(
        &mut self,
        lifecycle_id: i64,
        session_id: i64,
        reason_digest: &str,
        now: i64,
    ) -> Result<ReviewLifecycle, BrokerError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let lifecycle = tx.query_row(
            &format!("{REVIEW_LIFECYCLE_SELECT} WHERE id = ?1 AND active = 1"),
            [lifecycle_id],
            review_lifecycle_from_row,
        )??;
        let changed = tx.execute(
            "UPDATE review_lifecycles
             SET active = 0, abandoned_at = ?2, abandon_reason_digest = ?3,
                 generation = generation + 1, updated_at = ?2
             WHERE id = ?1 AND session_id = ?4 AND active = 1",
            params![lifecycle_id, now, reason_digest, session_id],
        )?;
        if changed != 1 {
            return Err(BrokerError::ReviewLifecycleIdentityConflict);
        }
        insert_event(
            &tx,
            now,
            crate::events::REVIEW_LIFECYCLE_ABANDONED,
            Some(session_id),
            Some(&crate::events::review_lifecycle_abandoned_payload(
                lifecycle_id,
                &lifecycle.repository,
                lifecycle.pr_number,
                reason_digest,
            )),
        )?;
        tx.commit()?;
        self.review_lifecycle_by_id(lifecycle_id)?
            .ok_or(BrokerError::ReviewLifecycleNotFound(session_id))
    }

    pub fn review_lifecycle_for_session(
        &self,
        session_id: i64,
    ) -> Result<Option<ReviewLifecycle>, BrokerError> {
        self.conn
            .query_row(
                &format!("{REVIEW_LIFECYCLE_SELECT} WHERE session_id = ?1 AND active = 1"),
                [session_id],
                review_lifecycle_from_row,
            )
            .optional()?
            .transpose()
    }

    pub fn review_lifecycle_for_pr(
        &self,
        repository: &str,
        pr_number: i64,
    ) -> Result<Option<ReviewLifecycle>, BrokerError> {
        self.conn
            .query_row(
                &format!(
                    "{REVIEW_LIFECYCLE_SELECT} WHERE repository = ?1 AND pr_number = ?2 AND active = 1"
                ),
                params![repository, pr_number],
                review_lifecycle_from_row,
            )
            .optional()?
            .transpose()
    }

    pub(super) fn review_lifecycle_by_id(
        &self,
        id: i64,
    ) -> Result<Option<ReviewLifecycle>, BrokerError> {
        self.conn
            .query_row(
                &format!("{REVIEW_LIFECYCLE_SELECT} WHERE id = ?1"),
                [id],
                review_lifecycle_from_row,
            )
            .optional()?
            .transpose()
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn transition_review_lifecycle(
        &mut self,
        id: i64,
        expected: ReviewLifecycleState,
        next: ReviewLifecycleState,
        queue_entry_id: Option<i64>,
        commit_sha: &str,
        evidence_digest: Option<&str>,
        operation_id: Option<i64>,
        now: i64,
    ) -> Result<ReviewLifecycle, BrokerError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "UPDATE review_lifecycles
             SET queue_entry_id = ?2, commit_sha = ?3, state = ?4,
                 generation = generation + 1,
                 evidence_digest = COALESCE(?5, evidence_digest),
                 unlock_operation_id = CASE WHEN ?4 = 'validation_unlocked'
                                            THEN ?6 ELSE unlock_operation_id END,
                 updated_at = ?7
             WHERE id = ?1 AND state = ?8",
            params![
                id,
                queue_entry_id,
                commit_sha,
                next.as_str(),
                evidence_digest,
                operation_id,
                now,
                expected.as_str(),
            ],
        )?;
        if changed != 1 {
            let actual: Option<String> = tx
                .query_row(
                    "SELECT state FROM review_lifecycles WHERE id = ?1",
                    [id],
                    |row| row.get(0),
                )
                .optional()?;
            return Err(match actual {
                Some(actual) => BrokerError::ReviewLifecycleStateChanged {
                    id,
                    expected: expected.as_str().into(),
                    actual,
                },
                None => BrokerError::ReviewLifecycleNotFound(id),
            });
        }
        tx.execute(
            "INSERT INTO review_lifecycle_transitions (
                 lifecycle_id, from_state, to_state, commit_sha,
                 queue_entry_id, evidence_digest, operation_id, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id,
                expected.as_str(),
                next.as_str(),
                commit_sha,
                queue_entry_id,
                evidence_digest,
                operation_id,
                now,
            ],
        )?;
        tx.commit()?;
        Ok(self
            .review_lifecycle_by_id(id)?
            .expect("transitioned review lifecycle should be readable"))
    }

    // ── review router ledger ─────────────────────────────────────────

    /// Record that the router asked for one review, or return the request that
    /// already exists for this exact (repository, pull request, dimension,
    /// head).
    ///
    /// The boolean says whether this call created the row. That is the whole
    /// point of the method: an executor that crashed between recording a
    /// request and starting the reviewer re-runs, sees `false`, and knows not
    /// to spawn a second one. Callers that treat a duplicate as an error would
    /// turn every crash into a stuck pull request.
    #[allow(clippy::too_many_arguments)]
    pub fn record_review_request(
        &mut self,
        repository: &str,
        pr_number: i64,
        review_type: &str,
        head_commit: &str,
        base_commit: Option<&str>,
        backend: &str,
        now: i64,
    ) -> Result<(ReviewRequest, bool), BrokerError> {
        self.record_review_request_with_trigger(
            repository,
            pr_number,
            review_type,
            head_commit,
            base_commit,
            backend,
            Some(ReviewTrigger::Manual),
            now,
        )
    }

    /// Record a request and the lifecycle trigger that caused it.
    ///
    /// The compatibility wrapper above treats a direct store call as an
    /// explicit/manual request. The router uses this method so the trigger it
    /// derived from the provider observation is persisted before any backend
    /// is invoked.
    #[allow(clippy::too_many_arguments)]
    pub fn record_review_request_with_trigger(
        &mut self,
        repository: &str,
        pr_number: i64,
        review_type: &str,
        head_commit: &str,
        base_commit: Option<&str>,
        backend: &str,
        trigger: Option<ReviewTrigger>,
        now: i64,
    ) -> Result<(ReviewRequest, bool), BrokerError> {
        let trigger = trigger.map(ReviewTrigger::as_str);
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = tx
            .query_row(
                &format!(
                    "{REVIEW_REQUEST_SELECT} WHERE repository = ?1 AND pr_number = ?2
                       AND review_type = ?3 AND head_commit = ?4"
                ),
                params![repository, pr_number, review_type, head_commit],
                review_request_from_row,
            )
            .optional()?
            .transpose()?;
        if let Some(existing) = existing {
            // A provider may finish before the router records its request.
            // Fill only Aethyme-owned request facts in that case; completion
            // facts belong to the provider and must survive reconciliation.
            if existing.requested_for_commit.is_none()
                && existing.state != ReviewRequestState::Waived
            {
                tx.execute(
                    "UPDATE review_requests
                        SET requested_for_commit = ?2, base_commit = ?3,
                            trigger = ?4, backend = ?5, requested_at = ?6,
                            updated_at = ?6
                      WHERE id = ?1",
                    params![existing.id, head_commit, base_commit, trigger, backend, now],
                )?;
                tx.commit()?;
                let reconciled = self
                    .review_request(existing.id)?
                    .expect("reconciled review request should be readable");
                return Ok((reconciled, false));
            }
            // A revivable row is one nobody was ever asked about -- a `gh` call
            // that failed, an executor that died before the handoff. Reusing it
            // is the only way past the unique index, and without it that single
            // failure would settle the dimension for this head forever. The row
            // is reset rather than duplicated so the ledger keeps one line per
            // review, and `backend` is refreshed because policy may have moved
            // since the attempt.
            if !existing.state.is_revivable() {
                return Ok((existing, false));
            }
            tx.execute(
                "UPDATE review_requests
                    SET state = 'requested', detail = NULL, backend = ?2,
                        requested_for_commit = ?3, base_commit = ?4,
                        trigger = ?5, requested_at = ?6,
                        completed_at = NULL, completed_for_commit = NULL,
                        verdict = NULL, reviewer_provider = NULL,
                        reviewer_model = NULL, updated_at = ?6
                  WHERE id = ?1",
                params![existing.id, backend, head_commit, base_commit, trigger, now],
            )?;
            tx.commit()?;
            let revived = self
                .review_request(existing.id)?
                .expect("just-revived review request should be readable");
            return Ok((revived, true));
        }
        tx.execute(
            "INSERT INTO review_requests (
                 repository, pr_number, review_type, head_commit,
                 requested_for_commit, base_commit, trigger, backend, state,
                 detail, requested_at, completed_at, completed_for_commit,
                 verdict, reviewer_provider, reviewer_model, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?4, ?7, ?8, ?5, 'requested', NULL,
                       ?6, NULL, NULL, NULL, NULL, NULL, ?6)",
            params![
                repository,
                pr_number,
                review_type,
                head_commit,
                backend,
                now,
                base_commit,
                trigger
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        let created = self
            .review_request(id)?
            .expect("just-inserted review request should be readable");
        Ok((created, true))
    }

    pub fn review_request(&self, id: i64) -> Result<Option<ReviewRequest>, BrokerError> {
        self.conn
            .query_row(
                &format!("{REVIEW_REQUEST_SELECT} WHERE id = ?1"),
                [id],
                review_request_from_row,
            )
            .optional()?
            .transpose()
    }

    /// Every review request or provider completion for one pull request,
    /// oldest fact first.
    ///
    /// Deliberately unfiltered: spend counts finished reviews and in-flight
    /// counts unfinished ones, so both callers need the whole history and a
    /// convenience filter here would only tempt one of them to use the wrong
    /// one.
    pub fn review_requests_for_pr(
        &self,
        repository: &str,
        pr_number: i64,
    ) -> Result<Vec<ReviewRequest>, BrokerError> {
        let mut statement = self.conn.prepare(&format!(
            "{REVIEW_REQUEST_SELECT} WHERE repository = ?1 AND pr_number = ?2
             ORDER BY COALESCE(requested_at, completed_at, updated_at), id"
        ))?;
        let rows = statement.query_map(params![repository, pr_number], review_request_from_row)?;
        let mut requests = Vec::new();
        for row in rows {
            requests.push(row??);
        }
        Ok(requests)
    }

    /// Every review request or provider completion in one repository, oldest
    /// fact first.
    ///
    /// This is what `review ledger` reads when no pull request is named. The
    /// ledger's whole purpose is answering "why was there no review" long after
    /// the fact, and that question is often asked about a repository rather
    /// than about one pull request somebody already has in mind.
    pub fn review_requests_for_repository(
        &self,
        repository: &str,
    ) -> Result<Vec<ReviewRequest>, BrokerError> {
        let mut statement = self.conn.prepare(&format!(
            "{REVIEW_REQUEST_SELECT} WHERE repository = ?1
             ORDER BY COALESCE(requested_at, completed_at, updated_at), id"
        ))?;
        let rows = statement
            .query_map(params![repository], review_request_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter().collect()
    }

    /// The most recent review request or completion of one dimension on one
    /// pull request.
    ///
    /// A reviewer reporting back knows which review it was asked to do, not
    /// which row id carries it, and the unique index means there is one row per
    /// head rather than one row per dimension. Newest wins because a reviewer
    /// that is reporting now was asked most recently; a report about an older
    /// head has to name that head explicitly.
    pub fn latest_review_request(
        &self,
        repository: &str,
        pr_number: i64,
        review_type: &str,
        head_commit: Option<&str>,
    ) -> Result<Option<ReviewRequest>, BrokerError> {
        let (clause, head): (&str, Option<&str>) = match head_commit {
            Some(head) => ("AND head_commit = ?4", Some(head)),
            None => ("AND ?4 IS NULL", None),
        };
        self.conn
            .query_row(
                &format!(
                    "{REVIEW_REQUEST_SELECT}
                      WHERE repository = ?1 AND pr_number = ?2 AND review_type = ?3 {clause}
                      ORDER BY COALESCE(requested_at, completed_at, updated_at) DESC, id DESC LIMIT 1"
                ),
                params![repository, pr_number, review_type, head],
                review_request_from_row,
            )
            .optional()?
            .transpose()
    }

    /// Reviews a provider refused, that nothing has re-asked for since.
    ///
    /// Every repository at once, because `broker status` is per-machine and the
    /// operator asking "why will this gate not clear" does not yet know which
    /// repository to name -- that is the question. The rows are the ledger's
    /// own: review type, pull request, head commit, when it was refused, and a
    /// `detail` that [`crate::ReviewRefusal::parse`] reads back into a
    /// classification plus the provider's words. Nothing new is stored (#173);
    /// what was missing was that the refusal was never written down at all.
    ///
    /// `NOT EXISTS` drops a refusal that a later request for the same
    /// dimension has superseded. A refusal on a head that has since been
    /// re-asked is history, and reporting it would tell an operator to go
    /// looking at a wall that is no longer there.
    ///
    /// `limit` bounds a view that already costs every caller of `broker
    /// status`; the newest refusals are the ones an operator can still act on.
    pub fn review_refusals(&self, limit: usize) -> Result<Vec<ReviewRequest>, BrokerError> {
        let mut statement = self.conn.prepare(&format!(
            "{REVIEW_REQUEST_SELECT}
              WHERE state = 'abandoned'
                AND detail IS NOT NULL
                AND NOT EXISTS (
                  SELECT 1 FROM review_requests newer
                   WHERE newer.repository = review_requests.repository
                     AND newer.pr_number = review_requests.pr_number
                     AND newer.review_type = review_requests.review_type
                     AND (COALESCE(newer.requested_at, newer.completed_at, newer.updated_at)
                              > COALESCE(review_requests.requested_at,
                                         review_requests.completed_at,
                                         review_requests.updated_at)
                          OR (COALESCE(newer.requested_at, newer.completed_at, newer.updated_at)
                                  = COALESCE(review_requests.requested_at,
                                             review_requests.completed_at,
                                             review_requests.updated_at)
                              AND newer.id > review_requests.id))
                )
              ORDER BY updated_at DESC, id DESC
              LIMIT ?1"
        ))?;
        let rows = statement
            .query_map(params![limit as i64], review_request_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        let rows = rows.into_iter().collect::<Result<Vec<_>, _>>()?;
        // The classification lives in `detail`, which also carries rule names
        // and debounce windows from every other path that abandons a row.
        // Filtering on what parses keeps this to refusals a provider actually
        // made.
        Ok(rows
            .into_iter()
            .filter(|row| {
                row.detail
                    .as_deref()
                    .is_some_and(|detail| crate::ReviewRefusal::parse(detail).is_some())
            })
            .collect())
    }

    /// Every request still occupying a concurrency slot in one repository.
    ///
    /// `ReviewRoute::max_concurrent` is a per-repository budget, not a
    /// per-pull-request one: four open pull requests must not each get their
    /// own security agent when the policy allows one. Reading this per pull
    /// request is what silently multiplies the cap by the number of open pull
    /// requests, so the scope of the query is the whole point of it.
    pub fn review_requests_in_flight(
        &self,
        repository: &str,
    ) -> Result<Vec<ReviewRequest>, BrokerError> {
        let mut statement = self.conn.prepare(&format!(
            "{REVIEW_REQUEST_SELECT} WHERE repository = ?1 AND state IN ('requested', 'running')
             ORDER BY COALESCE(requested_at, completed_at, updated_at), id"
        ))?;
        let rows = statement
            .query_map(params![repository], review_request_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter().collect()
    }

    /// What the router last saw of one pull request, if it has ever looked.
    pub fn pull_request_observation(
        &self,
        repository: &str,
        pr_number: i64,
    ) -> Result<Option<crate::PullRequestObservation>, BrokerError> {
        self.conn
            .query_row(
                "SELECT repository, pr_number, head_commit, base_ref, is_draft, state,
                        dismissed_reviews, observed_at, base_commit
                   FROM pull_request_observations
                  WHERE repository = ?1 AND pr_number = ?2",
                params![repository, pr_number],
                |row| {
                    Ok(crate::PullRequestObservation {
                        repository: row.get(0)?,
                        pr_number: row.get(1)?,
                        head_commit: row.get(2)?,
                        base_ref: row.get(3)?,
                        is_draft: row.get::<_, i64>(4)? != 0,
                        state: row.get(5)?,
                        dismissed_reviews: row.get(6)?,
                        observed_at: row.get(7)?,
                        base_commit: row.get(8)?,
                    })
                },
            )
            .optional()
            .map_err(BrokerError::from)
    }

    /// Remember this look, replacing the last one.
    ///
    /// Written *after* the tick has acted, never before. Recording the
    /// observation first would mean a tick that crashed mid-way had already
    /// declared the transition handled, and the next tick would derive
    /// `Scheduled` from its own unfinished work -- losing the event rather than
    /// retrying it. The ledger's unique index makes the retry harmless.
    pub fn record_pull_request_observation(
        &mut self,
        observation: &crate::PullRequestObservation,
    ) -> Result<(), BrokerError> {
        self.conn.execute(
            "INSERT INTO pull_request_observations (
                 repository, pr_number, head_commit, base_ref, is_draft, state,
                 dismissed_reviews, observed_at, base_commit
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT (repository, pr_number) DO UPDATE SET
                 head_commit = excluded.head_commit,
                 base_ref = excluded.base_ref,
                 base_commit = excluded.base_commit,
                 is_draft = excluded.is_draft,
                 state = excluded.state,
                 dismissed_reviews = excluded.dismissed_reviews,
                 observed_at = excluded.observed_at",
            params![
                observation.repository,
                observation.pr_number,
                observation.head_commit,
                observation.base_ref,
                i64::from(observation.is_draft),
                observation.state,
                observation.dismissed_reviews,
                observation.observed_at,
                observation.base_commit,
            ],
        )?;
        Ok(())
    }

    /// Every pull request the router has an observation for in one repository.
    ///
    /// This is what a sweep iterates: a pull request Aethyme has never looked
    /// at has no row here, which is why `review tick` takes its candidates from
    /// the provider and uses these only to decide what changed.
    pub fn observed_pull_requests(
        &self,
        repository: &str,
    ) -> Result<Vec<crate::PullRequestObservation>, BrokerError> {
        let mut statement = self.conn.prepare(
            "SELECT repository, pr_number, head_commit, base_ref, is_draft, state,
                    dismissed_reviews, observed_at, base_commit
               FROM pull_request_observations
              WHERE repository = ?1
              ORDER BY pr_number",
        )?;
        let rows = statement
            .query_map(params![repository], |row| {
                Ok(crate::PullRequestObservation {
                    repository: row.get(0)?,
                    pr_number: row.get(1)?,
                    head_commit: row.get(2)?,
                    base_ref: row.get(3)?,
                    is_draft: row.get::<_, i64>(4)? != 0,
                    state: row.get(5)?,
                    dismissed_reviews: row.get(6)?,
                    observed_at: row.get(7)?,
                    base_commit: row.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Move one request to a new state, recording why.
    pub fn set_review_request_state(
        &mut self,
        id: i64,
        state: ReviewRequestState,
        detail: Option<&str>,
        now: i64,
    ) -> Result<ReviewRequest, BrokerError> {
        if state == ReviewRequestState::Satisfied {
            return Err(BrokerError::InvalidEnumValue {
                field: "review_requests.satisfied",
                value: "completion facts are required; use complete_review_request".into(),
            });
        }
        let changed = self.conn.execute(
            "UPDATE review_requests
                SET state = ?2, detail = ?3, updated_at = ?4
              WHERE id = ?1",
            params![id, state.label(), detail, now],
        )?;
        if changed == 0 {
            return Err(BrokerError::InvalidEnumValue {
                field: "review_request_id",
                value: id.to_string(),
            });
        }
        Ok(self
            .review_request(id)?
            .expect("updated review request should be readable"))
    }

    /// Record the provider-owned facts for a completion of a requested row.
    ///
    /// Request facts are never rewritten here: the row id selects the request
    /// Aethyme recorded, while the provider supplies the completion commit,
    /// verdict, and identity. That separation is what lets a late result be
    /// attached to the head it actually reviewed.
    #[allow(clippy::too_many_arguments)]
    pub fn complete_review_request(
        &mut self,
        id: i64,
        completed_for_commit: &str,
        verdict: ReviewVerdict,
        reviewer_provider: &str,
        reviewer_model: Option<&str>,
        detail: Option<&str>,
        now: i64,
    ) -> Result<ReviewRequest, BrokerError> {
        let completed_for_commit = completed_for_commit.trim();
        let reviewer_provider = reviewer_provider.trim();
        let reviewer_model = reviewer_model
            .map(str::trim)
            .filter(|model| !model.is_empty());
        if completed_for_commit.is_empty() {
            return Err(BrokerError::InvalidEnumValue {
                field: "review_requests.completed_for_commit",
                value: "empty commit".into(),
            });
        }
        if reviewer_provider.is_empty() {
            return Err(BrokerError::InvalidEnumValue {
                field: "review_requests.reviewer_provider",
                value: "empty provider".into(),
            });
        }
        let changed = self.conn.execute(
            "UPDATE review_requests
                SET state = 'satisfied', detail = ?2, completed_at = ?3,
                    completed_for_commit = ?4, verdict = ?5,
                    reviewer_provider = ?6, reviewer_model = ?7,
                    updated_at = ?3
              WHERE id = ?1",
            params![
                id,
                detail,
                now,
                completed_for_commit,
                verdict.label(),
                reviewer_provider,
                reviewer_model
            ],
        )?;
        if changed == 0 {
            return Err(BrokerError::InvalidEnumValue {
                field: "review_request_id",
                value: id.to_string(),
            });
        }
        Ok(self
            .review_request(id)?
            .expect("completed review request should be readable"))
    }

    /// Record a provider completion that arrived without an Aethyme request.
    ///
    /// The effective `head_commit` identity is the completed commit solely so
    /// a later request for that same head can reconcile into this row. Request
    /// fields remain null and the trigger is `unsolicited`; no provenance is
    /// invented to make the row look like a routed request.
    #[allow(clippy::too_many_arguments)]
    pub fn record_unsolicited_review_completion(
        &mut self,
        repository: &str,
        pr_number: i64,
        review_type: &str,
        completed_for_commit: &str,
        verdict: ReviewVerdict,
        reviewer_provider: &str,
        reviewer_model: Option<&str>,
        detail: Option<&str>,
        now: i64,
    ) -> Result<(ReviewRequest, bool), BrokerError> {
        let completed_for_commit = completed_for_commit.trim();
        let reviewer_provider = reviewer_provider.trim();
        let reviewer_model = reviewer_model
            .map(str::trim)
            .filter(|model| !model.is_empty());
        if completed_for_commit.is_empty() {
            return Err(BrokerError::InvalidEnumValue {
                field: "review_requests.completed_for_commit",
                value: "empty commit".into(),
            });
        }
        if reviewer_provider.is_empty() {
            return Err(BrokerError::InvalidEnumValue {
                field: "review_requests.reviewer_provider",
                value: "empty provider".into(),
            });
        }

        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = tx
            .query_row(
                &format!(
                    "{REVIEW_REQUEST_SELECT} WHERE repository = ?1 AND pr_number = ?2
                       AND review_type = ?3 AND head_commit = ?4"
                ),
                params![repository, pr_number, review_type, completed_for_commit],
                review_request_from_row,
            )
            .optional()?
            .transpose()?;
        let (id, created) = match existing {
            Some(existing) => {
                tx.execute(
                    "UPDATE review_requests
                        SET state = 'satisfied', detail = ?2, completed_at = ?3,
                            completed_for_commit = ?4, verdict = ?5,
                            reviewer_provider = ?6, reviewer_model = ?7,
                            updated_at = ?3
                      WHERE id = ?1",
                    params![
                        existing.id,
                        detail,
                        now,
                        completed_for_commit,
                        verdict.label(),
                        reviewer_provider,
                        reviewer_model
                    ],
                )?;
                (existing.id, false)
            }
            None => {
                tx.execute(
                    "INSERT INTO review_requests (
                         repository, pr_number, review_type, head_commit,
                         requested_for_commit, base_commit, trigger, backend,
                         state, detail, requested_at, completed_at,
                         completed_for_commit, verdict, reviewer_provider,
                         reviewer_model, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, 'unsolicited',
                               'unsolicited', 'satisfied', ?5, NULL, ?6,
                               ?4, ?7, ?8, ?9, ?6)",
                    params![
                        repository,
                        pr_number,
                        review_type,
                        completed_for_commit,
                        detail,
                        now,
                        verdict.label(),
                        reviewer_provider,
                        reviewer_model
                    ],
                )?;
                (tx.last_insert_rowid(), true)
            }
        };
        tx.commit()?;
        let completed = self
            .review_request(id)?
            .expect("recorded review completion should be readable");
        Ok((completed, created))
    }

    /// Record that one dimension is excused at exactly this head.
    ///
    /// Creates the row when none exists, which is the difference between this
    /// and [`Self::set_review_request_state`] and the reason it is a separate
    /// method rather than a state argument. `review state` refuses to invent a
    /// row because it *reports* an outcome, and a report about a review nobody
    /// requested means the reporter and the router disagree. A waiver
    /// *decides*, and the dimension most worth excusing is precisely the one
    /// that never got requested -- a provider that refused on quota, a
    /// dispatch that never happened. Refusing there would leave the operator
    /// exactly where #172 found them.
    ///
    /// `backend` is the `waiver` sentinel, beside `record`, because no backend
    /// performed this and naming a real one would misattribute the decision to
    /// a reviewer.
    pub fn waive_review_request(
        &mut self,
        repository: &str,
        pr_number: i64,
        review_type: &str,
        head_commit: &str,
        detail: &str,
        now: i64,
    ) -> Result<ReviewRequest, BrokerError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = tx
            .query_row(
                &format!(
                    "{REVIEW_REQUEST_SELECT} WHERE repository = ?1 AND pr_number = ?2
                       AND review_type = ?3 AND head_commit = ?4"
                ),
                params![repository, pr_number, review_type, head_commit],
                review_request_from_row,
            )
            .optional()?
            .transpose()?;
        let id = match existing {
            Some(existing) => {
                tx.execute(
                    "UPDATE review_requests
                        SET state = 'waived', detail = ?2, updated_at = ?3
                      WHERE id = ?1",
                    params![existing.id, detail, now],
                )?;
                existing.id
            }
            None => {
                tx.execute(
                    "INSERT INTO review_requests (
                         repository, pr_number, review_type, head_commit,
                         requested_for_commit, base_commit, trigger, backend,
                         state, detail, requested_at, completed_at,
                         completed_for_commit, verdict, reviewer_provider,
                         reviewer_model, updated_at
                     ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, NULL, 'waiver',
                               'waived', ?5, NULL, NULL, NULL, NULL, NULL,
                               NULL, ?6)",
                    params![repository, pr_number, review_type, head_commit, detail, now],
                )?;
                tx.last_insert_rowid()
            }
        };
        tx.commit()?;
        Ok(self
            .review_request(id)?
            .expect("just-waived review request should be readable"))
    }

    // ── coordinated operations ───────────────────────────────────────

    pub fn create_coordinated_operation(
        &mut self,
        operation: &NewCoordinatedOperation,
    ) -> Result<CoordinatedOperation, BrokerError> {
        let agent_provenance_json =
            crate::session_holder::operation_agent_provenance(self, operation.session_id)?
                .to_string();
        let now = now_ms();
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO coordinated_operations (
                 session_id, provider, repository, scope, effect, status,
                 authorization_reason, command_json, pid, created_at, updated_at,
                 host_operation_id, identity_provenance, agent_provenance_json
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'prepared', ?6, ?7, ?8, ?9, ?9,
                       ?10, ?11, ?12)",
            params![
                operation.session_id,
                operation.provider.as_str(),
                operation.repository,
                operation.scope,
                operation.effect.as_str(),
                operation.authorization_reason,
                operation.command_json,
                operation.pid,
                now,
                operation.host_operation_id,
                operation.identity_provenance.as_str(),
                agent_provenance_json,
            ],
        )?;
        let id = tx.last_insert_rowid();
        let payload = crate::events::operation_payload(
            id,
            operation.provider,
            &operation.repository,
            &operation.scope,
            operation.effect,
            OperationStatus::Prepared,
            None,
        );
        insert_event(
            &tx,
            now,
            "operation.prepared",
            Some(operation.session_id),
            Some(&payload),
        )?;
        tx.commit()?;
        self.coordinated_operation(id)?
            .ok_or(BrokerError::CoordinatedOperationNotFound(id))
    }

    /// Attach durable queue-wait evidence while an operation is still
    /// prepared. The status guard makes a late diagnostic harmless if the
    /// operation acquired the lock between the observation and this update.
    pub fn annotate_prepared_operation(
        &mut self,
        id: i64,
        details_json: &str,
    ) -> Result<(), BrokerError> {
        self.conn.execute(
            "UPDATE coordinated_operations
             SET details_json = ?2, updated_at = ?3
             WHERE id = ?1 AND status = 'prepared'",
            params![id, details_json, now_ms()],
        )?;
        Ok(())
    }

    /// Refresh the liveness portion of a running operation without replacing
    /// the rest of its journal. Heartbeats are deliberately best-effort and
    /// guarded by the running status: a late heartbeat must never resurrect
    /// or overwrite the terminal outcome recorded by the operation owner.
    pub fn update_coordinated_operation_liveness(
        &mut self,
        id: i64,
        liveness: &serde_json::Value,
    ) -> Result<(), BrokerError> {
        let Some(operation) = self.coordinated_operation(id)? else {
            return Ok(());
        };
        if operation.status != OperationStatus::Running {
            return Ok(());
        }
        let mut details = operation
            .details_json
            .as_deref()
            .and_then(|details| serde_json::from_str::<serde_json::Value>(details).ok())
            .filter(serde_json::Value::is_object)
            .unwrap_or_else(|| serde_json::json!({}));
        details["operation_liveness"] = liveness.clone();
        self.conn.execute(
            "UPDATE coordinated_operations
             SET details_json = ?2, updated_at = ?3
             WHERE id = ?1 AND status = 'running'",
            params![id, details.to_string(), now_ms()],
        )?;
        Ok(())
    }

    pub fn coordinated_operation(
        &self,
        id: i64,
    ) -> Result<Option<CoordinatedOperation>, BrokerError> {
        self.conn
            .query_row(
                "SELECT id, session_id, provider, repository, scope, effect,
                        status, authorization_reason, command_json, pid,
                        exit_code, details_json,
                        created_at, updated_at, finished_at,
                        host_operation_id, identity_provenance, agent_provenance_json
                 FROM coordinated_operations WHERE id = ?1",
                [id],
                coordinated_operation_from_row,
            )
            .optional()?
            .transpose()
    }

    /// Find the repository journal row linked to a host-wide operation id.
    pub fn coordinated_operation_id_for_host_operation(
        &self,
        host_operation_id: &str,
    ) -> Result<Option<i64>, BrokerError> {
        self.conn
            .query_row(
                "SELECT id FROM coordinated_operations WHERE host_operation_id = ?1",
                [host_operation_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn coordinated_operations(&self) -> Result<Vec<CoordinatedOperation>, BrokerError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, provider, repository, scope, effect,
                    status, authorization_reason, command_json, pid,
                    exit_code, details_json,
                    created_at, updated_at, finished_at,
                    host_operation_id, identity_provenance, agent_provenance_json
             FROM coordinated_operations ORDER BY id",
        )?;
        let rows = stmt.query_map([], coordinated_operation_from_row)?;
        let mut operations = Vec::new();
        for row in rows {
            operations.push(row??);
        }
        Ok(operations)
    }

    /// Query one stable newest-first page of coordinated-operation history.
    ///
    /// Every value is bound, while the SQL shape contains only broker-owned
    /// column predicates. `before_id` is exclusive so a caller can pass
    /// `next_before_id` directly without duplicates.
    pub fn operation_history(
        &self,
        query: &OperationHistoryQuery,
    ) -> Result<OperationHistoryPage, BrokerError> {
        if query.limit == 0 || query.limit > MAX_OPERATION_HISTORY_LIMIT {
            return Err(BrokerError::InvalidOperationHistoryLimit {
                limit: query.limit,
                maximum: MAX_OPERATION_HISTORY_LIMIT,
            });
        }

        let mut sql = String::from(
            "SELECT id, session_id, provider, repository, scope, effect,
                    status, authorization_reason, command_json, pid,
                    exit_code, details_json,
                    created_at, updated_at, finished_at,
                    host_operation_id, identity_provenance, agent_provenance_json
             FROM coordinated_operations",
        );
        let mut clauses = Vec::new();
        let mut values = Vec::<rusqlite::types::Value>::new();
        if let Some(before_id) = query.before_id {
            clauses.push("id < ?");
            values.push(before_id.into());
        }
        if let Some(session_id) = query.session_id {
            clauses.push("session_id = ?");
            values.push(session_id.into());
        }
        if let Some(status) = query.status {
            clauses.push("status = ?");
            values.push(status.as_str().to_owned().into());
        }
        if let Some(repository) = &query.repository {
            clauses.push("repository = ?");
            values.push(repository.clone().into());
        }
        if let Some(provider) = query.provider {
            clauses.push("provider = ?");
            values.push(provider.as_str().to_owned().into());
        }
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        sql.push_str(" ORDER BY id DESC LIMIT ?");
        values.push((i64::from(query.limit) + 1).into());

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(values.iter()),
            coordinated_operation_from_row,
        )?;
        let mut operations = Vec::new();
        for row in rows {
            operations.push(row??);
        }
        let has_more = operations.len() > query.limit as usize;
        operations.truncate(query.limit as usize);
        let next_before_id = has_more
            .then(|| operations.last().map(|operation| operation.id))
            .flatten();
        Ok(OperationHistoryPage {
            operations,
            next_before_id,
        })
    }
}
