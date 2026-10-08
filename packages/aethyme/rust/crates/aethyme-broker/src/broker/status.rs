use super::*;

impl Broker {
    // ── status (Phase 6) ──────────────────────────────────────────────

    /// Focused status for promoted-but-unmerged work: the integration
    /// branch as a pending layer above the main checkout, plus live
    /// sessions whose leases overlap that layer.
    pub fn integration_status(
        &mut self,
        now_ms: i64,
    ) -> Result<IntegrationStatusView, BrokerOpError> {
        self.refresh_leases()?;
        self.agents(now_ms)?;
        let integration = self.integration_head()?;
        self.build_integration_status(integration)
    }

    /// Focused integration status without lease/liveness reconciliation or
    /// integration-ref creation/fast-forwarding.
    pub fn integration_status_snapshot(&self) -> Result<IntegrationStatusView, BrokerOpError> {
        let integration = self.integration_head_snapshot()?;
        self.build_integration_status(integration)
    }

    pub(super) fn build_integration_status(
        &self,
        (branch, head): (String, String),
    ) -> Result<IntegrationStatusView, BrokerOpError> {
        let main_head = self.repo.head_commit()?;
        let (upstream_ref, upstream_head) = self
            .repo
            .tracking_upstream()
            .map(|(name, commit)| (Some(name), Some(commit)))
            .unwrap_or((None, None));
        let (main_ahead_upstream_commits, main_behind_upstream_commits) =
            if let Some(upstream) = upstream_head.as_deref() {
                (
                    self.repo.commit_count_between(upstream, &main_head)?,
                    self.repo.commit_count_between(&main_head, upstream)?,
                )
            } else {
                (0, 0)
            };
        let comparison_head = upstream_head
            .as_deref()
            .filter(|_| main_behind_upstream_commits > 0)
            .unwrap_or(&main_head);
        let main_is_ancestor = self.repo.is_ancestor(&main_head, &head);
        let commits_ahead_main = self.repo.commit_count_between(&main_head, &head)?;
        let changed_files = if head == comparison_head {
            Vec::new()
        } else {
            self.repo.changed_between(comparison_head, &head)?
        };

        let mut promoted_entries = Vec::new();
        let mut latest_delivery_entry_id = None;
        for entry in self.store.merge_queue()? {
            if entry.status != MergeStatus::Promoted {
                continue;
            }
            let Some(merge_commit) = details_string_value(entry.details_json.as_deref(), "commit")
            else {
                continue;
            };
            if !self.repo.is_ancestor(&merge_commit, &head)
                || self.repo.is_ancestor(&merge_commit, &main_head)
            {
                continue;
            }
            latest_delivery_entry_id = Some(entry.id);
            if self.repo.is_ancestor(&merge_commit, comparison_head) {
                continue;
            }
            let session = self.store.session(entry.session_id).ok();
            let files = self
                .repo
                .first_parent(&merge_commit)
                .and_then(|parent| self.repo.changed_between(&parent, &merge_commit))
                .unwrap_or_default();
            promoted_entries.push(PromotedIntegrationEntry {
                queue_entry_id: entry.id,
                session_id: entry.session_id,
                branch: session.as_ref().map(|session| session.branch.clone()),
                task: session.and_then(|session| session.task),
                base_commit: entry.base_commit,
                head_commit: entry.head_commit,
                merge_commit,
                files,
            });
        }

        let conflicts = if changed_files.is_empty() {
            Vec::new()
        } else {
            self.promoted_conflicts()?
                .into_iter()
                .filter(|conflict| {
                    changed_files
                        .iter()
                        .any(|path| crate::leases::paths_overlap(path, &conflict.promoted_path))
                })
                .collect()
        };
        let mut next_action = integration_next_action(
            &branch,
            &head,
            &main_head,
            upstream_head.as_deref(),
            main_is_ancestor,
            latest_delivery_entry_id,
            &promoted_entries,
            &changed_files,
            &conflicts,
        );
        let integration_contains_upstream = upstream_head
            .as_deref()
            .is_some_and(|upstream| self.repo.is_ancestor(upstream, &head));
        let reconciliation = match (upstream_ref.as_deref(), upstream_head.as_deref()) {
            (Some(upstream_ref), Some(upstream_head)) if !integration_contains_upstream => self
                .assess_integration_drift(upstream_ref, upstream_head, &head)
                .ok(),
            _ => None,
        };
        if upstream_head.is_some() && !integration_contains_upstream {
            let upstream = upstream_ref.as_deref().unwrap_or("@{upstream}");
            next_action = if let Some(assessment) = reconciliation
                .as_ref()
                .filter(|assessment| assessment.stale_only)
            {
                IntegrationNextAction {
                    state: IntegrationDeliveryState::ReconciliationReady,
                    summary: assessment.explanation.clone(),
                    commands: vec![format!(
                        "aethyme broker advanced integration reconcile --upstream {upstream} --dry-run"
                    )],
                }
            } else {
                IntegrationNextAction {
                    state: IntegrationDeliveryState::Blocked,
                    summary: format!(
                        "external main movement detected: integration does not contain {upstream}; unresolved or unrecorded work requires reviewed reconciliation"
                    ),
                    commands: vec![format!(
                        "aethyme broker advanced integration reconcile --upstream {upstream} --dry-run"
                    )],
                }
            };
        }

        Ok(IntegrationStatusView {
            branch,
            head,
            main_head,
            upstream_ref,
            upstream_head,
            main_ahead_upstream_commits,
            main_behind_upstream_commits,
            main_is_ancestor,
            commits_ahead_main,
            changed_files,
            promoted_entries,
            conflicts,
            reconciliation,
            next_action,
        })
    }

    /// Sample the integration branch, wait for the requested window, then
    /// sample again so long-running checks can prove which integration tip
    /// they were run against.
    pub fn wait_integration_stable(
        &mut self,
        seconds: u64,
    ) -> Result<IntegrationStabilityReport, BrokerOpError> {
        let started = now_ms();
        let (branch, start_head) = self.integration_head()?;
        if seconds > 0 {
            std::thread::sleep(std::time::Duration::from_secs(seconds));
        }
        let (_, end_head) = self.integration_head()?;
        let observed_ms = now_ms().saturating_sub(started);
        let live_sessions = integration_live_sessions(self.store.live_sessions()?);
        let stable = start_head == end_head;
        let message = if stable {
            let mut message = format!(
                "{} stayed at {} for {}s",
                branch,
                short_commit(&end_head),
                seconds
            );
            if !live_sessions.is_empty() {
                message.push_str(&format!(
                    "; {} live {} may still submit later",
                    live_sessions.len(),
                    plural_word(live_sessions.len(), "session", "sessions")
                ));
            }
            message
        } else {
            format!(
                "{} moved from {} to {} during the {}s window; rerun needed before treating checks as current-tip proof",
                branch,
                short_commit(&start_head),
                short_commit(&end_head),
                seconds
            )
        };
        let mut commands = Vec::new();
        if stable {
            if !live_sessions.is_empty() {
                commands.push("aethyme broker advanced agents".into());
            }
        } else {
            commands.push(format!(
                "aethyme broker advanced integration wait-stable --seconds {seconds}"
            ));
            commands.push("aethyme broker status".into());
        }

        Ok(IntegrationStabilityReport {
            branch,
            start_head,
            end_head,
            stable,
            requested_seconds: seconds,
            observed_ms,
            live_sessions,
            message,
            commands,
        })
    }

    /// The whole picture in one call: refreshed leases + overlaps, agent
    /// views, promoted/unmerged conflicts, the merge queue, and the
    /// integration branch head.
    pub fn status(&mut self, now_ms: i64) -> Result<StatusView, BrokerOpError> {
        let started = std::time::Instant::now();
        let deadline = started + status_inspection_budget();
        let _git_deadline = crate::git::limit_git_until(deadline);
        let integration_refresh =
            self.refresh_disposable_integration(crate::IntegrationRefreshTrigger::Status);
        let leases_started = std::time::Instant::now();
        let overlaps = match self.refresh_leases() {
            Ok(overlaps) => overlaps,
            Err(_) if std::time::Instant::now() >= deadline => self.lease_overlaps_snapshot()?,
            Err(error) => return Err(error),
        };
        let lease_refresh_complete = std::time::Instant::now() < deadline;
        let leases_ms = leases_started.elapsed().as_millis() as u64;
        let agents = self.agents(now_ms)?;
        let integration = match self.integration_head() {
            Ok(integration) => integration,
            Err(_) if std::time::Instant::now() >= deadline => {
                (PromoteConfig::load(&self.main_root).branch, String::new())
            }
            Err(error) => return Err(error),
        };
        let mut view = self.build_status(agents, overlaps, integration, now_ms, true)?;
        if !lease_refresh_complete {
            view.leases_refreshed = false;
            if !view
                .deferred_checks
                .iter()
                .any(|check| check == "lease_refresh")
            {
                view.deferred_checks.push("lease_refresh".into());
            }
        }
        push_integration_refresh_advice(&mut view, integration_refresh);
        let auto_cleanup = self.last_auto_cleanup_report();
        push_auto_cleanup_advice(&mut view, auto_cleanup);
        self.store.record_advisories_shown(
            &view.outstanding_advisories,
            crate::AdvisoryDeliverySurface::Status,
        )?;
        view.advisory_delivery = self.store.advisory_delivery_summary()?;
        view.phase_timings_ms
            .insert("lease_refresh".into(), leases_ms);
        view.phase_timings_ms
            .insert("total".into(), started.elapsed().as_millis() as u64);
        Ok(view)
    }

    /// `status` minus the implicit-lease refresh: the fast path behind
    /// `broker status --summary` (#182).
    ///
    /// Liveness is still derived and still persisted, so the event timeline
    /// keeps recording stale-session transitions and a cheap call does not
    /// create a blind spot. Only the per-session diff is skipped, because that
    /// is both the expensive part and the part a caller reading `summary` and
    /// `advice` does not consume. Overlap counts therefore come from the last
    /// refresh -- `leases_refreshed: false` says so in the output.
    ///
    /// Outstanding advisories are deliberately not marked as shown: this view
    /// does not render them, and recording a delivery that never happened
    /// would corrupt the shown-to-action correlation.
    pub fn status_brief(&mut self, now_ms: i64) -> Result<StatusBrief, BrokerOpError> {
        let agents = self.agents(now_ms)?;
        self.build_status_brief(agents, now_ms)
    }

    pub fn status_brief_snapshot(&self, now_ms: i64) -> Result<StatusBrief, BrokerOpError> {
        self.build_status_brief(self.agents_snapshot(now_ms)?, now_ms)
    }

    pub(super) fn build_status_brief(
        &self,
        agents: Vec<AgentView>,
        now_ms: i64,
    ) -> Result<StatusBrief, BrokerOpError> {
        let started = std::time::Instant::now();
        let overlaps = self.lease_overlaps_snapshot()?;
        let pairs = self.overlap_pairs_snapshot(&overlaps);
        let promotes = PromoteConfig::load(&self.main_root).mode.promotes_at_all();
        let summary = status_summary(
            &agents,
            overlaps.len(),
            OverlapPairCounts {
                pairs: pairs.len(),
                conflicting: pairs
                    .iter()
                    .filter(|p| p.severity == crate::OverlapSeverity::High)
                    .count(),
            },
            0,
            0,
            &SummaryIntegration {
                branch: PromoteConfig::load(&self.main_root).branch,
                head: String::new(),
                baseline_ref: "not inspected".into(),
                baseline_head: String::new(),
                relation: StatusIntegrationRelation::NotChecked,
                ahead_baseline_commits: 0,
                main_head: String::new(),
                main_is_ancestor: false,
                ahead_main_commits: 0,
                promotes,
            },
        );
        let ids = agents.iter().map(|a| a.session.id).collect::<Vec<_>>();
        let latest = self.store.latest_merge_queue_for_sessions(&ids)?;
        let mut advice = self.status_advice(&agents, &[], &latest, ("", ""), promotes, None);
        let blockers = self.blockers();
        if let Some(item) = crate::blockers::status_advice(&blockers.blockers) {
            advice.push(item);
        }
        advice.extend(stalled_submit_advice(
            &crate::submit_progress::in_flight_submits(&self.main_root, now_ms),
        ));
        advice.extend(overlap_pair_advice(&pairs));
        advice.extend(crate::ownership::ownership_claim_advice(
            &self.ownership_claim_views(&agents, now_ms)?,
            now_ms,
        ));
        advice.push(StatusAdvice {
            id: "status.recorded-observations", severity: StatusAdviceSeverity::Notice,
            reason: "summary reads broker records without scanning Git history or retained checkouts",
            summary: "Recorded leases/overlaps only; Git refs, dirty worktrees, unpushed commits, disk retention and cleanup eligibility were not checked".into(),
            session_id: None, queue_entry_id: None, evidence: Vec::new(),
            commands: vec!["aethyme broker status --refresh".into()],
        });
        Ok(StatusBrief {
            phase_timings_ms: std::collections::BTreeMap::from([(
                "total".into(),
                started.elapsed().as_millis() as u64,
            )]),
            deferred_checks: vec![
                "git_refs",
                "dirty_worktrees",
                "unpushed_commits",
                "promoted_conflicts",
                "disk_retention",
                "cleanup_eligibility",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            leases_refreshed_at_ms: self
                .store
                .meta_get("leases.refreshed_at_ms")?
                .and_then(|v| v.parse().ok()),
            summary,
            advice,
            leases_refreshed: false,
        })
    }

    /// Render the same status contract from persisted state and derived
    /// filesystem observations, but never reconcile that state as a side
    /// effect. Used while repository deployment compatibility is degraded.
    pub fn status_snapshot(&self, now_ms: i64) -> Result<StatusView, BrokerOpError> {
        let started = std::time::Instant::now();
        let deadline = crate::git::active_git_deadline()
            .unwrap_or_else(|| started + status_inspection_budget());
        let _git_deadline = crate::git::limit_git_until(deadline);
        let overlaps = self.lease_overlaps_snapshot()?;
        let agents = self.agents_snapshot(now_ms)?;
        let branch = PromoteConfig::load(&self.main_root).branch;
        let observed_integration = self.integration_head_snapshot();
        let integration = if std::time::Instant::now() >= deadline {
            (branch, String::new())
        } else {
            observed_integration?
        };
        let mut view = self.build_status(agents, overlaps, integration, now_ms, true)?;
        view.leases_refreshed = false;
        Ok(view)
    }

    /// Routine, read-only variant for degraded CLI reporting.
    pub fn status_current_snapshot(&self, now_ms: i64) -> Result<StatusView, BrokerOpError> {
        let started = std::time::Instant::now();
        let deadline = crate::git::active_git_deadline()
            .unwrap_or_else(|| started + status_inspection_budget());
        let _git_deadline = crate::git::limit_git_until(deadline);
        let overlaps = self.lease_overlaps_snapshot()?;
        let agents = self.agents_snapshot(now_ms)?;
        let branch = PromoteConfig::load(&self.main_root).branch;
        let observed_integration = self.integration_head_snapshot();
        let integration = if std::time::Instant::now() >= deadline {
            (branch, String::new())
        } else {
            observed_integration?
        };
        self.build_status(agents, overlaps, integration, now_ms, false)
    }

    /// Routine reporting uses persisted leases and recorded sizes. It never
    /// proves cleanup eligibility or reclassifies Git conflicts. Explicit
    /// refresh and all mutation paths still perform their own checks.
    pub fn status_current(&mut self, now_ms: i64) -> Result<StatusView, BrokerOpError> {
        let started = std::time::Instant::now();
        let deadline = started + status_inspection_budget();
        let _git_deadline = crate::git::limit_git_until(deadline);
        let integration_refresh =
            self.refresh_disposable_integration(crate::IntegrationRefreshTrigger::Status);
        let overlaps = self.lease_overlaps_snapshot()?;
        let leases_ms = started.elapsed().as_millis() as u64;
        let sessions_started = std::time::Instant::now();
        let agents = self.agents(now_ms)?;
        let sessions_ms = sessions_started.elapsed().as_millis() as u64;
        let integration = match self.integration_head() {
            Ok(integration) => integration,
            Err(_) if std::time::Instant::now() >= deadline => {
                (PromoteConfig::load(&self.main_root).branch, String::new())
            }
            Err(error) => return Err(error),
        };
        self.record_gone_lease_holders()?;
        self.grant_stale_lease_requests()?;
        let mut view = self.build_status(agents, overlaps, integration, now_ms, false)?;
        push_integration_refresh_advice(&mut view, integration_refresh);
        let auto_cleanup = self.last_auto_cleanup_report();
        push_auto_cleanup_advice(&mut view, auto_cleanup);
        self.store.record_advisories_shown(
            &view.outstanding_advisories,
            crate::AdvisoryDeliverySurface::Status,
        )?;
        view.advisory_delivery = self.store.advisory_delivery_summary()?;
        view.phase_timings_ms.insert("leases".into(), leases_ms);
        view.phase_timings_ms.insert("sessions".into(), sessions_ms);
        view.phase_timings_ms
            .insert("total".into(), started.elapsed().as_millis() as u64);
        Ok(view)
    }

    pub(super) fn build_status(
        &self,
        agents: Vec<AgentView>,
        overlaps: Vec<crate::Overlap>,
        (integration_branch, integration_head): (String, String),
        now_ms: i64,
        refresh: bool,
    ) -> Result<StatusView, BrokerOpError> {
        let started = std::time::Instant::now();
        let mut phase_timings_ms = std::collections::BTreeMap::new();
        // In a verify-only repository nothing moves integration: submit
        // verifies against the default branch and sessions start from it. Every
        // row below that tells an agent integration may move, has drifted, or
        // needs reconciling is noise there (and some of it is `blocked`), so
        // the mode is read once and gates all of them.
        let promotes = PromoteConfig::load(&self.main_root).mode.promotes_at_all();
        // One deadline for every phase below whose cost grows with sessions
        // and history; each phase that runs out names what it skipped.
        let budget = status_inspection_budget();
        let deadline = crate::git::active_git_deadline().or_else(|| Some(started + budget));
        let _git_deadline = deadline.map(crate::git::limit_git_until);
        let mut budget_cut: Vec<&'static str> = Vec::new();
        let conflicts_started = std::time::Instant::now();
        let promoted_conflicts = if refresh {
            let (conflicts, cut) = self.promoted_conflicts_within(deadline)?;
            if cut {
                budget_cut.push("promoted_conflicts");
            }
            conflicts
        } else {
            Vec::new()
        };
        phase_timings_ms.insert(
            "promoted_conflicts".into(),
            conflicts_started.elapsed().as_millis() as u64,
        );
        let checkouts_started = std::time::Instant::now();
        let checkouts = refresh.then(|| inspect_session_checkouts(&agents, deadline));
        if checkouts.as_ref().is_some_and(|checkouts| {
            checkouts
                .values()
                .any(|inspection| *inspection == CheckoutInspection::NotInspected)
        }) {
            budget_cut.push("dirty_worktrees");
        }
        phase_timings_ms.insert(
            "checkouts".into(),
            checkouts_started.elapsed().as_millis() as u64,
        );
        // Before the drift assessment, the costliest and least urgent phase:
        // it gets whatever budget is left, and a cut there leaves the
        // per-session answers intact.
        let unpushed_started = std::time::Instant::now();
        let unpushed_work = if refresh {
            match self.unpushed_work_within(now_ms, deadline) {
                Ok(report) => report,
                Err(_)
                    if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) =>
                {
                    crate::UnpushedWorkReport {
                        not_inspected_sessions: agents
                            .iter()
                            .map(|agent| agent.session.id)
                            .collect(),
                        ..crate::UnpushedWorkReport::default()
                    }
                }
                Err(error) => return Err(error),
            }
        } else {
            crate::UnpushedWorkReport::default()
        };
        if !unpushed_work.not_inspected_sessions.is_empty() {
            budget_cut.push("unpushed_commits");
        }
        phase_timings_ms.insert(
            "unpushed".into(),
            unpushed_started.elapsed().as_millis() as u64,
        );
        // Declared intent, not yet visible in any diff. Read from the same
        // snapshot as the leases above so both halves of "who else is working
        // on this" describe one moment.
        let scope_overlaps = crate::detect_scope_overlaps(&self.store.active_session_scopes()?);
        let queue = self.store.current_merge_queue()?;
        let terminal_counts = self.store.terminal_merge_queue_counts()?;
        // One query for every live session rather than one per session: this
        // is on `status`, which every session runs as its first command.
        let session_ids = agents
            .iter()
            .map(|agent| agent.session.id)
            .collect::<Vec<i64>>();
        let latest_live_queue = self.store.latest_merge_queue_for_sessions(&session_ids)?;
        let before_deadline =
            || deadline.is_none_or(|deadline| std::time::Instant::now() < deadline);
        let mut refs_incomplete = false;
        let main_head = if before_deadline() {
            match self.repo.head_commit() {
                Ok(head) => head,
                Err(_) => {
                    refs_incomplete = true;
                    String::new()
                }
            }
        } else {
            refs_incomplete = true;
            String::new()
        };
        let (upstream_ref, upstream_head) = if refresh && before_deadline() {
            let upstream = self.repo.tracking_upstream();
            if !before_deadline() {
                refs_incomplete = true;
            }
            upstream
                .map(|(name, commit)| (Some(name), Some(commit)))
                .unwrap_or((None, None))
        } else {
            if refresh {
                refs_incomplete = true;
            }
            (None, None)
        };
        let mut main_ahead_upstream_commits = 0;
        let mut main_behind_upstream_commits = 0;
        if let Some(upstream) = upstream_head.as_deref() {
            if before_deadline() {
                match (
                    self.repo.commit_count_between(upstream, &main_head),
                    self.repo.commit_count_between(&main_head, upstream),
                ) {
                    (Ok(ahead), Ok(behind)) => {
                        main_ahead_upstream_commits = ahead;
                        main_behind_upstream_commits = behind;
                    }
                    _ => refs_incomplete = true,
                }
            } else {
                refs_incomplete = true;
            }
        }
        // Against the published branch, not the checkout: the number answers
        // "what would publishing add to the default branch", which is the only
        // reading anyone acts on.
        let (baseline_ref, baseline_head) = if before_deadline() {
            match self.publication_baseline() {
                Ok(baseline) => baseline,
                Err(_) => {
                    refs_incomplete = true;
                    ("not inspected".into(), String::new())
                }
            }
        } else {
            refs_incomplete = true;
            ("not inspected".into(), String::new())
        };
        let (mut integration_relation, mut integration_ahead_main_commits) = if !refresh {
            (StatusIntegrationRelation::NotChecked, 0)
        } else if !before_deadline() || baseline_head.is_empty() {
            refs_incomplete = true;
            (StatusIntegrationRelation::NotChecked, 0)
        } else if integration_head == baseline_head {
            (StatusIntegrationRelation::CurrentWithMain, 0)
        } else {
            match self
                .repo
                .is_ancestor_checked(&baseline_head, &integration_head)
            {
                Ok(true) if before_deadline() => {
                    match self
                        .repo
                        .commit_count_between(&baseline_head, &integration_head)
                    {
                        Ok(ahead) if before_deadline() => {
                            (StatusIntegrationRelation::AheadOfMain, ahead)
                        }
                        _ => {
                            refs_incomplete = true;
                            (StatusIntegrationRelation::NotChecked, 0)
                        }
                    }
                }
                Ok(false) if before_deadline() => (StatusIntegrationRelation::DivergedFromMain, 0),
                _ => {
                    refs_incomplete = true;
                    (StatusIntegrationRelation::NotChecked, 0)
                }
            }
        };
        if !before_deadline() && refresh {
            refs_incomplete = true;
            integration_relation = StatusIntegrationRelation::NotChecked;
            integration_ahead_main_commits = 0;
        }
        if refs_incomplete {
            budget_cut.push("git_refs");
        }
        let dirty_sessions = checkouts.as_ref().map_or(0, dirty_session_count);
        let overlap_pairs = self.overlap_pairs_snapshot(&overlaps);
        let main_is_ancestor = if refresh && before_deadline() && !main_head.is_empty() {
            let ancestry = if main_head == integration_head {
                Some(true)
            } else {
                self.repo
                    .is_ancestor_checked(&main_head, &integration_head)
                    .ok()
            };
            if !before_deadline() || ancestry.is_none() {
                if !budget_cut.contains(&"git_refs") {
                    budget_cut.push("git_refs");
                }
                false
            } else {
                ancestry.unwrap_or(false)
            }
        } else {
            false
        };
        let ahead_main_commits = if refresh && before_deadline() && !main_head.is_empty() {
            match self
                .repo
                .commit_count_between(&main_head, &integration_head)
            {
                Ok(count) if before_deadline() => count,
                _ => {
                    if !budget_cut.contains(&"git_refs") {
                        budget_cut.push("git_refs");
                    }
                    0
                }
            }
        } else {
            if refresh && !budget_cut.contains(&"git_refs") {
                budget_cut.push("git_refs");
            }
            0
        };
        let summary = status_summary(
            &agents,
            overlaps.len(),
            OverlapPairCounts {
                pairs: overlap_pairs.len(),
                conflicting: overlap_pairs
                    .iter()
                    .filter(|pair| pair.severity == crate::OverlapSeverity::High)
                    .count(),
            },
            promoted_conflicts.len(),
            dirty_sessions,
            &SummaryIntegration {
                branch: integration_branch.clone(),
                head: integration_head.clone(),
                baseline_ref: baseline_ref.clone(),
                baseline_head: baseline_head.clone(),
                relation: integration_relation,
                ahead_baseline_commits: integration_ahead_main_commits,
                main_head: main_head.clone(),
                main_is_ancestor,
                ahead_main_commits,
                promotes,
            },
        );
        let mut advice = self.status_advice(
            &agents,
            &promoted_conflicts,
            &latest_live_queue,
            (&integration_branch, &integration_head),
            promotes,
            checkouts.as_ref(),
        );
        let integration_contains_upstream = upstream_head.as_deref().and_then(|upstream| {
            if !before_deadline() {
                return None;
            }
            self.repo
                .is_ancestor_checked(upstream, &integration_head)
                .ok()
                .filter(|_| before_deadline())
        });
        if refresh && upstream_head.is_some() && integration_contains_upstream.is_none() {
            budget_cut.push("integration_drift");
        }
        let drift_started = std::time::Instant::now();
        let integration_reconciliation = match (upstream_ref.as_deref(), upstream_head.as_deref()) {
            (Some(upstream_ref), Some(upstream_head))
                if refresh && integration_contains_upstream == Some(false) =>
            {
                match self.assess_integration_drift_within(
                    upstream_ref,
                    upstream_head,
                    &integration_head,
                    deadline,
                ) {
                    Ok(Some(assessment)) => Some(assessment),
                    // Not assessed is not resolved: without an assessment
                    // the drift row below keeps its unassessed severity.
                    Ok(None) => {
                        budget_cut.push("integration_drift");
                        None
                    }
                    Err(_) => {
                        budget_cut.push("integration_drift");
                        None
                    }
                }
            }
            (Some(_), Some(_)) if refresh && integration_contains_upstream.is_none() => {
                if !budget_cut.contains(&"integration_drift") {
                    budget_cut.push("integration_drift");
                }
                None
            }
            _ => None,
        };
        phase_timings_ms.insert(
            "integration_drift".into(),
            drift_started.elapsed().as_millis() as u64,
        );
        if promotes
            && upstream_head.is_some()
            && integration_contains_upstream.is_some()
            && (main_behind_upstream_commits > 0 || !integration_contains_upstream.unwrap_or(false))
        {
            let upstream = upstream_ref.as_deref().unwrap_or("@{upstream}");
            let stale_only = integration_reconciliation
                .as_ref()
                .filter(|assessment| assessment.stale_only);
            // Integration strictly behind upstream is not ambiguity: it holds
            // nothing upstream lacks, so advancing it discards no work and
            // rewrites no history. Reporting that as `blocked` alongside
            // genuine divergence taught operators to reach for a reviewed
            // reconciliation when a fast-forward was the whole answer, and to
            // wait for the block rather than keeping the ref current (#290
            // phase 3.2). Divergence still blocks.
            let fast_forward_available = before_deadline()
                && !integration_contains_upstream.unwrap_or(false)
                && upstream_head
                    .as_deref()
                    .is_some_and(|upstream| self.repo.is_ancestor(&integration_head, upstream));
            if !before_deadline() && !budget_cut.contains(&"branch_drift") {
                budget_cut.push("branch_drift");
            }
            advice.insert(
                0,
                StatusAdvice {
                    id: if stale_only.is_some() {
                        "integration.stale-promotions"
                    } else if fast_forward_available {
                        "integration.fast-forward-available"
                    } else {
                        "integration.upstream-main-ahead"
                    },
                    severity: if integration_contains_upstream.unwrap_or(false)
                        || stale_only.is_some()
                        || fast_forward_available
                    {
                        StatusAdviceSeverity::Notice
                    } else {
                        StatusAdviceSeverity::Blocked
                    },
                    reason: if stale_only.is_some() {
                        "all recorded integration promotions have conclusive upstream landing evidence"
                    } else if fast_forward_available {
                        "integration holds nothing upstream lacks, so it can be advanced without review"
                    } else {
                        "configured upstream moved outside broker-managed integration"
                    },
                    summary: if integration_contains_upstream.unwrap_or(false) {
                        format!(
                            "local main is {main_behind_upstream_commits} commits behind {upstream}; integration already contains upstream, so broker operations remain safe"
                        )
                    } else if let Some(assessment) = stale_only {
                        assessment.explanation.clone()
                    } else if fast_forward_available {
                        // Keeps the external-movement signal verbatim: what
                        // changes is the severity and the named repair, not
                        // whether the operator is told main moved outside the
                        // broker.
                        format!(
                            "external main movement detected: integration is behind {upstream} \
                             and carries nothing of its own, so it can be fast-forwarded without \
                             review; do it so new sessions stop inheriting the gap"
                        )
                    } else {
                        format!(
                            "external main movement detected: integration does not contain {upstream}; unresolved or unrecorded work requires reviewed reconciliation"
                        )
                    },
                    session_id: None,
                    queue_entry_id: None,
                    evidence: vec![
                        format!(
                            "{baseline_ref}: {}",
                            short_commit(&baseline_head)
                        ),
                        format!("{upstream}: {}", short_commit(upstream_head.as_deref().unwrap_or(""))),
                    ],
                    commands: if integration_contains_upstream.unwrap_or(false) {
                        Vec::new()
                    } else {
                        vec![format!(
                            "aethyme broker advanced integration reconcile --upstream {upstream} --dry-run"
                        )]
                    },
                },
                );
        }
        if !promotes && before_deadline() {
            match leftover_integration_advice(
                &self.repo,
                &integration_head,
                (&baseline_ref, &baseline_head),
                integration_reconciliation.as_ref(),
            ) {
                Ok(Some(row)) => advice.insert(0, row),
                Ok(None) if !before_deadline() => budget_cut.push("branch_drift"),
                Ok(None) => {}
                Err(_) => budget_cut.push("branch_drift"),
            }
        } else if !promotes {
            budget_cut.push("branch_drift");
        }
        // "Never passed through submit" is every commit under verify-only,
        // where landing goes through pull requests and integration stays put.
        let external_writes = if promotes && before_deadline() {
            match self.external_default_branch_writes(&integration_head) {
                Ok(writes) => writes,
                Err(_) => {
                    budget_cut.push("branch_drift");
                    None
                }
            }
        } else {
            if promotes && refresh {
                budget_cut.push("branch_drift");
            }
            None
        };
        if promotes && refresh && !before_deadline() {
            budget_cut.push("branch_drift");
        }
        if let Some((branch, commits)) = external_writes {
            let count = commits.len();
            advice.push(StatusAdvice {
                id: "main.external-writes",
                severity: StatusAdviceSeverity::Warning,
                reason: "local default branch carries commits integration does not contain",
                summary: format!(
                    "{count} {} on {branch} never passed through submit; no session accounts for {}",
                    plural_word(count, "commit", "commits"),
                    if count == 1 { "it" } else { "them" }
                ),
                session_id: None,
                queue_entry_id: None,
                evidence: vec![
                    format!("integration head {}", &integration_head[..12.min(integration_head.len())]),
                    format!(
                        "unaccounted commits: {}",
                        commits
                            .iter()
                            .take(3)
                            .map(|commit| commit[..12.min(commit.len())].to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ],
                commands: vec![
                    format!("git log --oneline {}..{branch}", integration_head),
                    "aethyme broker start --task \"replay local default-branch work\" --short-name \"Replay work\"".into(),
                ],
            });
        }
        if refresh && promotes && !before_deadline() && !budget_cut.contains(&"branch_drift") {
            budget_cut.push("branch_drift");
        }
        phase_timings_ms.insert("coordination".into(), started.elapsed().as_millis() as u64);
        let retention_started = std::time::Instant::now();
        let cleanup_retention = self.cleanup_retention_with_audit(now_ms, refresh)?;
        if refresh && cleanup_retention.gate_headroom_deferred {
            budget_cut.push("gate_headroom");
        }
        if refresh && !cleanup_retention.inventory_complete {
            budget_cut.push("cleanup_eligibility");
        }
        if !cleanup_retention.reconciliation.complete {
            budget_cut.push("unclaimed_worktrees");
        }
        phase_timings_ms.insert(
            "retention".into(),
            retention_started.elapsed().as_millis() as u64,
        );
        if !refresh {
            advice.push(StatusAdvice {
                id: "status.recorded-observations", severity: StatusAdviceSeverity::Notice,
                reason: "routine status does not run Git conflict or cleanup eligibility audits",
                summary: "Recorded leases/overlaps only; dirty worktrees, unpushed commits, Git conflicts, branch drift and cleanup eligibility were not checked".into(),
                session_id: None, queue_entry_id: None,
                evidence: vec![format!("oldest recorded size: {:?}", cleanup_retention.sizes_measured_at_ms)],
                commands: vec!["aethyme broker status --refresh".into(), "aethyme broker gc plan".into()],
            });
        }
        if let Some(config_advice) = retention_config_advice(&cleanup_retention.retention_config) {
            advice.insert(0, config_advice);
        }
        // A starved volume is its own row, emitted whether or not this
        // repository retains anything: no gate here can start, which is a
        // reason nothing runs at all rather than a housekeeping backlog, and a
        // repository with nothing retained must not stay silent about it.
        if let Some(headroom_advice) = gate_headroom_advice(
            cleanup_retention.host_available_bytes,
            cleanup_retention.host_volume_probe.as_deref(),
            cleanup_retention.inodes_free,
            cleanup_retention.host_inode_volume_probe.as_deref(),
            crate::disk_headroom::DEFAULT_GATE_HEADROOM_BYTES,
            crate::disk_headroom::MIN_GATE_HEADROOM_INODES,
        ) {
            advice.insert(0, headroom_advice);
        }
        if refresh && cleanup_retention.broker_owned_worktree_count > 0 {
            let count = cleanup_retention.broker_owned_worktree_count;
            let required = crate::disk_headroom::DEFAULT_GATE_HEADROOM_BYTES;
            advice.push(StatusAdvice {
                id: "cleanup.retained-worktrees",
                severity: cleanup_retention.severity,
                reason: "closed broker-owned worktrees are retained until explicit cleanup",
                // An unmeetable budget is a different situation from a
                // backlog and must not be advised as one: telling an operator
                // to run cleanup when cleanup provably cannot close the gap is
                // how a budget stays a gauge (#176).
                summary: if cleanup_retention.over_retained_bytes_budget
                    && !cleanup_retention.clears_retained_bytes_budget
                {
                    format!(
                        "retained broker storage is {} bytes over the {} byte budget and removing everything currently eligible would reclaim only {}; the budget cannot be met by cleanup alone",
                        cleanup_retention.retained_bytes_deficit,
                        cleanup_retention.retained_bytes_budget,
                        cleanup_retention.estimated_reclaimable_bytes
                    )
                } else {
                    format!(
                        "{count} closed broker-owned {} remain on disk; review reclaimable bytes and remove only eligible worktrees",
                        plural_word(count, "worktree", "worktrees")
                    )
                },
                session_id: None,
                queue_entry_id: None,
                evidence: vec![
                    format!("retained broker-owned worktrees: {count}"),
                    format!(
                        "retained session branches: {}",
                        cleanup_retention.retained_session_branch_count
                    ),
                    format!(
                        "retained/reclaimable bytes: {}/{}",
                        cleanup_retention.estimated_retained_bytes,
                        cleanup_retention.estimated_reclaimable_bytes
                    ),
                    // The budget above is per repository; the disk is not.
                    // Every enrolled repository can sit inside its own budget
                    // while the shared volume is full, so the number that
                    // decides whether work can run belongs next to it — and it
                    // is the same reading `severity` was derived from, so this
                    // line and the advisory's level cannot disagree.
                    {
                        // Gates do not run beside the repository, so name the
                        // directory read: "the disk is full" is not actionable
                        // until the operator knows which disk.
                        let probe = cleanup_retention
                            .host_volume_probe
                            .as_ref()
                            .map(|probe| format!(" at {}", probe.display()))
                            .unwrap_or_default();
                        match cleanup_retention.host_available_bytes {
                            Some(available) if available < required => format!(
                                "host free space{probe}: {} of {} a gate needs to start \
                                 -- sweeps widen and run hourly until it clears",
                                crate::disk_headroom::format_gibibytes(available),
                                crate::disk_headroom::format_gibibytes(required)
                            ),
                            Some(available) => format!(
                                "host free space{probe}: {} (this budget is per repository; \
                                 the volume is shared)",
                                crate::disk_headroom::format_gibibytes(available)
                            ),
                            None => "host free space: unknown".to_string(),
                        }
                    },
                    format!(
                        "blocked/budget bytes: {}/{}{}",
                        cleanup_retention.estimated_blocked_bytes,
                        cleanup_retention.retained_bytes_budget,
                        if cleanup_retention.over_retained_bytes_budget {
                            " (budget exceeded)"
                        } else {
                            ""
                        }
                    ),
                    format!(
                        "budget deficit: {} bytes ({})",
                        cleanup_retention.retained_bytes_deficit,
                        match cleanup_retention.budget_verdict {
                            crate::BudgetVerdict::Unset => "no budget configured",
                            crate::BudgetVerdict::Within => "within budget",
                            crate::BudgetVerdict::Unknown =>
                                "unknown: part of the total was never measured",
                            crate::BudgetVerdict::Over
                                if cleanup_retention.clears_retained_bytes_budget =>
                                "reclaimable work closes the gap",
                            crate::BudgetVerdict::Over =>
                                "not closable by reclaiming eligible worktrees",
                        }
                    ),
                    // Status never walks the trees, so it has to say which of
                    // its numbers are measurements and which are floors.
                    // Reporting a floor as a total is how a budget reads as
                    // satisfied because nobody looked (#176).
                    format!(
                        "size measurement: {}{}",
                        if cleanup_retention.unmeasured_worktree_count == 0 {
                            "complete".to_string()
                        } else {
                            format!(
                                "{} worktree(s) never sized, so byte totals are a floor",
                                cleanup_retention.unmeasured_worktree_count
                            )
                        },
                        match cleanup_retention.sizes_measured_at_ms {
                            Some(measured_at) => format!(
                                "; oldest measurement {} days old",
                                now_ms.saturating_sub(measured_at).max(0) / 86_400_000
                            ),
                            None => String::new(),
                        }
                    ),
                    format!(
                        "oldest closed age/policy: {}/{} days",
                        cleanup_retention.oldest_closed_age_days,
                        cleanup_retention.closed_worktrees_policy_days
                    ),
                    closed_worktree_evidence(&cleanup_retention.closed_worktrees),
                ],
                commands: if cleanup_retention.over_retained_bytes_budget {
                    vec![
                        "aethyme broker gc plan".into(),
                        "aethyme broker gc apply --confirm <sha256-from-plan>".into(),
                    ]
                } else {
                    vec![
                        "aethyme broker finish cleanup --all-cleaned".into(),
                        "aethyme broker finish cleanup --all-cleaned --apply --confirm <sha256-from-plan>"
                            .into(),
                    ]
                },
            });
        }
        // Every other cleanup surface starts from a session row, so a
        // directory no row names is invisible to all of them -- not retained,
        // not reclaimable, not blocked, just absent from the arithmetic. That
        // is how a root grows to 54 directories while the broker reports 38
        // (#176). Naming them is the whole remedy: the broker cannot know what
        // is inside a directory it never created, so nothing here removes.
        let drift = &cleanup_retention.reconciliation;
        if drift.unclaimed_count > 0 {
            let count = drift.unclaimed_count;
            advice.push(StatusAdvice {
                id: "cleanup.unclaimed-worktrees",
                severity: unclaimed_worktree_severity(count),
                reason: "directories under a broker worktree root are claimed by no session",
                summary: format!(
                    "{count} {} under broker worktree roots {} to no session and no cleanup lane can reach {}; inspect and remove by hand",
                    plural_word(count, "directory", "directories"),
                    plural_word(count, "belongs", "belong"),
                    plural_word(count, "it", "them")
                ),
                session_id: None,
                queue_entry_id: None,
                evidence: {
                    let mut evidence = vec![format!(
                        "directories/claimed/unclaimed: {}/{}/{count}",
                        drift.directory_count, drift.claimed_count
                    )];
                    evidence.extend(
                        drift
                            .unclaimed
                            .iter()
                            .take(UNCLAIMED_EVIDENCE_LIMIT)
                            .map(|entry| format!("{} ({})", entry.path, entry.kind)),
                    );
                    if count > UNCLAIMED_EVIDENCE_LIMIT {
                        evidence.push(format!(
                            "... and {} more; `aethyme broker gc plan --json` lists all of them with sizes",
                            count - UNCLAIMED_EVIDENCE_LIMIT
                        ));
                    }
                    evidence
                },
                commands: vec!["aethyme broker gc plan --json".into()],
            });
        }
        // A review a provider refused, said out loud. The ledger has always
        // held these rows; what it could not say was why, so they arrived
        // looking exactly like a review nobody had asked for yet (#173).
        let review_refusals = self
            .store
            .review_refusals(REVIEW_REFUSAL_STATUS_LIMIT)?
            .into_iter()
            .filter_map(|row| {
                let refusal = crate::ReviewRefusal::parse(row.detail.as_deref()?)?;
                Some(ReviewRefusalView {
                    repository: row.repository,
                    pull_request: row.pr_number,
                    review_type: row.review_type,
                    head_commit: row.head_commit,
                    class: refusal.class,
                    text: refusal.text,
                    refused_at: row.updated_at,
                })
            })
            .collect::<Vec<_>>();
        if !review_refusals.is_empty() {
            let count = review_refusals.len();
            // Whether waiting helps is the question the operator in #173 could
            // not answer, so it leads. A spent budget is the class where
            // waiting is not the answer, which is why it is called out apart
            // from the classes where it is.
            let quota = review_refusals
                .iter()
                .filter(|refusal| refusal.class == crate::RefusalClass::QuotaExhausted)
                .count();
            advice.push(StatusAdvice {
                id: "review.provider-refusals",
                severity: if quota > 0 {
                    StatusAdviceSeverity::Warning
                } else {
                    StatusAdviceSeverity::Notice
                },
                reason: "a review provider refused, so those gates cannot clear on their own",
                summary: format!(
                    "{count} refused {} outstanding{}",
                    plural_word(count, "review", "reviews"),
                    if quota > 0 {
                        format!(
                            "; {quota} on exhausted provider quota, which retrying does not clear"
                        )
                    } else {
                        String::new()
                    }
                ),
                session_id: None,
                queue_entry_id: None,
                // The provider's own words, not a paraphrase: the
                // classification is scraped and may be wrong, and the reader
                // needs to be able to see that for themselves.
                evidence: review_refusals
                    .iter()
                    .take(4)
                    .map(|refusal| {
                        format!(
                            "{}#{} {} ({}) at {}: {}",
                            refusal.repository,
                            refusal.pull_request,
                            refusal.review_type,
                            short_commit(&refusal.head_commit),
                            refusal.class.label(),
                            refusal.text
                        )
                    })
                    .collect(),
                commands: review_refusals
                    .first()
                    .map(|refusal| {
                        vec![format!(
                            "aethyme broker advanced review ledger --repo {} --pr {}",
                            refusal.repository, refusal.pull_request
                        )]
                    })
                    .unwrap_or_default(),
            });
        }
        // Within a repository the lock is held by the running operation; every
        // other unresolved row there is parked behind it. Naming that
        // relationship is the point -- a parked caller cannot say so itself.
        let pending_rows = self.store.pending_coordinated_operations()?;
        let mut holder_by_repository: std::collections::HashMap<&str, i64> =
            std::collections::HashMap::new();
        for operation in &pending_rows {
            if operation.status == crate::types::OperationStatus::Running {
                holder_by_repository
                    .entry(operation.repository.as_str())
                    .or_insert(operation.id);
            }
        }
        let coordinated_operations = pending_rows
            .iter()
            .map(|operation| {
                let holder = holder_by_repository
                    .get(operation.repository.as_str())
                    .copied();
                let holding_lock = holder == Some(operation.id);
                PendingOperationView {
                    id: operation.id,
                    session_id: operation.session_id,
                    provider: operation.provider.as_str().to_string(),
                    repository: operation.repository.clone(),
                    scope: operation.scope.clone(),
                    status: operation.status.as_str().to_string(),
                    pid: operation.pid,
                    elapsed_seconds: now_ms.saturating_sub(operation.created_at).max(0) as u64
                        / 1_000,
                    holding_lock,
                    blocked_by: if holding_lock { None } else { holder },
                    liveness: crate::operations::operation_liveness_view(operation),
                }
            })
            .collect::<Vec<_>>();
        if let Some(stalled) = self.stalled_coordinated_operation_advice(&pending_rows) {
            advice.push(stalled);
        }
        let blocker_report = self.blockers();
        if let Some(unblock) = crate::blockers::status_advice(&blocker_report.blockers) {
            advice.push(unblock);
        }
        // A status that cannot read refs still reports everything else; the
        // unpushed count is context, not a precondition for any command.
        if let Some(row) = budget_cut_advice(
            &budget_cut,
            checkouts.as_ref(),
            &unpushed_work.not_inspected_sessions,
            upstream_ref.as_deref(),
        ) {
            advice.push(row);
        }
        advice.extend(unpushed_work_advice(&unpushed_work, now_ms, !promotes));
        if refresh && before_deadline() {
            advice.extend(self.integration_behind_upstream_advice());
        } else if refresh && !budget_cut.contains(&"branch_drift") {
            budget_cut.push("branch_drift");
        }
        advice.extend(overlap_pair_advice(&overlap_pairs));
        // Cached listing and local refs only: `status` never calls GitHub.
        if refresh && before_deadline() {
            advice.extend(self.pr_overlap_advice(now_ms));
        } else if refresh {
            budget_cut.push("pr_overlap_refresh");
        } else {
            advice.extend(self.recorded_pr_overlap_advice(now_ms, &baseline_head, &baseline_ref));
        }
        // Several sessions on one PR conflict by construction; name it.
        advice.extend(self.duplicate_work_advice(&agents));
        // Who is driving a release or another named operation, and whether
        // they still are: two drivers race each other's merges and tags.
        let ownership_claims = self.ownership_claim_views(&agents, now_ms)?;
        advice.extend(crate::ownership::ownership_claim_advice(
            &ownership_claims,
            now_ms,
        ));
        // A worktree stuck mid-merge cannot be classified or submitted.
        advice.extend(crate::overlap_pairs::mid_operation_advice(&agents));
        // The default branch moving under a session: last fetched copy only.
        if refresh && before_deadline() {
            advice.extend(self.behind_main_advice());
        } else if refresh && !budget_cut.contains(&"branch_drift") {
            budget_cut.push("branch_drift");
        }
        // Two sessions on one target: landing the shared edit first keeps
        // both on the default branch instead of chaining one onto the other.
        if refresh && before_deadline() {
            advice.extend(crate::shared_edit_advice::shared_edit_advice(
                &self.repo,
                &agents,
                &overlaps,
                &scope_overlaps,
            ));
        } else if refresh {
            budget_cut.push("shared_edit_classification");
        }
        let in_flight_submits = crate::submit_progress::in_flight_submits(&self.main_root, now_ms);
        advice.extend(stalled_submit_advice(&in_flight_submits));
        let foreign_started = std::time::Instant::now();
        // The main checkout is where agents gather before starting their own
        // sessions, so several of them there is normal, not co-tenancy.
        let main_checkout = self.main_root.to_string_lossy();
        let live_worktrees: Vec<(i64, String)> = agents
            .iter()
            .filter(|agent| {
                matches!(
                    agent.derived_status,
                    SessionStatus::Active | SessionStatus::Idle | SessionStatus::Stale
                ) && agent.session.worktree_path != main_checkout
            })
            .map(|agent| (agent.session.id, agent.session.worktree_path.clone()))
            .collect();
        let foreign = crate::session_holder::foreign_process_advice(&self.store, &live_worktrees);
        let foreign_deferred = foreign.is_none();
        advice.extend(foreign.unwrap_or_default());
        let live_session_ids: Vec<i64> = agents
            .iter()
            .filter(|agent| {
                matches!(
                    agent.derived_status,
                    SessionStatus::Active | SessionStatus::Idle | SessionStatus::Stale
                )
            })
            .map(|agent| agent.session.id)
            .collect();
        advice.extend(crate::install_replacement::advice(
            &self.store,
            &live_session_ids,
            &crate::version::current_binary_build(),
        ));
        phase_timings_ms.insert(
            "foreign_processes".into(),
            foreign_started.elapsed().as_millis() as u64,
        );

        let liveness_started = std::time::Instant::now();
        let leases = self.store.active_leases()?;
        let lease_liveness = crate::lease_liveness::assess(
            &self.store,
            &leases,
            crate::session_holder::ProcessTable::snapshot().as_ref(),
            now_ms,
            crate::lease_liveness::LeaseLivenessPolicy::load(&self.main_root),
        )?;
        phase_timings_ms.insert(
            "lease_liveness".into(),
            liveness_started.elapsed().as_millis() as u64,
        );
        let lease_release_requests: Vec<_> = self
            .lease_release_requests()?
            .into_iter()
            .filter(|request| {
                request.state == crate::lease_requests::RequestState::Pending
                    || request
                        .resolved_at
                        .is_some_and(|at| now_ms.saturating_sub(at) < 24 * 60 * 60 * 1000)
            })
            .collect();
        for request in lease_release_requests
            .iter()
            .filter(|request| request.state == crate::lease_requests::RequestState::Pending)
        {
            advice.push(StatusAdvice {
                id: "lease.release-requested",
                severity: StatusAdviceSeverity::Warning,
                reason: "another session asked this session to release a lease",
                summary: format!(
                    "session {} asks session {} to release {} (request {}): {}",
                    request.requester_session_id,
                    request.holder_session_id,
                    request.path,
                    request.request_id,
                    request.reason
                ),
                session_id: Some(request.holder_session_id),
                queue_entry_id: None,
                evidence: vec![
                    format!("requester: session {}", request.requester_session_id),
                    format!("holder: session {}", request.holder_session_id),
                    format!("state: {}", request.state.as_str()),
                ],
                commands: crate::lease_requests::holder_commands(request),
            });
        }

        phase_timings_ms.insert("build_total".into(), started.elapsed().as_millis() as u64);
        Ok(StatusView {
            deferred_checks: {
                let mut deferred: Vec<String> = if refresh {
                    Vec::new()
                } else {
                    vec![
                        "dirty_worktrees",
                        "unpushed_commits",
                        "promoted_conflicts",
                        "branch_drift",
                        "shared_edit_classification",
                        "pr_overlap_refresh",
                        "cleanup_eligibility",
                    ]
                    .into_iter()
                    .map(String::from)
                    .collect()
                };
                if foreign_deferred {
                    deferred.push("foreign_processes".into());
                }
                // Checks the inspection budget cut short (#460), named as the
                // routine view names the checks it never runs.
                for check in &budget_cut {
                    if !deferred.iter().any(|deferred| deferred == check) {
                        deferred.push((*check).into());
                    }
                }
                deferred.sort();
                deferred.dedup();
                deferred
            },
            leases_refreshed_at_ms: self
                .store
                .meta_get("leases.refreshed_at_ms")?
                .and_then(|v| v.parse().ok()),
            leases_refreshed: refresh,
            phase_timings_ms,
            publication_baseline_ref: baseline_ref,
            publication_baseline_head: baseline_head,
            summary,
            advice,
            outstanding_advisories: self.store.advisories(false)?,
            advisory_delivery: self.store.advisory_delivery_summary()?,
            outstanding_entry_exposures: self.store.outstanding_entry_path_exposures()?,
            agents,
            leases,
            lease_liveness,
            lease_release_requests,
            overlaps,
            overlap_pairs,
            scope_overlaps,
            ownership_claims,
            promoted_conflicts,
            coordinated_operations,
            queue,
            queue_history: StatusQueueHistory {
                schema_version: crate::MERGE_QUEUE_HISTORY_SCHEMA_VERSION,
                terminal_counts,
                command: "aethyme broker advanced queue history".into(),
            },
            integration_branch,
            integration_head,
            main_head,
            upstream_ref,
            upstream_head,
            main_ahead_upstream_commits,
            main_behind_upstream_commits,
            integration_reconciliation,
            cleanup_retention,
            review_refusals,
            blockers: blocker_report.blockers,
            blocker_sources_unavailable: blocker_report.unavailable,
            unpushed_work,
            in_flight_submits,
        })
    }

    /// The local default branch and its tip, resolved offline from
    /// `origin/HEAD`, which is written at clone time. Guessing a branch name
    /// would be worse than refusing: it would silently reconcile the wrong ref.
    /// Look for the default-branch commit that carried a session's work.
    ///
    /// Read-only. The answer is computed from content against fixed historical
    /// commits, so it is stable: re-running after the branch advances gives the
    /// same verdict, which is what makes it worth recording.
    pub fn scan_session_representation(
        &mut self,
        session_id: i64,
    ) -> Result<RepresentationScan, BrokerOpError> {
        let session = self.store().session(session_id)?;
        let worktree_path = PathBuf::from(&session.worktree_path);
        let checkout = GitRepo::discover(&worktree_path)?;
        let head = checkout.head_commit()?;
        let (branch, branch_ref, branch_tip) = self.default_branch_tip()?;
        // Landed work reaches the remote first, and the ship lane deliberately
        // leaves the local default branch where it was -- every publication
        // prints "Local main unchanged". Searching only the local ref therefore
        // excludes exactly the commits a squash-merged session is looking for:
        // on this machine that search examined zero commits while twenty-three
        // sat on the remote-tracking ref (#222).
        let (branch_ref, branch_tip) = self
            .remote_tracking_default_branch(&branch, &branch_tip)
            .unwrap_or((branch_ref, branch_tip));

        // Where the head diverged from the branch, as the cleanup audit and
        // plan measure it, so the three cannot disagree about which commits
        // this checkout holds (#408).
        let base = crate::landing_base(&checkout, &head, &branch_tip)?;

        let content = crate::session_content(&checkout, &base, &head)?;
        let search = crate::find_landing(
            &checkout,
            &content,
            &branch_tip,
            crate::representation::DEFAULT_SEARCH_CAP,
        )?;
        let representing = search.landing().map(|landing| landing.commit.clone());
        let digest = crate::representation_plan_digest(session_id, &head, representing.as_deref());

        Ok(RepresentationScan {
            session_id,
            session_head: head.clone(),
            base,
            branch,
            branch_ref,
            branch_tip,
            changed_paths: content.paths.keys().cloned().collect(),
            search,
            existing: self.store().session_representation(session_id, &head)?,
            digest,
        })
    }

    /// Persist a scan's verdict, bound to the digest of the plan that was shown.
    ///
    /// The digest covers the session, the exact head, and the representing
    /// commit, so a confirmation cannot be replayed against a session that has
    /// since committed more work.
    pub fn record_session_representation(
        &mut self,
        session_id: i64,
        confirm: &str,
    ) -> Result<RepresentationScan, BrokerOpError> {
        let scan = self.scan_session_representation(session_id)?;
        if confirm != scan.digest {
            return Err(BrokerOpError::RepresentationUnavailable {
                reason: format!(
                    "confirmation {confirm} does not match plan digest {}; re-run the scan and confirm the digest it prints",
                    scan.digest
                ),
            });
        }
        if !scan.search.represented() {
            return Err(BrokerOpError::RepresentationUnavailable {
                reason: format!(
                    "session {session_id} is not represented on {}; nothing to record",
                    scan.branch
                ),
            });
        }

        let (representing, evidence) = match &scan.search.outcome {
            crate::LandingOutcome::Landed(landing) => (
                Some(landing.commit.clone()),
                format!(
                    "{} path(s) match {} — {}",
                    landing.paths,
                    &landing.commit[..12.min(landing.commit.len())],
                    landing.subject
                ),
            ),
            _ => (
                None,
                format!("{} already holds the session's net content", scan.branch),
            ),
        };

        self.persist_representation(
            &scan,
            crate::types::RepresentationDiscovery::HistoryWalk,
            None,
            representing,
            evidence,
        )?;
        self.scan_session_representation(session_id)
    }

    pub(super) fn persist_representation(
        &mut self,
        scan: &RepresentationScan,
        discovery: crate::types::RepresentationDiscovery,
        pr_number: Option<i64>,
        representing_commit: Option<String>,
        evidence: String,
    ) -> Result<(), BrokerOpError> {
        let paths_json = serde_json::to_string(&scan.changed_paths).map_err(|source| {
            BrokerOpError::RepresentationUnavailable {
                reason: format!("cannot encode the session's changed paths: {source}"),
            }
        })?;
        self.store()
            .record_session_representation(&crate::types::NewSessionRepresentation {
                session_id: scan.session_id,
                session_head: scan.session_head.clone(),
                representing_commit,
                representing_ref: scan.branch_ref.clone(),
                discovery,
                pr_number,
                paths_json,
                evidence,
            })?;
        Ok(())
    }

    /// Record representation observed at merge time, for the session whose
    /// branch a just-merged pull request carried.
    ///
    /// This is an observation, not a reviewed apply, so it takes no digest --
    /// but it still re-proves the landing from content. That guard is what
    /// makes a wrong session link harmless: a session that merges someone
    /// else's pull request finds none of its own work on the branch and records
    /// nothing. Any failure is swallowed, because the merge itself succeeded
    /// and `representation scan` remains the explicit lane.
    pub(crate) fn note_merge_time_representation(
        &mut self,
        session_id: i64,
        pr_number: Option<i64>,
    ) -> Option<String> {
        let scan = self.scan_session_representation(session_id).ok()?;
        if scan.recorded() {
            return None;
        }
        let landing = scan.search.landing()?;
        let commit = landing.commit.clone();
        let evidence = format!(
            "merged pull request landed {} path(s) as {} — {}",
            landing.paths,
            &commit[..12.min(commit.len())],
            landing.subject
        );
        self.persist_representation(
            &scan,
            crate::types::RepresentationDiscovery::MergeTime,
            pr_number,
            Some(commit.clone()),
            evidence,
        )
        .ok()?;
        Some(commit)
    }

    /// A remote-tracking ref that still holds this commit, verified against the
    /// remote right now.
    ///
    /// Reachability from a pushed ref is the durability property cleanup
    /// actually needs: it holds whatever the merge strategy did, where ancestry
    /// against the default branch stops holding the moment a squash rewrites
    /// the SHA.
    ///
    /// The freshness guard is what makes it safe to act on. A remote-tracking
    /// ref left behind by a branch someone deleted would otherwise prove
    /// durability for commits the remote no longer has -- on the one path that
    /// authorizes removing a directory. So the ref must still match what the
    /// remote reports, and not knowing -- offline, slow, unreadable -- is
    /// unproven rather than absent. Offline, this evidence simply never fires
    /// and behaviour is exactly what it was.
    pub(super) fn remote_durability_evidence(
        &self,
        session_head: &str,
    ) -> Option<(String, String)> {
        const FRESHNESS_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);
        let repo = self.repo_handle();
        for remote_ref in repo.remote_refs_containing(session_head) {
            let Some(rest) = remote_ref.strip_prefix("refs/remotes/") else {
                continue;
            };
            let Some((remote, branch)) = rest.split_once('/') else {
                continue;
            };
            // `origin/HEAD` is a symbolic alias, not a branch the remote lists.
            if branch == "HEAD" {
                continue;
            }
            let Some(tracked) = repo.resolve_ref(&remote_ref) else {
                continue;
            };
            let Some(live) = repo.remote_branch_head(remote, branch, FRESHNESS_BUDGET) else {
                continue;
            };
            if live == tracked {
                return Some((remote_ref, tracked));
            }
        }
        None
    }

    /// The remote-tracking default branch, when it is strictly ahead of the
    /// local one.
    ///
    /// Returns `None` when the ref is missing, unreadable, or not a descendant
    /// of the local tip. "Not a descendant" is the case that matters: a local
    /// branch carrying commits the remote has not seen is not a superset, and
    /// silently searching the remote instead would drop them from the search
    /// space rather than add to it.
    pub(super) fn remote_tracking_default_branch(
        &self,
        branch: &str,
        local_tip: &str,
    ) -> Option<(String, String)> {
        let remote_ref = format!("refs/remotes/origin/{branch}");
        let remote_tip = self.repo_handle().resolve_ref(&remote_ref)?;
        if remote_tip == local_tip {
            return None;
        }
        self.repo_handle()
            .is_ancestor(local_tip, &remote_tip)
            .then_some((remote_ref, remote_tip))
    }

    /// The fetched remote default branch, as `(ref, tip)`: what auto-cleanup
    /// proves containment against (#588). Local branches and integration do
    /// not count -- work only there is not on the remote.
    pub(crate) fn remote_default_ref_and_tip(&self) -> Option<(String, String)> {
        let head_ref = self.repo.symbolic_ref("refs/remotes/origin/HEAD")?;
        let branch = head_ref.strip_prefix("refs/remotes/origin/")?;
        if branch.is_empty() || branch == "HEAD" {
            return None;
        }
        let tip = self.repo.resolve_ref(&head_ref)?;
        Some((head_ref, tip))
    }

    pub(super) fn remote_tracking_default_tip(&self) -> Option<String> {
        let head_ref = self.repo.symbolic_ref("refs/remotes/origin/HEAD")?;
        let branch = head_ref.strip_prefix("refs/remotes/origin/")?;
        if branch.is_empty() || branch == "HEAD" {
            return None;
        }
        self.repo.resolve_ref(&head_ref)
    }

    /// The commit publication actually targets, and a label naming it.
    ///
    /// `head_commit()` is whatever branch happens to be checked out, which is
    /// the default branch only by coincidence. Counting integration's lead
    /// against it reported "ahead of main by 382 commits" on a checkout sitting
    /// on an eleven-day-old feature branch, while the lead over the published
    /// branch was one commit -- and an agent refused to publish on that number.
    ///
    /// Preference order is what-is-published first: the remote-tracking default
    /// branch, then the local default branch, then the checkout. The last two
    /// keep a repository without a fetched remote working exactly as before,
    /// and the label says which one answered so the number is never read
    /// against the wrong baseline again.
    pub(super) fn publication_baseline(&self) -> Result<(String, String), BrokerOpError> {
        let branch = self
            .repo
            .symbolic_ref("refs/remotes/origin/HEAD")
            .and_then(|head_ref| head_ref.rsplit('/').next().map(str::to_string));
        if let Some(branch) = branch.as_deref() {
            let remote_ref = format!("refs/remotes/origin/{branch}");
            if let Some(commit) = self.repo.resolve_ref(&remote_ref) {
                return Ok((remote_ref, commit));
            }
            let local_ref = format!("refs/heads/{branch}");
            if let Some(commit) = self.repo.resolve_ref(&local_ref) {
                return Ok((local_ref, commit));
            }
        }
        Ok(("HEAD".to_string(), self.repo.head_commit()?))
    }

    /// Unpushed work across live sessions and the integration branch.
    ///
    /// Empty when the repository has no remote: there is nowhere to push to,
    /// so every commit would count and the number would mean nothing.
    pub fn unpushed_work(&self, now_ms: i64) -> Result<crate::UnpushedWorkReport, BrokerOpError> {
        self.unpushed_work_within(now_ms, None)
    }

    /// [`Self::unpushed_work`], reading session checkouts in parallel and
    /// listing in `not_inspected_sessions` every session not reached before
    /// `deadline` (#460). One cherry-marked `rev-list` per session against an
    /// upstream that moved far ahead is what kept `doctor` running for half a
    /// minute on a repository with seventy sessions.
    pub fn unpushed_work_within(
        &self,
        now_ms: i64,
        deadline: Option<std::time::Instant>,
    ) -> Result<crate::UnpushedWorkReport, BrokerOpError> {
        let push_session_branches = crate::session_push::session_push_enabled(&self.repo);
        let mut report = crate::UnpushedWorkReport {
            push_session_branches,
            ..Default::default()
        };
        let sessions = self.store.live_sessions()?;
        if self.repo.remotes()?.is_empty() {
            return Ok(report);
        }
        let (baseline_ref, _) = self.publication_baseline()?;
        let upstream = baseline_ref
            .starts_with("refs/remotes/")
            .then_some(baseline_ref.as_str());
        let integration_fallback = Some(self.integration_head_snapshot()?.1);
        let inspected = crate::worktree_report::inspect_in_parallel(&sessions, |session| {
            if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                return Err(());
            }
            session_off_remote_work(session, upstream, integration_fallback.as_deref())
                .map_err(|_| ())
        });
        for (session, inspected) in sessions.into_iter().zip(inspected) {
            let Ok(found) = inspected else {
                report.not_inspected_sessions.push(session.id);
                continue;
            };
            let Some((head, work)) = found else {
                continue;
            };
            if work.commits == 0 {
                continue;
            }
            let age = now_ms.saturating_sub(work.oldest_at_ms.unwrap_or(now_ms));
            report.sessions.push(crate::UnpushedSessionWork {
                session_id: session.id,
                branch: session.branch.clone(),
                head,
                unpushed_commits: work.commits,
                oldest_unpushed_at_ms: work.oldest_at_ms,
                severity: crate::unpushed::session_severity(age, push_session_branches),
                command: format!("aethyme broker push --session {}", session.id),
            });
        }
        if let Some(upstream) = upstream {
            let (branch, head) = self.integration_head_snapshot()?;
            let unpublished = self
                .repo
                .cherry_marked(upstream, &head, crate::CherrySide::Right)?;
            let off_remote =
                crate::unpushed::off_remote_work(&self.repo, &head, &[], Some(upstream))?;
            let pending = unpublished
                .iter()
                .filter(|(_, equivalent)| !equivalent)
                .map(|(commit, _)| commit.as_str())
                .collect::<Vec<_>>();
            if !pending.is_empty() {
                let oldest = self.repo.oldest_commit_time_ms(&pending);
                let age = now_ms.saturating_sub(oldest.unwrap_or(now_ms));
                report.integration = Some(crate::UnpublishedIntegrationWork {
                    branch,
                    head,
                    upstream_ref: upstream.to_string(),
                    unpublished_commits: u32::try_from(pending.len()).unwrap_or(u32::MAX),
                    on_no_remote: off_remote.commits,
                    oldest_unpublished_at_ms: oldest,
                    severity: crate::unpushed::integration_severity(age),
                });
            }
        }
        Ok(report)
    }

    pub(crate) fn default_branch_tip(&self) -> Result<(String, String, String), BrokerOpError> {
        let repo = self.repo_handle();
        let head_ref = repo.symbolic_ref("refs/remotes/origin/HEAD").ok_or_else(|| {
            BrokerOpError::MainReconcileUnavailable {
                reason: "refs/remotes/origin/HEAD is unset, so the default branch is unknown; set it with `git remote set-head origin --auto`".into(),
            }
        })?;
        let branch = head_ref
            .rsplit('/')
            .next()
            .ok_or_else(|| BrokerOpError::MainReconcileUnavailable {
                reason: format!("cannot read a branch name from {head_ref}"),
            })?
            .to_string();
        let local_ref = format!("refs/heads/{branch}");
        let local_sha = repo.resolve_ref(&local_ref).ok_or_else(|| {
            BrokerOpError::MainReconcileUnavailable {
                reason: format!("{local_ref} does not resolve"),
            }
        })?;
        Ok((branch, local_ref, local_sha))
    }

    /// Commits sitting on the local default branch that integration does not
    /// contain. They reached the branch without passing through submit, so no
    /// session accounts for them and `status` would otherwise say nothing
    /// (issue #141).
    ///
    /// Resolution is offline: `origin/HEAD` is written at clone time. When it is
    /// unset the check is skipped rather than guessing a branch name, because a
    /// wrong guess would report every repository as having external writes.
    pub(super) fn external_default_branch_writes(
        &self,
        integration_head: &str,
    ) -> Result<Option<(String, Vec<String>)>, BrokerOpError> {
        let repo = self.repo_handle();
        let Some(head_ref) = repo.symbolic_ref("refs/remotes/origin/HEAD") else {
            return Ok(None);
        };
        let Some(branch) = head_ref.rsplit('/').next().map(str::to_string) else {
            return Ok(None);
        };
        let local_ref = format!("refs/heads/{branch}");
        let Some(local_sha) = repo.resolve_ref(&local_ref) else {
            return Ok(None);
        };
        let commits = repo.commits_between_oldest(integration_head, &local_sha)?;
        if commits.is_empty() {
            return Ok(None);
        }
        Ok(Some((branch, commits)))
    }

    pub(super) fn status_advice(
        &self,
        agents: &[AgentView],
        promoted_conflicts: &[PromotedConflict],
        queue: &[MergeQueueEntry],
        integration: (&str, &str),
        promotes: bool,
        checkouts: Option<&std::collections::BTreeMap<i64, CheckoutInspection>>,
    ) -> Vec<StatusAdvice> {
        use std::collections::BTreeMap;
        let (integration_branch, integration_head) = integration;

        let mut advice = Vec::new();
        let mut latest_queue_by_session = BTreeMap::new();
        for entry in queue {
            latest_queue_by_session.insert(entry.session_id, entry);
        }
        let agents_by_id: BTreeMap<i64, &AgentView> = agents
            .iter()
            .map(|agent| (agent.session.id, agent))
            .collect();

        for agent in agents {
            let Some(entry) = latest_queue_by_session.get(&agent.session.id) else {
                continue;
            };
            match entry.status {
                MergeStatus::Rejected => advice.push(rejected_submit_advice(agent, entry)),
                // A deferred entry is left `Submitted` rather than `Rejected`,
                // because no gate judged it. Without its own advice, `status`
                // would say nothing about work sitting unverified -- but a
                // `Submitted` entry is also the normal state of one in flight,
                // so only the explicit marker raises it.
                MergeStatus::Submitted if submission_was_deferred(entry) => {
                    advice.push(deferred_submit_advice(agent, entry));
                }
                MergeStatus::Conflict => advice.push(conflict_submit_advice(agent, entry)),
                _ => {}
            }
        }

        // Graph integrity is advice (#280): the latest verdict recorded for a
        // session's tree is reported here and never blocks anything.
        for agent in agents {
            if let Ok(Some(graph)) = self.finish_last_graph_integrity(agent.session.id)
                && let Some(row) = graph_integrity_advice(agent, &graph)
            {
                advice.push(row);
            }
        }

        let mut promoted_by_session: BTreeMap<i64, Vec<&PromotedConflict>> = BTreeMap::new();
        for conflict in promoted_conflicts {
            promoted_by_session
                .entry(conflict.session_id)
                .or_default()
                .push(conflict);
        }
        for (session_id, conflicts) in promoted_by_session {
            let worktree = agents_by_id
                .get(&session_id)
                .map(|agent| agent.session.worktree_path.as_str())
                .unwrap_or("");
            advice.push(promoted_conflict_advice(
                session_id,
                worktree,
                integration_branch,
                &conflicts,
            ));
        }

        for agent in agents.iter().filter(|_| checkouts.is_some()) {
            let Some(CheckoutInspection::Read { dirty, head }) =
                checkouts.and_then(|checkouts| checkouts.get(&agent.session.id))
            else {
                continue;
            };
            if !dirty.is_empty() {
                advice.push(dirty_worktree_advice(agent, dirty));
                continue;
            }
            let Some(head) = head else {
                continue;
            };
            if let Some(entry) = queue.iter().rev().find(|entry| {
                entry.session_id == agent.session.id
                    && &entry.head_commit == head
                    && matches!(
                        entry.status,
                        MergeStatus::Promoted | MergeStatus::ExternallyLanded
                    )
            }) {
                advice.push(promoted_clean_finish_advice(agent, entry));
            }
        }

        if promotes && !agents.is_empty() {
            advice.push(integration_movement_advice(
                integration_branch,
                integration_head,
                agents,
            ));
        }

        advice
    }

    pub(super) fn stalled_coordinated_operation_advice(
        &self,
        operations: &[crate::types::CoordinatedOperation],
    ) -> Option<StatusAdvice> {
        let stalled = operations
            .iter()
            .filter(|operation| {
                let state = crate::operations::operation_liveness_view(operation).state;
                state == "progress_stale" || state == "heartbeat_stale"
            })
            .collect::<Vec<_>>();
        if stalled.is_empty() {
            return None;
        }
        // A quiet holder and a dead one are different claims, and only the
        // second justifies Blocked. The evidence line already prints the pid,
        // so the answer is available rather than inferred: a stale heartbeat
        // whose process is still alive is a slow operation, and saying it "may
        // have died" is what makes an operator re-issue work that is running.
        let heartbeat_stale = stalled.iter().any(|operation| {
            crate::operations::operation_liveness_view(operation).state == "heartbeat_stale"
                && operation.pid > 0
                && crate::operations::process_is_gone(operation.pid)
        });
        let subject = if stalled.len() == 1 {
            format!("operation {}", stalled[0].id)
        } else {
            format!("{} coordinated operations", stalled.len())
        };
        Some(StatusAdvice {
            id: "coordination.operation-stalled",
            severity: if heartbeat_stale {
                StatusAdviceSeverity::Blocked
            } else {
                StatusAdviceSeverity::Warning
            },
            reason: "running coordinated operation has stale progress or heartbeat",
            summary: format!(
                "{subject} has stopped reporting {}",
                if heartbeat_stale {
                    "a live heartbeat"
                } else {
                    "meaningful progress"
                }
            ),
            session_id: (stalled.len() == 1).then_some(stalled[0].session_id),
            queue_entry_id: None,
            evidence: stalled
                .iter()
                .map(|operation| {
                    format!(
                        "operation {} (session {}, pid {}): {}",
                        operation.id,
                        operation.session_id,
                        operation.pid,
                        crate::operations::operation_liveness_summary(operation)
                    )
                })
                .collect(),
            commands: if stalled.len() == 1 {
                vec![format!(
                    "aethyme broker advanced operations show {}",
                    stalled[0].id
                )]
            } else {
                vec!["aethyme broker advanced operations list".into()]
            },
        })
    }

    // ── doctor (operational health) ───────────────────────────────────

    /// Health checks an operator (or CI) can run cheaply: database
    /// integrity, live sessions whose worktree no longer exists, and
    /// orphaned gate pidfiles (whose process group is gone) — the latter
    /// are removed as part of the check.
    pub fn doctor(&mut self) -> Result<DoctorReport, BrokerOpError> {
        self.doctor_inner(false)
    }

    /// Same health checks as [`Self::doctor`], plus an explicit local CLI
    /// reinstall when the running binary is behind this checkout's
    /// integration branch. The repair installs from a detached worktree at
    /// integration, not from the operator's possibly dirty checkout.
    pub fn doctor_with_version_fix(&mut self) -> Result<DoctorReport, BrokerOpError> {
        self.doctor_inner(true)
    }

    pub(super) fn doctor_inner(
        &mut self,
        fix_version: bool,
    ) -> Result<DoctorReport, BrokerOpError> {
        let started = std::time::Instant::now();
        let deadline = started + status_inspection_budget();
        let _git_deadline = crate::git::limit_git_until(deadline);
        let mut phase_timings_ms = std::collections::BTreeMap::new();
        let mut budget_cut = Vec::new();
        let mut phase = |name: &str, since: std::time::Instant| {
            phase_timings_ms.insert(name.to_string(), since.elapsed().as_millis() as u64);
        };
        let integrity = self.store.integrity_check()?;

        let live_sessions = self.store.live_sessions()?;
        let mut missing_worktrees = Vec::new();
        let mut missing_worktrees_cut = false;
        for session in &live_sessions {
            if std::time::Instant::now() >= deadline {
                missing_worktrees_cut = true;
                break;
            }
            if matches!(
                session.status,
                SessionStatus::Active | SessionStatus::Idle | SessionStatus::Stale
            ) && !Path::new(&session.worktree_path).exists()
            {
                missing_worktrees.push(session.id);
            }
        }
        if missing_worktrees_cut {
            budget_cut.push("missing_worktrees".into());
        }

        let mut orphaned_pidfiles = Vec::new();
        let mut gate_pidfiles_cut = false;
        let run_dir = self.main_root.join(".aethyme/run/gates");
        if let Ok(entries) = std::fs::read_dir(&run_dir) {
            for entry in entries.flatten() {
                if std::time::Instant::now() >= deadline {
                    gate_pidfiles_cut = true;
                    break;
                }
                let Ok(content) = std::fs::read_to_string(entry.path()) else {
                    continue;
                };
                let alive = content
                    .split_whitespace()
                    .next()
                    .and_then(|pid| pid.parse::<i64>().ok())
                    .map(pid_alive)
                    .unwrap_or(false);
                if !alive {
                    let _ = std::fs::remove_file(entry.path());
                    orphaned_pidfiles.push(entry.file_name().to_string_lossy().into_owned());
                }
            }
        }
        if gate_pidfiles_cut {
            budget_cut.push("gate_pidfiles".into());
        }

        let purged_stale_leases = if std::time::Instant::now() < deadline {
            self.store.purge_leases_of_cleaned_sessions()?
        } else {
            budget_cut.push("stale_lease_purge".into());
            0
        };
        phase("store", started);
        let retention_started = std::time::Instant::now();
        let retention = self.gc_health_until(deadline)?;
        budget_cut.extend(retention.deferred_checks.iter().cloned());
        phase("retention", retention_started);
        let version_started = std::time::Instant::now();
        let version = crate::version::inspect_version(&self.main_root);
        let version_repair = fix_version.then(|| self.repair_local_cli_version(&version));
        phase("version", version_started);
        let movement_started = std::time::Instant::now();
        let integration_movement = if std::time::Instant::now() < deadline {
            match self.integration_movement_notice_from_sessions(&live_sessions) {
                Ok(notice) => notice,
                Err(_) if std::time::Instant::now() >= deadline => {
                    budget_cut.push("integration_movement".into());
                    None
                }
                Err(error) => return Err(error),
            }
        } else {
            budget_cut.push("integration_movement".into());
            None
        };
        phase("integration_movement", movement_started);
        let unpushed_started = std::time::Instant::now();
        let unpushed_work = match self.unpushed_work_within(now_ms(), Some(deadline)) {
            Ok(report) => report,
            Err(_) if std::time::Instant::now() >= deadline => {
                budget_cut.push("unpushed_commits".into());
                crate::UnpushedWorkReport {
                    not_inspected_sessions: live_sessions
                        .iter()
                        .map(|session| session.id)
                        .collect(),
                    ..crate::UnpushedWorkReport::default()
                }
            }
            Err(error) => return Err(error),
        };
        if !unpushed_work.not_inspected_sessions.is_empty() {
            budget_cut.push("unpushed_commits".to_string());
        }
        phase("unpushed", unpushed_started);
        let rest_started = std::time::Instant::now();
        let recent_command_failures = if std::time::Instant::now() < deadline {
            let failures = self.recent_command_failures(now_ms())?;
            if std::time::Instant::now() < deadline {
                failures
            } else {
                budget_cut.push("recent_command_failures".into());
                Vec::new()
            }
        } else {
            budget_cut.push("recent_command_failures".into());
            Vec::new()
        };
        let hooks_path = if std::time::Instant::now() < deadline {
            let finding = crate::hooks::inspect_hooks_path(&self.main_root);
            if std::time::Instant::now() < deadline {
                finding
            } else {
                budget_cut.push("hooks_path".into());
                None
            }
        } else {
            budget_cut.push("hooks_path".into());
            None
        };
        let leftover_integration_work = if std::time::Instant::now() < deadline {
            match self.leftover_integration_work() {
                Ok(work) => work,
                Err(_) if std::time::Instant::now() >= deadline => {
                    budget_cut.push("integration_drift".into());
                    None
                }
                Err(error) => return Err(error),
            }
        } else {
            budget_cut.push("integration_drift".into());
            None
        };
        phase("leftover_and_hooks", rest_started);
        phase("total", started);

        Ok(DoctorReport {
            integrity,
            version,
            version_repair,
            missing_worktrees,
            orphaned_pidfiles,
            purged_stale_leases,
            retention,
            integration_movement,
            unpushed_work,
            recent_command_failures,
            hooks_path,
            leftover_integration_work,
            phase_timings_ms,
            deferred_checks: budget_cut.clone(),
            budget_cut,
        })
    }

    /// The `status` leftover-work check, for `doctor`. Reads the integration
    /// ref without [`Self::integration_head`], which may fast-forward it: a
    /// health check moves no refs. A repository with no integration branch
    /// has nothing left over.
    pub(super) fn leftover_integration_work(&self) -> Result<Option<StatusAdvice>, BrokerOpError> {
        if PromoteConfig::load(&self.main_root).mode.promotes_at_all() {
            return Ok(None);
        }
        let Some(integration_head) = self.integration_tip() else {
            return Ok(None);
        };
        let (baseline_ref, baseline_head) = self.publication_baseline()?;
        leftover_integration_advice(
            &self.repo,
            &integration_head,
            (&baseline_ref, &baseline_head),
            None,
        )
    }

    /// Failed broker commands of the last day, for `doctor`.
    pub fn recent_command_failures(
        &self,
        now: i64,
    ) -> Result<Vec<RecentCommandFailure>, BrokerOpError> {
        Ok(self
            .store
            .recent_events_of_kind(
                crate::events::BROKER_COMMAND_FAILED,
                now - RECENT_COMMAND_FAILURE_WINDOW_MS,
                RECENT_COMMAND_FAILURE_LIMIT,
            )?
            .iter()
            .filter_map(recent_command_failure)
            .collect())
    }

    pub(super) fn integration_movement_notice_from_sessions(
        &mut self,
        sessions: &[Session],
    ) -> Result<Option<IntegrationMovementNotice>, BrokerOpError> {
        // Verify-only: sessions never submit into integration, so there is no
        // movement to wait out.
        if sessions.is_empty() || !PromoteConfig::load(&self.main_root).mode.promotes_at_all() {
            return Ok(None);
        }
        let (branch, head) = self.integration_head()?;
        let live_sessions = integration_live_sessions(sessions.to_vec());
        Ok(Some(IntegrationMovementNotice {
            branch: branch.clone(),
            head,
            message: format!(
                "{} live {} may submit and move {branch}; wait for a stable window before treating long checks as current-tip proof",
                live_sessions.len(),
                plural_word(live_sessions.len(), "session", "sessions")
            ),
            live_sessions,
            commands: vec![
                "aethyme broker advanced integration wait-stable --seconds 30".into(),
                "aethyme broker status".into(),
            ],
        }))
    }

    pub(super) fn repair_local_cli_version(
        &mut self,
        version: &VersionDriftReport,
    ) -> VersionRepairReport {
        let placeholder_commands = local_cli_repair_commands(None, None);
        let placeholder_command = placeholder_commands[0].clone();
        match version.status {
            VersionDriftStatus::Current | VersionDriftStatus::AheadOfIntegration => {
                return VersionRepairReport {
                    status: DoctorRepairStatus::NotNeeded,
                    attempted: false,
                    command: placeholder_command.clone(),
                    install_source: None,
                    integration_head: version.integration_head.clone(),
                    exit_code: None,
                    duration_ms: 0,
                    message: format!(
                        "no local CLI repair needed for version status {}",
                        version.status.as_str()
                    ),
                    stdout_tail: Vec::new(),
                    stderr_tail: Vec::new(),
                    commands: placeholder_commands.clone(),
                    steps: Vec::new(),
                };
            }
            VersionDriftStatus::NotAethymeSource | VersionDriftStatus::Unknown => {
                return VersionRepairReport {
                    status: DoctorRepairStatus::Skipped,
                    attempted: false,
                    command: placeholder_command.clone(),
                    install_source: None,
                    integration_head: version.integration_head.clone(),
                    exit_code: None,
                    duration_ms: 0,
                    message: format!(
                        "local CLI repair is available only for comparable Aethyme source checkouts; version status is {}",
                        version.status.as_str()
                    ),
                    stdout_tail: Vec::new(),
                    stderr_tail: Vec::new(),
                    commands: placeholder_commands.clone(),
                    steps: Vec::new(),
                };
            }
            VersionDriftStatus::BehindIntegration
            | VersionDriftStatus::ReleaseBehindIntegration => {}
        }

        let Some(integration_head) = version.integration_head.as_deref() else {
            return VersionRepairReport {
                status: DoctorRepairStatus::Skipped,
                attempted: false,
                command: placeholder_command.clone(),
                install_source: None,
                integration_head: None,
                exit_code: None,
                duration_ms: 0,
                message: "integration head is unavailable; cannot choose a repair source".into(),
                stdout_tail: Vec::new(),
                stderr_tail: Vec::new(),
                commands: placeholder_commands.clone(),
                steps: Vec::new(),
            };
        };
        if !version.repo_is_aethyme_source {
            return VersionRepairReport {
                status: DoctorRepairStatus::Skipped,
                attempted: false,
                command: placeholder_command,
                install_source: None,
                integration_head: Some(integration_head.to_string()),
                exit_code: None,
                duration_ms: 0,
                message: "not an Aethyme source checkout; refusing to reinstall local CLI".into(),
                stdout_tail: Vec::new(),
                stderr_tail: Vec::new(),
                commands: placeholder_commands,
                steps: Vec::new(),
            };
        }

        let temp_root = self
            .main_root
            .join(".aethyme/run/version-repair")
            .join(format!(
                "install-{}-{}-{}",
                short_commit(integration_head),
                std::process::id(),
                now_ms()
            ));
        let install_bin = cargo_install_bin_dir();
        let commands = local_cli_repair_commands(Some(&temp_root), Some(&install_bin));
        let command = commands[0].clone();
        let start = now_ms();
        let worktree = self
            .repo
            .worktree_add_detached(&temp_root, integration_head);
        if let Err(err) = worktree {
            return VersionRepairReport {
                status: DoctorRepairStatus::Fail,
                attempted: true,
                command,
                install_source: Some(temp_root.to_string_lossy().into_owned()),
                integration_head: Some(integration_head.to_string()),
                exit_code: None,
                duration_ms: now_ms().saturating_sub(start),
                message: format!("failed to create temporary integration worktree: {err}"),
                stdout_tail: Vec::new(),
                stderr_tail: Vec::new(),
                commands,
                steps: Vec::new(),
            };
        }

        let specs = local_cli_repair_step_specs(Some(&temp_root), Some(&install_bin));
        let steps = execute_version_repair_steps(&specs, |command| {
            let output = Command::new(&command[0])
                .args(&command[1..])
                .current_dir(&temp_root)
                .output()
                .map_err(|error| error.to_string())?;
            let mut success = output.status.success();
            let mut stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            if success && !active_version_matches(command, &stdout, integration_head) {
                success = false;
                stderr.push_str(&format!(
                    "\nactive PATH binary does not identify integration commit {}; PATH precedence still needs repair",
                    short_commit(integration_head)
                ));
            }
            Ok(RepairCommandOutput {
                success,
                exit_code: output.status.code(),
                stdout,
                stderr,
            })
        });
        let duration_ms = now_ms().saturating_sub(start);
        let _ = self.repo.worktree_remove(&temp_root, true);
        let all_passed = steps.iter().all(|step| step.success);
        let failed = steps
            .iter()
            .filter(|step| !step.success)
            .map(|step| format!("{} {}", step.component, step.action))
            .collect::<Vec<_>>();
        let stdout_tail = combined_repair_tail(&steps, true);
        let stderr_tail = combined_repair_tail(&steps, false);
        let exit_code = steps
            .iter()
            .find(|step| !step.success)
            .or_else(|| steps.last())
            .and_then(|step| step.exit_code);
        VersionRepairReport {
            status: if all_passed {
                DoctorRepairStatus::Pass
            } else {
                DoctorRepairStatus::Fail
            },
            attempted: true,
            command,
            install_source: Some(temp_root.to_string_lossy().into_owned()),
            integration_head: Some(integration_head.to_string()),
            exit_code,
            duration_ms,
            message: if all_passed {
                format!(
                    "installed and verified aethyme plus aethyme-engine-cli from {} {}; rerun doctor to observe the repaired binaries",
                    version.integration_branch,
                    short_commit(integration_head)
                )
            } else {
                format!(
                    "local binary repair from {} {} failed at: {}",
                    version.integration_branch,
                    short_commit(integration_head),
                    failed.join(", ")
                )
            },
            stdout_tail,
            stderr_tail,
            commands,
            steps,
        }
    }

    // ── finish ────────────────────────────────────────────────────────

    pub(super) fn handoff_report(
        event: crate::types::Event,
    ) -> Result<SessionHandoffReport, BrokerOpError> {
        let payload =
            event
                .payload_json
                .as_deref()
                .ok_or_else(|| BrokerOpError::InvalidHandoffEvent {
                    event_id: event.id,
                    reason: "payload is missing".into(),
                })?;
        let handoff = serde_json::from_str::<FinishHandoff>(payload).map_err(|error| {
            BrokerOpError::InvalidHandoffEvent {
                event_id: event.id,
                reason: error.to_string(),
            }
        })?;
        if event.session_id != Some(handoff.session_id) {
            return Err(BrokerOpError::InvalidHandoffEvent {
                event_id: event.id,
                reason: "payload session_id does not match event session_id".into(),
            });
        }
        Ok(SessionHandoffReport {
            event_id: event.id,
            recorded_at: event.ts,
            handoff,
        })
    }
}
