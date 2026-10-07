use super::*;

impl Broker {
    // ── agents (liveness) ─────────────────────────────────────────────

    /// Live sessions with liveness derived from what the broker can
    /// actually observe: store activity, worktree git metadata mtimes,
    /// and PID liveness for spawned sessions. Reconciles dead spawned
    /// processes to `exited` in the store as a side effect.
    pub fn agents(&mut self, now_ms: i64) -> Result<Vec<AgentView>, BrokerOpError> {
        let sessions = self.store.live_sessions()?;
        let abandon_after_ms = self.abandonment_window_ms();
        let mut views = Vec::with_capacity(sessions.len());
        for session in sessions {
            let fs_activity = worktree_activity_ms(&self.main_root, &session);
            let activity_at = fs_activity.unwrap_or(0).max(session.last_activity_at);

            let pid_alive = session.pid.map(pid_alive);
            let mut derived_status = session.status;
            if matches!(
                session.status,
                SessionStatus::Active | SessionStatus::Idle | SessionStatus::Stale
            ) {
                if pid_alive == Some(false) {
                    // The broker spawned it and it is gone: record the exit.
                    self.store
                        .set_session_status(session.id, SessionStatus::Exited, None)?;
                    derived_status = SessionStatus::Exited;
                } else {
                    let age = now_ms.saturating_sub(activity_at);
                    derived_status = if age > STALE_AFTER_MS {
                        SessionStatus::Stale
                    } else if age > IDLE_AFTER_MS {
                        SessionStatus::Idle
                    } else {
                        SessionStatus::Active
                    };
                    // Persist liveness *transitions* so they land in the
                    // event timeline exactly once (session.stale is the
                    // "stale worktree detected" signal from issue #24).
                    if derived_status != session.status {
                        self.store
                            .set_session_status(session.id, derived_status, None)?;
                    }
                }
            }
            // Staleness above is only a label: `stale` is still a live status,
            // so a stale session keeps pinning its worktree and its branch
            // forever. Abandonment is the terminal transition that label never
            // had (#176). Closing here does not delete anything -- it makes the
            // worktree a cleanup *candidate*, which `cleanup_item` then judges
            // on its own merits. Candidacy is granted; eligibility is earned.
            if !derived_status.is_closed() {
                let activity = SessionActivity {
                    session_id: session.id,
                    last_activity_at: activity_at,
                    created_at: session.created_at,
                    agent_alive: pid_alive,
                    closed: false,
                };
                if let AbandonmentVerdict::Abandoned { idle_ms } =
                    decide_abandonment(&activity, now_ms, abandon_after_ms)
                {
                    self.store.append_event(
                        "session.abandoned",
                        Some(session.id),
                        Some(&format!(
                            r#"{{"idle_ms":{idle_ms},"abandon_after_ms":{abandon_after_ms},"pid_alive":{}}}"#,
                            match pid_alive {
                                Some(true) => "true",
                                Some(false) => "false",
                                None => "null",
                            }
                        )),
                    )?;
                    self.store
                        .set_session_status(session.id, SessionStatus::Closed, None)?;
                    derived_status = SessionStatus::Closed;
                }
            }
            views.push(AgentView {
                session,
                activity_at,
                derived_status,
                pid_alive,
            });
        }
        Ok(views)
    }

    // ── disk / ledger reconciliation ──────────────────────────────────

    /// Compare the directories under broker-owned worktree roots against the
    /// worktrees sessions actually claim (#176, ownership drift).
    ///
    /// `sized` walks every unclaimed directory to estimate its bytes, which is
    /// the expensive half. Counts are available without it, and the report says
    /// which of the two it is rather than leaving a caller to infer that zero
    /// bytes across sixteen directories means "unsized" and not "empty".
    ///
    /// Reporting only. A directory with no session row is a directory whose
    /// contents the broker cannot reason about, so naming it is the most this
    /// may do; nothing downstream removes on this evidence.
    pub fn reconcile_worktree_directories(
        &self,
        sized: bool,
    ) -> Result<WorktreeReconciliation, BrokerOpError> {
        let roots = self.broker_owned_worktree_roots()?;
        // Both sides are compared as the strings the broker itself produced,
        // which is sound only because they descend from one canonical root:
        // `Broker::open` canonicalises `main_root`, so a session started
        // through a symlinked checkout still records the real path, and
        // `read_dir` on a root derived from the same place yields the same
        // spelling. `a_worktree_reached_by_another_spelling_is_still_owned`
        // pins that invariant; if it ever stops holding, healthy worktrees
        // start being reported as drift rather than silently mismatching.
        let claims: Vec<(i64, String)> = self
            .store
            .live_sessions()?
            .into_iter()
            .chain(self.store.cleaned_sessions()?)
            .map(|session| (session.id, session.worktree_path.clone()))
            .collect();

        let mut observed = Vec::new();
        for root in &roots {
            let Ok(entries) = std::fs::read_dir(root) else {
                // An unreadable root is not drift; claiming it held nothing
                // would understate the problem this sweep exists to surface.
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !is_real_directory(&path) || is_worktree_root_infrastructure(&entry.file_name())
                {
                    continue;
                }
                observed.push(crate::worktree_reconcile::ObservedDirectory {
                    path: path.to_string_lossy().into_owned(),
                    git_marker: path.join(".git").exists(),
                    estimated_bytes: None,
                });
            }
        }

        let mut reconciled = crate::worktree_reconcile::reconcile(&observed, &claims);
        if sized {
            for entry in reconciled.iter_mut().filter(|entry| entry.unclaimed()) {
                entry.estimated_bytes = Some(
                    directory_size_without_following_links(Path::new(&entry.path)).unwrap_or(0),
                );
            }
        }
        Ok(crate::worktree_reconcile::summarise(
            &reconciled,
            roots.len(),
        ))
    }

    /// The worktree roots this broker creates session worktrees in.
    ///
    /// Deliberately only these. A session may be adopted at any path --
    /// including the main checkout -- and sweeping the parent of an adopted
    /// worktree would enumerate unrelated repositories as broker drift.
    pub(super) fn broker_owned_worktree_roots(&self) -> Result<Vec<PathBuf>, BrokerOpError> {
        let plan = self.worktree_root_plan()?;
        let mut roots = Vec::new();
        for root in [
            plan.preferred_root.clone(),
            plan.host_state_fallback_root.clone(),
            Some(plan.legacy_fallback_root),
        ]
        .into_iter()
        .flatten()
        {
            if is_real_directory(&root) && !roots.contains(&root) {
                roots.push(root);
            }
        }
        Ok(roots)
    }

    /// How long a session may go unattended before the broker treats its agent
    /// as gone, in milliseconds. `0` disables the lane.
    ///
    /// A malformed `.aethyme/broker.toml` disables abandonment rather than
    /// failing the caller. Every caller of this is a listing or status surface,
    /// and a retention typo must not make the broker unable to say what is
    /// running -- least of all when the operator is reading status precisely
    /// because something is wrong. Config errors still surface, loudly, from
    /// the retention commands that exist to report them.
    pub(super) fn abandonment_window_ms(&self) -> i64 {
        crate::load_retention_policy(&self.main_root)
            .map(|policy| i64::from(policy.session_abandoned_after_hours) * 3_600_000)
            .unwrap_or(0)
    }

    /// Open sessions whose agent has shown no evidence of work for `window_ms`.
    ///
    /// The rule is the one that closes abandoned sessions -- [`decide_abandonment`]
    /// over the liveness [`Broker::agents_snapshot`] derives -- applied over a
    /// caller-chosen window, so a live agent process is never included however
    /// quiet it is. Read-only: nothing is closed and no event is written.
    pub(crate) fn idle_open_sessions(
        &self,
        now_ms: i64,
        window_ms: i64,
    ) -> Result<Vec<Session>, BrokerOpError> {
        Ok(self
            .agents_snapshot(now_ms)?
            .into_iter()
            .filter(|view| !view.derived_status.is_closed())
            .filter(|view| {
                let activity = SessionActivity {
                    session_id: view.session.id,
                    last_activity_at: view.activity_at,
                    created_at: view.session.created_at,
                    agent_alive: view.pid_alive,
                    closed: false,
                };
                decide_abandonment(&activity, now_ms, window_ms).is_abandoned()
            })
            .map(|view| view.session)
            .collect())
    }

    /// Derive current liveness without persisting status transitions. This is
    /// deliberately allowed to inspect PIDs and worktree metadata, but never
    /// changes a session row or appends an event.
    pub fn agents_snapshot(&self, now_ms: i64) -> Result<Vec<AgentView>, BrokerOpError> {
        let sessions = self.store.live_sessions()?;
        let mut views = Vec::with_capacity(sessions.len());
        for session in sessions {
            let fs_activity = worktree_activity_ms(&self.main_root, &session);
            let activity_at = fs_activity.unwrap_or(0).max(session.last_activity_at);
            let pid_alive = session.pid.map(pid_alive);
            let mut derived_status = session.status;
            if matches!(
                session.status,
                SessionStatus::Active | SessionStatus::Idle | SessionStatus::Stale
            ) {
                derived_status = if pid_alive == Some(false) {
                    SessionStatus::Exited
                } else {
                    let age = now_ms.saturating_sub(activity_at);
                    if age > STALE_AFTER_MS {
                        SessionStatus::Stale
                    } else if age > IDLE_AFTER_MS {
                        SessionStatus::Idle
                    } else {
                        SessionStatus::Active
                    }
                };
            }
            views.push(AgentView {
                session,
                activity_at,
                derived_status,
                pid_alive,
            });
        }
        Ok(views)
    }

    /// What every worktree on this host holds, and whether removing it would
    /// lose anything. Read-only.
    ///
    /// A directory is a worktree when it carries its own `.git` entry, not when
    /// it sits at a particular depth. The host mixes layouts: most roots hold
    /// one directory per session, but some roots *are* a checkout. Keying on
    /// depth descends into those and reports each of their source directories
    /// as a worktree, every one inheriting the parent's git state.
    ///
    /// Sizing follows `sizing`: [`crate::WorktreeSizing::Bounded`] reuses each
    /// repository's recorded measurements and walks the rest only until its
    /// deadline, so a host with hundreds of checkouts still answers (#559).
    pub fn worktree_report(
        &mut self,
        sizing: crate::WorktreeSizing,
    ) -> Result<crate::WorktreeReport, BrokerOpError> {
        fn is_checkout(path: &std::path::Path) -> bool {
            path.join(".git").exists()
        }

        let plan = self.worktree_root_plan()?;
        // Keyed by path: `preferred_root` normally sits inside `root_container`,
        // so both walks reach the same checkouts and a plain vector reports each
        // of this repository's worktrees twice.
        let mut worktrees: std::collections::BTreeMap<std::path::PathBuf, String> =
            std::collections::BTreeMap::new();
        let mut roots = Vec::new();
        if let Some(container) = plan.root_container.as_ref() {
            roots.push(container.clone());
        }
        if let Some(root) = plan.preferred_root.as_ref()
            && !roots.contains(root)
        {
            roots.push(root.clone());
        }
        for root in roots {
            let Ok(entries) = std::fs::read_dir(&root) else {
                continue;
            };
            for entry in entries.flatten() {
                if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                    continue;
                }
                let path = entry.path();
                let label = entry.file_name().to_string_lossy().into_owned();
                if is_checkout(&path) {
                    // A root that is itself a checkout.
                    worktrees.entry(path).or_insert(label);
                    continue;
                }
                let Ok(children) = std::fs::read_dir(&path) else {
                    continue;
                };
                for child in children.flatten() {
                    if !child.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                        continue;
                    }
                    let child_path = child.path();
                    // Shared caches sit beside worktrees under the same root.
                    if is_checkout(&child_path) {
                        worktrees.entry(child_path).or_insert_with(|| label.clone());
                    }
                }
            }
        }
        let live = self
            .store
            .live_sessions()?
            .into_iter()
            .map(|session| std::path::PathBuf::from(session.worktree_path))
            .collect();
        let worktrees: Vec<(String, std::path::PathBuf)> = worktrees
            .into_iter()
            .map(|(path, label)| (label, path))
            .collect();
        let mut report = crate::build_worktree_report_with(&worktrees, &live, sizing);
        let registrations = self.repo.worktree_inventory()?;
        crate::append_prunable_registrations(
            &mut report,
            &plan.repository_key,
            &registrations,
            &live,
        );
        Ok(report)
    }

    /// Return overlaps from the persisted lease snapshot without recomputing
    /// implicit leases, extending expiries, or emitting overlap events.
    /// Like [`Broker::refresh_leases`], only sessions still working are
    /// paired.
    pub fn lease_overlaps_snapshot(&self) -> Result<Vec<crate::Overlap>, BrokerOpError> {
        let coordinating = self.coordinating_session_ids()?;
        Ok(crate::detect_overlaps(
            &self.coordinating_leases(&coordinating)?,
        ))
    }

    /// Collisions between what live sessions say they will work on.
    ///
    /// Read-only and independent of leases: a scope exists before any edit, so
    /// this reports pairs that no diff has revealed yet.
    pub fn scope_overlaps_snapshot(&self) -> Result<Vec<crate::ScopeOverlap>, BrokerOpError> {
        Ok(crate::detect_scope_overlaps(
            &self.store.active_session_scopes()?,
        ))
    }

    /// Record what a session says it will work on.
    ///
    /// `declared` is what the operator stated; anything the task text implies
    /// is derived from the graph and recorded separately, so a reader can tell
    /// an assertion from an inference. Derivation is best-effort by design:
    /// graph state is repository opt-in, and a session must still start on a
    /// repository that has none. When it is unavailable the reason is returned
    /// rather than swallowed, so the caller can say why nothing was derived.
    pub fn capture_session_scopes(
        &mut self,
        session_id: i64,
        declared: &[(crate::ScopeKind, String, crate::ScopeOperation)],
        task: Option<&str>,
    ) -> Result<ScopeCaptureReport, BrokerOpError> {
        let mut rows: Vec<(
            crate::ScopeKind,
            String,
            crate::ScopeOperation,
            crate::ScopeSource,
        )> = declared
            .iter()
            .map(|(kind, value, operation)| {
                (
                    *kind,
                    value.clone(),
                    *operation,
                    crate::ScopeSource::Declared,
                )
            })
            .collect();
        let declared_count = rows.len();

        let (derived, degraded) = match task {
            Some(text) if !text.trim().is_empty() => {
                derive_scopes_from_task(self.main_root(), text)
            }
            _ => (Vec::new(), Some("session has no task text".to_string())),
        };
        let already: std::collections::HashSet<String> =
            rows.iter().map(|(_, value, _, _)| value.clone()).collect();
        let derived_count = derived
            .iter()
            .filter(|value| !already.contains(*value))
            .count();
        for value in derived {
            if already.contains(&value) {
                continue;
            }
            rows.push((
                crate::ScopeKind::Symbol,
                value,
                crate::ScopeOperation::Unknown,
                crate::ScopeSource::Derived,
            ));
        }

        if !rows.is_empty() {
            self.store.record_session_scopes(session_id, &rows)?;
        }
        Ok(ScopeCaptureReport {
            declared: declared_count,
            derived: derived_count,
            degraded,
        })
    }

    // ── leases (Phase 3) ──────────────────────────────────────────────

    /// Recompute the implicit leases of every session that is still
    /// working (active or idle) from its diff against its recorded base
    /// (ignore rules applied), then return the overlap set between those
    /// sessions. Overlaps are classified per session pair and announced once
    /// per pair: when it starts overlapping, and again only when its severity
    /// or conflicting paths change (see `overlap_pairs`).
    ///
    /// Stale sessions are left out: their recorded leases stay as data, but
    /// they are neither rescanned nor paired. Nobody is working there to
    /// coordinate with, and in a repository with dozens of abandoned sessions
    /// rescanning and pairing them made every `status` run Git hundreds of
    /// times. A stale session that resumes work becomes active again and is
    /// included from its next refresh.
    ///
    /// Sessions whose worktree is gone or whose base no longer resolves
    /// are skipped, never fatal: lease freshness must not take the broker
    /// down.
    pub fn refresh_leases(&mut self) -> Result<Vec<crate::leases::Overlap>, BrokerOpError> {
        self.refresh_leases_including(None)
    }

    /// [`Broker::refresh_leases`], always rescanning `acting` as well: the
    /// session whose own command (submit, claim, guarded exec) needs its
    /// leases current even when it has been quiet long enough to read as
    /// stale.
    pub(crate) fn refresh_leases_including(
        &mut self,
        acting: Option<i64>,
    ) -> Result<Vec<crate::leases::Overlap>, BrokerOpError> {
        use crate::leases::{LeaseIgnoreRules, detect_overlaps};

        let rules = LeaseIgnoreRules::load(&self.main_root);
        // The integration tip is the same for every session; resolving it
        // inside the loop cost one git subprocess per live session.
        let integration = self.integration_tip();
        let upstream = self.repo.upstream_default().map(|(_, commit)| commit);
        let mut coordinating = self.coordinating_session_ids()?;
        coordinating.extend(acting);
        for session in self.store.live_sessions()? {
            if !coordinating.contains(&session.id) {
                continue;
            }
            let Ok(checkout) = GitRepo::discover(Path::new(&session.worktree_path)) else {
                continue;
            };
            // #41: derive the baseline instead of trusting the stored
            // adoption-time diff_base — after a conflict-rebase the stored
            // value inflates the diff with everyone else's promoted work.
            let base = integration
                .as_ref()
                .and_then(|tip| crate::merge::session_baseline(&checkout, tip, upstream.as_deref()))
                .or_else(|| session.diff_base.clone())
                .unwrap_or_else(|| "HEAD".to_string());
            let Ok(changed) = checkout.changed_files(&base) else {
                continue;
            };
            let paths: Vec<String> = changed
                .into_iter()
                .filter(|path| !rules.is_ignored(path))
                .collect();
            self.store.set_implicit_leases(session.id, &paths)?;
        }

        let after = detect_overlaps(&self.coordinating_leases(&coordinating)?);
        self.classify_and_announce_overlaps(&after)?;
        self.store
            .meta_set("leases.refreshed_at_ms", &now_ms().to_string())?;
        Ok(after)
    }

    /// Sessions someone may still be working in: live, with activity
    /// within the stale threshold. Read without persisting status
    /// transitions.
    pub(super) fn coordinating_session_ids(
        &self,
    ) -> Result<std::collections::BTreeSet<i64>, BrokerOpError> {
        let now = now_ms();
        Ok(self
            .agents_snapshot(now)?
            .into_iter()
            // Stale by activity, not by PID: a spawned session whose command
            // has exited still holds work that needs its leases.
            .filter(|agent| {
                matches!(
                    agent.session.status,
                    SessionStatus::Active | SessionStatus::Idle | SessionStatus::Stale
                ) && now.saturating_sub(agent.activity_at) <= STALE_AFTER_MS
            })
            .map(|agent| agent.session.id)
            .collect())
    }

    /// Active leases held by the given sessions.
    pub(super) fn coordinating_leases(
        &self,
        coordinating: &std::collections::BTreeSet<i64>,
    ) -> Result<Vec<crate::types::Lease>, BrokerOpError> {
        Ok(self
            .store
            .active_leases()?
            .into_iter()
            .filter(|lease| coordinating.contains(&lease.session_id))
            .collect())
    }

    /// Claim an explicit write lease. Another session's lease refuses the
    /// claim only under `LeaseRefusalPolicy`: an explicit lease held by a
    /// session that is actively working, outside verify-only. Every other
    /// overlapping lease of a live session is returned as a warning with how
    /// to coordinate; leases implied by stale sessions' old edits are omitted.
    pub fn claim_lease(
        &mut self,
        session_id: i64,
        path: &str,
        ttl_ms: Option<i64>,
    ) -> Result<LeaseClaimReport, BrokerOpError> {
        let session = self.store.session(session_id)?;
        if session.status.is_closed() {
            return Err(BrokerOpError::ClosedSessionOperation {
                session_id,
                repository_root: self.main_root().display().to_string(),
            });
        }
        let path = normalize_lease_path(path)?;
        self.refresh_leases_including(Some(session_id))?;
        let policy = self.lease_refusal_policy()?;
        let mut blockers = Vec::new();
        let mut warnings = Vec::new();
        for mut lease in self.lease_blockers(session_id, &path)? {
            if lease.kind == LeaseKind::Implicit && !policy.is_live(lease.session_id) {
                continue;
            }
            if let Some(status) = policy.live.get(&lease.session_id) {
                lease.holder_status = Some(status.as_str().to_string());
            }
            if policy.may_block(&lease) {
                lease.reason =
                    Some("explicitly claimed by a session that is actively working".into());
                blockers.push(lease);
            } else {
                lease.reason = Some(policy.non_blocking_reason(session_id, &lease));
                warnings.push(lease);
            }
        }
        if !blockers.is_empty() {
            return Err(BrokerOpError::LeaseClaimConflict {
                session_id,
                path,
                blocker_count: blockers.len(),
                blockers,
            });
        }
        self.store.claim_lease(session_id, &path, ttl_ms)?;
        Ok(LeaseClaimReport {
            session_id,
            path,
            accepted: true,
            blockers: Vec::new(),
            warnings,
        })
    }

    /// The lease refusal rule for this repository right now: the promote mode
    /// from the configuration committed on the default branch, and each live
    /// session's derived status.
    pub(crate) fn lease_refusal_policy(&mut self) -> Result<LeaseRefusalPolicy, BrokerOpError> {
        let verify_only = LeaseRefusalPolicy::verify_only_at(&self.main_root);
        let live = self
            .agents(crate::clock::epoch_ms())?
            .into_iter()
            .filter(|agent| {
                matches!(
                    agent.derived_status,
                    SessionStatus::Active | SessionStatus::Idle
                )
            })
            .map(|agent| (agent.session.id, agent.derived_status))
            .collect::<std::collections::HashMap<_, _>>();
        // A holder gone past its stale grace no longer refuses (#360); its
        // leases are still listed.
        let mut live = live;
        for session_id in self.sessions_released_by_grace()? {
            live.remove(&session_id);
        }
        Ok(LeaseRefusalPolicy { verify_only, live })
    }

    /// Record, once per holder, that a live session's holder process is
    /// gone, so its leases' stale grace runs from the first sighting (#360).
    pub fn record_gone_lease_holders(&mut self) -> Result<(), BrokerOpError> {
        let Some(table) = crate::session_holder::ProcessTable::snapshot() else {
            return Ok(());
        };
        let mut sessions: Vec<i64> = self
            .store
            .active_leases()?
            .iter()
            .map(|lease| lease.session_id)
            .collect();
        sessions.sort_unstable();
        sessions.dedup();
        crate::lease_liveness::record_gone_holders(&mut self.store, &sessions, &table)?;
        Ok(())
    }

    /// Liveness of every active lease right now.
    pub fn lease_liveness(
        &self,
        now_ms: i64,
    ) -> Result<Vec<crate::lease_liveness::LeaseLivenessView>, BrokerOpError> {
        crate::lease_liveness::assess(
            &self.store,
            &self.store.active_leases()?,
            crate::session_holder::ProcessTable::snapshot().as_ref(),
            now_ms,
            crate::lease_liveness::LeaseLivenessPolicy::load(&self.main_root),
        )
    }

    /// Every lease release request, with its state (#359).
    pub fn lease_release_requests(
        &self,
    ) -> Result<Vec<crate::lease_requests::LeaseReleaseRequest>, BrokerOpError> {
        let leases = self.store.active_leases()?;
        crate::lease_requests::requests(&self.store, |holder, path| {
            leases.iter().any(|lease| {
                lease.session_id == holder && crate::leases::paths_overlap(path, &lease.path)
            })
        })
    }

    /// Ask every other session holding a lease on `path` to release it. One
    /// request per holder; an identical pending request is returned, not
    /// repeated. Requests against a holder already gone past its grace are
    /// granted at once.
    pub fn request_lease_release(
        &mut self,
        requester: i64,
        path: &str,
        reason: &str,
    ) -> Result<Vec<crate::lease_requests::LeaseReleaseRequest>, BrokerOpError> {
        self.store.session(requester)?;
        let path = normalize_lease_path(path)?;
        if reason.trim().is_empty() {
            return Err(BrokerOpError::LeaseRequestRefused {
                reason: "a release request needs --reason".into(),
            });
        }
        let mut holders: Vec<i64> = self
            .store
            .active_leases()?
            .iter()
            .filter(|lease| {
                lease.session_id != requester && crate::leases::paths_overlap(&path, &lease.path)
            })
            .map(|lease| lease.session_id)
            .collect();
        holders.sort_unstable();
        holders.dedup();
        if holders.is_empty() {
            return Err(BrokerOpError::LeaseRequestRefused {
                reason: format!("no other session holds a lease on {path}"),
            });
        }
        let existing = self.lease_release_requests()?;
        let mut ids = Vec::new();
        for holder in holders {
            if let Some(pending) = existing.iter().find(|request| {
                request.state == crate::lease_requests::RequestState::Pending
                    && request.requester_session_id == requester
                    && request.holder_session_id == holder
                    && request.path == path
            }) {
                ids.push(pending.request_id);
                continue;
            }
            ids.push(self.store.append_event(
                crate::lease_requests::REQUESTED,
                Some(requester),
                Some(&crate::lease_requests::request_payload(
                    &path, requester, holder, reason,
                )),
            )?);
        }
        self.grant_stale_lease_requests()?;
        Ok(self
            .lease_release_requests()?
            .into_iter()
            .filter(|request| ids.contains(&request.request_id))
            .collect())
    }

    /// The pending request `request_id`, if `holder` is the one it asks.
    pub(super) fn pending_request_for_holder(
        &self,
        request_id: i64,
        holder: i64,
    ) -> Result<crate::lease_requests::LeaseReleaseRequest, BrokerOpError> {
        let request = self
            .lease_release_requests()?
            .into_iter()
            .find(|request| request.request_id == request_id)
            .ok_or_else(|| BrokerOpError::LeaseRequestRefused {
                reason: format!("no lease release request {request_id}"),
            })?;
        if request.holder_session_id != holder {
            return Err(BrokerOpError::LeaseRequestRefused {
                reason: format!(
                    "request {request_id} asks session {}, not session {holder}",
                    request.holder_session_id
                ),
            });
        }
        if request.state != crate::lease_requests::RequestState::Pending {
            return Err(BrokerOpError::LeaseRequestRefused {
                reason: format!("request {request_id} is already {}", request.state.as_str()),
            });
        }
        Ok(request)
    }

    /// Release `holder`'s leases on a request's path and record `kind`.
    pub(super) fn release_for_request(
        &mut self,
        request: &crate::lease_requests::LeaseReleaseRequest,
        kind: &str,
        reason: Option<&str>,
    ) -> Result<(), BrokerOpError> {
        let held: Vec<String> = self
            .store
            .active_leases()?
            .into_iter()
            .filter(|lease| {
                lease.session_id == request.holder_session_id
                    && crate::leases::paths_overlap(&request.path, &lease.path)
            })
            .map(|lease| lease.path)
            .collect();
        // The same audited release a finish records (#358), with the reason
        // `request_acked` or `request_granted`.
        let release_reason = if kind == crate::lease_requests::GRANTED {
            "request_granted"
        } else {
            "request_acked"
        };
        for path in held {
            self.store
                .release_lease_for(request.holder_session_id, &path, release_reason)?;
        }
        self.store.append_event(
            kind,
            Some(request.holder_session_id),
            Some(&crate::lease_requests::outcome_payload(
                request.request_id,
                &request.path,
                reason,
            )),
        )?;
        Ok(())
    }

    /// The holder acknowledges a request: its leases on the path are released.
    pub fn ack_lease_release(
        &mut self,
        holder: i64,
        request_id: i64,
    ) -> Result<crate::lease_requests::LeaseReleaseRequest, BrokerOpError> {
        let request = self.pending_request_for_holder(request_id, holder)?;
        self.release_for_request(&request, crate::lease_requests::ACKED, None)?;
        self.lease_release_request(request_id)
    }

    /// The holder declines a request; its leases stay.
    pub fn decline_lease_release(
        &mut self,
        holder: i64,
        request_id: i64,
        reason: &str,
    ) -> Result<crate::lease_requests::LeaseReleaseRequest, BrokerOpError> {
        if reason.trim().is_empty() {
            return Err(BrokerOpError::LeaseRequestRefused {
                reason: "declining needs --reason".into(),
            });
        }
        let request = self.pending_request_for_holder(request_id, holder)?;
        self.store.append_event(
            crate::lease_requests::DECLINED,
            Some(holder),
            Some(&crate::lease_requests::outcome_payload(
                request_id,
                &request.path,
                Some(reason),
            )),
        )?;
        self.lease_release_request(request_id)
    }

    pub fn lease_release_request(
        &self,
        request_id: i64,
    ) -> Result<crate::lease_requests::LeaseReleaseRequest, BrokerOpError> {
        self.lease_release_requests()?
            .into_iter()
            .find(|request| request.request_id == request_id)
            .ok_or_else(|| BrokerOpError::LeaseRequestRefused {
                reason: format!("no lease release request {request_id}"),
            })
    }

    /// Grant every pending request whose holder is gone past the stale grace
    /// on all its leases covering the path (#360). A holder that is running,
    /// within its grace, or of unknown liveness is never granted against.
    pub fn grant_stale_lease_requests(&mut self) -> Result<Vec<i64>, BrokerOpError> {
        let pending: Vec<_> = self
            .lease_release_requests()?
            .into_iter()
            .filter(|request| request.state == crate::lease_requests::RequestState::Pending)
            .collect();
        if pending.is_empty() {
            return Ok(Vec::new());
        }
        let liveness = self.lease_liveness(crate::clock::epoch_ms())?;
        let mut granted = Vec::new();
        for request in pending {
            let covering: Vec<_> = liveness
                .iter()
                .filter(|view| {
                    view.session_id == request.holder_session_id
                        && crate::leases::paths_overlap(&request.path, &view.path)
                })
                .collect();
            let gone_past_grace = !covering.is_empty()
                && covering.iter().all(|view| {
                    view.liveness == crate::lease_liveness::LeaseLiveness::Stale
                        && !view.liveness_evidence.holds
                });
            if gone_past_grace {
                self.release_for_request(
                    &request,
                    crate::lease_requests::GRANTED,
                    Some("holder_stale"),
                )?;
                granted.push(request.request_id);
            }
        }
        Ok(granted)
    }

    /// Sessions whose leases no longer hold: holder gone past the grace.
    pub(super) fn sessions_released_by_grace(
        &self,
    ) -> Result<std::collections::HashSet<i64>, BrokerOpError> {
        Ok(crate::lease_liveness::released_by_grace(
            &self.lease_liveness(crate::clock::epoch_ms())?,
        ))
    }

    /// Inspect how proposed explicit lease claims intersect the current
    /// active lease set. This deliberately does not refresh implicit
    /// leases, touch expiries, or append events: it is a snapshot query.
    pub fn plan_leases(
        &self,
        paths: &[String],
        session_id: Option<i64>,
    ) -> Result<LeasePlan, BrokerOpError> {
        if let Some(session_id) = session_id {
            self.store.session(session_id)?;
        }

        let mut normalized = paths
            .iter()
            .map(|path| normalize_lease_path(path))
            .collect::<Result<Vec<_>, _>>()?;
        normalized.sort();
        normalized.dedup();

        let leases = self.store.active_leases()?;
        let agents = self.agents_snapshot(now_ms())?;
        let verify_only = LeaseRefusalPolicy::verify_only_at(&self.main_root);
        let liveness = crate::lease_liveness::assess(
            &self.store,
            &leases,
            crate::session_holder::ProcessTable::snapshot().as_ref(),
            now_ms(),
            crate::lease_liveness::LeaseLivenessPolicy::load(&self.main_root),
        )?;
        let released_by_grace = crate::lease_liveness::released_by_grace(&liveness);
        let mut planned = Vec::with_capacity(normalized.len());
        for path in normalized {
            let mut owned = Vec::new();
            let mut conflicts = Vec::new();
            for lease in leases
                .iter()
                .filter(|lease| crate::leases::paths_overlap(&path, &lease.path))
            {
                let owned_by_requester = Some(lease.session_id) == session_id;
                let owner = agents
                    .iter()
                    .find(|agent| agent.session.id == lease.session_id)
                    .ok_or(BrokerError::SessionNotFound(lease.session_id))?;
                let overlap = LeasePlanOverlap {
                    relation: if path == lease.path {
                        LeaseOverlapRelation::Exact
                    } else {
                        LeaseOverlapRelation::Directory
                    },
                    session_id: lease.session_id,
                    path: lease.path.clone(),
                    kind: lease.kind,
                    expires_at: lease.expires_at,
                    owner_status: owner.derived_status,
                    owner_worktree: owner.session.worktree_path.clone(),
                    owner_activity_at: owner.activity_at,
                    owner_pid_alive: owner.pid_alive,
                    owner_context: owner.session.context_label(),
                    liveness: liveness
                        .iter()
                        .find(|view| view.lease_id == lease.id)
                        .map(|view| view.liveness),
                    liveness_evidence: liveness
                        .iter()
                        .find(|view| view.lease_id == lease.id)
                        .map(|view| view.liveness_evidence.clone()),
                    safe_next_actions: if owned_by_requester {
                        Vec::new()
                    } else {
                        crate::leases::planned_lease_next_actions(
                            &owner.session.worktree_path,
                            owner.session.id,
                            owner.derived_status,
                            &path,
                        )
                    },
                };
                if owned_by_requester {
                    owned.push(overlap);
                } else {
                    conflicts.push(overlap);
                }
            }
            owned.sort_by(lease_plan_overlap_order);
            conflicts.sort_by(lease_plan_overlap_order);
            let would_conflict = conflicts.iter().any(|blocker| {
                !released_by_grace.contains(&blocker.session_id)
                    && LeaseRefusalPolicy::refuses(
                        verify_only,
                        blocker.kind,
                        Some(blocker.owner_status),
                    )
            });
            planned.push(LeasePathPlan {
                path,
                owned,
                conflicts,
                would_conflict,
            });
        }

        Ok(LeasePlan {
            session_id,
            would_conflict: planned.iter().any(|path| path.would_conflict),
            paths: planned,
        })
    }

    pub(super) fn ensure_planned_paths_available(
        &mut self,
        paths: &[String],
        owner_session_id: Option<i64>,
    ) -> Result<(), BrokerOpError> {
        if paths.is_empty() {
            return Ok(());
        }
        // Persist liveness transitions first: the store rechecks planned
        // leases inside the registering transaction against the stored
        // status, which would otherwise still read `active` for a session
        // that has gone stale since the last status pass.
        self.agents(now_ms())?;
        let plan = self.plan_leases(paths, owner_session_id)?;
        let verify_only = LeaseRefusalPolicy::verify_only_at(&self.main_root);
        let refusing = plan
            .paths
            .iter()
            .filter(|path| path.would_conflict)
            .find_map(|path| {
                path.conflicts
                    .iter()
                    .find(|blocker| {
                        blocker
                            .liveness_evidence
                            .as_ref()
                            .is_none_or(|evidence| evidence.holds)
                            && LeaseRefusalPolicy::refuses(
                                verify_only,
                                blocker.kind,
                                Some(blocker.owner_status),
                            )
                    })
                    .map(|blocker| (path, blocker))
            });
        if let Some((path, blocker)) = refusing {
            return Err(BrokerError::PlannedLeaseConflict(Box::new(
                crate::error::PlannedLeaseConflict {
                    path: path.path.clone(),
                    blocker_session_id: blocker.session_id,
                    blocker_path: blocker.path.clone(),
                    blocker_kind: blocker.kind.as_str().to_string(),
                    blocker_status: blocker.owner_status.as_str().to_string(),
                    blocker_worktree: blocker.owner_worktree.clone(),
                    remediation: blocker.safe_next_actions.join("\n  "),
                },
            ))
            .into());
        }
        Ok(())
    }

    pub(super) fn planned_explicit_leases(
        &self,
        session_id: i64,
        paths: &[String],
    ) -> Result<Vec<crate::Lease>, BrokerOpError> {
        let mut leases = self
            .store
            .active_leases()?
            .into_iter()
            .filter(|lease| {
                lease.session_id == session_id
                    && lease.kind == LeaseKind::Explicit
                    && paths.binary_search(&lease.path).is_ok()
            })
            .collect::<Vec<_>>();
        leases.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(leases)
    }

    /// Preflight a session's committed diff before submit. Every
    /// non-ignored changed path must be owned by this session, must not
    /// overlap another live session's lease, and must not be an
    /// adoption-time foreign untracked path unless explicitly claimed.
    pub fn audit_submit_ownership(
        &mut self,
        session_id: i64,
    ) -> Result<OwnershipAuditReport, BrokerOpError> {
        let session = self.store.session(session_id)?;
        let checkout = GitRepo::discover(Path::new(&session.worktree_path))?;
        let head = checkout.head_commit()?;
        let base = self
            .session_change_base(&checkout)
            .or(session.diff_base)
            .unwrap_or_else(|| "HEAD".to_string());
        self.refresh_leases_including(Some(session_id))?;
        let changed = self.repo.changed_between(&base, &head)?;
        self.audit_paths(session_id, &base, &head, changed, true, true)
    }

    pub(super) fn audit_paths(
        &mut self,
        session_id: i64,
        base: &str,
        head: &str,
        mut changed: Vec<String>,
        allow_implicit: bool,
        // Submit only: another session's lease blocks when the shared
        // `LeaseRefusalPolicy` allows it AND the two sessions' edits conflict.
        // Guarded exec applies the policy alone, like a lease claim.
        block_only_on_conflicts: bool,
    ) -> Result<OwnershipAuditReport, BrokerOpError> {
        use crate::leases::{LeaseIgnoreRules, paths_overlap};

        changed.sort();
        changed.dedup();
        let rules = LeaseIgnoreRules::load(&self.main_root);
        let leases = self.store.active_leases()?;
        let foreign = self.store.session_foreign_files(session_id)?;
        let mut missing_lease_paths = Vec::new();
        let mut conflicting_leases = Vec::new();
        let mut foreign_paths = Vec::new();

        for path in changed.iter().filter(|path| !rules.is_ignored(path)) {
            let owns = leases.iter().any(|lease| {
                lease.session_id == session_id
                    && (allow_implicit || lease.kind == LeaseKind::Explicit)
                    && paths_overlap(&lease.path, path)
            });
            if !owns {
                missing_lease_paths.push(path.clone());
            }

            for blocker in leases
                .iter()
                .filter(|lease| lease.session_id != session_id)
                .filter(|lease| lease.kind == LeaseKind::Explicit)
                .filter(|lease| paths_overlap(&lease.path, path))
            {
                conflicting_leases.push(LeaseBlocker {
                    session_id: blocker.session_id,
                    path: blocker.path.clone(),
                    kind: blocker.kind,
                    holder_status: self.lease_holder_status(blocker.session_id),
                    holder_context: self.lease_holder_context(blocker.session_id),
                    severity: None,
                    reason: None,
                });
            }

            let explicitly_claimed = leases.iter().any(|lease| {
                lease.session_id == session_id
                    && lease.kind == LeaseKind::Explicit
                    && paths_overlap(&lease.path, path)
            });
            if !explicitly_claimed
                && foreign
                    .iter()
                    .any(|foreign_path| paths_overlap(foreign_path, path))
            {
                foreign_paths.push(path.clone());
            }
        }

        conflicting_leases.sort_by(|a, b| {
            (a.session_id, a.path.as_str(), a.kind.as_str()).cmp(&(
                b.session_id,
                b.path.as_str(),
                b.kind.as_str(),
            ))
        });
        conflicting_leases
            .dedup_by(|a, b| a.session_id == b.session_id && a.path == b.path && a.kind == b.kind);
        let mut warned_leases = Vec::new();
        // Under verify-only a submit promotes nothing: each session delivers
        // through its own pull request, so a conflict is resolved when one of
        // them merges, and refusing here only stalls an agent. The overlap is
        // still reported, with how to coordinate. `auto` and `manual` keep
        // the block because they promote onto a shared integration branch.
        let policy = if conflicting_leases.is_empty() {
            None
        } else {
            Some(self.lease_refusal_policy()?)
        };
        if let (Some(policy), false) = (policy.as_ref(), block_only_on_conflicts) {
            // Guarded exec: the same rule as a claim. Leases of stale or idle
            // holders, and every lease under verify-only, are reported only.
            let (block, warn): (Vec<_>, Vec<_>) = conflicting_leases
                .into_iter()
                .map(|mut blocker| {
                    let blocks = policy.may_block(&blocker);
                    blocker.reason = Some(if blocks {
                        "explicitly claimed by a session that is actively working".into()
                    } else {
                        policy.non_blocking_reason(session_id, &blocker)
                    });
                    (blocker, blocks)
                })
                .partition(|(_, blocks)| *blocks);
            conflicting_leases = block.into_iter().map(|(blocker, _)| blocker).collect();
            warned_leases = warn.into_iter().map(|(blocker, _)| blocker).collect();
        }
        if let (Some(policy), true) = (policy.as_ref(), block_only_on_conflicts) {
            let verify_only = policy.verify_only;
            // A lease refresh pairs only sessions still working, so a stale
            // holder's pair with this session may never have been classified.
            // Classify just this session's pairs with its blockers: bounded by
            // the blockers, and the verdict tells the agent whether the stale
            // work it overlaps would actually conflict.
            let holders: std::collections::BTreeSet<i64> = conflicting_leases
                .iter()
                .map(|blocker| blocker.session_id)
                .chain(std::iter::once(session_id))
                .collect();
            let own: Vec<crate::Overlap> = crate::detect_overlaps(
                &leases
                    .iter()
                    .filter(|lease| holders.contains(&lease.session_id))
                    .cloned()
                    .collect::<Vec<_>>(),
            )
            .into_iter()
            .filter(|overlap| overlap.session_a == session_id || overlap.session_b == session_id)
            .collect();
            self.classify_overlap_subset(&own)?;
            let overlaps = crate::detect_overlaps(&leases);
            let (block, warn): (Vec<_>, Vec<_>) = conflicting_leases
                .into_iter()
                .map(|mut blocker| {
                    let pair = self.overlap_pair_between(&overlaps, session_id, blocker.session_id);
                    let conflicting = pair.as_ref().is_some_and(|pair| {
                        pair.conflicting_paths
                            .iter()
                            .any(|path| paths_overlap(&blocker.path, path))
                    });
                    blocker.severity = Some(
                        if conflicting { "high" } else { "low" }.to_string(),
                    );
                    let holder_active =
                        policy.live.get(&blocker.session_id) == Some(&SessionStatus::Active);
                    blocker.reason = Some(match (conflicting, holder_active) {
                        (true, true) if verify_only => format!(
                            "the holder is actively working and Git reports a conflict with this \
                             session's edits; this repository delivers through pull requests \
                             (verify-only), so the conflict is resolved when one of them merges. \
                             Coordinate: aethyme broker advanced note send --session {session_id} \
                             --to-session {} --message \"<who lands the shared change first>\", \
                             or follow the shared-edit advice in aethyme broker status",
                            blocker.session_id
                        ),
                        (true, true) => "the holder is actively working and Git reports a conflict with this session's edits".into(),
                        (true, false) => "Git reports a conflict, but the holder is not actively working".into(),
                        (false, _) => pair
                            .map(|pair| pair.reason)
                            .unwrap_or_else(|| "the holder has not edited this path".into()),
                    });
                    let blocks = conflicting && policy.may_block(&blocker);
                    (blocker, blocks)
                })
                .partition(|(_, blocks)| *blocks);
            conflicting_leases = block.into_iter().map(|(blocker, _)| blocker).collect();
            warned_leases = warn.into_iter().map(|(blocker, _)| blocker).collect();
        }
        missing_lease_paths.sort();
        missing_lease_paths.dedup();
        foreign_paths.sort();
        foreign_paths.dedup();
        let ok = missing_lease_paths.is_empty()
            && conflicting_leases.is_empty()
            && foreign_paths.is_empty();
        Ok(OwnershipAuditReport {
            session_id,
            base_commit: base.to_string(),
            head_commit: head.to_string(),
            changed_paths: changed,
            missing_lease_paths,
            conflicting_leases,
            warned_leases,
            foreign_paths,
            ok,
        })
    }

    pub(super) fn lease_blockers(
        &self,
        session_id: i64,
        path: &str,
    ) -> Result<Vec<LeaseBlocker>, BrokerOpError> {
        use crate::leases::paths_overlap;

        let mut blockers: Vec<LeaseBlocker> = self
            .store
            .active_leases()?
            .into_iter()
            .filter(|lease| lease.session_id != session_id)
            .filter(|lease| paths_overlap(&lease.path, path))
            .map(|lease| LeaseBlocker {
                session_id: lease.session_id,
                holder_status: self.lease_holder_status(lease.session_id),
                holder_context: self.lease_holder_context(lease.session_id),
                path: lease.path,
                kind: lease.kind,
                severity: None,
                reason: None,
            })
            .collect();
        blockers.sort_by(|a, b| {
            (a.session_id, a.path.as_str(), a.kind.as_str()).cmp(&(
                b.session_id,
                b.path.as_str(),
                b.kind.as_str(),
            ))
        });
        blockers
            .dedup_by(|a, b| a.session_id == b.session_id && a.path == b.path && a.kind == b.kind);
        Ok(blockers)
    }

    /// Run a command inside a session worktree and fail the guard when it
    /// changes paths outside explicit leases. Both newly dirty paths and
    /// content changes to paths that were already dirty are attributed to
    /// the command. Its exit status remains separate so callers can
    /// distinguish command failure from ownership failure.
    pub fn guarded_exec(
        &mut self,
        session_id: i64,
        command: &[String],
    ) -> Result<GuardedExecReport, BrokerOpError> {
        self.guarded_exec_with_env(session_id, command, &[])
    }

    pub(crate) fn guarded_exec_with_env(
        &mut self,
        session_id: i64,
        command: &[String],
        environment: &[(String, String)],
    ) -> Result<GuardedExecReport, BrokerOpError> {
        self.guarded_exec_with_env_and_heartbeat(session_id, command, environment, || Ok(()))
    }

    pub(crate) fn guarded_exec_with_env_and_heartbeat<F>(
        &mut self,
        session_id: i64,
        command: &[String],
        environment: &[(String, String)],
        mut heartbeat: F,
    ) -> Result<GuardedExecReport, BrokerOpError>
    where
        F: FnMut() -> Result<(), BrokerOpError>,
    {
        if command.is_empty() {
            return Err(BrokerOpError::MissingExecCommand);
        }
        let session = self.store.session(session_id)?;
        self.refresh_leases_including(Some(session_id))?;
        let checkout = GitRepo::discover(Path::new(&session.worktree_path))?;
        let mut before_dirty = checkout.dirty_paths()?;
        before_dirty.sort();
        before_dirty.dedup();
        let before_untracked = checkout
            .untracked_paths()?
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        let before_identities = snapshot_path_identities(checkout.root(), &before_dirty)?;

        let mut child = Command::new(&command[0]);
        child
            .args(&command[1..])
            .current_dir(&session.worktree_path)
            .env("AETHYME_BROKER_SESSION_ID", session_id.to_string())
            .env("AETHYME_GATE_WORKER_ID", format!("s{session_id}-exec"))
            .env("AETHYME_TEST_DB_SUFFIX", format!("s{session_id}-exec"));
        for (name, value) in environment {
            child.env(name, value);
        }
        let mut child = child.spawn().map_err(|source| BrokerOpError::Spawn {
            command: command.join(" "),
            source,
        })?;
        let status = loop {
            if let Some(status) = child.try_wait().map_err(|source| BrokerOpError::Spawn {
                command: command.join(" "),
                source,
            })? {
                break status;
            }
            if let Err(error) = heartbeat() {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        };

        let mut after_dirty = checkout.dirty_paths()?;
        after_dirty.sort();
        after_dirty.dedup();
        let before_set: std::collections::BTreeSet<String> = before_dirty.iter().cloned().collect();
        let newly_dirty_paths: Vec<String> = after_dirty
            .iter()
            .filter(|path| !before_set.contains(*path))
            .cloned()
            .collect();
        let mut new_untracked_paths = checkout
            .untracked_paths()?
            .into_iter()
            .filter(|path| !before_untracked.contains(path))
            .collect::<Vec<_>>();
        new_untracked_paths.sort();
        new_untracked_paths.dedup();
        let modified_preexisting_dirty_paths: Vec<String> = before_dirty
            .iter()
            .filter_map(|path| {
                let before = before_identities.get(path)?;
                match working_path_identity(checkout.root(), path) {
                    Ok(after) if &after != before => Some(Ok(path.clone())),
                    Ok(_) => None,
                    Err(error) => Some(Err(error)),
                }
            })
            .collect::<Result<_, BrokerOpError>>()?;
        let mut touched = newly_dirty_paths.clone();
        touched.extend(modified_preexisting_dirty_paths.iter().cloned());
        touched.sort();
        touched.dedup();
        let audit = self.audit_paths(
            session_id,
            "GUARDED_EXEC_BEFORE",
            "GUARDED_EXEC_AFTER",
            touched.clone(),
            false,
            false,
        )?;
        let command_success = status.success();
        let ok = command_success && audit.ok;
        let report = GuardedExecReport {
            session_id,
            command: command.to_vec(),
            exit_code: status.code(),
            command_success,
            before_dirty_paths: before_dirty,
            after_dirty_paths: after_dirty,
            newly_dirty_paths,
            new_untracked_paths,
            modified_preexisting_dirty_paths,
            touched_paths: touched,
            outside_lease_paths: audit.missing_lease_paths,
            foreign_paths: audit.foreign_paths,
            ok,
        };
        if !report.outside_lease_paths.is_empty() {
            self.store.append_event(
                crate::events::GUARD_OUT_OF_LEASE_WRITE,
                Some(session_id),
                Some(&crate::events::guard_paths_payload(
                    &report.outside_lease_paths,
                )),
            )?;
        }
        if !report.new_untracked_paths.is_empty() {
            self.store.append_event(
                crate::events::GUARD_UNTRACKED_ARTIFACT,
                Some(session_id),
                Some(&crate::events::guard_paths_payload(
                    &report.new_untracked_paths,
                )),
            )?;
        }
        Ok(report)
    }
}
