use super::*;

impl Broker {
    pub fn open(path_inside_repo: &Path) -> Result<Self, BrokerOpError> {
        Self::open_with_graph_impact_provider(path_inside_repo, GraphStoreImpactProvider)
    }

    /// Open a broker for compatibility diagnostics without changing the
    /// repository database, contracts, prepared operations, events, or refs.
    /// Readable older storage is copied and migrated only in a temporary
    /// snapshot owned by this broker instance.
    pub fn open_snapshot(path_inside_repo: &Path) -> Result<Self, BrokerOpError> {
        let here = GitRepo::discover(path_inside_repo)?;
        let main_root = here.main_root()?;
        let repo = GitRepo::discover(&main_root)?;
        let store = BrokerStore::open_snapshot_in_repo(&main_root)?;
        Ok(Self {
            repo,
            store,
            main_root,
            graph_impact_provider: Box::new(GraphStoreImpactProvider),
            host_operation_db_path: None,
            worktree_root_override: None,
            landing_deadline: std::cell::Cell::new(None),
            main_common_dir: std::cell::OnceCell::new(),
        })
    }

    /// Open only far enough to migrate compatible broker storage and backfill
    /// the immutable contract of live pre-v9 sessions. The CLI uses this for
    /// an explicitly requested session continuation; unlike normal open it
    /// does not reconcile prepared shared operations before compatibility has
    /// authorized the command itself.
    pub fn open_for_compatibility_backfill(path_inside_repo: &Path) -> Result<Self, BrokerOpError> {
        let here = GitRepo::discover(path_inside_repo)?;
        let main_root = here.main_root()?;
        let repo = GitRepo::discover(&main_root)?;
        let store = BrokerStore::open_in_repo(&main_root)?;
        let mut broker = Self {
            repo,
            store,
            main_root,
            graph_impact_provider: Box::new(GraphStoreImpactProvider),
            host_operation_db_path: None,
            worktree_root_override: None,
            landing_deadline: std::cell::Cell::new(None),
            main_common_dir: std::cell::OnceCell::new(),
        };
        broker.backfill_live_repository_contracts()?;
        Ok(broker)
    }

    /// Open a broker with an alternate read-only graph-impact provider.
    /// Provider outcomes remain confined to [`Self::semantic_gate_advice`].
    pub fn open_with_graph_impact_provider(
        path_inside_repo: &Path,
        graph_impact_provider: impl GraphImpactProvider + 'static,
    ) -> Result<Self, BrokerOpError> {
        let here = GitRepo::discover(path_inside_repo)?;
        let main_root = here.main_root()?;
        let repo = GitRepo::discover(&main_root)?;
        let store = BrokerStore::open_in_repo(&main_root)?;
        let mut broker = Self {
            repo,
            store,
            main_root,
            graph_impact_provider: Box::new(graph_impact_provider),
            host_operation_db_path: None,
            worktree_root_override: None,
            landing_deadline: std::cell::Cell::new(None),
            main_common_dir: std::cell::OnceCell::new(),
        };
        broker.backfill_live_repository_contracts()?;
        broker.reap_abandoned_prepared_operations()?;
        broker.recover_interrupted_promotion()?;
        broker.recover_prepared_reconciliation()?;
        broker.backfill_promoted_path_exposures()?;
        // Best effort and silent: only an already-confirmed journal may be
        // resumed here. Doctor exposes any remaining work or artifact drift.
        let _ = broker.resume_gc_maintenance();
        Ok(broker)
    }

    pub fn main_root(&self) -> &Path {
        &self.main_root
    }

    pub(super) fn session_context(&self, mut context: SessionContext) -> SessionContext {
        if context.repository_name.is_none()
            && (context.tab_name.is_some() || context.ai_provider.is_some())
        {
            context.repository_name = self
                .main_root
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string);
        }
        context
    }

    /// Override the per-user host-operation database.
    ///
    /// Production callers should use the platform default. This hook lets
    /// embedders and tests isolate host coordination without mutating process
    /// environment variables shared by concurrent threads.
    #[doc(hidden)]
    pub fn with_host_operation_database(mut self, path: impl Into<PathBuf>) -> Self {
        self.host_operation_db_path = Some(path.into());
        self
    }

    /// Override the exact broker worktree root.
    ///
    /// Production callers should use host-state placement. Tests and
    /// constrained embedders can inject an isolated root without changing
    /// process-wide environment variables.
    #[doc(hidden)]
    pub fn with_worktree_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.worktree_root_override = Some(path.into());
        self
    }

    pub(crate) fn host_operation_database_path(&self) -> Result<PathBuf, BrokerOpError> {
        self.host_operation_db_path
            .clone()
            .map(Ok)
            .unwrap_or_else(|| crate::default_host_operation_db_path().map_err(Into::into))
    }

    pub fn store(&mut self) -> &mut BrokerStore {
        &mut self.store
    }

    pub(crate) fn store_ref(&self) -> &BrokerStore {
        &self.store
    }

    /// Build a bounded, allowlist-only lease routing snapshot without
    /// refreshing leases, events, sessions, or command telemetry.
    pub fn export_lease_routing(
        &self,
        options: crate::LeaseRoutingExportOptions,
        source_time: i64,
    ) -> Result<crate::LeaseRoutingExport, crate::LeaseRoutingExportError> {
        crate::lease_export::build_lease_routing_export(
            &self.repo,
            &self.store,
            options,
            source_time,
        )
    }

    /// Persist an immutable non-blocking advisory and refresh the generated
    /// outstanding-advisory projection from authoritative database state.
    pub fn persist_advisory(
        &mut self,
        mut advisory: NewAdvisory,
    ) -> Result<Advisory, BrokerOpError> {
        let identity = advisory.identity.trim();
        if identity.is_empty() || identity.len() > 256 {
            return Err(BrokerOpError::InvalidAdvisory {
                reason: "identity must contain 1..=256 bytes".into(),
            });
        }
        advisory.identity = identity.to_string();
        if advisory.paths.len() > 100 || advisory.evidence.len() > 100 {
            return Err(BrokerOpError::InvalidAdvisory {
                reason: "paths and evidence are each limited to 100 entries".into(),
            });
        }
        advisory.paths = advisory
            .paths
            .iter()
            .map(|path| {
                if path.len() > 4_096 {
                    return Err(BrokerOpError::InvalidAdvisory {
                        reason: "each advisory path is limited to 4096 bytes".into(),
                    });
                }
                normalize_lease_path(path)
            })
            .collect::<Result<Vec<_>, _>>()?;
        advisory.paths.sort();
        advisory.paths.dedup();
        for evidence in &advisory.evidence {
            if evidence.kind.trim().is_empty()
                || evidence.kind.len() > 128
                || evidence.summary.trim().is_empty()
                || evidence.summary.len() > 4_096
            {
                return Err(BrokerOpError::InvalidAdvisory {
                    reason: "evidence kind must contain 1..=128 bytes and summary 1..=4096 bytes"
                        .into(),
                });
            }
        }
        if let Some(sha) = &advisory.integration_sha
            && (sha.len() != 40 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(BrokerOpError::InvalidAdvisory {
                reason: "integration SHA must be a full 40-character hexadecimal object id".into(),
            });
        }
        if let Some(session_id) = advisory.session_id {
            self.store.session(session_id)?;
        }
        if let Some(queue_entry_id) = advisory.queue_entry_id {
            let queue = self.store.merge_queue()?;
            queue
                .iter()
                .find(|entry| entry.id == queue_entry_id)
                .ok_or_else(|| BrokerOpError::InvalidAdvisory {
                    reason: format!("queue entry {queue_entry_id} does not exist"),
                })?;
        }
        let stored = self.store.record_advisory(&advisory)?;
        self.refresh_advisory_projection()?;
        Ok(stored)
    }

    pub fn advisories(&self, include_all: bool) -> Result<Vec<Advisory>, BrokerOpError> {
        Ok(self.store.advisories(include_all)?)
    }

    /// Derive current maintainer recommendations from a bounded, redacted
    /// history window without mutating advisory or repository state.
    pub fn maintainer_recommendations(
        &self,
    ) -> Result<Vec<crate::MaintainerRecommendation>, BrokerOpError> {
        Ok(crate::recommendations::recommendations_from_store(
            &self.store,
        )?)
    }

    /// Synchronize the explicit maintainer advisory inventory from bounded
    /// history. This is used only by deliberate advisory commands; readiness
    /// and gate doctor keep using the read-only projection above.
    pub fn refresh_maintainer_recommendations(&mut self) -> Result<(), BrokerOpError> {
        let snapshot = crate::recommendations::recommendation_snapshot_from_store(&self.store)?;
        Ok(self.store.sync_maintainer_recommendations(&snapshot)?)
    }

    pub fn advisory_list(&self, include_acknowledged: bool) -> Result<AdvisoryList, BrokerOpError> {
        let advisories = self.store.advisories(include_acknowledged)?;
        let outstanding_count = advisories
            .iter()
            .filter(|advisory| {
                advisory.resolution_state == crate::AdvisoryResolutionState::Outstanding
            })
            .count();
        Ok(AdvisoryList {
            advisories,
            outstanding_count,
            includes_acknowledged: include_acknowledged,
        })
    }

    pub fn advisory(&self, id: i64) -> Result<Advisory, BrokerOpError> {
        self.store
            .advisory(id)?
            .ok_or_else(|| BrokerError::AdvisoryNotFound(id).into())
    }

    pub fn record_advisories_shown(
        &mut self,
        advisories: &[Advisory],
        surface: crate::AdvisoryDeliverySurface,
    ) -> Result<(), BrokerOpError> {
        Ok(self.store.record_advisories_shown(advisories, surface)?)
    }

    pub fn advisory_delivery_metrics(
        &self,
    ) -> Result<Vec<crate::AdvisoryDeliveryMetric>, BrokerOpError> {
        Ok(self.store.advisory_delivery_metrics()?)
    }

    pub fn advisory_delivery_summary(
        &self,
    ) -> Result<crate::AdvisoryDeliverySummary, BrokerOpError> {
        Ok(self.store.advisory_delivery_summary()?)
    }

    pub fn acknowledge_advisory(&mut self, id: i64) -> Result<Advisory, BrokerOpError> {
        let advisory = self.store.acknowledge_advisory(id)?;
        self.refresh_advisory_projection()?;
        Ok(advisory)
    }

    pub fn suppress_maintainer_advisory(&mut self, id: i64) -> Result<Advisory, BrokerOpError> {
        Ok(self.store.suppress_maintainer_advisory(id)?)
    }

    pub fn send_session_note(
        &mut self,
        sender_session_id: i64,
        recipient_session_id: i64,
        message: &str,
    ) -> Result<SessionNote, BrokerOpError> {
        if sender_session_id == recipient_session_id {
            return Err(BrokerOpError::InvalidSessionNote {
                reason: "sender and recipient must be different live sessions".into(),
            });
        }
        let message = message.trim();
        if message.is_empty() {
            return Err(BrokerOpError::InvalidSessionNote {
                reason: "message must not be empty".into(),
            });
        }
        if message.len() > SESSION_NOTE_MAX_BYTES {
            return Err(BrokerOpError::InvalidSessionNote {
                reason: format!(
                    "message is {} bytes; maximum is {SESSION_NOTE_MAX_BYTES}",
                    message.len()
                ),
            });
        }
        if message.chars().any(char::is_control) {
            return Err(BrokerOpError::InvalidSessionNote {
                reason: "message must be one line of plain text without control characters".into(),
            });
        }
        for (role, id) in [
            ("sender", sender_session_id),
            ("recipient", recipient_session_id),
        ] {
            let session = self.store.session(id)?;
            if session.status.is_closed() {
                return Err(BrokerOpError::InvalidSessionNote {
                    reason: format!("{role} session {id} is closed"),
                });
            }
        }
        Ok(self
            .store
            .record_session_note(sender_session_id, recipient_session_id, message)?)
    }

    pub fn session_note_list(
        &self,
        recipient_session_id: i64,
    ) -> Result<SessionNoteList, BrokerOpError> {
        self.store.session(recipient_session_id)?;
        let notes = self.store.session_notes(recipient_session_id)?;
        let unread_count = notes
            .iter()
            .filter(|note| note.acknowledged_at.is_none())
            .count();
        Ok(SessionNoteList {
            notes,
            unread_count,
        })
    }

    pub fn acknowledge_session_note(
        &mut self,
        recipient_session_id: i64,
        note_id: i64,
    ) -> Result<SessionNote, BrokerOpError> {
        let recipient = self.store.session(recipient_session_id)?;
        if recipient.status.is_closed() {
            return Err(BrokerOpError::InvalidSessionNote {
                reason: format!("recipient session {recipient_session_id} is closed"),
            });
        }
        let note = self
            .store
            .session_note(note_id)?
            .ok_or(BrokerError::SessionNoteNotFound(note_id))?;
        if note.recipient_session_id != recipient_session_id {
            return Err(BrokerOpError::SessionNoteRecipientMismatch {
                session_id: recipient_session_id,
                note_id,
                recipient_session_id: note.recipient_session_id,
            });
        }
        Ok(self.store.acknowledge_session_note(note_id)?)
    }

    pub fn refresh_advisory_projection(&mut self) -> Result<PathBuf, BrokerOpError> {
        let main_root = self.main_root.clone();
        crate::advisories::project(&main_root, || {
            Ok(self
                .store
                .advisories(false)?
                .into_iter()
                .filter(|advisory| advisory.audience == crate::AdvisoryAudience::Session)
                .collect())
        })
    }

    pub(crate) fn main_root_path(&self) -> PathBuf {
        self.main_root.clone()
    }

    pub(crate) fn repo_handle(&self) -> &GitRepo {
        &self.repo
    }

    pub(super) fn capture_repository_contract(
        &self,
        checkout_root: &Path,
        backfilled: bool,
    ) -> Result<crate::RepositoryContract, BrokerOpError> {
        // Canonical deployment files travel with each worktree. Local-only
        // artifacts are intentionally untracked and belong to the clone's
        // primary checkout, so a spawned linked worktree cannot contain them.
        let contract_root = if crate::detect_repository_mode(&self.main_root)
            == crate::RepositoryDeploymentMode::LocalOnly
        {
            self.main_root.as_path()
        } else {
            checkout_root
        };
        crate::RepositoryContract::capture(contract_root, backfilled).map_err(|reason| {
            BrokerOpError::RepositoryContract {
                path: contract_root.display().to_string(),
                reason,
            }
        })
    }

    pub(super) fn backfill_live_repository_contracts(&mut self) -> Result<(), BrokerOpError> {
        for session in self.store.live_sessions()? {
            if session.repository_contract.is_some() {
                continue;
            }
            let recorded_worktree = PathBuf::from(&session.worktree_path);
            let checkout_root = if recorded_worktree.is_dir() {
                recorded_worktree.as_path()
            } else {
                self.main_root.as_path()
            };
            let contract = self.capture_repository_contract(checkout_root, true)?;
            self.store
                .backfill_session_repository_contract(session.id, &contract)?;
        }
        Ok(())
    }

    pub fn pr_check(
        &mut self,
        options: crate::PrCheckOptions,
    ) -> Result<crate::PrCheckReport, BrokerOpError> {
        crate::pr::check_pr_followup(self, options)
    }

    // ── adopt (attach-first) ──────────────────────────────────────────

    /// Register an existing worktree the user already launched an agent
    /// in. `worktree` may be any path inside it.
    pub fn adopt(&mut self, worktree: &Path, task: Option<&str>) -> Result<Session, BrokerOpError> {
        Ok(self
            .adopt_with(worktree, task, AdoptMode::New, None)?
            .session)
    }

    /// `adopt` with an explicit policy for the "this worktree already has
    /// a session" case (dogfood feedback 2026-07-14: the bare constraint
    /// error left no obvious follow-up path).
    ///
    /// `agent_identity` is the `Name <email>` the session's agent is
    /// credited under on the promote commit; `None` leaves it unknown.
    pub fn adopt_with(
        &mut self,
        worktree: &Path,
        task: Option<&str>,
        mode: AdoptMode,
        agent_identity: Option<&str>,
    ) -> Result<AdoptReport, BrokerOpError> {
        self.adopt_with_options(worktree, task, AdoptOptions::new(mode), agent_identity)
    }

    pub fn adopt_with_options(
        &mut self,
        worktree: &Path,
        task: Option<&str>,
        options: AdoptOptions,
        agent_identity: Option<&str>,
    ) -> Result<AdoptReport, BrokerOpError> {
        self.adopt_with_options_and_context(
            worktree,
            task,
            options,
            agent_identity,
            SessionContext::default(),
        )
    }

    pub fn adopt_with_options_and_context(
        &mut self,
        worktree: &Path,
        task: Option<&str>,
        options: AdoptOptions,
        agent_identity: Option<&str>,
        context: SessionContext,
    ) -> Result<AdoptReport, BrokerOpError> {
        let mut report = self.adopt_with_options_and_context_unrefreshed(
            worktree,
            task,
            options,
            agent_identity,
            context,
        )?;
        // Refresh the default branch and say how far this checkout drifted
        // while it sat unused -- the comparison `push` makes (#462).
        let (drift, note) = self.reused_session_drift(&report.session);
        report.default_branch = drift;
        report.default_branch_note = note;
        Ok(report)
    }

    pub(super) fn adopt_with_options_and_context_unrefreshed(
        &mut self,
        worktree: &Path,
        task: Option<&str>,
        options: AdoptOptions,
        agent_identity: Option<&str>,
        context: SessionContext,
    ) -> Result<AdoptReport, BrokerOpError> {
        let context = self.session_context(context);
        if options.sync_integration && options.mode != AdoptMode::Reuse {
            return Err(BrokerOpError::ReuseSyncRequiresReuse);
        }
        let checkout = GitRepo::discover(worktree)?;
        let branch = checkout.current_branch()?;
        let worktree_path = checkout.root().to_string_lossy().into_owned();
        let existing = self.store.session_for_worktree(&worktree_path)?;
        if options.mode == AdoptMode::New
            && let Some(existing) = &existing
        {
            return Err(BrokerOpError::SessionExistsForWorktree {
                id: existing.id,
                status: existing.status.as_str(),
                task: existing
                    .task
                    .as_deref()
                    .map(|task| format!(", task: {task:?}"))
                    .unwrap_or_default(),
            });
        }
        let planned_paths = normalize_planned_paths(&options.planned_paths)?;
        let plan_owner = existing.as_ref().and_then(|session| match options.mode {
            AdoptMode::Reuse | AdoptMode::ReplaceStale => Some(session.id),
            AdoptMode::New => None,
        });
        self.ensure_planned_paths_available(&planned_paths, plan_owner)?;
        let integration_sync = if options.sync_integration {
            Some(self.synchronize_reuse_checkout(&checkout)?)
        } else {
            None
        };
        let diff_base = checkout.head_commit().ok();
        let repository_contract = self.capture_repository_contract(checkout.root(), false)?;
        let foreign_files = checkout.untracked_paths()?;
        let mut outcome = AdoptOutcome::Created;
        let mut replaced_session_id = None;

        if let Some(existing) = existing {
            match options.mode {
                AdoptMode::Reuse => {
                    // A live session's baseline is its durable ownership
                    // boundary. A plain reuse may update task text and
                    // liveness, but must never move that boundary across
                    // pending commits. Explicit fast-forward synchronization
                    // is the one safe refresh: it already proved the checkout
                    // has no unique work before moving it to integration.
                    let refreshed_base = integration_sync
                        .as_ref()
                        .map(|sync| sync.after_head.as_str());
                    let session = self.store.reuse_session_with_context_and_leases(
                        LeaseRefusalPolicy::verify_only_at(&self.main_root),
                        existing.id,
                        task,
                        refreshed_base,
                        agent_identity,
                        &context,
                        &planned_paths,
                    )?;
                    self.store
                        .set_session_foreign_files(session.id, &foreign_files)?;
                    let integration_drift =
                        Some(self.adopt_integration_drift(&checkout, session.id, true)?);
                    let planned_explicit_leases =
                        self.planned_explicit_leases(session.id, &planned_paths)?;
                    let preparation = self.preparation_status(session.id)?;
                    return Ok(AdoptReport {
                        session,
                        outcome: AdoptOutcome::Reused,
                        integration_drift,
                        renamed_targets: Vec::new(),
                        integration_sync,
                        planned_explicit_leases,
                        preparation,
                        default_branch: None,
                        default_branch_note: None,
                        carried_ownership: None,
                    });
                }
                AdoptMode::ReplaceStale => {
                    outcome = AdoptOutcome::Replaced;
                    replaced_session_id = Some(existing.id);
                }
                AdoptMode::New => {
                    return Err(BrokerOpError::SessionExistsForWorktree {
                        id: existing.id,
                        status: existing.status.as_str(),
                        task: existing
                            .task
                            .as_deref()
                            .map(|t| format!(", task: {t:?}"))
                            .unwrap_or_default(),
                    });
                }
            }
        }

        // Issue #294: the same protection reuse gives a live session's
        // baseline, for the session this adoption succeeds. Without it a
        // close followed by adopt silently disowned every unsubmitted commit.
        let predecessor = match replaced_session_id {
            Some(id) => Some(self.store.session(id)?),
            None => self.store.closed_session_for_worktree(&worktree_path)?,
        };
        let carried_ownership = match (predecessor.as_ref(), diff_base.as_deref()) {
            (Some(previous), Some(head)) if previous.branch == branch => {
                self.carried_ownership(previous, head)
            }
            _ => None,
        };
        let ownership_base = carried_ownership
            .as_ref()
            .map(|carried| carried.baseline.clone())
            .or_else(|| diff_base.clone());
        let new_session = NewSession {
            worktree_path,
            branch,
            origin: SessionOrigin::Adopted,
            task: task.map(str::to_string),
            adoption_base: ownership_base.clone(),
            adopted_head: diff_base.clone(),
            diff_base: ownership_base,
            repository_contract: Some(repository_contract),
            pid: None,
            command: None,
            log_path: None,
            agent_identity: agent_identity.map(str::to_string),
        };
        let session = if let Some(replaced_session_id) = replaced_session_id {
            self.store.replace_session_with_context_and_leases(
                LeaseRefusalPolicy::verify_only_at(&self.main_root),
                replaced_session_id,
                &new_session,
                &context,
                &planned_paths,
            )?
        } else {
            self.store.register_session_with_context_and_leases(
                LeaseRefusalPolicy::verify_only_at(&self.main_root),
                &new_session,
                &context,
                &planned_paths,
            )?
        };
        self.store
            .set_session_foreign_files(session.id, &foreign_files)?;
        // Computed for every adopt, not only reuse. Adopting a worktree that
        // already has commits records the current HEAD as the baseline, so those
        // commits are not session-owned and submit will not replay them. Saying
        // nothing here is what turns that into a JSON archaeology exercise at
        // submit time (issue #144).
        let integration_drift = Some(self.adopt_integration_drift(
            &checkout,
            session.id,
            options.mode == AdoptMode::Reuse,
        )?);
        let planned_explicit_leases = self.planned_explicit_leases(session.id, &planned_paths)?;
        let preparation = self.preparation_status(session.id)?;
        // Reported here because this is before a replay is attempted; failing
        // later looks like a plain modify/delete conflict (issue #145).
        let renamed_targets = self.session_renamed_targets(session.id).unwrap_or_default();
        Ok(AdoptReport {
            session,
            outcome,
            integration_drift,
            integration_sync,
            planned_explicit_leases,
            preparation,
            renamed_targets,
            default_branch: None,
            default_branch_note: None,
            carried_ownership,
        })
    }

    /// The previous session's ownership boundary, when it still owns commits
    /// at `head` that integration does not have yet. `None` leaves the new
    /// session's baseline at `head`, as before.
    pub(super) fn carried_ownership(
        &self,
        previous: &Session,
        head: &str,
    ) -> Option<AdoptCarriedOwnership> {
        let (_, integration_head) = self.integration_head_snapshot().ok()?;
        let plan = self
            .build_submission_plan(previous, head, &integration_head)
            .ok()?;
        let pending_owned_commits = plan.pending_owned_commit_ids().len();
        if pending_owned_commits == 0 {
            return None;
        }
        Some(AdoptCarriedOwnership {
            from_session: previous.id,
            baseline: plan.recorded_baseline?,
            pending_owned_commits,
        })
    }

    pub(super) fn synchronize_reuse_checkout(
        &mut self,
        checkout: &GitRepo,
    ) -> Result<AdoptIntegrationSync, BrokerOpError> {
        let dirty_paths = checkout.dirty_paths()?;
        if !dirty_paths.is_empty() {
            return Err(BrokerOpError::ReuseSyncDirty { paths: dirty_paths });
        }

        let before_head = checkout.head_commit()?;
        let (integration_branch, integration_head) = self.integration_head()?;
        let outcome = if before_head == integration_head {
            AdoptIntegrationSyncOutcome::AlreadyCurrent
        } else {
            if !checkout.is_ancestor(&before_head, &integration_head) {
                let ahead = checkout.commit_count_between(&integration_head, &before_head)?;
                let behind = checkout.commit_count_between(&before_head, &integration_head)?;
                let relation = match (ahead, behind) {
                    (_, 0) => AdoptIntegrationRelation::Ahead,
                    _ => AdoptIntegrationRelation::Diverged,
                };
                return Err(BrokerOpError::ReuseSyncNotFastForward {
                    session_head: before_head,
                    integration_head,
                    relation: relation.as_str(),
                });
            }
            checkout.fast_forward_checkout(&integration_head)?;
            AdoptIntegrationSyncOutcome::FastForwarded
        };

        let after_head = checkout.head_commit()?;
        if after_head != integration_head {
            return Err(BrokerOpError::ReuseSyncVerification {
                expected: integration_head,
                actual: after_head,
            });
        }
        Ok(AdoptIntegrationSync {
            outcome,
            integration_branch,
            integration_head,
            before_head,
            after_head,
        })
    }

    /// `may_create_integration` keeps reuse behaviour unchanged while letting a
    /// plain adopt report drift without bringing the integration branch into
    /// existence as a side effect of describing state (issue #144).
    pub(super) fn adopt_integration_drift(
        &mut self,
        checkout: &GitRepo,
        session_id: i64,
        may_create_integration: bool,
    ) -> Result<AdoptIntegrationDrift, BrokerOpError> {
        let session_head = checkout.head_commit()?;
        let (integration_branch, integration_head) = if may_create_integration {
            self.integration_head()?
        } else {
            self.integration_head_snapshot()?
        };
        let ahead_commits = checkout.commit_count_between(&integration_head, &session_head)?;
        let behind_commits = checkout.commit_count_between(&session_head, &integration_head)?;
        let relation = match (ahead_commits, behind_commits) {
            (0, 0) => AdoptIntegrationRelation::Current,
            (0, _) => AdoptIntegrationRelation::Behind,
            (_, 0) => AdoptIntegrationRelation::Ahead,
            _ => AdoptIntegrationRelation::Diverged,
        };

        let overlapping_changed_paths = checkout
            .merge_base(&session_head, &integration_head)
            .ok()
            .map(|base| -> Result<Vec<String>, BrokerOpError> {
                let session_paths = checkout.changed_files(&base)?;
                let integration_paths = checkout.changed_between(&base, &integration_head)?;
                let mut overlaps = session_paths
                    .into_iter()
                    .filter(|session_path| {
                        integration_paths.iter().any(|integration_path| {
                            crate::leases::paths_overlap(session_path, integration_path)
                        })
                    })
                    .collect::<Vec<_>>();
                overlaps.sort();
                overlaps.dedup();
                Ok(overlaps)
            })
            .transpose()?
            .unwrap_or_default();

        let submission_plan = self.store.session(session_id).ok().and_then(|session| {
            self.build_submission_plan(&session, &session_head, &integration_head)
                .ok()
        });
        let pending_owned_commits = submission_plan
            .as_ref()
            .map(|plan| plan.pending_owned_commit_ids().len());
        let submission_plan_safe = submission_plan.as_ref().is_some_and(|plan| plan.safe);

        let (warning, safe_next_action) = match relation {
            AdoptIntegrationRelation::Current => (
                None,
                format!("continue with session {session_id} on the current integration baseline"),
            ),
            AdoptIntegrationRelation::Behind => (
                Some(format!(
                    "session HEAD is {behind_commits} commit(s) behind {integration_branch}; inspect drift before editing"
                )),
                "aethyme broker advanced integration status".into(),
            ),
            AdoptIntegrationRelation::Ahead
                if submission_plan_safe && pending_owned_commits.is_some_and(|count| count > 0) =>
            {
                (
                    Some(format!(
                        "session HEAD is {ahead_commits} commit(s) ahead of {integration_branch}; {pending} pending session-owned commit(s) are safe to submit before starting a follow-up",
                        pending = pending_owned_commits.unwrap_or_default()
                    )),
                    format!("aethyme broker submit --session {session_id}"),
                )
            }
            AdoptIntegrationRelation::Ahead => (
                Some(format!(
                    "session HEAD is {ahead_commits} commit(s) ahead of {integration_branch}, but none are session-owned under the recorded baseline, so submit will not replay them; they predate this adoption. To submit them, re-adopt from a base that precedes them"
                )),
                "aethyme broker advanced integration status".into(),
            ),
            AdoptIntegrationRelation::Diverged => (
                Some(format!(
                    "session HEAD and {integration_branch} have diverged ({ahead_commits} ahead, {behind_commits} behind); reconcile before editing"
                )),
                "aethyme broker advanced integration status".into(),
            ),
        };

        Ok(AdoptIntegrationDrift {
            session_head,
            integration_branch,
            integration_head,
            relation,
            ahead_commits,
            behind_commits,
            overlapping_changed_paths,
            warning,
            safe_next_action,
        })
    }

    /// Close broker state without removing the worktree. Policy-eligible,
    /// ignored build artifacts may be reclaimed while the checkout is retained.
    pub fn close(&mut self, session_id: i64) -> Result<(), BrokerOpError> {
        let session = self.store.session(session_id)?;
        if let Some((head, unpushed_commits)) = self.unpushed_close_check(&session) {
            return Err(BrokerOpError::UnpushedSessionWork {
                session_id,
                branch: session.branch,
                head,
                unpushed_commits,
            });
        }
        self.close_unchecked(session_id)
    }

    /// [`Broker::close`] for a session whose unpushed commits the caller has
    /// decided to leave behind. The decision and its reason are recorded, so
    /// the work is abandoned on the record rather than silently.
    pub fn close_abandoning_unpushed(
        &mut self,
        session_id: i64,
        reason: &str,
    ) -> Result<(), BrokerOpError> {
        let session = self.store.session(session_id)?;
        if let Some((head, unpushed_commits)) = self.unpushed_close_check(&session) {
            self.record_abandoned_unpushed(&session, &head, unpushed_commits, reason)?;
        }
        self.close_unchecked(session_id)
    }

    /// Commits only this session's worktree holds, when the repository opted
    /// into the push lane and has a remote to push to; `None` otherwise.
    pub(super) fn unpushed_close_check(&self, session: &Session) -> Option<(String, u32)> {
        if !crate::session_push::session_push_enabled(&self.repo)
            || self.repo.remotes().unwrap_or_default().is_empty()
        {
            return None;
        }
        let upstream = self
            .publication_baseline()
            .ok()
            .map(|(reference, _)| reference)
            .filter(|reference| reference.starts_with("refs/remotes/"));
        let integration_head = self.integration_head_snapshot().ok().map(|(_, head)| head);
        let (head, work) =
            session_off_remote_work(session, upstream.as_deref(), integration_head.as_deref())?;
        (work.commits > 0).then_some((head, work.commits))
    }

    pub(super) fn record_abandoned_unpushed(
        &mut self,
        session: &Session,
        head: &str,
        unpushed_commits: u32,
        reason: &str,
    ) -> Result<(), BrokerOpError> {
        let payload = crate::events::session_abandoned_unpushed_payload(
            session.id,
            &session.branch,
            head,
            unpushed_commits,
            reason,
        );
        self.store.append_event(
            crate::events::BROKER_SESSION_ABANDONED_UNPUSHED,
            Some(session.id),
            Some(&payload),
        )?;
        Ok(())
    }

    pub(super) fn close_unchecked(&mut self, session_id: i64) -> Result<(), BrokerOpError> {
        self.store
            .set_session_status(session_id, SessionStatus::Closed, None)?;
        // Closing must not fail because a best-effort artifact sweep did;
        // report the failure instead of discarding it.
        crate::warn_unrecorded(
            "reclaim a closed session's build artifacts",
            self.reclaim_closed_session_artifacts(session_id),
        );
        Ok(())
    }

    /// Ordinary starts are intentionally anchored to integration. A caller
    /// that explicitly identifies a pull request must use the routed review
    /// adapter, which provisions and verifies that pull request's exact head.
    pub(crate) fn reject_integration_based_review_task(
        pull_request: Option<i64>,
    ) -> Result<(), BrokerOpError> {
        if let Some(pull_request) = pull_request {
            return Err(BrokerOpError::ReviewRequiresPullRequestHead { pull_request });
        }
        Ok(())
    }

    // ── start-agent (spawn convenience) ───────────────────────────────

    /// Create a broker-owned worktree + branch for `task` without
    /// spawning a process. This is the preferred entrypoint for agents
    /// already running in an existing shell: the caller can `cd` into the
    /// returned path and continue with an isolated index and checkout.
    /// The caller here IS the agent that will work the worktree -- unlike
    /// [`Broker::start_agent`], no separate process is involved -- so
    /// `agent_identity` is its own identity for promote-commit credit.
    pub fn start_worktree(
        &mut self,
        task: &str,
        agent_identity: Option<&str>,
    ) -> Result<Session, BrokerOpError> {
        Ok(self
            .start_worktree_with_planned_paths(task, &[], agent_identity)?
            .session)
    }

    pub fn start_worktree_with_planned_paths(
        &mut self,
        task: &str,
        paths: &[String],
        agent_identity: Option<&str>,
    ) -> Result<StartReport, BrokerOpError> {
        self.start_worktree_with_planned_paths_and_context(
            task,
            paths,
            agent_identity,
            SessionContext::default(),
            None,
        )
    }

    pub fn start_worktree_with_planned_paths_and_context(
        &mut self,
        task: &str,
        paths: &[String],
        agent_identity: Option<&str>,
        context: SessionContext,
        explicit_base: Option<&str>,
    ) -> Result<StartReport, BrokerOpError> {
        let context = self.session_context(context);
        let planned_paths = normalize_planned_paths(paths)?;
        self.ensure_planned_paths_available(&planned_paths, None)?;
        let (_slug, branch, start_base, worktree, worktree_placement) =
            self.create_session_worktree(task, explicit_base)?;
        let base = start_base.commit.clone();
        let repository_contract = self.capture_repository_contract(worktree.root(), false)?;
        let new_session = NewSession {
            worktree_path: worktree.root().to_string_lossy().into_owned(),
            branch: branch.clone(),
            origin: SessionOrigin::Spawned,
            task: Some(task.to_string()),
            adoption_base: Some(base.clone()),
            adopted_head: Some(base.clone()),
            diff_base: Some(base.clone()),
            repository_contract: Some(repository_contract),
            pid: None,
            command: None,
            log_path: None,
            agent_identity: agent_identity.map(str::to_string),
        };
        let session = match self.store.register_session_with_context_and_leases(
            LeaseRefusalPolicy::verify_only_at(&self.main_root),
            &new_session,
            &context,
            &planned_paths,
        ) {
            Ok(session) => session,
            Err(error) => {
                let worktree_path = worktree.root().to_path_buf();
                let _ = self.repo.worktree_remove(&worktree_path, true);
                let _ = self.repo.delete_branch_ref_checked(&branch, &base);
                return Err(error.into());
            }
        };
        self.store.set_session_foreign_files(session.id, &[])?;
        let planned_explicit_leases = self.planned_explicit_leases(session.id, &planned_paths)?;
        let preparation = self.preparation_status(session.id)?;
        Ok(StartReport {
            session,
            start_base,
            worktree_placement,
            planned_explicit_leases,
            preparation,
        })
    }

    /// Create a worktree + branch for `task` and spawn `command` in it via
    /// `sh -c` with stdout/stderr teed to a log file. Returns the session;
    /// the child runs detached (the broker never owns the process beyond
    /// recording its PID).
    pub fn start_agent(
        &mut self,
        task: &str,
        command: &str,
        agent_identity: Option<&str>,
    ) -> Result<Session, BrokerOpError> {
        Ok(self
            .start_agent_report(task, command, agent_identity)?
            .session)
    }

    pub fn start_agent_report(
        &mut self,
        task: &str,
        command: &str,
        agent_identity: Option<&str>,
    ) -> Result<StartAgentReport, BrokerOpError> {
        self.start_agent_report_with_context(
            task,
            command,
            agent_identity,
            SessionContext::default(),
            None,
        )
    }

    pub fn start_agent_report_with_context(
        &mut self,
        task: &str,
        command: &str,
        agent_identity: Option<&str>,
        context: SessionContext,
        explicit_base: Option<&str>,
    ) -> Result<StartAgentReport, BrokerOpError> {
        let context = self.session_context(context);
        let (slug, branch, start_base, worktree, worktree_placement) =
            self.create_session_worktree(task, explicit_base)?;
        let base = start_base.commit.clone();
        let repository_contract = self.capture_repository_contract(worktree.root(), false)?;

        let log_dir = self.main_root.join(".aethyme/logs");
        std::fs::create_dir_all(&log_dir).map_err(|source| BrokerError::Io {
            path: log_dir.clone(),
            source,
        })?;
        let log_path = log_dir.join(format!("{slug}.log"));
        let log_file = std::fs::File::create(&log_path).map_err(|source| BrokerError::Io {
            path: log_path.clone(),
            source,
        })?;
        let log_clone = log_file.try_clone().map_err(|source| BrokerError::Io {
            path: log_path.clone(),
            source,
        })?;

        let child = Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(worktree.root())
            .stdin(Stdio::null())
            .stdout(Stdio::from(log_file))
            .stderr(Stdio::from(log_clone))
            .spawn()
            .map_err(|source| BrokerOpError::Spawn {
                command: command.to_string(),
                source,
            })?;

        let session = self.store.register_session_with_context_and_leases(
            LeaseRefusalPolicy::verify_only_at(&self.main_root),
            &NewSession {
                worktree_path: worktree.root().to_string_lossy().into_owned(),
                branch,
                origin: SessionOrigin::Spawned,
                task: Some(task.to_string()),
                adoption_base: Some(base.clone()),
                adopted_head: Some(base.clone()),
                diff_base: Some(base),
                repository_contract: Some(repository_contract),
                pid: Some(child.id() as i64),
                command: Some(command.to_string()),
                log_path: Some(log_path.to_string_lossy().into_owned()),
                agent_identity: agent_identity.map(str::to_string),
            },
            &context,
            &[],
        )?;
        self.store.set_session_foreign_files(session.id, &[])?;
        Ok(StartAgentReport {
            session,
            start_base,
            worktree_placement,
        })
    }

    pub(super) fn create_session_worktree(
        &mut self,
        task: &str,
        explicit_base: Option<&str>,
    ) -> Result<(String, String, SessionStartBase, GitRepo, WorktreePlacement), BrokerOpError> {
        let placement = self.prepare_broker_worktree_root()?;
        let slug = self.next_worktree_slug(task, &placement.root);
        let worktree_path = placement.root.join(&slug);
        self.refuse_nested_worktree_path(&worktree_path)?;
        let branch = format!("agent/{slug}");
        let refresh = self.refresh_default_branch_before_start();
        // Verify-only sessions start from the fetched default branch either
        // way; advancing a disposable integration here keeps it from feeding
        // a stale copy of main to this session's submit and finish (#352).
        self.refresh_disposable_integration(crate::IntegrationRefreshTrigger::Start);
        let mut start_base = self.select_session_start_base(explicit_base)?;
        refresh.record(&mut start_base);
        let worktree = self
            .repo
            .worktree_add(&worktree_path, &branch, &start_base.commit)?;
        if placement.source == WorktreeRootSource::RepositoryConfig {
            // A failed lock must not fail the start, but it leaves the
            // registration unprotected while the drive is unplugged, so say so.
            crate::warn_unrecorded(
                "lock the worktree on the configured root against pruning",
                self.repo.worktree_lock(&worktree_path),
            );
        }
        Ok((slug, branch, start_base, worktree, placement))
    }

    pub fn worktree_root_plan(&self) -> Result<WorktreeRootPlan, BrokerOpError> {
        let repository_key = self.repository_worktree_key()?;
        let host_state_container = crate::host_state::default_host_state_dir()
            .filter(|_| {
                // Only the implicit platform default is withheld from ephemeral
                // repositories; an explicitly named host state directory is a
                // deliberate choice and is always honoured.
                crate::host_state::host_state_dir_is_explicit()
                    || !crate::host_state::path_is_ephemeral(&self.main_root)
            })
            .map(|base| base.join("worktrees"));
        // The configured root is read only when nothing more specific was
        // asked for. An invalid section must not stop a session from
        // starting: it is ignored, and the reason is reported.
        let mut preferred_unavailable_reason = None;
        let configured_root =
            match crate::worktree_location::WorktreeLocationConfig::load(&self.main_root) {
                Ok(config) => {
                    config.and_then(|config| config.root.clone().map(|root| (root, config)))
                }
                Err(error) => {
                    preferred_unavailable_reason =
                        Some(format!("[worktrees] configuration ignored: {error}"));
                    None
                }
            };
        let mut host_state_fallback_root = None;
        let (preferred_root, preferred_source, root_container) =
            if let Some(root) = &self.worktree_root_override {
                (
                    Some(self.absolute_worktree_root(root)),
                    Some(WorktreeRootSource::LibraryOverride),
                    None,
                )
            } else if let Some(base) =
                std::env::var_os("AETHYME_WORKTREE_ROOT").filter(|value| !value.is_empty())
            {
                let container = self.absolute_worktree_root(&PathBuf::from(base));
                (
                    Some(container.join(&repository_key)),
                    Some(WorktreeRootSource::EnvironmentOverride),
                    Some(container),
                )
            } else if let Some((container, settings)) = configured_root {
                host_state_fallback_root = host_state_container
                    .as_ref()
                    .map(|container| container.join(&repository_key));
                preferred_unavailable_reason =
                    self.configured_root_unavailable(&container, settings.min_free_bytes());
                (
                    Some(container.join(&repository_key)),
                    Some(WorktreeRootSource::RepositoryConfig),
                    Some(container),
                )
            } else if let Some(container) = host_state_container {
                (
                    Some(container.join(&repository_key)),
                    Some(WorktreeRootSource::HostState),
                    Some(container),
                )
            } else {
                (None, None, None)
            };
        let preferred_outside_repository = preferred_root
            .as_deref()
            .is_some_and(|root| !self.path_is_inside_repository(root));
        Ok(WorktreeRootPlan {
            schema_version: WORKTREE_ROOT_SCHEMA_VERSION,
            repository_root: self.main_root.clone(),
            repository_key,
            preferred_root,
            preferred_source,
            root_container,
            legacy_fallback_root: self.legacy_broker_worktree_root(),
            preferred_outside_repository,
            host_state_fallback_root,
            preferred_unavailable_reason,
        })
    }

    /// Why the configured worktree root cannot take a new session now.
    ///
    /// The base itself is never created -- only the per-repository directory
    /// beneath it -- so an unplugged drive is reported, not replaced by an
    /// empty directory on the startup disk.
    pub(super) fn configured_root_unavailable(
        &self,
        base: &Path,
        min_free_bytes: u64,
    ) -> Option<String> {
        if let Some(reason) = crate::worktree_location::base_unavailable_reason(base) {
            return Some(reason);
        }
        if self.path_is_inside_repository(base) {
            return Some(format!(
                "{} resolves inside repository {}",
                base.display(),
                self.main_root.display()
            ));
        }
        match crate::disk_headroom::available_bytes_for(&self.main_root, base) {
            Some(free) if free < min_free_bytes => Some(format!(
                "{} has {} free, below the {} required (worktrees.min_free_bytes)",
                base.display(),
                crate::disk_headroom::format_gibibytes(free),
                crate::disk_headroom::format_gibibytes(min_free_bytes)
            )),
            _ => None,
        }
    }

    pub(super) fn prepare_broker_worktree_root(&self) -> Result<WorktreePlacement, BrokerOpError> {
        let plan = self.worktree_root_plan()?;
        if plan.preferred_source == Some(WorktreeRootSource::RepositoryConfig)
            && let Some(root) = &plan.preferred_root
        {
            let reason = match &plan.preferred_unavailable_reason {
                Some(reason) => reason.clone(),
                None => {
                    match self.prepare_worktree_root(
                        root,
                        WorktreeRootSource::RepositoryConfig,
                        true,
                    ) {
                        Ok(root) => {
                            return Ok(WorktreePlacement {
                                root,
                                source: WorktreeRootSource::RepositoryConfig,
                                outside_repository: true,
                                fallback_reason: None,
                            });
                        }
                        Err(error) => error.to_string(),
                    }
                }
            };
            let reason = format!("configured worktree root is unavailable: {reason}");
            if let Some(fallback) = &plan.host_state_fallback_root
                && let Ok(root) =
                    self.prepare_worktree_root(fallback, WorktreeRootSource::HostState, true)
            {
                return Ok(WorktreePlacement {
                    root,
                    source: WorktreeRootSource::HostState,
                    outside_repository: true,
                    fallback_reason: Some(reason),
                });
            }
            let root = self.prepare_worktree_root(
                &plan.legacy_fallback_root,
                WorktreeRootSource::RepositoryFallback,
                false,
            )?;
            return Ok(WorktreePlacement {
                root,
                source: WorktreeRootSource::RepositoryFallback,
                outside_repository: false,
                fallback_reason: Some(reason),
            });
        }
        if let (Some(root), Some(source)) = (&plan.preferred_root, plan.preferred_source) {
            match self.prepare_worktree_root(root, source, true) {
                Ok(root) => {
                    return Ok(WorktreePlacement {
                        root,
                        source,
                        outside_repository: true,
                        // Set only when an invalid `[worktrees]` section was
                        // ignored in favour of this default.
                        fallback_reason: plan.preferred_unavailable_reason.clone(),
                    });
                }
                Err(error)
                    if matches!(
                        source,
                        WorktreeRootSource::EnvironmentOverride
                            | WorktreeRootSource::LibraryOverride
                    ) =>
                {
                    return Err(error);
                }
                Err(error) => {
                    let fallback_reason = error.to_string();
                    let root = self.prepare_worktree_root(
                        &plan.legacy_fallback_root,
                        WorktreeRootSource::RepositoryFallback,
                        false,
                    )?;
                    return Ok(WorktreePlacement {
                        root,
                        source: WorktreeRootSource::RepositoryFallback,
                        outside_repository: false,
                        fallback_reason: Some(fallback_reason),
                    });
                }
            }
        }

        let root = self.prepare_worktree_root(
            &plan.legacy_fallback_root,
            WorktreeRootSource::RepositoryFallback,
            false,
        )?;
        Ok(WorktreePlacement {
            root,
            source: WorktreeRootSource::RepositoryFallback,
            outside_repository: false,
            fallback_reason: Some(
                "no per-user host-state directory is available; set AETHYME_WORKTREE_ROOT to a writable external base"
                    .into(),
            ),
        })
    }

    pub(super) fn prepare_worktree_root(
        &self,
        path: &Path,
        source: WorktreeRootSource,
        require_external: bool,
    ) -> Result<PathBuf, BrokerOpError> {
        std::fs::create_dir_all(path).map_err(|error| BrokerOpError::WorktreeRootUnavailable {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;
        let root = path
            .canonicalize()
            .map_err(|error| BrokerOpError::WorktreeRootUnavailable {
                path: path.to_path_buf(),
                reason: error.to_string(),
            })?;
        if require_external && self.path_is_inside_repository(&root) {
            return Err(BrokerOpError::WorktreeRootUnavailable {
                path: root,
                reason: format!(
                    "{} must resolve outside repository {}; choose an external AETHYME_WORKTREE_ROOT",
                    source.as_str(),
                    self.main_root.display()
                ),
            });
        }
        crate::host_state::protect_host_state_path(&root, true).map_err(|error| {
            BrokerOpError::WorktreeRootUnavailable {
                path: root.clone(),
                reason: format!("cannot apply private directory permissions: {error}"),
            }
        })?;
        self.write_or_verify_worktree_root_marker(&root)?;
        // Best effort on purpose: this is a disk-hygiene default, and failing
        // to start a session over one is a far worse outcome than building a
        // worktree the way cargo would have anyway.
        let _ = write_worktree_build_defaults(&root);
        Ok(root)
    }

    pub(super) fn write_or_verify_worktree_root_marker(
        &self,
        root: &Path,
    ) -> Result<(), BrokerOpError> {
        let marker_path = root.join(WORKTREE_ROOT_MARKER);
        let expected = WorktreeRootMarker {
            schema_version: WORKTREE_ROOT_SCHEMA_VERSION,
            repository_key: self.repository_worktree_key()?,
            repository_root: self.main_root.clone(),
        };
        if marker_path.exists() {
            return self.verify_worktree_root_marker(&marker_path, &expected);
        }
        let mut bytes = serde_json::to_vec_pretty(&expected).map_err(|error| {
            BrokerOpError::WorktreeRootUnavailable {
                path: marker_path.clone(),
                reason: error.to_string(),
            }
        })?;
        bytes.push(b'\n');
        let temporary = root.join(format!(
            ".aethyme-worktree-root.{}.{}.tmp",
            std::process::id(),
            now_ms()
        ));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| BrokerOpError::WorktreeRootUnavailable {
                path: temporary.clone(),
                reason: error.to_string(),
            })?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| BrokerOpError::WorktreeRootUnavailable {
                path: temporary.clone(),
                reason: error.to_string(),
            })?;
        crate::host_state::protect_host_state_path(&temporary, false).map_err(|error| {
            BrokerOpError::WorktreeRootUnavailable {
                path: temporary.clone(),
                reason: format!("cannot apply private marker permissions: {error}"),
            }
        })?;
        match std::fs::rename(&temporary, &marker_path) {
            Ok(()) => Ok(()),
            Err(_error) if marker_path.exists() => {
                let _ = std::fs::remove_file(&temporary);
                self.verify_worktree_root_marker(&marker_path, &expected)
            }
            Err(error) => Err(BrokerOpError::WorktreeRootUnavailable {
                path: marker_path,
                reason: error.to_string(),
            }),
        }
    }

    pub(super) fn verify_worktree_root_marker(
        &self,
        marker_path: &Path,
        expected: &WorktreeRootMarker,
    ) -> Result<(), BrokerOpError> {
        let bytes =
            std::fs::read(marker_path).map_err(|error| BrokerOpError::WorktreeRootUnavailable {
                path: marker_path.to_path_buf(),
                reason: error.to_string(),
            })?;
        let actual: WorktreeRootMarker = serde_json::from_slice(&bytes).map_err(|error| {
            BrokerOpError::WorktreeRootUnavailable {
                path: marker_path.to_path_buf(),
                reason: format!("invalid ownership marker: {error}"),
            }
        })?;
        if &actual != expected {
            return Err(BrokerOpError::WorktreeRootUnavailable {
                path: marker_path.to_path_buf(),
                reason: "ownership marker belongs to a different repository checkout".into(),
            });
        }
        Ok(())
    }

    pub(super) fn repository_worktree_key(&self) -> Result<String, BrokerOpError> {
        Ok(crate::host_state::repository_key(
            &self.main_root,
            Some(&self.main_git_common_dir()?),
        ))
    }

    /// [`GitRepo::git_common_dir`] of the main repository, cached after the
    /// first successful read; it cannot change while this broker is open.
    pub(super) fn main_git_common_dir(&self) -> Result<PathBuf, GitError> {
        if let Some(dir) = self.main_common_dir.get() {
            return Ok(dir.clone());
        }
        let dir = self.repo.git_common_dir()?;
        Ok(self.main_common_dir.get_or_init(|| dir).clone())
    }

    pub(super) fn absolute_worktree_root(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.main_root.join(path)
        }
    }

    pub(super) fn path_is_inside_repository(&self, path: &Path) -> bool {
        let repository = self
            .main_root
            .canonicalize()
            .unwrap_or_else(|_| self.main_root.clone());
        let candidate = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        candidate.starts_with(repository)
    }

    pub(super) fn refuse_nested_worktree_path(&self, path: &Path) -> Result<(), BrokerOpError> {
        for owner in self.repo.worktree_paths()? {
            let owner = owner.canonicalize().unwrap_or(owner);
            if path.starts_with(&owner) {
                return Err(BrokerOpError::NestedWorktreePath {
                    path: path.to_path_buf(),
                    owner,
                });
            }
        }
        Ok(())
    }

    /// A `status` row when a promoting integration branch no longer contains
    /// the fetched default branch's tip. New sessions start from upstream in
    /// that state (see [`Self::select_session_start_base`]), but promotion and
    /// submit still target integration, so it needs reconciling. Silent in
    /// verify-only repositories, where integration is not used as a base.
    pub(super) fn integration_behind_upstream_advice(&self) -> Option<StatusAdvice> {
        let promote = PromoteConfig::load(&self.main_root);
        if promote.mode == crate::merge::PromoteMode::VerifyOnly {
            return None;
        }
        let integration_ref = format!("refs/heads/{}", promote.branch);
        let integration = self.repo.resolve_ref(&integration_ref)?;
        let (upstream_ref, upstream) = self.repo.upstream_default()?;
        if self.repo.is_ancestor(&upstream, &integration) {
            return None;
        }
        let (behind, ahead, _) = self.start_base_drift(&integration);
        let behind = behind.unwrap_or(0);
        let ahead = ahead.unwrap_or(0);
        Some(StatusAdvice {
            id: "integration.behind-upstream",
            severity: StatusAdviceSeverity::Warning,
            reason: "integration no longer contains the default branch",
            summary: format!(
                "{} is {behind} commit(s) behind {upstream_ref} and {ahead} ahead. New \
                 sessions start from {upstream_ref} instead; reconcile integration so \
                 submit and promotion stop targeting an old tree",
                promote.branch
            ),
            session_id: None,
            queue_entry_id: None,
            evidence: vec![
                format!("{} {}", promote.branch, short_commit(&integration)),
                format!("{upstream_ref} {}", short_commit(&upstream)),
            ],
            commands: vec![format!(
                "aethyme broker advanced integration reconcile --upstream {upstream_ref}"
            )],
        })
    }

    /// Compare a chosen start base against the fetched default branch.
    pub(super) fn start_base_drift(
        &self,
        commit: &str,
    ) -> (Option<u64>, Option<u64>, Option<String>) {
        let Some((upstream_ref, upstream_head)) = self.repo.upstream_default() else {
            return (None, None, None);
        };
        let behind = self.repo.commit_count_between(commit, &upstream_head).ok();
        let ahead = self.repo.commit_count_between(&upstream_head, commit).ok();
        (behind, ahead, Some(upstream_ref))
    }

    /// Choose the commit a new session's branch is cut from.
    ///
    /// An explicit `--base` skips inference entirely: the operator named the
    /// ref, so guessing on their behalf would be worse than failing. It is
    /// still measured against the default branch, because naming a base does
    /// not make its inherited commits stop landing in a pull request (#290).
    ///
    /// Without `--base`, integration is the base only when it is current: the
    /// promote mode promotes, and integration contains the fetched default
    /// branch's tip. Otherwise the session is cut from the fetched default
    /// branch, and the bypassed integration is reported. Choosing integration
    /// whenever it existed cut sessions 2,101 commits behind upstream in one
    /// repository, where it had stopped moving three days earlier. Nothing here
    /// fetches: `create_session_worktree` refreshes the default branch just
    /// before calling it (see `Broker::refresh_default_branch_before_start`),
    /// so the fetched tip it reads is current unless that refresh failed.
    pub(super) fn select_session_start_base(
        &self,
        explicit: Option<&str>,
    ) -> Result<SessionStartBase, BrokerOpError> {
        if let Some(reference) = explicit {
            let Some(commit) = self.repo.resolve_ref(reference) else {
                return Err(BrokerOpError::StartBaseUnavailable {
                    reason: format!(
                        "--base {reference} does not resolve to a commit in this repository"
                    ),
                });
            };
            let (behind_default_commits, ahead_default_commits, default_ref) =
                self.start_base_drift(&commit);
            return Ok(SessionStartBase {
                ref_name: reference.to_string(),
                commit,
                evidence: SessionStartBaseEvidence::ExplicitBase,
                behind_default_commits,
                ahead_default_commits,
                default_ref,
                bypassed_integration: None,
                fetched: None,
                fetch_error: None,
                cached_ref_age_seconds: None,
            });
        }
        let promote = PromoteConfig::load(&self.main_root);
        let verify_only = promote.mode == crate::merge::PromoteMode::VerifyOnly;
        let integration_ref = format!("refs/heads/{}", promote.branch);
        let integration = self.repo.resolve_ref(&integration_ref);
        let upstream = self.repo.upstream_default();
        if let Some(commit) = &integration
            && !verify_only
            && upstream
                .as_ref()
                .is_none_or(|(_, tip)| self.repo.is_ancestor(tip, commit))
        {
            let (behind_default_commits, ahead_default_commits, default_ref) =
                self.start_base_drift(commit);
            return Ok(SessionStartBase {
                ref_name: integration_ref,
                commit: commit.clone(),
                evidence: SessionStartBaseEvidence::IntegrationTip,
                behind_default_commits,
                ahead_default_commits,
                default_ref,
                bypassed_integration: None,
                fetched: None,
                fetch_error: None,
                cached_ref_age_seconds: None,
            });
        }

        if let Some((upstream_ref, upstream_commit)) = upstream {
            let bypassed_integration = integration.map(|commit| {
                let (behind_default_commits, ahead_default_commits, _) =
                    self.start_base_drift(&commit);
                let behind = !verify_only;
                BypassedIntegration {
                    ref_name: integration_ref,
                    commit,
                    reason: if behind {
                        IntegrationBypassReason::BehindUpstream
                    } else {
                        IntegrationBypassReason::VerifyOnly
                    },
                    behind_default_commits,
                    ahead_default_commits,
                    recovery_command: behind.then(|| {
                        format!(
                            "aethyme broker advanced integration reconcile --upstream {upstream_ref}"
                        )
                    }),
                }
            });
            return Ok(SessionStartBase {
                ref_name: format!("refs/remotes/{upstream_ref}"),
                commit: upstream_commit,
                evidence: SessionStartBaseEvidence::FetchedDefaultBranch,
                behind_default_commits: Some(0),
                ahead_default_commits: Some(0),
                default_ref: Some(upstream_ref),
                bypassed_integration,
                fetched: None,
                fetch_error: None,
                cached_ref_age_seconds: None,
            });
        }
        // No fetched default branch: a promoting integration was taken above
        // (there is nothing to be behind), so only a verify-only repository or
        // one without integration reaches its local default branch here.
        if let Some(remote_head) = self.repo.symbolic_ref("refs/remotes/origin/HEAD")
            && let Some(branch_name) = remote_head.strip_prefix("refs/remotes/origin/")
        {
            let local_ref = format!("refs/heads/{branch_name}");
            if let Some(commit) = self.repo.resolve_ref(&local_ref) {
                return Ok(SessionStartBase {
                    ref_name: local_ref,
                    commit,
                    evidence: SessionStartBaseEvidence::RemoteDefaultBranch,
                    behind_default_commits: None,
                    ahead_default_commits: None,
                    default_ref: None,
                    bypassed_integration: None,
                    fetched: None,
                    fetch_error: None,
                    cached_ref_age_seconds: None,
                });
            }
        }

        let main = self.repo.resolve_ref("refs/heads/main");
        let master = self.repo.resolve_ref("refs/heads/master");
        match (main, master) {
            (Some(commit), None) => Ok(SessionStartBase {
                ref_name: "refs/heads/main".into(),
                commit,
                evidence: SessionStartBaseEvidence::ConventionalMain,
                behind_default_commits: None,
                ahead_default_commits: None,
                default_ref: None,
                bypassed_integration: None,
                fetched: None,
                fetch_error: None,
                cached_ref_age_seconds: None,
            }),
            (None, Some(commit)) => Ok(SessionStartBase {
                ref_name: "refs/heads/master".into(),
                commit,
                evidence: SessionStartBaseEvidence::ConventionalMaster,
                behind_default_commits: None,
                ahead_default_commits: None,
                default_ref: None,
                bypassed_integration: None,
                fetched: None,
                fetch_error: None,
                cached_ref_age_seconds: None,
            }),
            (Some(_), Some(_)) => Err(BrokerOpError::StartBaseUnavailable {
                reason: "both refs/heads/main and refs/heads/master exist, but origin/HEAD does not select one".into(),
            }),
            (None, None) => Err(BrokerOpError::StartBaseUnavailable {
                reason: "no integration tip, origin/HEAD-backed local branch, or unambiguous main/master ref exists".into(),
            }),
        }
    }

    pub(super) fn next_worktree_slug(&self, task: &str, worktree_root: &Path) -> String {
        let base = slugify(task);
        for attempt in 0..1000 {
            let slug = if attempt == 0 {
                base.clone()
            } else {
                format!("{base}-{}", attempt + 1)
            };
            let branch = format!("refs/heads/agent/{slug}");
            let worktree_path = worktree_root.join(&slug);
            if !worktree_path.exists() && self.repo.resolve_ref(&branch).is_none() {
                return slug;
            }
        }
        format!("{base}-{}", now_ms())
    }
}
