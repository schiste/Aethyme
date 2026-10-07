use super::*;

impl Broker {
    /// Detect live-session leases that overlap already-promoted work on
    /// the integration branch. This intentionally does NOT keep leases for
    /// cleaned sessions alive: closed-session rows remain purged, and the
    /// promoted branch is its own conflict surface.
    pub(super) fn promoted_conflicts(&self) -> Result<Vec<PromotedConflict>, BrokerOpError> {
        Ok(self.promoted_conflicts_within(None)?.0)
    }

    /// [`Self::promoted_conflicts`], stopping at `deadline`. The flag is true
    /// when it stopped before judging every leased session, so a caller can
    /// say the list is incomplete rather than that there is nothing more.
    pub(super) fn promoted_conflicts_within(
        &self,
        deadline: Option<std::time::Instant>,
    ) -> Result<(Vec<PromotedConflict>, bool), BrokerOpError> {
        use std::collections::{BTreeMap, BTreeSet};

        use crate::leases::{LeaseIgnoreRules, paths_overlap};

        let Some(integration) = self.integration_tip() else {
            return Ok((Vec::new(), false));
        };
        let rules = LeaseIgnoreRules::load(&self.main_root);
        let mut leases_by_session: BTreeMap<i64, Vec<String>> = BTreeMap::new();
        for lease in self.store.active_leases()? {
            if !rules.is_ignored(&lease.path) {
                leases_by_session
                    .entry(lease.session_id)
                    .or_default()
                    .push(lease.path);
            }
        }
        if leases_by_session.is_empty() {
            return Ok((Vec::new(), false));
        }

        let mut conflicts = BTreeSet::new();
        for session in self.store.live_sessions()? {
            if !matches!(
                session.status,
                SessionStatus::Active | SessionStatus::Idle | SessionStatus::Stale
            ) {
                continue;
            }
            let Some(session_paths) = leases_by_session.get(&session.id) else {
                continue;
            };
            if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                return Ok((conflicts.into_iter().collect(), true));
            }
            let Ok(checkout) = GitRepo::discover(Path::new(&session.worktree_path)) else {
                continue;
            };
            let Ok(session_head) = checkout.head_commit() else {
                continue;
            };
            if self.submitted_head_is_represented_on(session.id, &session_head, &integration)? {
                continue;
            }
            let Ok(base) = checkout.merge_base(&integration, "HEAD") else {
                continue;
            };
            if base == integration {
                continue;
            }
            let Ok(promoted_paths) = self.repo.changed_between(&base, &integration) else {
                continue;
            };
            for promoted_path in promoted_paths
                .into_iter()
                .filter(|path| !rules.is_ignored(path))
            {
                for session_path in session_paths {
                    if !paths_overlap(session_path, &promoted_path) {
                        continue;
                    }
                    let path = if session_path.len() >= promoted_path.len() {
                        session_path.clone()
                    } else {
                        promoted_path.clone()
                    };
                    conflicts.insert(PromotedConflict {
                        session_id: session.id,
                        path,
                        session_path: session_path.clone(),
                        promoted_path: promoted_path.clone(),
                    });
                }
            }
        }
        Ok((conflicts.into_iter().collect(), false))
    }

    // ── checkpoint recovery and conflict-scoped repair ────────────────

    /// Recover a blocked session by applying the documented local rebase
    /// path when there is an actionable conflict, then refresh leases and
    /// return the affected gate selection. Does not submit or promote.
    pub fn plan_session_checkpoint_recovery(
        &mut self,
        session_id: i64,
    ) -> Result<SessionCheckpointRecoveryPlan, BrokerOpError> {
        let session = self.store.session(session_id)?;
        let checkout = GitRepo::discover(Path::new(&session.worktree_path))?;
        let session_head = checkout.head_commit()?;
        let integration_branch = crate::PromoteConfig::load(&self.main_root).branch;
        let integration_head = self.integration_tip();
        let old_checkpoint = session.accepted_session_head.clone();
        let preservation_branch = format!(
            "aethyme/recovery/session-{}-{}",
            session_id,
            session_head.get(..12).unwrap_or(&session_head)
        );
        let clean_worktree = !checkout.is_dirty()?;
        let mut refusals = Vec::new();
        let mut refusal_codes = Vec::new();
        if !clean_worktree {
            record_checkpoint_refusal(
                &mut refusals,
                &mut refusal_codes,
                CheckpointRefusalCode::DirtyWorktree,
                "session worktree is dirty; commit through the managed hook before planning recovery",
            );
        }
        let Some(old) = old_checkpoint.as_deref() else {
            record_checkpoint_refusal(
                &mut refusals,
                &mut refusal_codes,
                CheckpointRefusalCode::MissingAcceptedCheckpoint,
                "session has no accepted checkpoint to re-anchor",
            );
            return finish_checkpoint_recovery_plan(SessionCheckpointRecoveryPlan {
                session_id,
                old_checkpoint,
                proposed_checkpoint: integration_head.clone(),
                session_head,
                integration_branch,
                integration_head,
                integration_relation: None,
                ahead_commits: 0,
                behind_commits: 0,
                pending_commits: Vec::new(),
                submission_plan: None,
                preservation_branch,
                clean_worktree,
                safe: false,
                refusals,
                refusal_codes,
                next_actions: Vec::new(),
                digest: String::new(),
            });
        };
        if checkout.resolve_ref(old).is_none() {
            record_checkpoint_refusal(
                &mut refusals,
                &mut refusal_codes,
                CheckpointRefusalCode::MissingAcceptedCheckpointObject,
                format!("accepted checkpoint {old} is missing"),
            );
        } else if checkout.is_ancestor(old, &session_head) {
            record_checkpoint_refusal(
                &mut refusals,
                &mut refusal_codes,
                CheckpointRefusalCode::NoReanchorRequired,
                format!(
                    "accepted checkpoint {old} is still an ancestor of session HEAD; no re-anchor is required"
                ),
            );
        }

        let mut relation = None;
        let mut ahead_commits = 0;
        let mut behind_commits = 0;
        let mut pending_commits = Vec::new();
        let mut submission_plan = None;
        if let Some(integration) = integration_head.as_deref() {
            ahead_commits = checkout.commit_count_between(integration, &session_head)?;
            behind_commits = checkout.commit_count_between(&session_head, integration)?;
            let current_relation = match (ahead_commits, behind_commits) {
                (0, 0) => AdoptIntegrationRelation::Current,
                (0, _) => AdoptIntegrationRelation::Behind,
                (_, 0) => AdoptIntegrationRelation::Ahead,
                _ => AdoptIntegrationRelation::Diverged,
            };
            relation = Some(current_relation);
            if !matches!(
                current_relation,
                AdoptIntegrationRelation::Current | AdoptIntegrationRelation::Ahead
            ) {
                record_checkpoint_refusal(
                    &mut refusals,
                    &mut refusal_codes,
                    CheckpointRefusalCode::IntegrationNotAncestor,
                    format!(
                        "session HEAD is {} relative to {integration_branch}; recovery requires integration to be an ancestor of the session",
                        current_relation.as_str()
                    ),
                );
            }

            match session.accepted_integration_commit.as_deref() {
                Some(accepted_integration)
                    if checkout.resolve_ref(accepted_integration).is_none() =>
                {
                    record_checkpoint_refusal(
                        &mut refusals,
                        &mut refusal_codes,
                        CheckpointRefusalCode::MissingAcceptedIntegrationProof,
                        format!(
                            "recorded accepted integration commit {accepted_integration} is missing"
                        ),
                    );
                }
                Some(accepted_integration)
                    if !checkout.is_ancestor(accepted_integration, integration) =>
                {
                    record_checkpoint_refusal(
                        &mut refusals,
                        &mut refusal_codes,
                        CheckpointRefusalCode::AcceptedIntegrationNotContained,
                        format!(
                            "current integration {integration} does not contain the recorded accepted integration commit {accepted_integration}"
                        ),
                    );
                }
                None => record_checkpoint_refusal(
                    &mut refusals,
                    &mut refusal_codes,
                    CheckpointRefusalCode::MissingAcceptedIntegrationProof,
                    "session has no recorded accepted integration commit proving the old contribution was preserved",
                ),
                Some(_) => {}
            }

            if matches!(
                current_relation,
                AdoptIntegrationRelation::Current | AdoptIntegrationRelation::Ahead
            ) {
                let mut candidate = session.clone();
                candidate.accepted_session_head = Some(integration.to_string());
                candidate.diff_base = Some(integration.to_string());
                match self.build_submission_plan(&candidate, &session_head, integration) {
                    Ok(plan) => {
                        pending_commits = plan.pending_owned_commit_ids();
                        if let Err(error) = self.validate_submission_plan(&plan) {
                            record_checkpoint_refusal(
                                &mut refusals,
                                &mut refusal_codes,
                                CheckpointRefusalCode::SubmissionProvenanceUnsafe,
                                format!(
                                    "normalized submission provenance is not executable: {error}"
                                ),
                            );
                        }
                        submission_plan = Some(plan);
                    }
                    Err(error) => record_checkpoint_refusal(
                        &mut refusals,
                        &mut refusal_codes,
                        CheckpointRefusalCode::SubmissionProvenanceUnavailable,
                        format!("normalized submission provenance could not be built: {error}"),
                    ),
                }
            }
        } else {
            record_checkpoint_refusal(
                &mut refusals,
                &mut refusal_codes,
                CheckpointRefusalCode::MissingIntegrationRef,
                format!("integration ref refs/heads/{integration_branch} is missing"),
            );
        }

        let safe = refusals.is_empty();
        finish_checkpoint_recovery_plan(SessionCheckpointRecoveryPlan {
            session_id,
            old_checkpoint,
            proposed_checkpoint: integration_head.clone(),
            session_head,
            integration_branch,
            integration_head,
            integration_relation: relation,
            ahead_commits,
            behind_commits,
            pending_commits,
            submission_plan,
            preservation_branch,
            clean_worktree,
            safe,
            refusals,
            refusal_codes,
            next_actions: Vec::new(),
            digest: String::new(),
        })
    }

    pub fn apply_session_checkpoint_recovery(
        &mut self,
        session_id: i64,
        confirm: &str,
    ) -> Result<SessionCheckpointApplyReport, BrokerOpError> {
        if confirm.len() != 64
            || !confirm
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        {
            return Err(BrokerOpError::CheckpointConfirmationNotSha256);
        }
        let plan = self.plan_session_checkpoint_recovery(session_id)?;
        if plan.digest != confirm {
            return Err(BrokerOpError::CheckpointConfirmationMismatch {
                actual: confirm.to_string(),
            });
        }
        if !plan.safe {
            return Err(BrokerOpError::UnsafeCheckpointRecovery {
                reasons: plan.refusals.join("; "),
            });
        }
        let old_checkpoint = plan
            .old_checkpoint
            .clone()
            .expect("safe recovery has an old checkpoint");
        let proposed_checkpoint = plan
            .proposed_checkpoint
            .clone()
            .expect("safe recovery has a proposed checkpoint");
        let preservation_ref = format!("refs/heads/{}", plan.preservation_branch);
        match self.repo.resolve_ref(&preservation_ref) {
            Some(actual) if actual != plan.session_head => {
                return Err(BrokerOpError::CheckpointPreservationRefConflict {
                    reference: preservation_ref,
                    actual,
                    expected: plan.session_head,
                });
            }
            Some(_) => {}
            None => self.repo.update_branch_ref_checked(
                &plan.preservation_branch,
                &plan.session_head,
                "0000000000000000000000000000000000000000",
            )?,
        }
        let event_payload = crate::events::session_checkpoint_reanchored_payload(
            &old_checkpoint,
            &proposed_checkpoint,
            &plan.session_head,
            &plan.digest,
            &preservation_ref,
        );
        self.store.reanchor_session_checkpoint(
            session_id,
            &old_checkpoint,
            &proposed_checkpoint,
            &event_payload,
        )?;
        Ok(SessionCheckpointApplyReport {
            plan,
            applied: true,
            accepted_session_head: proposed_checkpoint,
            preservation_ref,
        })
    }

    pub fn repair(&mut self, session_id: i64) -> Result<RepairReport, BrokerOpError> {
        let session = self.store.session(session_id)?;
        let worktree_path = session.worktree_path.clone();
        let checkout = GitRepo::discover(Path::new(&worktree_path))?;
        let (source, base) = self.repair_target(session_id)?;
        if source == RepairSource::None {
            return Err(BrokerOpError::RepairNotApplicable { id: session_id });
        }
        let plan_base = match base.as_deref() {
            Some(base) => base.to_string(),
            None => self.integration_head()?.1,
        };
        checkout.fetch_local_commit(&plan_base)?;
        let session_head = checkout.head_commit()?;
        let submission_plan = self.build_submission_plan(&session, &session_head, &plan_base)?;
        let pending_commits = submission_plan.pending_owned_commit_ids();
        let action = if let Some(base) = base.as_deref() {
            let dirty = checkout.dirty_paths()?;
            if !dirty.is_empty() {
                return Err(BrokerOpError::DirtyWorktree {
                    id: session_id,
                    reason: format!(
                        "worktree has uncommitted changes; commit through the managed pre-commit lane before repair, e.g. {}",
                        dirty.first().map(String::as_str).unwrap_or("-")
                    ),
                });
            }
            let repair_upstream =
                submission_plan
                    .automatic_repair_upstream()
                    .map_err(|reason| {
                        self.unsafe_repair_plan_error(
                            &session,
                            &submission_plan,
                            &session_head,
                            base,
                            reason,
                        )
                    })?;
            if let Some(repair_upstream) = repair_upstream {
                let upstream_is_integrated_session_commit = submission_plan.commits.iter().any(
                    |commit| {
                        commit.commit == repair_upstream
                            && commit.ownership
                                == crate::SubmissionCommitOwnership::SessionOwned
                            && matches!(
                                commit.integration_state,
                                crate::SubmissionIntegrationState::AlreadyIntegratedByAncestry
                                    | crate::SubmissionIntegrationState::AlreadyIntegratedByStablePatchIdentity
                            )
                    },
                );
                let upstream_is_accepted_checkpoint = session.accepted_session_head.as_deref()
                    == Some(repair_upstream.as_str())
                    && session.accepted_integration_commit.as_deref().is_some_and(
                        |accepted_integration| checkout.is_ancestor(accepted_integration, base),
                    );
                if !checkout.is_ancestor(&repair_upstream, base)
                    && !upstream_is_integrated_session_commit
                    && !upstream_is_accepted_checkpoint
                {
                    return Err(self.unsafe_repair_plan_error(
                        &session,
                        &submission_plan,
                        &session_head,
                        base,
                        format!(
                            "target integration {base} does not contain replay boundary {repair_upstream}, and SubmissionPlan does not prove that boundary already integrated"
                        ),
                    ));
                }
                checkout
                    .rebase_onto_range(base, &repair_upstream)
                    .map_err(|err| BrokerOpError::RepairRebaseFailed {
                        id: session_id,
                        base: base.to_string(),
                        message: err.to_string(),
                    })?;
                // This is the target below the replayed pending suffix, never
                // the pending tip itself. Submission provenance may use it
                // only when the older accepted session checkpoint was
                // necessarily rewritten by this broker-controlled repair.
                self.store.set_session_diff_base(session_id, base)?;
                let _ = std::fs::remove_file(
                    Path::new(&worktree_path).join(crate::ACTION_REQUIRED_RELPATH),
                );
                RepairAction::Rebased
            } else {
                RepairAction::None
            }
        } else {
            RepairAction::None
        };

        self.refresh_leases()?;
        let affected_gates = self
            .affected_gates(session_id)?
            .into_iter()
            .map(|(gate, triggered_by)| RepairGateSelection { gate, triggered_by })
            .collect();
        Ok(RepairReport {
            session_id,
            worktree_path,
            source,
            action,
            base,
            pending_commits,
            submission_plan,
            leases_refreshed: true,
            affected_gates,
            next_command: format!("aethyme broker submit --session {session_id}"),
        })
    }

    pub(super) fn unsafe_repair_plan_error(
        &self,
        session: &Session,
        plan: &crate::SubmissionPlan,
        session_head: &str,
        target: &str,
        reason: String,
    ) -> BrokerOpError {
        let commits = plan.preservation_commit_ids();
        let preserve_branch = format!(
            "aethyme/preserve-session-{}-{}",
            session.id,
            &session_head[..12.min(session_head.len())]
        );
        let commits_text = if commits.is_empty() {
            format!("  - {session_head} (ownership ambiguous; preserve the complete session tip)")
        } else {
            commits
                .iter()
                .map(|commit| format!("  - {commit}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let cherry_pick = if commits.is_empty() {
            format!("git log --reverse --oneline {target}..{preserve_branch}")
        } else {
            format!("git cherry-pick {}", commits.join(" "))
        };
        let guidance = [
            format!("  1. git branch {preserve_branch} {session_head}"),
            format!("  2. git reset --hard {target}"),
            format!(
                "  3. aethyme broker start --reuse --sync-integration --task \"continue preserved session {}\" --short-name \"Continue\"",
                session.id
            ),
            format!("  4. {cherry_pick}"),
            format!("  5. aethyme broker submit --session {}", session.id),
        ]
        .join("\n");
        BrokerOpError::UnsafeRepairPlan {
            id: session.id,
            reason,
            commits: commits_text,
            guidance,
        }
    }

    pub(super) fn repair_target(
        &mut self,
        session_id: i64,
    ) -> Result<(RepairSource, Option<String>), BrokerOpError> {
        let latest = self
            .store
            .merge_queue()?
            .into_iter()
            .rfind(|entry| entry.session_id == session_id);
        if let Some(entry) = latest
            && entry.status == MergeStatus::Conflict
        {
            let base = details_string_value(entry.details_json.as_deref(), "base")
                .unwrap_or(entry.base_commit);
            return Ok((RepairSource::LatestSubmitConflict, Some(base)));
        }

        self.refresh_leases()?;
        if self
            .promoted_conflicts()?
            .iter()
            .any(|conflict| conflict.session_id == session_id)
        {
            let (_branch, head) = self.integration_head()?;
            return Ok((RepairSource::PromotedConflict, Some(head)));
        }

        Ok((RepairSource::None, None))
    }

    // ── gates (Phase 4) ───────────────────────────────────────────────

    /// Affected-gate selection for a session's current diff, without
    /// running anything (`gates affected [--why]`).
    pub fn affected_gates(
        &mut self,
        session_id: i64,
    ) -> Result<Vec<(String, Option<String>)>, BrokerOpError> {
        Ok(self.affected_gates_with_timings(session_id)?.selected_gates)
    }

    pub(crate) fn affected_gates_with_timings(
        &mut self,
        session_id: i64,
    ) -> Result<AffectedGatesReport, BrokerOpError> {
        let (_, gates, changed, mut phase_timings_ms) =
            self.gate_selection_inputs_with_timings(session_id)?;
        let selection_started = std::time::Instant::now();
        let selected_gates = crate::gates::select_gates(&gates, &changed)
            .into_iter()
            .map(|s| (s.gate.name.clone(), s.triggered_by))
            .collect();
        phase_timings_ms.selection = selection_started.elapsed().as_millis() as u64;
        let over_budget_phases = phase_timings_ms.over_budget_phases();

        Ok(AffectedGatesReport {
            selected_gates,
            phase_timings_ms,
            phase_budget_ms: GATES_AFFECTED_PHASE_BUDGET_MS,
            over_budget_phases,
        })
    }

    /// Advisory semantic gate-selection surface. The returned semantic
    /// suggestions are not used by [`Self::run_gates`], submit, or CI.
    pub fn semantic_gate_advice(
        &mut self,
        session_id: i64,
    ) -> Result<SemanticGateAdvice, BrokerOpError> {
        let (_, gates, changed) = self.gate_selection_inputs(session_id)?;
        let path_selected_gates = crate::gates::select_gates(&gates, &changed)
            .into_iter()
            .map(|selection| {
                let triggered_by = selection.triggered_by;
                let reason = if triggered_by.is_some() {
                    "path trigger"
                } else {
                    "always runs"
                };
                SemanticGateSelection {
                    gate: selection.gate.name.clone(),
                    triggered_by,
                    reason: reason.into(),
                    chain: None,
                }
            })
            .collect::<Vec<_>>();

        let lookup = self
            .graph_impact_provider
            .lookup(&GraphImpactQuery {
                repo_root: &self.main_root,
                changed_files: &changed,
                mode: GraphImpactMode::Calls,
                max_results: GRAPH_IMPACT_RESULT_LIMIT,
                max_depth: GRAPH_IMPACT_MAX_DEPTH,
                max_nodes: GRAPH_IMPACT_MAX_NODES,
            })
            .bounded(GRAPH_IMPACT_RESULT_LIMIT);
        let path_selected_names = path_selected_gates
            .iter()
            .map(|selection| selection.gate.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        let semantic_suggested_gates = if lookup.status == GraphImpactStatus::Ready {
            crate::gates::select_gates(&gates, &lookup.impacted_paths)
                .into_iter()
                .filter(|selection| !path_selected_names.contains(selection.gate.name.as_str()))
                .map(|selection| {
                    let gate = selection.gate.name.clone();
                    let triggered_by = selection.triggered_by;
                    let chain = triggered_by.as_ref().and_then(|caller_file| {
                        lookup
                            .chains
                            .iter()
                            .find(|chain| &chain.caller_file == caller_file)
                            .map(|chain| SemanticGateSuggestionChain {
                                changed_file: chain.changed_file.clone(),
                                caller_file: chain.caller_file.clone(),
                                suggested_gate: gate.clone(),
                            })
                    });
                    SemanticGateSelection {
                        gate,
                        triggered_by,
                        reason: format!("incoming {} frontier", lookup.mode.label()),
                        chain,
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        let semantic = SemanticGateSource {
            provider: self.graph_impact_provider.name().into(),
            mode: lookup.mode,
            status: lookup.status,
            reason: lookup.explanation,
            graph_store_path: ".aethyme/graph_store.redb".into(),
            graph_fragments_path: ".aethyme/graph/".into(),
            impacted_paths: lookup.impacted_paths,
            chains: lookup.chains,
            result_limit: GRAPH_IMPACT_RESULT_LIMIT,
            frontier_max_depth: GRAPH_IMPACT_MAX_DEPTH,
            frontier_max_nodes: GRAPH_IMPACT_MAX_NODES,
            frontier_visited_nodes: lookup.visited_nodes,
            truncated: lookup.truncated,
        };

        let next_action = if path_selected_gates.is_empty() {
            "No path-triggered gates are selected; semantic suggestions are advisory and currently do not add enforced gates.".into()
        } else {
            format!(
                "Run `aethyme broker advanced gates run --session {session_id}` to execute the enforced path-triggered gates; treat semantic suggestions as hints only."
            )
        };

        Ok(SemanticGateAdvice {
            session_id,
            mode: "advisory".into(),
            enforced: false,
            changed_files: changed,
            path_selected_gates,
            semantic_suggested_gates,
            semantic,
            next_action,
        })
    }

    /// Evaluate the read-only graph-impact contract for an exact revision and
    /// diff. This is intentionally independent from gate selection: callers
    /// receive provenance and conservative status, while repository-owned
    /// gate policy remains the only authority for mandatory checks.
    pub fn graph_impact_report(
        &mut self,
        revision: &str,
        changed_files: Vec<String>,
        mode: GraphImpactMode,
        budget: usize,
    ) -> Result<GraphImpactReport, BrokerOpError> {
        let report = self.graph_impact_evaluate(revision, changed_files, mode, budget)?;
        self.store().append_event(
            crate::events::GRAPH_IMPACT_EVALUATED,
            None,
            Some(&crate::events::graph_impact_evaluated_payload(&report)),
        )?;
        Ok(report)
    }

    /// Evaluate graph impact without recording it, so a read-only broker
    /// snapshot can answer: [`Self::graph_impact_report`] is this plus the
    /// `graph.impact_evaluated` event.
    pub fn graph_impact_evaluate(
        &self,
        revision: &str,
        changed_files: Vec<String>,
        mode: GraphImpactMode,
        budget: usize,
    ) -> Result<GraphImpactReport, BrokerOpError> {
        let resolved_revision =
            self.repo
                .resolve_ref(revision)
                .ok_or_else(|| BrokerOpError::GraphImpactInvalid {
                    reason: format!("revision {revision:?} does not resolve in this repository"),
                })?;
        revision_bound_impact_report(
            &self.main_root,
            revision,
            &resolved_revision,
            &changed_files,
            mode,
            budget,
            self.graph_impact_provider.as_ref(),
        )
        .map_err(|error| BrokerOpError::GraphImpactInvalid {
            reason: error.to_string(),
        })
    }

    /// Run the affected gates for a session's worktree: cheap-first,
    /// tree-hash cached, cancelling this session's obsolete in-flight
    /// runs first. Stops at the first failure.
    pub fn run_gates(
        &mut self,
        session_id: i64,
    ) -> Result<Vec<crate::gates::GateRunOutcome>, BrokerOpError> {
        self.run_gates_with_policy(session_id, crate::gates::CachePolicy::Use)
    }

    /// Run affected session gates with an explicit cache lookup policy.
    pub fn run_gates_with_policy(
        &mut self,
        session_id: i64,
        cache_policy: crate::gates::CachePolicy,
    ) -> Result<Vec<crate::gates::GateRunOutcome>, BrokerOpError> {
        let (checkout, gates, changed) = self.gate_inputs(session_id)?;
        crate::gates::run_affected(
            &mut self.store,
            &self.main_root,
            &checkout,
            &gates,
            &changed,
            Some(session_id),
            cache_policy,
        )
    }

    /// Run one configured gate for a session, regardless of whether its
    /// path triggers match the current diff. This is the targeted rerun lane
    /// after a gate failure; resource ownership still uses matching changed
    /// paths when they are available.
    pub fn run_named_gate_with_policy(
        &mut self,
        session_id: i64,
        gate_name: &str,
        cache_policy: crate::gates::CachePolicy,
    ) -> Result<Vec<crate::gates::GateRunOutcome>, BrokerOpError> {
        let (checkout, gates, changed) = self.gate_inputs(session_id)?;
        crate::gates::run_named(
            &mut self.store,
            &self.main_root,
            &checkout,
            &gates,
            &changed,
            gate_name,
            Some(session_id),
            cache_policy,
        )
    }

    /// Test/non-CLI entrypoint for gate runs with injectable progress
    /// reporting. The default [`Self::run_gates`] sink writes to stderr.
    pub fn run_gates_with_progress(
        &mut self,
        session_id: i64,
        progress: &dyn crate::gates::GateProgressSink,
    ) -> Result<Vec<crate::gates::GateRunOutcome>, BrokerOpError> {
        self.run_gates_with_policy_and_progress(
            session_id,
            crate::gates::CachePolicy::Use,
            progress,
        )
    }

    /// Run affected session gates with explicit cache policy and progress.
    pub fn run_gates_with_policy_and_progress(
        &mut self,
        session_id: i64,
        cache_policy: crate::gates::CachePolicy,
        progress: &dyn crate::gates::GateProgressSink,
    ) -> Result<Vec<crate::gates::GateRunOutcome>, BrokerOpError> {
        let (checkout, gates, changed) = self.gate_inputs(session_id)?;
        crate::gates::run_affected_with_progress(
            &mut self.store,
            &self.main_root,
            &checkout,
            &gates,
            &changed,
            Some(session_id),
            crate::gates::GateExecutionContext {
                cache_policy,
                progress,
            },
        )
    }

    /// Cancel this session's in-flight gate runs whose tree differs from
    /// the worktree's current state (also done automatically at the start
    /// of [`Self::run_gates`]). Returns the cancelled gate names.
    pub fn cancel_obsolete_gate_runs(
        &mut self,
        session_id: i64,
    ) -> Result<Vec<String>, BrokerOpError> {
        let session = self.store.session(session_id)?;
        let checkout = GitRepo::discover(Path::new(&session.worktree_path))?;
        let tree = checkout.working_tree_hash()?;
        Ok(crate::gates::cancel_obsolete_runs(
            &mut self.store,
            &self.main_root,
            session_id,
            &tree,
        )?)
    }

    /// Run every configured gate against the checkout containing `dir`,
    /// in cost order with no diff selection (`gates run --all`) — the CI
    /// entrypoint, making gates.toml the single definition of "verified"
    /// for CI and broker alike. No session attribution: results are still
    /// recorded and tree-hash cached, so a broker run on the same tree
    /// reuses them (and vice versa).
    pub fn run_all_gates(
        &mut self,
        dir: &Path,
    ) -> Result<Vec<crate::gates::GateRunOutcome>, BrokerOpError> {
        self.run_all_gates_with_policy(dir, crate::gates::CachePolicy::Use)
    }

    /// Run every configured gate with an explicit cache lookup policy.
    pub fn run_all_gates_with_policy(
        &mut self,
        dir: &Path,
        cache_policy: crate::gates::CachePolicy,
    ) -> Result<Vec<crate::gates::GateRunOutcome>, BrokerOpError> {
        let checkout = GitRepo::discover(dir)?;
        self.record_graph_integrity(&checkout, None)?;
        let config_root = checkout.root().to_path_buf();
        let gates = self.load_and_sync_gates_from(&config_root)?;
        crate::gates::run_all(
            &mut self.store,
            &self.main_root,
            &checkout,
            &gates,
            None,
            cache_policy,
        )
    }

    /// Run one configured gate against the checkout containing `dir`.
    pub fn run_named_gate_for_checkout_with_policy(
        &mut self,
        dir: &Path,
        gate_name: &str,
        cache_policy: crate::gates::CachePolicy,
    ) -> Result<Vec<crate::gates::GateRunOutcome>, BrokerOpError> {
        let checkout = GitRepo::discover(dir)?;
        self.record_graph_integrity(&checkout, None)?;
        let config_root = checkout.root().to_path_buf();
        let gates = self.load_and_sync_gates_from(&config_root)?;
        crate::gates::run_named(
            &mut self.store,
            &self.main_root,
            &checkout,
            &gates,
            &[],
            gate_name,
            None,
            cache_policy,
        )
    }

    /// Validate Git's exact outgoing tip, then run the repository's complete
    /// gate set. The repository owns the hook; the broker owns truthful
    /// planning, gate execution, and any declared host-resource leases.
    pub fn run_pre_push_gates(
        &mut self,
        dir: &Path,
        remote: &str,
        hook_input: &str,
        cache_policy: crate::gates::CachePolicy,
    ) -> Result<crate::gates::PrePushReport, BrokerOpError> {
        let checkout = GitRepo::discover(dir)?;
        let plan = crate::gates::plan_pre_push(&checkout, remote, hook_input)?;
        let gate_outcomes = if plan.pushed_sha.is_some() {
            self.run_all_gates_with_policy(dir, cache_policy)?
        } else {
            Vec::new()
        };
        Ok(crate::gates::PrePushReport {
            plan,
            gate_outcomes,
        })
    }

    /// Test/non-CLI entrypoint for [`Self::run_all_gates`] with injectable
    /// progress reporting.
    pub fn run_all_gates_with_progress(
        &mut self,
        dir: &Path,
        progress: &dyn crate::gates::GateProgressSink,
    ) -> Result<Vec<crate::gates::GateRunOutcome>, BrokerOpError> {
        self.run_all_gates_with_policy_and_progress(dir, crate::gates::CachePolicy::Use, progress)
    }

    /// Run all gates with explicit cache policy and progress reporting.
    pub fn run_all_gates_with_policy_and_progress(
        &mut self,
        dir: &Path,
        cache_policy: crate::gates::CachePolicy,
        progress: &dyn crate::gates::GateProgressSink,
    ) -> Result<Vec<crate::gates::GateRunOutcome>, BrokerOpError> {
        let checkout = GitRepo::discover(dir)?;
        self.record_graph_integrity(&checkout, None)?;
        let config_root = checkout.root().to_path_buf();
        let gates = self.load_and_sync_gates_from(&config_root)?;
        crate::gates::run_all_with_progress(
            &mut self.store,
            &self.main_root,
            &checkout,
            &gates,
            None,
            cache_policy,
            progress,
        )
    }

    pub(super) fn gate_inputs(
        &mut self,
        session_id: i64,
    ) -> Result<(GitRepo, Vec<crate::gates::Gate>, Vec<String>), BrokerOpError> {
        self.gate_inputs_with_integrity(session_id, true)
    }

    /// Read-only gate selection does not rebuild graph artifacts in the
    /// repository-wide verification slot. Execution records that verdict
    /// before its gates run. Selection therefore never reports graph
    /// integrity, and graph-backed semantic advice on this path reads the
    /// graph without exact-tree verification. That advice is only advisory,
    /// and no gate runs on it.
    pub(super) fn gate_selection_inputs(
        &mut self,
        session_id: i64,
    ) -> Result<(GitRepo, Vec<crate::gates::Gate>, Vec<String>), BrokerOpError> {
        let (checkout, gates, changed, _) = self.gate_selection_inputs_with_timings(session_id)?;
        Ok((checkout, gates, changed))
    }

    pub(super) fn gate_selection_inputs_with_timings(
        &mut self,
        session_id: i64,
    ) -> Result<
        (
            GitRepo,
            Vec<crate::gates::Gate>,
            Vec<String>,
            AffectedGatePhaseTimings,
        ),
        BrokerOpError,
    > {
        let graph_read_started = std::time::Instant::now();
        let session = self.store.session(session_id)?;
        let checkout = GitRepo::discover(Path::new(&session.worktree_path))?;
        let graph_read_before_manifest = graph_read_started.elapsed();

        let manifest_started = std::time::Instant::now();
        let config_root = checkout.root().to_path_buf();
        let gates = self.load_and_sync_gates_from(&config_root)?;
        let manifest_ms = manifest_started.elapsed().as_millis() as u64;

        let diff_started = std::time::Instant::now();
        let base = self
            .session_change_base(&checkout)
            .or(session.diff_base)
            .unwrap_or_else(|| "HEAD".to_string());
        let changed = checkout.changed_files(&base)?;
        let graph_read_ms = graph_read_before_manifest
            .saturating_add(diff_started.elapsed())
            .as_millis() as u64;

        // This inspection route intentionally skips `record_graph_integrity`;
        // no repository-wide graph-integrity lock is acquired or waited on.
        let phase_timings_ms = AffectedGatePhaseTimings {
            graph_read: graph_read_ms,
            manifest: manifest_ms,
            selection: 0,
            lock_wait: 0,
        };
        Ok((checkout, gates, changed, phase_timings_ms))
    }

    pub(super) fn gate_inputs_with_integrity(
        &mut self,
        session_id: i64,
        check_graph_integrity: bool,
    ) -> Result<(GitRepo, Vec<crate::gates::Gate>, Vec<String>), BrokerOpError> {
        let session = self.store.session(session_id)?;
        let checkout = GitRepo::discover(Path::new(&session.worktree_path))?;
        if check_graph_integrity {
            self.record_graph_integrity(&checkout, Some(session_id))?;
        }
        let config_root = checkout.root().to_path_buf();
        let gates = self.load_and_sync_gates_from(&config_root)?;
        let base = self
            .session_change_base(&checkout)
            .or(session.diff_base)
            .unwrap_or_else(|| "HEAD".to_string());
        let changed = checkout.changed_files(&base)?;
        Ok((checkout, gates, changed))
    }

    pub(super) fn record_graph_integrity(
        &mut self,
        checkout: &GitRepo,
        session_id: Option<i64>,
    ) -> Result<crate::GraphIntegrityOutcome, BrokerOpError> {
        let policy = crate::GraphIntegrityPolicy::load(checkout.root())?;
        let outcome = crate::graph_integrity::verify_checkout_without_mutation(
            &self.main_root,
            checkout,
            &policy,
        )?;
        if outcome.enforced {
            self.store.append_event(
                crate::events::GRAPH_INTEGRITY_CHECKED,
                session_id,
                Some(&crate::events::graph_integrity_checked_payload(&outcome)),
            )?;
        }
        // Advice, never a refusal (#280, #292): the verdict is recorded above
        // and surfaced by `broker status` as `graph.stale` / `graph.unknown`.
        if let Some(advice) = outcome.advice() {
            eprintln!("[graph-integrity] {advice}");
        }
        Ok(outcome)
    }

    /// Load gates.toml and sync the definition snapshot so recorded
    /// results stay interpretable after config edits.
    ///
    /// Callers that run gates load them here, and so do selection-only
    /// callers (`gates affected`, semantic advice), which run nothing. Either
    /// way this is where the checkout's policy must be trusted: nothing is
    /// synced or selected from an untrusted policy.
    pub(crate) fn load_and_sync_gates_from(
        &mut self,
        config_root: &Path,
    ) -> Result<Vec<crate::gates::Gate>, BrokerOpError> {
        let gates = crate::gates::load_gates(config_root)?;
        let prepare = crate::preparation::load_config(config_root)?;
        let policy = gate_trust::GatePolicy::from_parts(&gates, prepare.as_ref());
        self.require_trusted_policy(&policy, None)?;
        self.sync_gate_definitions(&gates)?;
        Ok(gates)
    }

    /// Refuse unless `policy` is trusted for this repository.
    pub(crate) fn require_trusted_policy(
        &mut self,
        policy: &gate_trust::GatePolicy,
        session_id: Option<i64>,
    ) -> Result<(), BrokerOpError> {
        let main_root = self.main_root.clone();
        gate_trust::require_trusted(&main_root, policy, Some(&mut self.store), session_id)?;
        Ok(())
    }

    /// Refuse unless the policy committed at `commit` -- the base a submission
    /// lands on, whose gates judge it -- is trusted for this repository.
    pub(crate) fn require_trusted_policy_at_commit(
        &mut self,
        commit: &str,
        session_id: Option<i64>,
    ) -> Result<(), BrokerOpError> {
        let policy = gate_trust::policy_at_commit(&self.repo, commit)?;
        self.require_trusted_policy(&policy, session_id)
    }

    /// Load gates.toml as committed at `commit` and sync its definitions.
    /// `None` means the commit has no gate configuration.
    pub(crate) fn load_and_sync_gates_at_commit(
        &mut self,
        commit: &str,
    ) -> Result<Option<Vec<crate::gates::Gate>>, BrokerOpError> {
        let Some(text) = self
            .repo_handle()
            .file_at_commit(commit, crate::gates::GATES_CONFIG_RELPATH)?
        else {
            return Ok(None);
        };
        let gates = crate::gates::parse_gates(&text)?;
        self.sync_gate_definitions(&gates)?;
        Ok(Some(gates))
    }

    pub(super) fn sync_gate_definitions(
        &mut self,
        gates: &[crate::gates::Gate],
    ) -> Result<(), BrokerOpError> {
        for gate in gates {
            self.store.upsert_gate(&crate::types::GateDef {
                name: gate.name.clone(),
                command: gate.command.clone(),
                cost_tier: gate.cost,
                triggers_json: serde_json::to_string(&gate.triggers)?,
                resources_json: serde_json::to_string(&gate.resources)?,
                resource_ttl_seconds: gate.resource_ttl_seconds as i64,
                resource_wait_seconds: gate.resource_wait_seconds as i64,
                managed_cache_json: gate
                    .managed_cache
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
                definition_hash: gate.definition_hash.clone(),
                updated_at: 0,
            })?;
        }
        Ok(())
    }
}
