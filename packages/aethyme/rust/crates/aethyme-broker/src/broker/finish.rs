use super::*;

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
        } else if report.delivery.promoted && !report.delivery.published {
            report
                .latest_queue_entry_id
                .map(|entry| format!("aethyme broker advanced ship plan --entry {entry}"))
        } else {
            report.next_commands.first().cloned().or_else(|| {
                (report.delivery.published && !report.cleanup_safe)
                    .then(|| "aethyme broker advanced integration status".into())
            })
        };
    }

    pub(super) fn persist_finish_report(
        &mut self,
        report: &FinishReport,
    ) -> Result<(), BrokerOpError> {
        let payload = crate::events::session_finished_payload(report);
        self.store.finish_session(report.session_id, &payload)?;
        Ok(())
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
        let worktree_path = PathBuf::from(&session.worktree_path);
        let item = self.cleanup_item(session)?;
        report.cleanup_safe = item.as_ref().is_none_or(|item| item.eligible());
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
            self.record_finished_handoff(report)?;
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

        let start_payload =
            crate::events::session_finish_cleanup_started_payload(worktree_present, branch_present);
        self.store
            .begin_finish_cleanup(report.session_id, &start_payload)?;
        // `finish --timeout` bounds the checks that decide whether to close.
        // Once closed, reclaiming the worktree runs to completion: killing a
        // `git worktree remove` of a multi-gigabyte tree halfway leaves an
        // orphaned directory instead of a faster answer.
        match crate::git::without_git_deadline(|| self.cleanup(report.session_id, false)) {
            Ok(()) => {
                report.status = FinishStatus::Cleaned;
                report.cleanup.completed = true;
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
        self.record_finished_handoff(report)?;
        Ok(())
    }

    /// Finish a session at the operator level: close it when there is no
    /// dirty work and no committed work waiting for submit/promotion;
    /// otherwise return actionable guidance without mutating state.
    pub fn finish(&mut self, session_id: i64) -> Result<FinishReport, BrokerOpError> {
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
                .active_leases()?
                .iter()
                .any(|lease| lease.session_id == session_id);
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
            match self.reclaim_closed_session_artifacts(session_id) {
                Ok(outcome) => {
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
        let session = self.store.session(session_id)?;
        let worktree_path = PathBuf::from(&session.worktree_path);
        let at_ms = now_ms();
        let queue = self.store.merge_queue()?;
        let latest_for_session = queue
            .iter()
            .rev()
            .find(|entry| entry.session_id == session_id);
        let retention = self.cleanup_retention(at_ms)?;
        let retention_warning = cleanup_retention_warning(&retention);
        // Cleanup is safe only when the repository policy can be read and
        // explicitly permits automation. A malformed policy therefore keeps
        // a worktree available for manual review rather than deleting it.
        let auto_cleanup_policy = crate::load_retention_policy(&self.main_root);
        let auto_cleanup_enabled = auto_cleanup_policy
            .as_ref()
            .is_ok_and(|policy| policy.auto_cleanup_worktrees_on_finish);
        let mut report = FinishReport {
            session_id,
            worktree_path: session.worktree_path.clone(),
            status: FinishStatus::Blocked,
            closed: false,
            dirty_paths: Vec::new(),
            unsubmitted_commits: 0,
            latest_queue_entry_id: latest_for_session.map(|entry| entry.id),
            latest_queue_status: latest_for_session.map(|entry| entry.status),
            delivery: self.finish_delivery(latest_for_session),
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
            report.status = FinishStatus::AlreadyCleaned;
            report.closed = true;
            report.cleanup.completed = true;
            report.cleanup.worktree_removed = !worktree_path.exists();
            report.summary = format!("session {session_id} is already physically cleaned");
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
            self.persist_finish_report(&report)?;
            return Ok(report);
        }

        let checkout = GitRepo::discover(&worktree_path)?;
        let head = checkout.head_commit()?;
        report.dirty_paths = checkout.dirty_paths()?;

        let delivery = self.head_delivery(&session, &checkout, &head, &queue)?;
        if let Some(entry) = delivery.visible_entry {
            report.latest_queue_entry_id = Some(entry.id);
            report.latest_queue_status = Some(entry.status);
            report.delivery = self.finish_delivery(Some(entry));
        }
        report.representation = delivery.representation;
        if delivery.on_remote_default {
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
                MergeStatus::Promoted | MergeStatus::ExternallyLanded => {}
                // A repository that never promotes leaves every entry
                // `Verified`, so demanding a promotion before finish would
                // block every session in it permanently (#290 phase 2.2).
                MergeStatus::Verified
                    if !crate::PromoteConfig::load(&self.main_root_path())
                        .mode
                        .promotes_at_all() => {}
                MergeStatus::Verified => {
                    report.warnings.push(format!(
                        "queue entry {} is verified but not promoted; promote it before finish",
                        entry.id
                    ));
                    report.next_commands.push(format!(
                        "aethyme broker submit promote --entry {}",
                        entry.id
                    ));
                    report
                        .next_commands
                        .push(format!("aethyme broker finish --session {session_id}"));
                    self.finalize_finish_report(&mut report);
                    return Ok(report);
                }
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
                            "latest submit qid {} could not run its gates on this host (low disk, \
                             locks or environment); free the resource and resubmit before finish",
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
                        "latest submit qid {} was deferred: a gate could not run on this host, \
                         so the change was not judged; free the resource and resubmit before \
                         finish",
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

        if report.unsubmitted_commits > 0 {
            // The guard is right either way -- these commits exist nowhere but
            // this branch, and closing a session holding the only copy is what
            // #222 exists to prevent. What differs is the remedy.
            //
            // Where the repository does not promote, `submit` has already run
            // and can never satisfy this: it verifies and moves nothing by
            // design (#290 phase 2.2). Naming it anyway sends the operator
            // round a loop with no exit, which is the shape of the engine-pin
            // refusal in #254. The reachable path is to land the work and then
            // prove it landed, and proving it needs `record`, not just `scan`.
            let promotes = crate::PromoteConfig::load(&self.main_root)
                .mode
                .promotes_at_all();
            if promotes {
                report.warnings.push(format!(
                    "HEAD has {} committed {} not yet represented in promoted integration; submit before finish",
                    report.unsubmitted_commits,
                    plural_word(report.unsubmitted_commits as usize, "change", "changes")
                ));
                report
                    .next_commands
                    .push(format!("aethyme broker submit --session {session_id}"));
            } else {
                report.warnings.push(format!(
                    "HEAD has {} committed {} not represented on any delivery target. This \
                     repository does not promote, so submitting again cannot change that — land \
                     the work, then prove it landed",
                    report.unsubmitted_commits,
                    plural_word(report.unsubmitted_commits as usize, "change", "changes")
                ));
            }
            // `scan` only: it is read-only, and its output names `record` with
            // the real digest. Listing `record` here would mean printing a
            // placeholder the operator cannot run -- the same unreachable
            // remedy this block exists to remove.
            report.next_commands.push(format!(
                "aethyme broker advanced representation scan --session {session_id}"
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
        if let Some((unpushed_head, unpushed)) = self.unpushed_close_check(&session) {
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
        self.persist_finish_report(&report)?;
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

        // Use cleanup's exact delivery proof so a historical queue row or a
        // stale representation record cannot make finish disagree with cleanup.
        let delivery_targets = self.cleanup_delivery_targets()?;
        let (delivery_provenance, _) = self.cleanup_provenance(session, head, &delivery_targets)?;
        let head_is_delivered =
            delivery_provenance.representation == crate::CleanupRepresentation::Represented;

        // A stored record appears in the report only when cleanup's record
        // validation finds its carrying commit on one of the current targets.
        let representation = if self
            .recorded_representation_evidence(session, head, &delivery_targets)?
            .is_some()
        {
            self.store.session_representation(session_id, head)?
        } else {
            None
        };
        let remote_default_tip = self.remote_tracking_default_tip();
        let on_remote_default = if let Some(tip) = remote_default_tip.as_ref() {
            self.landing_on_delivery_targets(head, std::slice::from_ref(tip))?
                .is_some()
        } else {
            false
        };
        let unsubmitted_commits = if head_is_delivered {
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
                let upstream = self.repo.upstream_default().map(|(_, commit)| commit);
                let base = session
                    .diff_base
                    .clone()
                    .or_else(|| {
                        integration_head.as_deref().and_then(|integration| {
                            crate::merge::session_baseline(
                                checkout,
                                integration,
                                upstream.as_deref(),
                            )
                        })
                    })
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
            unsubmitted_commits,
        })
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
        if session.origin != SessionOrigin::Spawned || path == self.main_root.as_path() {
            return false;
        }
        let Some(parent) = path.parent() else {
            return false;
        };
        let parent = parent
            .canonicalize()
            .unwrap_or_else(|_| parent.to_path_buf());
        let legacy = self
            .legacy_broker_worktree_root()
            .canonicalize()
            .unwrap_or_else(|_| self.legacy_broker_worktree_root());
        let owned_root = parent == legacy || self.worktree_root_marker_matches(&parent);
        if !owned_root {
            return false;
        }
        if !path.exists() {
            return true;
        }
        let Ok(canonical_path) = path.canonicalize() else {
            return false;
        };
        if canonical_path.parent() != Some(parent.as_path()) {
            return false;
        }
        let Ok(checkout) = GitRepo::discover(&canonical_path) else {
            // A worktree whose removal was interrupted after deregistration is
            // no longer discoverable, so this arm used to report it as "outside
            // the broker-owned worktree directory" -- a claim that is false on
            // its face, since containment under the owned root was proven
            // above. That wrong answer is what blocked the documented recovery
            // and forced a manual `rm -rf` (#165). Fall back to the orphan's
            // own `.git` pointer, which still names the repository it belonged
            // to.
            return self.is_orphaned_worktree_of_this_repository(&canonical_path);
        };
        match (checkout.git_common_dir(), self.main_git_common_dir()) {
            (Ok(actual), Ok(expected)) => actual == expected,
            _ => false,
        }
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
    /// Promoted, represented, or already on the remote default branch.
    pub(crate) head_is_delivered: bool,
    /// Commits not represented on any delivery target; `finish` refuses
    /// while this is above zero.
    pub(crate) unsubmitted_commits: u64,
}
