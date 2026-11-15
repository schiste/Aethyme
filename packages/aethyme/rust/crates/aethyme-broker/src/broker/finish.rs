use super::*;

/// Bound on the ownership re-proof `finish` makes before recording its
/// recovery proof when no landing deadline is set.
const FINISH_REPROOF_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

impl Broker {
    /// Retrieve the latest durable finish handoff for one session.
    pub fn latest_handoff_for_session(
        &self,
        session_id: i64,
    ) -> Result<SessionHandoffReport, BrokerOpError> {
        self.store.session(session_id)?;
        let event = self
            .store
            .latest_session_finished_event(session_id)?
            .ok_or(BrokerOpError::HandoffNotFoundForSession { session_id })?;
        Self::handoff_report(event)
    }

    /// Retrieve the newest durable finish handoff across every session
    /// registered for exactly this worktree path.
    pub fn latest_handoff_for_worktree(
        &self,
        worktree: &Path,
    ) -> Result<SessionHandoffReport, BrokerOpError> {
        let worktree = worktree.to_string_lossy().into_owned();
        let event = self
            .store
            .latest_worktree_finished_event(&worktree)?
            .ok_or_else(|| BrokerOpError::HandoffNotFoundForWorktree {
                worktree: worktree.clone(),
            })?;
        Self::handoff_report(event)
    }

    pub(super) fn finish_delivery(&self, entry: Option<&MergeQueueEntry>) -> FinishDelivery {
        let Some(entry) = entry else {
            return FinishDelivery::default();
        };
        let promoted = matches!(
            entry.status,
            MergeStatus::Promoted | MergeStatus::ExternallyLanded
        );
        let promotion_commit = details_string_value(entry.details_json.as_deref(), "commit");
        let published = entry.status == MergeStatus::ExternallyLanded
            || (promoted
                && promotion_commit.as_deref().is_some_and(|promotion| {
                    self.repo
                        .tracking_upstream()
                        .is_some_and(|(_, upstream)| self.repo.is_ancestor(promotion, &upstream))
                }));
        FinishDelivery {
            submitted: true,
            promoted,
            published,
            promotion_commit,
        }
    }

    pub(super) fn finish_leases(
        &self,
        session_id: i64,
        at_ms: i64,
    ) -> Result<Vec<FinishLease>, BrokerOpError> {
        let mut leases = self
            .store
            .session_leases(session_id)?
            .into_iter()
            .map(|lease| FinishLease {
                path: if Path::new(&lease.path).is_absolute() {
                    "<absolute-path-redacted>".into()
                } else {
                    lease.path
                },
                kind: lease.kind,
                state: if lease.released_at.is_some() {
                    FinishLeaseState::Released
                } else if lease
                    .expires_at
                    .is_some_and(|expires_at| expires_at <= at_ms)
                {
                    FinishLeaseState::Expired
                } else {
                    FinishLeaseState::Active
                },
                expires_at: lease.expires_at,
                released_at: lease.released_at,
            })
            .collect::<Vec<_>>();
        leases.sort_by(|a, b| {
            (
                a.path.as_str(),
                a.kind.as_str(),
                a.state,
                a.expires_at,
                a.released_at,
            )
                .cmp(&(
                    b.path.as_str(),
                    b.kind.as_str(),
                    b.state,
                    b.expires_at,
                    b.released_at,
                ))
        });
        Ok(leases)
    }

    pub(super) fn finish_last_gate(
        &self,
        session_id: i64,
    ) -> Result<Option<FinishGateRun>, BrokerOpError> {
        let Some(event) = self.store.latest_session_gate_event(session_id)? else {
            return Ok(None);
        };
        let Some(payload) = event
            .payload_json
            .as_deref()
            .and_then(|payload| serde_json::from_str::<serde_json::Value>(payload).ok())
        else {
            return Ok(None);
        };
        let Some(gate) = payload.get("gate").and_then(serde_json::Value::as_str) else {
            return Ok(None);
        };
        let Some(tree_hash) = payload.get("tree").and_then(serde_json::Value::as_str) else {
            return Ok(None);
        };
        let (status, cache_source) = if event.kind == crate::events::GATE_CACHED {
            let Some(status) = payload
                .get("cached_status")
                .and_then(serde_json::Value::as_str)
                .and_then(|status| GateStatus::parse(status).ok())
            else {
                return Ok(None);
            };
            (status, FinishGateCacheSource::CacheHit)
        } else {
            let Some(status) = event
                .kind
                .strip_prefix("gate.")
                .and_then(|status| GateStatus::parse(status).ok())
            else {
                return Ok(None);
            };
            (status, FinishGateCacheSource::Executed)
        };
        Ok(Some(FinishGateRun {
            gate: gate.to_string(),
            status,
            tree_hash: tree_hash.to_string(),
            recorded_at: event.ts,
            cache_source,
        }))
    }

    pub(super) fn finish_last_graph_integrity(
        &self,
        session_id: i64,
    ) -> Result<Option<FinishGraphIntegrity>, BrokerOpError> {
        let Some(event) = self
            .store
            .latest_session_graph_integrity_event(session_id)?
        else {
            return Ok(None);
        };
        let Some(payload) = event
            .payload_json
            .as_deref()
            .and_then(|payload| serde_json::from_str::<serde_json::Value>(payload).ok())
        else {
            return Ok(None);
        };
        let Some(status) = payload
            .get("status")
            .and_then(serde_json::Value::as_str)
            .and_then(crate::GraphIntegrityStatus::parse)
        else {
            return Ok(None);
        };
        let Some(tree_hash) = payload.get("tree").and_then(serde_json::Value::as_str) else {
            return Ok(None);
        };
        let Some(policy_digest) = payload.get("policy").and_then(serde_json::Value::as_str) else {
            return Ok(None);
        };
        let changed_paths = payload
            .get("changed_paths")
            .and_then(serde_json::Value::as_array)
            .map(|paths| {
                paths
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Ok(Some(FinishGraphIntegrity {
            status,
            tree_hash: tree_hash.to_string(),
            policy_digest: policy_digest.to_string(),
            engine_version: payload
                .get("engine_version")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            changed_paths,
            recorded_at: event.ts,
        }))
    }

    pub(super) fn finalize_finish_report(&self, report: &mut FinishReport) {
        report.pending_work = FinishPendingWork {
            present: !report.dirty_paths.is_empty() || report.unsubmitted_commits > 0,
            dirty_path_count: report.dirty_paths.len(),
            unsubmitted_commits: report.unsubmitted_commits,
            worktree_missing: report.pending_work.worktree_missing,
        };
        report.recommended_next_action = if report.cleanup.attempted && !report.cleanup.completed {
            report.cleanup.recovery_action.clone()
        } else {
            report.next_commands.first().cloned().or_else(|| {
                (report.delivery.published && !report.cleanup_safe)
                    .then(|| "aethyme broker status".into())
            })
        };
    }

    pub(super) fn persist_finish_report(
        &mut self,
        report: &FinishReport,
        checkout_cleanup_request: Option<CheckoutCleanupRequestPolicy>,
    ) -> Result<(), BrokerOpError> {
        let payload = crate::events::session_finished_payload(report);
        self.store.finish_session_with_cleanup_request(
            report.session_id,
            &payload,
            checkout_cleanup_request.map(|request| request.grace_hours),
            checkout_cleanup_request.is_some_and(|request| request.compact_checkout),
        )?;
        Ok(())
    }

    /// Policy for a durable post-close checkout request. `None` means the
    /// caller must not create an automatic-removal obligation.
    pub(super) fn checkout_cleanup_request_policy(
        &self,
        session: &Session,
    ) -> Option<CheckoutCleanupRequestPolicy> {
        let retention = crate::load_retention_policy(&self.main_root).ok()?;
        let cleanup = crate::AutoCleanupPolicy::load(&self.main_root).ok()?;
        checkout_cleanup_request_policy_for(session.origin, &retention, cleanup.auto_remove)
    }

    pub(super) fn record_finished_handoff(
        &mut self,
        report: &FinishReport,
    ) -> Result<(), BrokerOpError> {
        let payload = crate::events::session_finished_payload(report);
        self.store
            .record_finished_handoff(report.session_id, &payload)?;
        Ok(())
    }

    pub(super) fn run_finish_cleanup(
        &mut self,
        report: &mut FinishReport,
        session: &Session,
    ) -> Result<(), BrokerOpError> {
        // Acquire before persisting finish's cleanup intent and keep the claim
        // through physical removal. Cleanup calls the already-claimed helper
        // below so this path never tries to acquire its own lock recursively.
        let _lifecycle_claim = self.claim_worktree_lifecycle()?;
        let checkout_cleanup_request = self.checkout_cleanup_request_policy(session);
        let current_session = self.store.session(report.session_id)?;
        super::cleanup::ensure_lifecycle_session_unchanged(
            report.session_id,
            session,
            &current_session,
        )?;
        let session = &current_session;
        let worktree_path = PathBuf::from(&session.worktree_path);
        let item = self.cleanup_item(session)?;
        report.cleanup_safe = item.as_ref().is_none_or(|item| item.eligible());
        let recovery_retry =
            !report.cleanup_safe && self.interrupted_finish_cleanup_proof(session).is_some();
        report.cleanup_safe |= recovery_retry;
        if !report.cleanup_safe {
            report.status = FinishStatus::Closed;
            report.cleanup.requested = true;
            report.cleanup.recovery_action =
                item.as_ref().map(|item| item.force_cleanup_command.clone());
            report.summary = format!(
                "session {} is closed, but physical cleanup is not proven safe",
                report.session_id
            );
            report.warnings.push(
                "retained artifacts are not fully represented; inspect the cleanup plan before any force cleanup"
                    .into(),
            );
            self.finalize_finish_report(report);
            let handoff_result = self.record_finished_handoff(report);
            drop(_lifecycle_claim);
            if checkout_cleanup_request.is_some() {
                self.wake_checkout_cleanup_after_close(report.session_id, &mut report.cleanup);
            }
            handoff_result?;
            return Ok(());
        }
        let worktree_present = worktree_path.exists();
        let branch_present = item
            .as_ref()
            .and_then(|item| item.branch_tip.as_ref())
            .is_some();
        report.cleanup.requested = true;
        report.cleanup.attempted = true;
        report.cleanup.reclaimed_bytes = item
            .as_ref()
            .and_then(|item| item.estimated_bytes)
            .unwrap_or(0);
        report.cleanup.branch_ref = item.as_ref().map(|item| item.branch_ref.clone());
        report.cleanup.branch_tip = item.as_ref().and_then(|item| item.branch_tip.clone());
        report.cleanup.recovery_action = Some(format!(
            "aethyme broker finish cleanup {}",
            report.session_id
        ));

        // The ownership re-proof `finish` itself needed to record its
        // recovery proof (the spawn witness predates a remount). Reported
        // even when the removal that follows is clean and needs none.
        let mut finish_reproof: Option<&'static str> = None;
        if !recovery_retry {
            let spawned_witness = if session.origin == SessionOrigin::Spawned {
                self.store()
                    .spawned_worktree_instance_witness(session.id)
                    .ok()
                    .flatten()
            } else {
                None
            };
            let recovery_proof =
                if session.origin == SessionOrigin::Spawned && worktree_present && branch_present {
                    GitRepo::discover(&worktree_path)
                        .ok()
                        .and_then(|checkout| {
                            checkout.finish_cleanup_recovery_proof(&format!(
                                "refs/heads/{}",
                                session.branch
                            ))
                        })
                        .and_then(|mut proof| {
                            let witness = spawned_witness.as_ref()?;
                            if !proof.matches_spawned_instance(witness) {
                                // The spawn witness predates a remount: only
                                // the ownership re-proof every deletion lane
                                // shares may accept other device numbers. It
                                // judges the session as the cleanup start
                                // below records it under this same lifecycle
                                // claim (closed, cleanup pending), since this
                                // proof is written by that very transition.
                                let mut closing = session.clone();
                                closing.status = SessionStatus::Cleaned;
                                closing.cleanup_state = crate::SessionCleanupState::Closed;
                                let deadline = self.landing_deadline.get().unwrap_or_else(|| {
                                    std::time::Instant::now() + FINISH_REPROOF_BUDGET
                                });
                                proof.ownership_reproof = Some(
                                    self.checkout_ownership_reproof(
                                        crate::auto_cleanup::OwnershipReproofLane::Finish,
                                        &closing,
                                        &worktree_path,
                                        witness,
                                        &proof.instance_witness(),
                                        None,
                                        Some(deadline),
                                    )
                                    .ok()?,
                                );
                            }
                            if item.as_ref().and_then(|item| item.branch_tip.as_deref())
                                != Some(proof.branch_tip.as_str())
                            {
                                return None;
                            }
                            let policy = crate::AutoCleanupPolicy::load(self.main_root()).ok()?;
                            proof.disposable_ignored_root_prefixes =
                                crate::auto_cleanup::disposable_ignored_directory_prefixes(
                                    &worktree_path,
                                    &policy,
                                )
                                .ok()?;
                            proof.schema_version = 3;
                            Some(proof)
                        })
                } else {
                    None
                };
            finish_reproof = recovery_proof
                .as_ref()
                .and_then(|proof| proof.ownership_reproof);
            let start_payload = crate::events::session_finish_cleanup_started_payload_with_proof(
                worktree_present,
                branch_present,
                recovery_proof.as_ref(),
            );
            self.store.begin_finish_cleanup_with_cleanup_request(
                report.session_id,
                &start_payload,
                checkout_cleanup_request.map(|request| request.grace_hours),
                checkout_cleanup_request.is_some_and(|request| request.compact_checkout),
            )?;
        }
        let cleanup_session = self.store.session(report.session_id)?;
        self.cleanup_ownership_reproof.set(None);
        // `finish --timeout` bounds the checks that decide whether to close.
        // Once closed, reclaiming the worktree runs to completion: killing a
        // `git worktree remove` of a multi-gigabyte tree halfway leaves an
        // orphaned directory instead of a faster answer.
        match crate::git::without_git_deadline(|| {
            self.cleanup_without_archive_under_lifecycle_claim(&cleanup_session, false)
        }) {
            Ok(()) => {
                report.status = FinishStatus::Cleaned;
                report.cleanup.completed = true;
                // The removal reports the re-proof a resumed interrupted
                // cleanup needed; otherwise the one `finish` made above.
                report.cleanup.ownership_reproof = self
                    .cleanup_ownership_reproof
                    .take()
                    .or(finish_reproof)
                    .map(str::to_string);
                report.cleanup.worktree_removed = worktree_present && !worktree_path.exists();
                report.cleanup.branch_removed = branch_present
                    && report
                        .cleanup
                        .branch_ref
                        .as_deref()
                        .is_some_and(|branch| self.repo.resolve_ref(branch).is_none());
                report.cleanup.failure = None;
                report.cleanup.recovery_action = None;
                report.summary = format!(
                    "session {} closed and reclaimed {} bytes",
                    report.session_id, report.cleanup.reclaimed_bytes
                );
                report.next_commands.clear();
            }
            Err(error) => {
                report.status = FinishStatus::Closed;
                report.cleanup.completed = false;
                report.cleanup.ownership_reproof = finish_reproof.map(str::to_string);
                report.cleanup.worktree_removed = worktree_present && !worktree_path.exists();
                report.cleanup.branch_removed = branch_present
                    && report
                        .cleanup
                        .branch_ref
                        .as_deref()
                        .is_some_and(|branch| self.repo.resolve_ref(branch).is_none());
                report.cleanup.failure = Some(error.to_string());
                report.summary = format!(
                    "session {} closed; physical cleanup needs recovery",
                    report.session_id
                );
                report.warnings.push(format!(
                    "physical cleanup did not complete: {error}; retained artifacts remain represented"
                ));
                report.next_commands.clear();
                report.next_commands.push(format!(
                    "aethyme broker finish cleanup {}",
                    report.session_id
                ));
            }
        }
        self.finalize_finish_report(report);
        let handoff_result = self.record_finished_handoff(report);
        drop(_lifecycle_claim);
        if checkout_cleanup_request.is_some() {
            self.wake_checkout_cleanup_after_close(report.session_id, &mut report.cleanup);
        }
        handoff_result?;
        Ok(())
    }

    /// Queue post-close checkout cleanup. When another process already holds
    /// the GC lock, say so in `cleanup` so a JSON reader can tell reclaim was
    /// deferred to it rather than skipped; the stderr warning is kept.
    pub(super) fn wake_checkout_cleanup_after_close(
        &mut self,
        session_id: i64,
        cleanup: &mut FinishCleanupReport,
    ) {
        if let Err(error) = self.request_checkout_cleanup_after_close(session_id) {
            if let BrokerOpError::GcLocked { pid } = &error {
                cleanup.reclaim_deferred = true;
                cleanup.reclaim_worker_pid = pid.trim().parse().ok();
            }
            crate::warn_unrecorded("wake post-close checkout cleanup", Err::<(), _>(error));
        }
    }

    /// Finish a session at the operator level: close it when there is no
    /// dirty work and no committed work waiting for submit/promotion;
    /// otherwise return actionable guidance without mutating state.
    pub fn finish(&mut self, session_id: i64) -> Result<FinishReport, BrokerOpError> {
        let _span = crate::phase_telemetry::span("finish_session");
        self.finish_with_options(session_id, FinishOptions::default())
    }

    pub fn finish_with_options(
        &mut self,
        session_id: i64,
        options: FinishOptions,
    ) -> Result<FinishReport, BrokerOpError> {
        self.finish_with_abandon(session_id, options, None)
    }

    /// [`Broker::finish_with_options`] that closes even when the session
    /// holds commits no remote has, recording that they were abandoned and
    /// why. Every other finish check still applies.
    pub fn finish_abandoning_unpushed(
        &mut self,
        session_id: i64,
        options: FinishOptions,
        reason: &str,
    ) -> Result<FinishReport, BrokerOpError> {
        self.finish_with_abandon(session_id, options, Some(reason))
    }

    pub(super) fn finish_with_abandon(
        &mut self,
        session_id: i64,
        options: FinishOptions,
        abandon_reason: Option<&str>,
    ) -> Result<FinishReport, BrokerOpError> {
        let mut report = self.finish_with_options_inner(session_id, options, abandon_reason)?;
        // The snapshot is taken while the session is still open, because a
        // blocked finish needs it to explain what is held. Cleanup is what
        // actually removes the leases, so only a completed cleanup makes the
        // snapshot history; it must not then read "active ... released never"
        // (issue #141). Relabelled rather than dropped, because a handoff is
        // more useful when it records what the session owned.
        //
        // Closing alone deliberately does not qualify: `close` only sets the
        // session status, so a closed session still holds its leases -- and
        // saying otherwise would hide a real block on other sessions. A
        // verified terminal finish (`Closed` or `Cleaned`) releases them in
        // the transaction that closes the session, before any physical
        // cleanup (#358), so it qualifies too -- checked against the store,
        // not assumed from the status.
        let released_by_close = report.closed
            && matches!(report.status, FinishStatus::Closed | FinishStatus::Cleaned)
            && !self
                .store
                .has_unreleased_unexpired_lease_for_session_at(session_id, now_ms())?;
        if report.cleanup.completed || released_by_close {
            let released_at = now_ms();
            for lease in &mut report.leases_held {
                if lease.state == FinishLeaseState::Active {
                    lease.state = FinishLeaseState::Released;
                    lease.released_at.get_or_insert(released_at);
                }
            }
        }
        if report.closed && !report.cleanup.worktree_removed {
            let reclaim = {
                let _span = crate::phase_telemetry::span("finish_kept_worktree_artifact_reclaim");
                self.reclaim_closed_session_artifacts(session_id)
            };
            match reclaim {
                Ok(outcome) => {
                    report.cleanup.artifact_bytes_reclaimed = outcome.observed_logical_file_bytes;
                    if outcome.directories_reclaimed > 0 {
                        let count = outcome.directories_reclaimed;
                        let noun = if count == 1 {
                            "directory"
                        } else {
                            "directories"
                        };
                        report.warnings.push(format!(
                            "reclaimed build artifacts from {count} {noun} while retaining the worktree"
                        ));
                    }
                    if !outcome.complete {
                        report.warnings.push(
                            "build artifact reclaim was deferred; a later artifact sweep will retry it"
                                .into(),
                        );
                    }
                }
                Err(error) => report.warnings.push(format!(
                    "build artifact reclaim was skipped and left for GC: {error}"
                )),
            }
        }
        if report.closed {
            // Broker open only nibbles at an unfinished sweep; an agent that
            // is leaving pays the full budget to drain it instead.
            match self.continue_artifact_sweep_backlog() {
                Ok(Some(outcome)) if outcome.directories_reclaimed > 0 => {
                    report.warnings.push(format!(
                        "continued the build-artifact sweep: reclaimed {} director{} from idle or finished sessions{}",
                        outcome.directories_reclaimed,
                        if outcome.directories_reclaimed == 1 { "y" } else { "ies" },
                        if outcome.complete { "" } else { "; more remains for `aethyme broker gc sweep`" }
                    ));
                }
                Ok(_) => {}
                Err(error) => report.warnings.push(format!(
                    "the build-artifact sweep could not continue and is left for `aethyme broker gc sweep`: {error}"
                )),
            }
        }
        Ok(report)
    }

    pub(super) fn finish_with_options_inner(
        &mut self,
        session_id: i64,
        options: FinishOptions,
        abandon_reason: Option<&str>,
    ) -> Result<FinishReport, BrokerOpError> {
        self.finish_with_options_inner_with_retention_audit(
            session_id,
            options,
            abandon_reason,
            |broker, at_ms| broker.cleanup_retention(at_ms),
        )
    }

    pub(super) fn finish_with_options_inner_with_retention_audit(
        &mut self,
        session_id: i64,
        options: FinishOptions,
        abandon_reason: Option<&str>,
        retention_audit: impl FnOnce(&Self, i64) -> Result<CleanupRetention, BrokerOpError>,
    ) -> Result<FinishReport, BrokerOpError> {
        let session = self.store.session(session_id)?;
        let worktree_path = PathBuf::from(&session.worktree_path);
        let at_ms = now_ms();
        let latest_for_session = self.store.latest_merge_queue_for_session(session_id)?;
        let retention_warning =
            finish_retention_warning(crate::git::active_git_deadline().is_some(), || {
                retention_audit(self, at_ms)
            })?;
        // Cleanup is safe only when the repository policy can be read and
        // explicitly permits automation. A malformed policy therefore keeps
        // a worktree available for manual review rather than deleting it.
        let auto_cleanup_policy = crate::load_retention_policy(&self.main_root);
        let auto_cleanup_enabled = auto_cleanup_policy
            .as_ref()
            .is_ok_and(|policy| policy.auto_cleanup_worktrees_on_finish);
        let checkout_cleanup_request = if options.keep_worktree {
            None
        } else {
            self.checkout_cleanup_request_policy(&session)
        };
        let mut report = FinishReport {
            session_id,
            worktree_path: session.worktree_path.clone(),
            status: FinishStatus::Blocked,
            closed: false,
            dirty_paths: Vec::new(),
            unsubmitted_commits: 0,
            latest_queue_entry_id: latest_for_session.as_ref().map(|entry| entry.id),
            latest_queue_status: latest_for_session.as_ref().map(|entry| entry.status),
            delivery: self.finish_delivery(latest_for_session.as_ref()),
            representation: None,
            pending_work: FinishPendingWork::default(),
            leases_held: self.finish_leases(session_id, at_ms)?,
            last_gate: self.finish_last_gate(session_id)?,
            last_graph_integrity: self.finish_last_graph_integrity(session_id)?,
            unpushed_commits: None,
            cleanup_safe: false,
            cleanup: FinishCleanupReport::default(),
            recommended_next_action: None,
            summary: format!("session {session_id} is not finished yet"),
            warnings: retention_warning.into_iter().collect(),
            next_commands: Vec::new(),
        };

        if session.status == SessionStatus::Cleaned {
            report.closed = true;
            // A historical ledger state cannot prove current path absence:
            // the checkout may have been recreated or replaced by a symlink.
            match std::fs::symlink_metadata(&worktree_path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    report.status = FinishStatus::AlreadyCleaned;
                    report.cleanup.completed = true;
                    report.cleanup.worktree_removed = true;
                    report.summary = format!("session {session_id} is already physically cleaned");
                }
                observation => {
                    report.status = FinishStatus::AlreadyClosed;
                    report.cleanup.kept = true;
                    report.summary = format!(
                        "session {session_id} is recorded cleaned, but physical cleanup is not confirmed"
                    );
                    report.warnings.push(match observation {
                        Ok(_) => "the recorded worktree path exists; retain it until current ownership and cleanup eligibility are revalidated".into(),
                        Err(error) => format!("cannot inspect the recorded worktree path: {error}; retain it pending verification"),
                    });
                    report
                        .next_commands
                        .push(format!("aethyme broker finish cleanup {session_id}"));
                }
            }
            self.finalize_finish_report(&mut report);
            return Ok(report);
        }
        if session.status == SessionStatus::Closed {
            report.status = FinishStatus::AlreadyClosed;
            report.closed = true;
            if options.keep_worktree || session.origin != SessionOrigin::Spawned {
                report.cleanup.kept = options.keep_worktree;
                report.summary = format!("session {session_id} is already closed");
                report.cleanup_safe = self
                    .cleanup_item(&session)?
                    .is_some_and(|item| item.eligible());
                if report.cleanup_safe {
                    report
                        .next_commands
                        .push(format!("aethyme broker finish cleanup {session_id}"));
                }
                self.finalize_finish_report(&mut report);
                return Ok(report);
            }
            self.run_finish_cleanup(&mut report, &session)?;
            return Ok(report);
        }

        if !worktree_path.exists() {
            report.pending_work.worktree_missing = true;
            report.status = FinishStatus::Closed;
            report.closed = true;
            if session.origin == SessionOrigin::Spawned && !options.keep_worktree {
                self.run_finish_cleanup(&mut report, &session)?;
                return Ok(report);
            }
            report.cleanup.kept = options.keep_worktree;
            report.summary = format!(
                "session {session_id} closed in broker state; worktree was already missing"
            );
            report.warnings.push(
                "worktree path was already absent; no broker-owned physical cleanup was attempted"
                    .into(),
            );
            self.finalize_finish_report(&mut report);
            self.persist_finish_report(&report, None)?;
            return Ok(report);
        }

        let checkout = GitRepo::discover(&worktree_path)?;
        let head = checkout.head_commit()?;
        report.dirty_paths = checkout.dirty_paths()?;

        let queue = self
            .store
            .latest_merge_queue_for_session_and_head(session_id, &head)?;
        let latest_for_session = queue.last();
        report.latest_queue_entry_id = latest_for_session.map(|entry| entry.id);
        report.latest_queue_status = latest_for_session.map(|entry| entry.status);
        report.delivery = self.finish_delivery(latest_for_session);
        let delivery = self.head_delivery(&session, &checkout, &head, &queue)?;
        if let Some(entry) = delivery.visible_entry {
            report.latest_queue_entry_id = Some(entry.id);
            report.latest_queue_status = Some(entry.status);
            report.delivery = self.finish_delivery(Some(entry));
        }
        report.representation = delivery.representation;
        if delivery.on_remote_default || delivery.default_landing.is_some() {
            report.delivery.published = true;
        }
        report.unsubmitted_commits = delivery.unsubmitted_commits;

        if !report.dirty_paths.is_empty() {
            // `dirty_paths` lists every untracked file because cleanup safety
            // depends on it; the warning counts the way `git status` does, so
            // one untracked build folder reads as one entry, not thousands.
            let uncommitted = checkout
                .uncommitted_summary()
                .ok()
                .filter(|summary| !summary.is_empty())
                .map_or_else(
                    || {
                        format!(
                            "{} uncommitted or untracked {}",
                            report.dirty_paths.len(),
                            plural_word(report.dirty_paths.len(), "path", "paths")
                        )
                    },
                    |summary| format!("uncommitted changes: {}", summary.describe(5)),
                );
            report.warnings.push(format!(
                "worktree has {uncommitted}; commit through the managed pre-commit lane before finish"
            ));
            report
                .next_commands
                .push(format!("git -C {} status --short", session.worktree_path));
            report
                .next_commands
                .push(format!("git -C {} add ...", session.worktree_path));
            report
                .next_commands
                .push(format!("git -C {} commit", session.worktree_path));
            if crate::session_push::session_push_enabled(&self.repo) {
                report
                    .next_commands
                    .push(format!("aethyme broker push --session {session_id}"));
            }
            self.finalize_finish_report(&mut report);
            return Ok(report);
        }

        if let Some(entry) = delivery
            .latest_for_head
            .filter(|_| !delivery.head_is_delivered)
        {
            match entry.status {
                // Nothing promotes, so every verified entry stays `Verified`;
                // demanding a promotion before finish would block every
                // session permanently (#290 phase 2.2).
                MergeStatus::Promoted | MergeStatus::ExternallyLanded | MergeStatus::Verified => {}
                MergeStatus::Conflict => {
                    report.warnings.push(format!(
                        "latest submit qid {} conflicted; repair and resubmit before finish",
                        entry.id
                    ));
                    report.next_commands.push(format!(
                        "aethyme broker advanced repair --session {session_id}"
                    ));
                    report
                        .next_commands
                        .push(format!("aethyme broker submit --session {session_id}"));
                    self.finalize_finish_report(&mut report);
                    return Ok(report);
                }
                MergeStatus::Rejected => {
                    let environmental = crate::exit_status::failures_are_environmental(
                        gate_failures(entry.details_json.as_deref())
                            .iter()
                            .map(|failure| failure.failure_class.as_deref()),
                    );
                    report.warnings.push(if environmental {
                        format!(
                            "latest submit qid {} reported an environment or resource error; inspect \
                             the gate log before deciding whether to fix code or retry",
                            entry.id
                        )
                    } else {
                        format!(
                            "latest submit qid {} was rejected; commit a fix and resubmit before finish",
                            entry.id
                        )
                    });
                    report
                        .next_commands
                        .push(format!("aethyme broker submit --session {session_id}"));
                    self.finalize_finish_report(&mut report);
                    return Ok(report);
                }
                // A deferred entry is `Submitted` too, but nothing is running
                // it: "wait for it" would leave the agent waiting forever.
                MergeStatus::Submitted if submission_was_deferred(entry) => {
                    report.warnings.push(format!(
                        "latest submit qid {} was deferred: a gate did not complete on this host, \
                         so the change was not judged; inspect gate evidence and use only \
                         authorized validation before retrying. Remote checks do not complete \
                         the broker submission",
                        entry.id
                    ));
                    report
                        .next_commands
                        .push(format!("aethyme broker submit --session {session_id}"));
                    self.finalize_finish_report(&mut report);
                    return Ok(report);
                }
                MergeStatus::Submitted | MergeStatus::Simulating => {
                    report.warnings.push(format!(
                        "queue entry {} is still {}; wait for it before finish",
                        entry.id,
                        entry.status.as_str()
                    ));
                    report
                        .next_commands
                        .push("aethyme broker advanced queue".into());
                    self.finalize_finish_report(&mut report);
                    return Ok(report);
                }
                MergeStatus::Superseded => {}
            }
        }

        // `--abandon --reason` closes over undelivered work too, as its help
        // promises: otherwise a session whose work never lands can only be
        // closed by a lower-level command.
        // Recorded only once every refusal check below has passed, so a
        // refused retry never records an abandonment that did not happen.
        let abandon_undelivered = abandon_reason
            .filter(|_| report.unsubmitted_commits > 0)
            .map(|reason| (report.unsubmitted_commits, reason));
        if abandon_undelivered.is_none() && report.unsubmitted_commits > 0 {
            // The guard is right either way -- these commits exist nowhere but
            // this branch, and closing a session holding the only copy is what
            // #222 exists to prevent. What differs is the remedy.
            //
            // `submit` has already run and can never satisfy this: it verifies
            // and moves nothing by design (#290 phase 2.2). Naming it anyway sends the operator
            // round a loop with no exit, which is the shape of the engine-pin
            // refusal in #254. The reachable path is to land the work by pull
            // request: finish recognises it once the fetched default branch
            // holds it.
            let has_default = self.repo.upstream_default().is_some();
            // With no fetched default branch there is nothing to push to,
            // so name the landing that this repository can actually do.
            let land = if has_default {
                "push the session branch and merge its pull request; finish recognises the \
                 work once the fetched default branch holds it"
            } else {
                "merge the session branch into the local default branch, then prove it landed"
            };
            report.warnings.push(format!(
                "HEAD has {} committed {} not represented on any delivery target. This \
                 repository does not promote, so submitting again cannot change that — {land}; \
                 or close without it with --abandon --reason",
                report.unsubmitted_commits,
                plural_word(report.unsubmitted_commits as usize, "change", "changes")
            ));
            // Every command named here can change the state it reports. Where
            // the work lands by pull request, `push --pr` publishes it (and
            // fetches the default branch, so a merged one is then seen);
            // elsewhere `scan` is read-only and its output names `record` with
            // the real digest, which a placeholder could not.
            if has_default && crate::session_push::session_push_enabled(&self.repo) {
                report
                    .next_commands
                    .push(format!("aethyme broker push --session {session_id} --pr"));
            } else {
                report.next_commands.push(format!(
                    "aethyme broker advanced representation scan --session {session_id}"
                ));
            }
            report.next_commands.push(format!(
                "aethyme broker finish --session {session_id} --abandon --reason \"<why>\""
            ));
            self.finalize_finish_report(&mut report);
            return Ok(report);
        }

        // A gate running for this session still relies on what it leased:
        // closing now would release those leases under it (#358). Unknown
        // pidfiles count as running, so this fails closed.
        let running_gates = crate::gates::running_session_gates(&self.main_root, session_id);
        if !running_gates.is_empty() {
            report.warnings.push(format!(
                "gate {} is still running for this session; its leases stay held until it ends",
                running_gates.join(", ")
            ));
            report.next_commands.push(format!(
                "wait for the gate to end, then: aethyme broker finish --session {session_id}"
            ));
            self.finalize_finish_report(&mut report);
            return Ok(report);
        }

        // Under the push lane a session is not done while its commits exist
        // only here: closing it is what turned 94 worktrees into the sole
        // copies of their work (see `unpushed`). Abandoning stays possible,
        // but only on the record.
        // Work the fetched default branch already holds is on the remote,
        // even once a squash merge deleted the branch that carried it.
        if delivery.default_landing.is_none()
            && let Some((unpushed_head, unpushed)) = self.unpushed_close_check(&session)
        {
            report.unpushed_commits = Some(unpushed);
            match abandon_reason {
                None => {
                    report.warnings.push(format!(
                        "HEAD has {unpushed} {} on no remote; push the session branch before \
                         finish, or abandon them explicitly with a reason",
                        plural_word(unpushed as usize, "commit", "commits")
                    ));
                    report
                        .next_commands
                        .push(format!("aethyme broker push --session {session_id}"));
                    report.next_commands.push(format!(
                        "aethyme broker finish --session {session_id} --abandon --reason \"<why>\""
                    ));
                    self.finalize_finish_report(&mut report);
                    return Ok(report);
                }
                Some(_) if abandon_undelivered.is_some() => {}
                Some(reason) => {
                    self.record_abandoned_unpushed(&session, &unpushed_head, unpushed, reason)?;
                    report.warnings.push(format!(
                        "closed with {unpushed} unpushed {} abandoned: {reason}",
                        plural_word(unpushed as usize, "commit", "commits")
                    ));
                }
            }
        } else if crate::session_push::session_push_enabled(&self.repo) {
            report.unpushed_commits = Some(0);
        }

        if let Some((count, reason)) = abandon_undelivered {
            self.record_abandoned_unpushed(
                &session,
                &head,
                u32::try_from(count).unwrap_or(u32::MAX),
                reason,
            )?;
            report.warnings.push(format!(
                "closed with {count} undelivered committed {} abandoned: {reason}",
                plural_word(count as usize, "change", "changes")
            ));
        }

        // The landing is established, so this session's verified rows are
        // terminal: retention prunes them instead of keeping them forever.
        if let Some(landing) = delivery.default_landing.as_ref() {
            match self.mark_landed_queue_entries(session_id, &head, landing) {
                Ok(marked)
                    if report
                        .latest_queue_entry_id
                        .is_some_and(|id| marked.contains(&id)) =>
                {
                    report.latest_queue_status = Some(MergeStatus::ExternallyLanded);
                }
                Ok(_) => {}
                Err(error) => report.warnings.push(format!(
                    "could not mark the landed queue entries terminal; retention keeps them: {error}"
                )),
            }
        }

        report.status = FinishStatus::Closed;
        report.closed = true;
        report.summary = format!("session {session_id} closed; worktree retained");
        report.cleanup_safe =
            self.finish_cleanup_safe(session_id, &worktree_path, &report.dirty_paths)?;
        let broker_owned = session.origin == SessionOrigin::Spawned
            && self.is_broker_owned_worktree(&session, &worktree_path);
        if options.keep_worktree {
            report.cleanup.kept = true;
        }
        if report.cleanup_safe && broker_owned && !options.keep_worktree && auto_cleanup_enabled {
            self.run_finish_cleanup(&mut report, &session)?;
            return Ok(report);
        } else if report.cleanup_safe {
            if broker_owned && !options.keep_worktree && !auto_cleanup_enabled {
                report.cleanup.kept = true;
                if auto_cleanup_policy.is_err() {
                    report.summary = format!(
                        "session {session_id} closed; worktree retained because retention policy could not be loaded"
                    );
                    report.warnings.push(
                        "automatic finish cleanup was skipped because the repository retention policy could not be loaded"
                            .into(),
                    );
                } else {
                    report.summary = format!(
                        "session {session_id} closed; worktree retained by repository policy"
                    );
                    report
                        .warnings
                        .push("automatic finish cleanup is disabled by repository policy".into());
                }
            }
            report
                .next_commands
                .push(format!("aethyme broker finish cleanup {session_id}"));
        } else if worktree_path.as_path() != self.main_root.as_path() {
            report.warnings.push(
                "cleanup not suggested yet; cleanup only removes worktrees with no dirty paths \
                 and no commits beyond main"
                    .into(),
            );
        }
        self.finalize_finish_report(&mut report);
        self.persist_finish_report(&report, checkout_cleanup_request)?;
        if checkout_cleanup_request.is_some() {
            self.wake_checkout_cleanup_after_close(session_id, &mut report.cleanup);
        }
        Ok(report)
    }

    /// Where a session's HEAD stands against its delivery targets: the
    /// evidence `finish` refuses on when committed work is not yet delivered
    /// (#222). Read-only, so `broker unblock` can ask the same question
    /// before naming `finish` as the remedy for a stale lease (#637).
    pub(crate) fn head_delivery<'q>(
        &self,
        session: &Session,
        checkout: &GitRepo,
        head: &str,
        queue: &'q [MergeQueueEntry],
    ) -> Result<HeadDelivery<'q>, BrokerOpError> {
        let session_id = session.id;
        let latest_for_head = queue
            .iter()
            .rev()
            .find(|entry| entry.session_id == session_id && entry.head_commit == head);
        let latest_for_session = queue
            .iter()
            .rev()
            .find(|entry| entry.session_id == session_id);
        let visible_entry = latest_for_head.or(latest_for_session);

        // Finish, cleanup, and integration reconciliation share the same
        // landing proof. Finish keeps #152's explicit confirmation gate for
        // non-ancestry deliveries: a content or patch match is a candidate,
        // not permission to delete the only worktree, until it is recorded --
        // except on the fetched default branch of a repository that does not
        // promote, where a merged pull request is the only landing there is.
        // An adopted session can use the primary checkout itself. Its own
        // HEAD must not count as proof that its commits were delivered; only
        // independent integration and upstream refs can establish that.
        let delivery_targets =
            if std::path::Path::new(&session.worktree_path) == self.main_root.as_path() {
                let mut targets = Vec::new();
                if let Some(integration) = self.integration_tip() {
                    targets.push(integration);
                }
                if let Some((_, upstream)) = self.repo.tracking_upstream() {
                    targets.push(upstream);
                }
                targets.sort();
                targets.dedup();
                targets
            } else {
                self.cleanup_delivery_targets()?
            };
        // Unlike a historical queue status, a representation record only
        // counts when its carrying commit is still reachable from a current
        // delivery target.
        let representation_evidence =
            self.recorded_representation_evidence(session, head, &delivery_targets)?;
        let representation = if representation_evidence.is_some() {
            self.store.session_representation(session_id, head)?
        } else {
            None
        };
        // A record is bound to this exact head and remains valid only while
        // its carrying commit is reachable from a current delivery target.
        // Once checked above it is sufficient for finish; avoid a potentially
        // expensive historical search for the same evidence.
        // A session that only republishes its starting tree has no net work
        // to land. Require both a true ancestor baseline and a live remote
        // containing this exact head; this is durability, not delivery.
        let remotely_durable_no_net = if representation.is_none() {
            if let Some(base) = session.diff_base.as_deref() {
                self.repo.is_ancestor_checked(base, head)?
                    && self.repo.commit_tree_id(base)? == self.repo.commit_tree_id(head)?
                    && self.remote_durability_evidence(head).is_some()
            } else {
                false
            }
        } else {
            false
        };
        let landing = if representation.is_some() || remotely_durable_no_net {
            None
        } else {
            self.landing_on_delivery_targets(head, &delivery_targets)?
        };

        // A promoted queue row is a trusted acceptance only while both its
        // recorded promotion commit and this exact session head still have
        // shared landing evidence on current targets. A later squash that
        // overwrote the contribution cannot keep a historical `Promoted` row
        // alive as delivery evidence.
        let queued_merge_commit = latest_for_head
            .and_then(|entry| entry.details_json.as_deref())
            .and_then(|details| serde_json::from_str::<serde_json::Value>(details).ok())
            .and_then(|details| {
                details
                    .get("merge_commit")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            });
        let accepted_integration_commit = session
            .accepted_integration_commit
            .as_deref()
            .or(queued_merge_commit.as_deref());
        let accepted_commit_is_current = accepted_integration_commit.is_some_and(|accepted| {
            delivery_targets
                .iter()
                .any(|target| self.repo.is_ancestor(accepted, target))
        });
        let queue_accepts_head = latest_for_head.is_some_and(|entry| {
            matches!(
                entry.status,
                MergeStatus::Promoted | MergeStatus::ExternallyLanded
            )
        }) || session.accepted_session_head.as_deref() == Some(head);
        // Exact ancestry on a current delivery target proves that this
        // worktree HEAD is already present there. Content/patch matches remain
        // gated on accepted queue evidence, a recorded representation, or (in
        // a repository that does not promote) the fetched default branch.
        let ancestry_proves_delivery = landing
            .as_ref()
            .is_some_and(|(_, evidence, _)| evidence == &crate::LandingEvidence::Ancestry);
        let submitted_head_is_delivered = ancestry_proves_delivery
            || (queue_accepts_head && accepted_commit_is_current && landing.is_some());
        // A repository that does not promote lands work only by pull request,
        // and a squash leaves the content as the only evidence: no queue row
        // there is ever accepted. The fetched default branch holding the
        // session's net content is the landing itself, and the work is on the
        // remote, so it needs no further confirmation. It is asked even when
        // ancestry already proved delivery, since a merge commit lands work
        // too and its verified rows become terminal the same way. `landing`
        // was searched whenever it is consulted here.
        let default_landing = if representation.is_some() || remotely_durable_no_net {
            None
        } else {
            self.default_branch_landing(head, landing.as_ref(), &delivery_targets)?
        };

        let remote_default_tip = self.remote_tracking_default_tip();
        let on_remote_default = if let Some(tip) = remote_default_tip.as_ref() {
            // This flag accepts ancestry only. A full landing search cannot
            // make a squash or rebase satisfy it, and can repeat the expensive
            // search already performed for the same target above.
            self.repo.is_ancestor_checked(head, tip)?
        } else {
            false
        };
        let head_is_delivered = submitted_head_is_delivered
            || representation.is_some()
            || on_remote_default
            || default_landing.is_some();
        let unsubmitted_commits = if head_is_delivered || remotely_durable_no_net {
            0
        } else {
            let integration_head = self.integration_tip();
            let pending_from_plan = integration_head.as_deref().and_then(|integration_head| {
                self.build_submission_plan(session, head, integration_head)
                    .ok()
                    .filter(|plan| plan.safe)
                    .map(|plan| {
                        plan.pending_owned_commit_ids()
                            .into_iter()
                            .filter(|commit| {
                                !remote_default_tip
                                    .as_deref()
                                    .is_some_and(|tip| self.repo.is_ancestor(commit, tip))
                            })
                            .count() as u64
                    })
            });
            if let Some(pending) = pending_from_plan.filter(|pending| *pending > 0) {
                pending
            } else {
                let base = session
                    .diff_base
                    .clone()
                    .or_else(|| crate::merge::session_baseline(checkout, &self.baseline_refs()))
                    .unwrap_or_else(|| "HEAD".to_string());
                checkout.commit_count_between(&base, "HEAD")?
            }
        };
        Ok(HeadDelivery {
            latest_for_head,
            visible_entry,
            head_is_delivered,
            representation,
            on_remote_default,
            default_landing,
            unsubmitted_commits,
        })
    }

    /// Where `head`'s work landed on the fetched default branch, in a
    /// repository that does not promote. `None` in a promoting repository,
    /// without a fetched default branch, when the branch does not hold every
    /// path the session changed, or when the session changed nothing.
    ///
    /// `known` is the landing already found on `searched` targets, which are
    /// asked in turn and the first holding the work answers: a default branch
    /// among them with no landing found holds nothing, and is not searched
    /// again.
    pub(crate) fn default_branch_landing(
        &self,
        head: &str,
        known: Option<&(String, crate::LandingEvidence, Option<String>)>,
        searched: &[String],
    ) -> Result<Option<DefaultBranchLanding>, BrokerOpError> {
        let Some((reference, tip)) = self.upstream_verification_head() else {
            return Ok(None);
        };
        let found = match known {
            Some((target, evidence, landed_by)) if *target == tip => {
                Some((*evidence, landed_by.clone()))
            }
            None if searched.contains(&tip) => None,
            _ => {
                let _span = crate::phase_telemetry::span("default_branch_landing");
                self.landing_on_delivery_targets(head, std::slice::from_ref(&tip))?
                    .map(|(_, evidence, landed_by)| (evidence, landed_by))
            }
        };
        // A head that changes nothing has landed nothing: whether its commits
        // may be left behind is the remote-durability question `finish`
        // already asks of a no-net session, not a delivery.
        let Some(landed_by) = found
            .filter(|(evidence, _)| *evidence != crate::LandingEvidence::NoNetChange)
            .map(|(_, landed_by)| landed_by)
        else {
            return Ok(None);
        };
        let landing = DefaultBranchLanding {
            reference,
            tip,
            landed_by,
        };
        // The search above compares the net diff. That is not enough to
        // stand in for the unpushed and unsubmitted guards: a later commit
        // that restored a path, or deleted a file an earlier one added,
        // leaves the net diff matching while its own change exists only
        // here. The landing must account for every commit.
        Ok(self.landing_covers(&landing, head)?.then_some(landing))
    }

    /// Whether `commit`'s every commit is accounted for by `landing`: it is
    /// on the default branch, or the landing commit holds `commit`'s content
    /// on every path any session commit since the default branch touched --
    /// including a path a later commit restored to its base state.
    pub(crate) fn landing_covers(
        &self,
        landing: &DefaultBranchLanding,
        commit: &str,
    ) -> Result<bool, BrokerOpError> {
        if self.repo.is_ancestor_checked(commit, &landing.tip)? {
            return Ok(true);
        }
        let Some(landed_by) = landing.landed_by.as_deref() else {
            return Ok(false);
        };
        let _span = crate::phase_telemetry::span("default_branch_landing_coverage");
        let base = self.repo.merge_base(commit, &landing.tip)?;
        let touched = self.repo.paths_touched_between(&base, commit)?;
        Ok(self.repo.paths_equal(landed_by, commit, &touched)?)
    }

    /// Make this session's `verified` queue rows terminal once its work has
    /// landed on the default branch, so retention can prune them. Only rows
    /// whose head `head` contains and whose every commit the landing
    /// accounts for are marked: a row whose change the session later
    /// reverted did not land. Returns the ids marked.
    pub(crate) fn mark_landed_queue_entries(
        &mut self,
        session_id: i64,
        head: &str,
        landing: &DefaultBranchLanding,
    ) -> Result<Vec<i64>, BrokerOpError> {
        let _span = crate::phase_telemetry::span("mark_landed_queue_entries");
        let mut marked = Vec::new();
        for entry in self.store.verified_merge_queue_for_session(session_id)? {
            if entry.head_commit != head
                && (!self.repo.is_ancestor(&entry.head_commit, head)
                    || !self.landing_covers(landing, &entry.head_commit)?)
            {
                continue;
            }
            let mut details = entry
                .details_json
                .as_deref()
                .and_then(|details| serde_json::from_str::<serde_json::Value>(details).ok())
                .filter(serde_json::Value::is_object)
                .unwrap_or_else(|| serde_json::json!({}));
            details["externally_landed"] = true.into();
            details["classification"] = "already_landed".into();
            details["upstream_ref"] = landing.reference.clone().into();
            details["upstream_landing"] = landing.landed_by.clone().into();
            if self
                .store
                .mark_verified_merge_externally_landed(entry.id, &details.to_string())?
            {
                marked.push(entry.id);
            }
        }
        Ok(marked)
    }

    pub(super) fn finish_cleanup_safe(
        &self,
        session_id: i64,
        worktree_path: &Path,
        dirty_paths: &[String],
    ) -> Result<bool, BrokerOpError> {
        if worktree_path == self.main_root.as_path()
            || !worktree_path.exists()
            || !dirty_paths.is_empty()
        {
            return Ok(false);
        }
        Ok(matches!(
            self.cleanup_eligibility(session_id, worktree_path)?,
            (CleanupDisposition::Eligible, _, _)
        ))
    }

    pub(super) fn submitted_head_is_represented_on(
        &self,
        session_id: i64,
        session_head: &str,
        target_head: &str,
    ) -> Result<bool, BrokerOpError> {
        // The queue is read once per call, and this is called once per live
        // session from `promoted_conflicts`, which is on `status`. It used to
        // select a few candidate rows by identity and only then shell out; the
        // selection is pure string work, so doing it before the query keeps
        // `git merge-base` off the common path entirely.
        //
        // `is_ancestor` is a subprocess, so it is only reached for an entry
        // that actually claims to represent this exact (session, head) — at
        // most one or two rows, rather than one call per queue entry.
        let candidate = self
            .store
            .latest_representation_for_session(session_id, session_head)?;
        let Some(promotion) = candidate else {
            return Ok(false);
        };
        Ok(self.repo.is_ancestor(&promotion, target_head))
    }

    // ── cleanup ───────────────────────────────────────────────────────

    pub(super) fn legacy_broker_worktree_root(&self) -> PathBuf {
        self.main_root.join(".aethyme/worktrees")
    }

    /// Status of the session holding a lease, for a refusal that can be acted on.
    pub(super) fn lease_holder_status(&self, session_id: i64) -> Option<String> {
        self.store_ref()
            .session(session_id)
            .ok()
            .map(|session| session.status.as_str().to_string())
    }

    pub(super) fn lease_holder_context(&self, session_id: i64) -> Option<String> {
        self.store_ref()
            .session(session_id)
            .ok()
            .and_then(|session| session.context_label())
    }

    pub(crate) fn is_broker_owned_worktree(&self, session: &Session, path: &Path) -> bool {
        self.is_broker_owned_worktree_checked(session, path)
            .unwrap_or(false)
    }

    // The root marker identifies the repository, but does not by itself prove
    // that an existing checkout is owned. Callers must also verify Git or an
    // exact durable interrupted-cleanup proof before acting on the path.
    pub(super) fn is_broker_owned_worktree_root_checked(
        &self,
        session: &Session,
        path: &Path,
    ) -> Result<bool, GitError> {
        if session.origin != SessionOrigin::Spawned || path == self.main_root.as_path() {
            return Ok(false);
        }
        let Some(parent) = path.parent() else {
            return Ok(false);
        };
        let parent = canonical_path_or_missing(parent)?;
        let legacy = canonical_path_or_missing(&self.legacy_broker_worktree_root())?;
        Ok(parent == legacy || self.worktree_root_marker_matches_checked(&parent)?)
    }

    /// Check worktree ownership without collapsing Git or filesystem
    /// uncertainty into the same `false` result as a known non-owned path.
    /// Callers doing bounded maintenance must hold `limit_git_until` around
    /// this method so all nested Git invocations share their pass deadline.
    pub(crate) fn is_broker_owned_worktree_checked(
        &self,
        session: &Session,
        path: &Path,
    ) -> Result<bool, GitError> {
        if !self.is_broker_owned_worktree_root_checked(session, path)? {
            return Ok(false);
        }
        let Some(parent) = path.parent() else {
            return Ok(false);
        };
        let parent = canonical_path_or_missing(parent)?;
        match std::fs::symlink_metadata(path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
            Err(error) => return Err(ownership_inspection_error(path, "inspect", error)),
        }
        let canonical_path = canonical_path_or_missing(path)?;
        if canonical_path.parent() != Some(parent.as_path()) {
            return Ok(false);
        }
        let actual = match GitRepo::worktree_common_dir(&canonical_path) {
            Ok(common_dir) => common_dir,
            Err(inspection_error) => {
                // The pointer is independent evidence only if it names this
                // repository and its gitdir is actually gone. A timeout or
                // other Git failure while the gitdir still exists must remain
                // unavailable, not become an ownership refusal or approval.
                if self
                    .is_orphaned_worktree_of_this_repository_checked(&canonical_path)
                    .unwrap_or(false)
                {
                    return Ok(true);
                }
                return Err(inspection_error);
            }
        };
        let expected = self.main_git_common_dir()?;
        Ok(actual == expected)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CheckoutCleanupRequestPolicy {
    pub(super) grace_hours: u32,
    pub(super) compact_checkout: bool,
}

fn checkout_cleanup_request_policy_for(
    origin: SessionOrigin,
    retention: &crate::RetentionPolicy,
    auto_remove: bool,
) -> Option<CheckoutCleanupRequestPolicy> {
    if origin != SessionOrigin::Spawned
        || !retention.auto_cleanup_worktrees_on_finish
        || !auto_remove
    {
        return None;
    }
    Some(CheckoutCleanupRequestPolicy {
        grace_hours: retention.closed_worktree_grace_hours,
        compact_checkout: retention.compact_closed_worktrees,
    })
}

fn finish_retention_warning(
    deadline_active: bool,
    retention_audit: impl FnOnce() -> Result<CleanupRetention, BrokerOpError>,
) -> Result<Option<String>, BrokerOpError> {
    if deadline_active {
        return Ok(Some(
            "retention audit deferred because finish has a command-scoped Git deadline; inspect with `aethyme broker gc plan`"
                .into(),
        ));
    }

    let retention = retention_audit()?;
    Ok(cleanup_retention_warning(&retention))
}

fn canonical_path_or_missing(path: &Path) -> Result<PathBuf, GitError> {
    match path.canonicalize() {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(path.to_path_buf()),
        Err(error) => Err(ownership_inspection_error(path, "resolve", error)),
    }
}

fn ownership_inspection_error(path: &Path, action: &str, error: std::io::Error) -> GitError {
    GitError::UntrustedOutput {
        detail: format!(
            "could not {action} worktree ownership path {}: {error}",
            path.display()
        ),
    }
}

/// What [`Broker::head_delivery`] found for one session HEAD.
pub(crate) struct HeadDelivery<'q> {
    /// The queue row recorded for exactly this HEAD.
    pub(crate) latest_for_head: Option<&'q MergeQueueEntry>,
    /// The queue row for this HEAD, else the session's latest one.
    pub(crate) visible_entry: Option<&'q MergeQueueEntry>,
    pub(crate) representation: Option<SessionRepresentation>,
    pub(crate) on_remote_default: bool,
    /// The fetched default branch holds this head's work, in a repository
    /// that does not promote: a merged pull request, whatever its style.
    pub(crate) default_landing: Option<DefaultBranchLanding>,
    /// Promoted, represented, or already on the remote default branch.
    pub(crate) head_is_delivered: bool,
    /// Commits not represented on any delivery target; `finish` refuses
    /// while this is above zero.
    pub(crate) unsubmitted_commits: u64,
}

/// Where a session's work landed on the fetched default branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DefaultBranchLanding {
    /// The default branch, e.g. `origin/main`.
    pub(crate) reference: String,
    /// The default branch's commit the landing was judged against.
    pub(crate) tip: String,
    /// The commit that carried the work, when the evidence names one.
    pub(crate) landed_by: Option<String>,
}

#[cfg(test)]
mod checkout_cleanup_grace_tests {
    use super::*;

    #[test]
    fn a_held_gc_lock_reports_reclaim_as_deferred_not_skipped() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["commit", "-q", "--allow-empty", "-m", "fixture"],
        ] {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        }
        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(root).unwrap();

        let mut free = FinishCleanupReport::default();
        broker.wake_checkout_cleanup_after_close(1, &mut free);
        assert!(!free.reclaim_deferred);
        assert_eq!(free.reclaim_worker_pid, None);
        let json = serde_json::to_value(&free).unwrap();
        assert!(
            json.get("reclaim_deferred").is_none() && json.get("reclaim_worker_pid").is_none(),
            "absent unless deferred, so existing finish shapes are unchanged: {json}"
        );

        // A live process (this one) holds the GC lock, as the background
        // reclaim worker does while it runs.
        std::fs::write(
            root.join(".aethyme/gc.lock"),
            format!("{}\n", std::process::id()),
        )
        .unwrap();
        let mut held = FinishCleanupReport::default();
        broker.wake_checkout_cleanup_after_close(1, &mut held);
        assert!(held.reclaim_deferred);
        assert_eq!(held.reclaim_worker_pid, Some(i64::from(std::process::id())));
        let json = serde_json::to_value(&held).unwrap();
        assert_eq!(json["reclaim_deferred"], true);
        assert_eq!(json["reclaim_worker_pid"], i64::from(std::process::id()));
        assert_eq!(json["attempted"], false);
        let saved: FinishCleanupReport =
            serde_json::from_str(r#"{"requested":true,"kept":false,"attempted":false,"completed":false,"reclaimed_bytes":0,"worktree_removed":false,"branch_removed":false,"branch_ref":null,"branch_tip":null,"failure":null,"recovery_action":null}"#)
                .unwrap();
        assert!(
            !saved.reclaim_deferred,
            "saved reports without the field still load"
        );
    }

    #[test]
    fn close_scheduling_uses_compact_opt_in_without_lowering_archive_retention() {
        let mut retention = crate::RetentionPolicy::default();
        assert_eq!(retention.closed_worktree_grace_hours, 24);
        assert_eq!(
            checkout_cleanup_request_policy_for(SessionOrigin::Spawned, &retention, true),
            Some(CheckoutCleanupRequestPolicy {
                grace_hours: 24,
                compact_checkout: false,
            })
        );

        retention.compact_closed_worktrees = true;
        assert_eq!(
            checkout_cleanup_request_policy_for(SessionOrigin::Spawned, &retention, true),
            Some(CheckoutCleanupRequestPolicy {
                grace_hours: 24,
                compact_checkout: true,
            })
        );
        assert_eq!(retention.closed_worktree_grace_hours, 24);
        assert_eq!(
            checkout_cleanup_request_policy_for(SessionOrigin::Adopted, &retention, true),
            None
        );
        assert_eq!(
            checkout_cleanup_request_policy_for(SessionOrigin::Spawned, &retention, false),
            None
        );

        retention.auto_cleanup_worktrees_on_finish = false;
        assert_eq!(
            checkout_cleanup_request_policy_for(SessionOrigin::Spawned, &retention, true),
            None
        );
    }
}
