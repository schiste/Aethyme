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

            let pid_alive = session.pid.map(session_pid_alive);
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
        self.reconcile_worktree_directories_until(sized, None)
    }

    pub(crate) fn reconcile_worktree_directories_until(
        &self,
        sized: bool,
        deadline: Option<std::time::Instant>,
    ) -> Result<WorktreeReconciliation, BrokerOpError> {
        self.require_bound_repository_worktree_root("worktree-directory reconciliation")?;
        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            let mut summary = crate::worktree_reconcile::summarise(&[], 0);
            summary.complete = false;
            summary.sized = false;
            return Ok(summary);
        }
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
        let mut complete = true;
        for root in &roots {
            if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                complete = false;
                break;
            }
            let Ok(entries) = std::fs::read_dir(root) else {
                // An unreadable root is not drift; claiming it held nothing
                // would understate the problem this sweep exists to surface.
                complete = false;
                continue;
            };
            for entry in entries.flatten() {
                if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                    complete = false;
                    break;
                }
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
                if let Some(deadline) = deadline {
                    entry.estimated_bytes = crate::disk_headroom::directory_usage_bounded(
                        Path::new(&entry.path),
                        deadline,
                    )
                    .map(|usage| usage.bytes);
                    if entry.estimated_bytes.is_none() {
                        complete = false;
                        break;
                    }
                } else {
                    entry.estimated_bytes = Some(
                        directory_size_without_following_links(Path::new(&entry.path)).unwrap_or(0),
                    );
                }
            }
        }
        let mut summary = crate::worktree_reconcile::summarise(&reconciled, roots.len());
        summary.complete = complete;
        if !complete {
            summary.sized = false;
        }
        Ok(summary)
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

    /// Derive current liveness without persisting status transitions. This is
    /// deliberately allowed to inspect PIDs and worktree metadata, but never
    /// changes a session row or appends an event.
    pub fn agents_snapshot(&self, now_ms: i64) -> Result<Vec<AgentView>, BrokerOpError> {
        let sessions = self.store.live_sessions()?;
        let mut views = Vec::with_capacity(sessions.len());
        for session in sessions {
            let fs_activity = worktree_activity_ms(&self.main_root, &session);
            let activity_at = fs_activity.unwrap_or(0).max(session.last_activity_at);
            let pid_alive = session.pid.map(session_pid_alive);
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
        let deadline = match sizing {
            crate::WorktreeSizing::Measure => None,
            crate::WorktreeSizing::Bounded { budget } => Some(std::time::Instant::now() + budget),
        };
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
        let mut discovery_deferred = false;
        for root in roots {
            if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                discovery_deferred = true;
                break;
            }
            let entries = match std::fs::read_dir(&root) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => {
                    discovery_deferred = true;
                    continue;
                }
            };
            for entry in entries {
                if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                    discovery_deferred = true;
                    break;
                }
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(_) => {
                        discovery_deferred = true;
                        continue;
                    }
                };
                match entry.file_type() {
                    Ok(kind) if kind.is_dir() => {}
                    Ok(_) => continue,
                    Err(_) => {
                        discovery_deferred = true;
                        continue;
                    }
                }
                let path = entry.path();
                let label = entry.file_name().to_string_lossy().into_owned();
                if is_checkout(&path) {
                    // A root that is itself a checkout.
                    worktrees.entry(path).or_insert(label);
                    continue;
                }
                let children = match std::fs::read_dir(&path) {
                    Ok(children) => children,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(_) => {
                        discovery_deferred = true;
                        continue;
                    }
                };
                for child in children {
                    if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                        discovery_deferred = true;
                        break;
                    }
                    let child = match child {
                        Ok(child) => child,
                        Err(_) => {
                            discovery_deferred = true;
                            continue;
                        }
                    };
                    match child.file_type() {
                        Ok(kind) if kind.is_dir() => {}
                        Ok(_) => continue,
                        Err(_) => {
                            discovery_deferred = true;
                            continue;
                        }
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
        let mut report =
            crate::worktree_report::build_with_deadline(&worktrees, &live, sizing, deadline);
        if discovery_deferred {
            report.deferred_checks.push("worktree_discovery".into());
        }
        let registrations = match deadline {
            Some(deadline) if std::time::Instant::now() >= deadline => {
                report.deferred_checks.push("git_inventory".into());
                None
            }
            Some(deadline) => {
                let _git_deadline = crate::git::limit_git_until(deadline);
                match self.repo.worktree_inventory() {
                    Ok(entries) => Some(entries),
                    Err(crate::GitError::TimedOut { .. }) => {
                        report.deferred_checks.push("git_inventory".into());
                        None
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            None => Some(self.repo.worktree_inventory()?),
        };
        if let Some(registrations) = registrations {
            crate::worktree_report::append_prunable_registrations_until(
                &mut report,
                &plan.repository_key,
                &registrations,
                &live,
                deadline,
            );
        }
        report.deferred_checks.sort();
        report.deferred_checks.dedup();
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
            let Some(paths) = implicit_lease_paths(
                &session,
                &rules,
                integration.as_deref(),
                upstream.as_deref(),
            ) else {
                continue;
            };
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
        Ok(LeaseRefusalPolicy::new(verify_only, live))
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

    /// [`Self::audit_submit_ownership`] without writing anything: the leases
    /// are the ones its refresh would set, held in memory, and overlapping
    /// pairs are classified without the shared cache. For building a
    /// candidate for inspection (#663). Session liveness is read as a
    /// snapshot, so a session `agents` would abandon on this call still
    /// counts as live here.
    pub(crate) fn audit_submit_ownership_read_only(
        &self,
        session_id: i64,
    ) -> Result<OwnershipAuditReport, BrokerOpError> {
        use crate::leases::detect_overlaps;

        let session = self.store.session(session_id)?;
        let checkout = GitRepo::discover(Path::new(&session.worktree_path))?;
        let head = checkout.head_commit()?;
        let integration = self.integration_tip();
        let upstream = self.repo.upstream_default().map(|(_, commit)| commit);
        let base = integration
            .as_deref()
            .and_then(|tip| crate::merge::session_baseline(&checkout, tip, upstream.as_deref()))
            .or(session.diff_base.clone())
            .unwrap_or_else(|| "HEAD".to_string());
        let leases = self.leases_as_refreshed(session_id)?;
        let changed = self.repo.changed_between(&base, &head)?;
        let mut audit = self.audit_ownership_paths(session_id, &changed, &leases, true)?;
        if !audit.conflicting.is_empty() {
            let policy = self.lease_refusal_policy_snapshot()?;
            let holders: std::collections::BTreeSet<i64> = audit
                .conflicting
                .iter()
                .map(|blocker| blocker.session_id)
                .chain(std::iter::once(session_id))
                .collect();
            let own: Vec<crate::Overlap> = detect_overlaps(
                &leases
                    .iter()
                    .filter(|lease| holders.contains(&lease.session_id))
                    .cloned()
                    .collect::<Vec<_>>(),
            )
            .into_iter()
            .filter(|overlap| overlap.session_a == session_id || overlap.session_b == session_id)
            .collect();
            let pairs = self.classify_overlaps_now(&own)?;
            let pair_with = |holder: i64| {
                let key = (session_id.min(holder), session_id.max(holder));
                pairs
                    .iter()
                    .find(|pair| (pair.session_a, pair.session_b) == key)
                    .cloned()
            };
            audit.judge_on_conflicts(session_id, &policy, pair_with);
        }
        Ok(audit.into_report(session_id, &base, &head, changed))
    }

    /// The active leases as [`Self::refresh_leases_including`] with `acting`
    /// would leave them, computed without writing: each coordinating
    /// session's implicit leases are replaced by the paths it changed now.
    pub(crate) fn leases_as_refreshed(
        &self,
        acting: i64,
    ) -> Result<Vec<crate::types::Lease>, BrokerOpError> {
        use crate::leases::LeaseIgnoreRules;

        let rules = LeaseIgnoreRules::load(&self.main_root);
        let integration = self.integration_tip();
        let upstream = self.repo.upstream_default().map(|(_, commit)| commit);
        let mut coordinating = self.coordinating_session_ids()?;
        coordinating.insert(acting);
        let mut refreshed = std::collections::BTreeMap::new();
        for session in self.store.live_sessions()? {
            if !coordinating.contains(&session.id) {
                continue;
            }
            if let Some(paths) = implicit_lease_paths(
                &session,
                &rules,
                integration.as_deref(),
                upstream.as_deref(),
            ) {
                refreshed.insert(session.id, paths);
            }
        }
        let now = now_ms();
        let mut leases: Vec<crate::types::Lease> = self
            .store
            .active_leases()?
            .into_iter()
            .filter(|lease| {
                !(lease.kind == LeaseKind::Implicit && refreshed.contains_key(&lease.session_id))
            })
            .collect();
        for (session_id, paths) in refreshed {
            leases.extend(paths.into_iter().map(|path| crate::types::Lease {
                id: 0,
                session_id,
                path,
                kind: LeaseKind::Implicit,
                created_at: now,
                expires_at: None,
                released_at: None,
            }));
        }
        Ok(leases)
    }

    /// [`Self::lease_refusal_policy`] from a liveness snapshot, persisting no
    /// status transition.
    pub(crate) fn lease_refusal_policy_snapshot(
        &self,
    ) -> Result<LeaseRefusalPolicy, BrokerOpError> {
        let mut live = self
            .agents_snapshot(crate::clock::epoch_ms())?
            .into_iter()
            .filter(|agent| {
                matches!(
                    agent.derived_status,
                    SessionStatus::Active | SessionStatus::Idle
                )
            })
            .map(|agent| (agent.session.id, agent.derived_status))
            .collect::<std::collections::HashMap<_, _>>();
        for session_id in self.sessions_released_by_grace()? {
            live.remove(&session_id);
        }
        Ok(LeaseRefusalPolicy::new(
            LeaseRefusalPolicy::verify_only_at(&self.main_root),
            live,
        ))
    }

    pub(super) fn audit_paths(
        &mut self,
        session_id: i64,
        base: &str,
        head: &str,
        changed: Vec<String>,
        allow_implicit: bool,
        // Submit only: another session's lease blocks when the shared
        // `LeaseRefusalPolicy` allows it AND the two sessions' edits conflict.
        // Guarded exec applies the policy alone, like a lease claim.
        block_only_on_conflicts: bool,
    ) -> Result<OwnershipAuditReport, BrokerOpError> {
        let leases = self.store.active_leases()?;
        let mut audit =
            self.audit_ownership_paths(session_id, &changed, &leases, allow_implicit)?;
        if !audit.conflicting.is_empty() {
            let policy = self.lease_refusal_policy()?;
            if block_only_on_conflicts {
                // A lease refresh pairs only sessions still working, so a stale
                // holder's pair with this session may never have been classified.
                // Classify just this session's pairs with its blockers: bounded by
                // the blockers, and the verdict tells the agent whether the stale
                // work it overlaps would actually conflict.
                let holders: std::collections::BTreeSet<i64> = audit
                    .conflicting
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
                .filter(|overlap| {
                    overlap.session_a == session_id || overlap.session_b == session_id
                })
                .collect();
                self.classify_overlap_subset(&own)?;
                let overlaps = crate::detect_overlaps(&leases);
                audit.judge_on_conflicts(session_id, &policy, |holder| {
                    self.overlap_pair_between(&overlaps, session_id, holder)
                });
            } else {
                // Guarded exec: the same rule as a claim. Leases of stale or idle
                // holders, and every lease under verify-only, are reported only.
                audit.judge_by_policy(session_id, &policy);
            }
        }
        Ok(audit.into_report(session_id, base, head, changed))
    }

    /// The path checks of an ownership audit against `leases`. Another
    /// session's overlapping explicit lease is collected in `conflicting`
    /// for the caller to judge.
    fn audit_ownership_paths(
        &self,
        session_id: i64,
        changed: &[String],
        leases: &[crate::types::Lease],
        allow_implicit: bool,
    ) -> Result<PathAudit, BrokerOpError> {
        use crate::leases::{LeaseIgnoreRules, paths_overlap};

        let mut changed = changed.to_vec();
        changed.sort();
        changed.dedup();
        let rules = LeaseIgnoreRules::load(&self.main_root);
        let foreign = self.store.session_foreign_files(session_id)?;
        let mut audit = PathAudit::default();

        for path in changed.iter().filter(|path| !rules.is_ignored(path)) {
            let owns = leases.iter().any(|lease| {
                lease.session_id == session_id
                    && (allow_implicit || lease.kind == LeaseKind::Explicit)
                    && paths_overlap(&lease.path, path)
            });
            if !owns {
                audit.missing.push(path.clone());
            }

            for blocker in leases
                .iter()
                .filter(|lease| lease.session_id != session_id)
                .filter(|lease| lease.kind == LeaseKind::Explicit)
                .filter(|lease| paths_overlap(&lease.path, path))
            {
                audit.conflicting.push(LeaseBlocker {
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
                audit.foreign.push(path.clone());
            }
        }

        audit.conflicting.sort_by(|a, b| {
            (a.session_id, a.path.as_str(), a.kind.as_str()).cmp(&(
                b.session_id,
                b.path.as_str(),
                b.kind.as_str(),
            ))
        });
        audit
            .conflicting
            .dedup_by(|a, b| a.session_id == b.session_id && a.path == b.path && a.kind == b.kind);
        Ok(audit)
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
        heartbeat: F,
    ) -> Result<GuardedExecReport, BrokerOpError>
    where
        F: FnMut() -> Result<(), BrokerOpError>,
    {
        self.guarded_exec_with_env_heartbeat_and_mode(
            session_id,
            command,
            environment,
            heartbeat,
            crate::inline_exec_supervisor::generic_exec_mode,
        )
    }

    fn guarded_exec_with_env_heartbeat_and_mode<F, M>(
        &mut self,
        session_id: i64,
        command: &[String],
        environment: &[(String, String)],
        mut heartbeat: F,
        select_mode: M,
    ) -> Result<GuardedExecReport, BrokerOpError>
    where
        F: FnMut() -> Result<(), BrokerOpError>,
        M: FnOnce() -> crate::inline_exec_supervisor::GenericExecMode,
    {
        let supervisor_executable = self.agent_supervisor_executable.clone();
        self.guarded_exec_with_runner(session_id, command, move |session| {
            let mut execution_environment = vec![
                (
                    "AETHYME_BROKER_SESSION_ID".to_owned(),
                    session_id.to_string(),
                ),
                (
                    "AETHYME_GATE_WORKER_ID".to_owned(),
                    format!("s{session_id}-exec"),
                ),
                (
                    "AETHYME_TEST_DB_SUFFIX".to_owned(),
                    format!("s{session_id}-exec"),
                ),
            ];
            execution_environment.extend_from_slice(environment);

            let result = match select_mode() {
                crate::inline_exec_supervisor::GenericExecMode::Supervised => {
                    let executable = crate::agent_supervisor::resolve_executable(
                        supervisor_executable.as_deref(),
                    )
                    .map_err(|source| BrokerOpError::Spawn {
                        command: command.join(" "),
                        source,
                    })?;
                    let supervisor =
                        crate::inline_exec_supervisor::InlineExecSupervisor::start_inheriting_stdin(
                            &executable,
                        )
                        .map_err(|error| map_guarded_exec_supervisor_error(session_id, command, error))?;
                    let result = supervisor
                        .run(
                            Path::new(&session.worktree_path),
                            command,
                            &execution_environment,
                            &mut heartbeat,
                        )
                        .map_err(|error| map_guarded_exec_supervisor_error(session_id, command, error))?;
                    GuardedCommandResult {
                        exit_code: result.exit_code,
                        succeeded: result.succeeded,
                    }
                }
                crate::inline_exec_supervisor::GenericExecMode::Legacy(reason) => {
                    eprintln!(
                        "warning: guarded exec for session {session_id} is using the legacy unsupervised runner ({reason}); descendant cleanup is not verified"
                    );
                    let mut child = Command::new(&command[0]);
                    child
                        .args(&command[1..])
                        .current_dir(&session.worktree_path);
                    for (name, value) in &execution_environment {
                        child.env(name, value);
                    }
                    let mut child = child.spawn().map_err(|source| BrokerOpError::Spawn {
                        command: command.join(" "),
                        source,
                    })?;
                    let status = loop {
                        if let Some(status) = child.try_wait().map_err(|source| {
                            BrokerOpError::Spawn {
                                command: command.join(" "),
                                source,
                            }
                        })? {
                            break status;
                        }
                        if let Err(error) = heartbeat() {
                            if let Err(cleanup_error) = child.kill() {
                                crate::warn_unrecorded(
                                    "stop guarded command after heartbeat failure",
                                    Err::<(), _>(cleanup_error),
                                );
                            }
                            if let Err(cleanup_error) = child.wait() {
                                crate::warn_unrecorded(
                                    "reap guarded command after heartbeat failure",
                                    Err::<(), _>(cleanup_error),
                                );
                            }
                            return Err(error);
                        }
                        std::thread::sleep(std::time::Duration::from_millis(200));
                    };
                    GuardedCommandResult {
                        exit_code: status.code(),
                        succeeded: status.success(),
                    }
                }
            };
            Ok(result)
        })
    }

    pub(crate) fn start_preparation_exec_supervisor(
        &self,
    ) -> Result<crate::inline_exec_supervisor::InlineExecSupervisor, BrokerOpError> {
        let executable = crate::agent_supervisor::resolve_executable(
            self.agent_supervisor_executable.as_deref(),
        )
        .map_err(|error| crate::PreparationError::SupervisorUnavailable {
            reason: error.to_string(),
        })?;
        crate::inline_exec_supervisor::InlineExecSupervisor::start(&executable).map_err(|error| {
            match error {
                crate::inline_exec_supervisor::InlineExecError::CleanupUnknown(reason) => {
                    crate::PreparationError::SupervisorUnavailable {
                        reason: format!("startup helper cleanup could not be verified: {reason}"),
                    }
                    .into()
                }
                crate::inline_exec_supervisor::InlineExecError::CommandOutcomeUnknown(reason) => {
                    crate::PreparationError::SupervisorUnavailable {
                        reason: format!("unexpected startup-helper result state: {reason}"),
                    }
                    .into()
                }
                crate::inline_exec_supervisor::InlineExecError::OperationAfterDispatch(error) => {
                    crate::PreparationError::SupervisorUnavailable {
                        reason: format!(
                            "unexpected post-dispatch supervisor state while starting helper: {error}"
                        ),
                    }
                    .into()
                }
                crate::inline_exec_supervisor::InlineExecError::Setup(error) => {
                    crate::PreparationError::SupervisorUnavailable {
                        reason: error.to_string(),
                    }
                    .into()
                }
                crate::inline_exec_supervisor::InlineExecError::CommandStart(error) => {
                    crate::PreparationError::SupervisorUnavailable {
                        reason: error.to_string(),
                    }
                    .into()
                }
            }
        })
    }

    pub(crate) fn guarded_preparation_exec_with_supervisor<F>(
        &mut self,
        session_id: i64,
        command: &[String],
        environment: &[(String, String)],
        supervisor: crate::inline_exec_supervisor::InlineExecSupervisor,
        heartbeat: F,
    ) -> Result<GuardedExecReport, BrokerOpError>
    where
        F: FnMut() -> Result<(), BrokerOpError>,
    {
        if command.is_empty() {
            return Err(BrokerOpError::MissingExecCommand);
        }
        let mut execution_environment = vec![
            (
                "AETHYME_BROKER_SESSION_ID".to_owned(),
                session_id.to_string(),
            ),
            (
                "AETHYME_GATE_WORKER_ID".to_owned(),
                format!("s{session_id}-exec"),
            ),
            (
                "AETHYME_TEST_DB_SUFFIX".to_owned(),
                format!("s{session_id}-exec"),
            ),
        ];
        execution_environment.extend_from_slice(environment);
        self.guarded_exec_with_runner(session_id, command, move |session| {
            let result = supervisor
                .run(
                    Path::new(&session.worktree_path),
                    command,
                    &execution_environment,
                    heartbeat,
                )
                .map_err(|error| {
                    map_preparation_exec_supervisor_error(session_id, command, error)
                })?;
            Ok(GuardedCommandResult {
                exit_code: result.exit_code,
                succeeded: result.succeeded,
            })
        })
    }

    fn guarded_exec_with_runner<F>(
        &mut self,
        session_id: i64,
        command: &[String],
        run: F,
    ) -> Result<GuardedExecReport, BrokerOpError>
    where
        F: FnOnce(&crate::types::Session) -> Result<GuardedCommandResult, BrokerOpError>,
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

        let status = run(&session)?;

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
        let command_success = status.succeeded;
        let ok = command_success && audit.ok;
        let report = GuardedExecReport {
            session_id,
            command: command.to_vec(),
            exit_code: status.exit_code,
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

fn map_preparation_exec_supervisor_error(
    session_id: i64,
    command: &[String],
    error: crate::inline_exec_supervisor::InlineExecError,
) -> BrokerOpError {
    match error {
        crate::inline_exec_supervisor::InlineExecError::CleanupUnknown(reason) => {
            crate::PreparationError::CleanupUnknown { session_id, reason }.into()
        }
        crate::inline_exec_supervisor::InlineExecError::CommandOutcomeUnknown(reason) => {
            crate::PreparationError::CommandOutcomeUnknown { session_id, reason }.into()
        }
        crate::inline_exec_supervisor::InlineExecError::OperationAfterDispatch(error) => {
            crate::PreparationError::CommandOutcomeUnknown {
                session_id,
                reason: format!("supervisor operation failed after command dispatch: {error}"),
            }
            .into()
        }
        crate::inline_exec_supervisor::InlineExecError::Setup(error) => {
            crate::PreparationError::SupervisorUnavailable {
                reason: error.to_string(),
            }
            .into()
        }
        crate::inline_exec_supervisor::InlineExecError::CommandStart(source) => {
            BrokerOpError::Spawn {
                command: command.join(" "),
                source,
            }
        }
    }
}

fn map_guarded_exec_supervisor_error(
    session_id: i64,
    command: &[String],
    error: crate::inline_exec_supervisor::InlineExecError,
) -> BrokerOpError {
    match error {
        crate::inline_exec_supervisor::InlineExecError::CleanupUnknown(reason) => {
            BrokerOpError::GuardedExecCleanupUnknown { session_id, reason }
        }
        crate::inline_exec_supervisor::InlineExecError::CommandOutcomeUnknown(reason) => {
            BrokerOpError::GuardedExecOutcomeUnknown { session_id, reason }
        }
        crate::inline_exec_supervisor::InlineExecError::OperationAfterDispatch(error) => {
            BrokerOpError::GuardedExecOutcomeUnknown {
                session_id,
                reason: format!("supervisor operation failed after command dispatch: {error}"),
            }
        }
        crate::inline_exec_supervisor::InlineExecError::Setup(source) => BrokerOpError::Spawn {
            command: command.join(" "),
            source,
        },
        crate::inline_exec_supervisor::InlineExecError::CommandStart(source) => {
            BrokerOpError::Spawn {
                command: command.join(" "),
                source,
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct GuardedCommandResult {
    exit_code: Option<i32>,
    succeeded: bool,
}

/// The paths a lease refresh records as `session`'s implicit leases, or
/// `None` where the refresh leaves them as they are.
fn implicit_lease_paths(
    session: &crate::types::Session,
    rules: &crate::leases::LeaseIgnoreRules,
    integration: Option<&str>,
    upstream: Option<&str>,
) -> Option<Vec<String>> {
    let checkout = GitRepo::discover(Path::new(&session.worktree_path)).ok()?;
    // #41: derive the baseline instead of trusting the stored
    // adoption-time diff_base — after a conflict-rebase the stored
    // value inflates the diff with everyone else's promoted work.
    let base = integration
        .and_then(|tip| crate::merge::session_baseline(&checkout, tip, upstream))
        .or_else(|| session.diff_base.clone())
        .unwrap_or_else(|| "HEAD".to_string());
    let changed = checkout.changed_files(&base).ok()?;
    Some(
        changed
            .into_iter()
            .filter(|path| !rules.is_ignored(path))
            .collect(),
    )
}

/// An ownership audit between its path checks and its verdict.
#[derive(Default)]
struct PathAudit {
    missing: Vec<String>,
    conflicting: Vec<LeaseBlocker>,
    warned: Vec<LeaseBlocker>,
    foreign: Vec<String>,
}

impl PathAudit {
    /// Guarded exec: the policy alone decides, as for a lease claim.
    fn judge_by_policy(&mut self, session_id: i64, policy: &LeaseRefusalPolicy) {
        let (block, warn): (Vec<_>, Vec<_>) = std::mem::take(&mut self.conflicting)
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
        self.conflicting = block.into_iter().map(|(blocker, _)| blocker).collect();
        self.warned = warn.into_iter().map(|(blocker, _)| blocker).collect();
    }

    /// Submit: a lease blocks only when the policy allows it and Git reports
    /// a conflict between the two sessions' edits on it. `pair_with` gives
    /// the classified pair of this session with a holder.
    fn judge_on_conflicts(
        &mut self,
        session_id: i64,
        policy: &LeaseRefusalPolicy,
        pair_with: impl Fn(i64) -> Option<crate::OverlapPair>,
    ) {
        use crate::leases::paths_overlap;

        let verify_only = policy.is_verify_only();
        let (block, warn): (Vec<_>, Vec<_>) = std::mem::take(&mut self.conflicting)
            .into_iter()
            .map(|mut blocker| {
                let pair = pair_with(blocker.session_id);
                let conflicting = pair.as_ref().is_some_and(|pair| {
                    pair.conflicting_paths
                        .iter()
                        .any(|path| paths_overlap(&blocker.path, path))
                });
                blocker.severity = Some(if conflicting { "high" } else { "low" }.to_string());
                let holder_active = policy.status_of(blocker.session_id) == Some(SessionStatus::Active);
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
        self.conflicting = block.into_iter().map(|(blocker, _)| blocker).collect();
        self.warned = warn.into_iter().map(|(blocker, _)| blocker).collect();
    }

    fn into_report(
        mut self,
        session_id: i64,
        base: &str,
        head: &str,
        mut changed: Vec<String>,
    ) -> OwnershipAuditReport {
        changed.sort();
        changed.dedup();
        self.missing.sort();
        self.missing.dedup();
        self.foreign.sort();
        self.foreign.dedup();
        let ok = self.missing.is_empty() && self.conflicting.is_empty() && self.foreign.is_empty();
        OwnershipAuditReport {
            session_id,
            base_commit: base.to_string(),
            head_commit: head.to_string(),
            changed_paths: changed,
            missing_lease_paths: self.missing,
            conflicting_leases: self.conflicting,
            warned_leases: self.warned,
            foreign_paths: self.foreign,
            ok,
        }
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod generic_guarded_exec_tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn fixture() -> (tempfile::TempDir, crate::test_storage::TestStorage, Broker) {
        let repository = tempfile::tempdir().unwrap();
        git(repository.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(repository.path().join("tracked.txt"), "before\n").unwrap();
        git(repository.path(), &["add", "tracked.txt"]);
        git(repository.path(), &["commit", "-qm", "fixture"]);
        let storage = crate::test_storage::TestStorage::new();
        let broker = storage.open(repository.path()).unwrap();
        (repository, storage, broker)
    }

    #[test]
    fn forced_supervised_generic_exec_preserves_environment_exit_and_ownership_audit() {
        let (_repository, _storage, mut broker) = fixture();
        let session = broker.start_worktree("generic guarded exec", None).unwrap();
        broker.claim_lease(session.id, "tracked.txt", None).unwrap();
        let mut heartbeats = 0;
        let report = broker
            .guarded_exec_with_env_heartbeat_and_mode(
                session.id,
                &[
                    "/bin/sh".into(),
                    "-c".into(),
                    "printf '%s:%s' \"$AETHYME_BROKER_SESSION_ID\" \"$CUSTOM_VALUE\" > tracked.txt; exit 7".into(),
                ],
                &[
                    ("AETHYME_BROKER_SESSION_ID".into(), "user-override".into()),
                    ("CUSTOM_VALUE".into(), "preserved".into()),
                ],
                || {
                    heartbeats += 1;
                    Ok(())
                },
                || crate::inline_exec_supervisor::GenericExecMode::Supervised,
            )
            .unwrap();

        assert!(heartbeats > 0);
        assert_eq!(report.exit_code, Some(7));
        assert!(!report.command_success);
        assert!(!report.ok);
        assert!(report.outside_lease_paths.is_empty());
        assert_eq!(report.touched_paths, vec!["tracked.txt".to_owned()]);
        assert_eq!(
            std::fs::read_to_string(Path::new(&session.worktree_path).join("tracked.txt")).unwrap(),
            "user-override:preserved"
        );
    }

    #[test]
    fn generic_heartbeat_abort_marks_outcome_unknown_and_stops_descendant() {
        use std::time::Duration;

        let (_repository, _storage, mut broker) = fixture();
        let session = broker
            .start_worktree("heartbeat guarded exec", None)
            .unwrap();
        let output = Path::new(&session.worktree_path).join("descendant.txt");
        let mut heartbeats = 0;
        let error = broker
            .guarded_exec_with_env_heartbeat_and_mode(
                session.id,
                &[
                    "/bin/sh".into(),
                    "-c".into(),
                    r#"(i=0; while [ "$i" -lt 500 ]; do printf x >> "$OUTPUT_PATH"; sleep 0.02; i=$((i + 1)); done) & wait"#.into(),
                ],
                &[("OUTPUT_PATH".into(), output.to_string_lossy().into_owned())],
                || {
                    heartbeats += 1;
                    if heartbeats >= 3 {
                        Err(crate::PreparationError::LivenessDeadlineElapsed.into())
                    } else {
                        Ok(())
                    }
                },
                || crate::inline_exec_supervisor::GenericExecMode::Supervised,
            )
            .unwrap_err();

        assert!(
            matches!(
                &error,
                BrokerOpError::GuardedExecOutcomeUnknown { session_id, reason }
                    if *session_id == session.id
                        && reason == "supervisor operation failed after command dispatch: preparation cache liveness deadline elapsed"
            ),
            "post-dispatch heartbeat failure lost its liveness cause: {error:?}"
        );
        let bytes_after_abort = std::fs::metadata(&output).unwrap().len();
        assert!(bytes_after_abort > 0, "descendant never started writing");
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(
            std::fs::metadata(output).unwrap().len(),
            bytes_after_abort,
            "a descendant continued writing after guarded exec returned"
        );
    }

    #[test]
    fn empty_generic_exec_refuses_before_selecting_a_helper() {
        let (_repository, _storage, mut broker) = fixture();
        let error = broker
            .guarded_exec_with_env_heartbeat_and_mode(
                0,
                &[],
                &[],
                || Ok(()),
                || panic!("helper mode was selected for empty argv"),
            )
            .unwrap_err();
        assert!(matches!(error, BrokerOpError::MissingExecCommand));
    }

    #[test]
    fn wrong_helper_refuses_before_the_guarded_command_can_write() {
        use std::os::unix::fs::PermissionsExt as _;

        let (_repository, _storage, mut broker) = fixture();
        let session = broker.start_worktree("wrong guarded helper", None).unwrap();
        let wrong_helper = tempfile::tempdir().unwrap();
        let executable = wrong_helper.path().join("not-aethyme-helper");
        std::fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&executable, permissions).unwrap();
        broker = broker
            .with_agent_supervisor_executable(&executable)
            .unwrap();
        let marker = Path::new(&session.worktree_path).join("must-not-exist");

        let error = broker
            .guarded_exec_with_env_heartbeat_and_mode(
                session.id,
                &[
                    "/bin/sh".into(),
                    "-c".into(),
                    "printf launched > must-not-exist".into(),
                ],
                &[],
                || Ok(()),
                || crate::inline_exec_supervisor::GenericExecMode::Supervised,
            )
            .unwrap_err();

        assert!(matches!(error, BrokerOpError::Spawn { .. }), "{error}");
        assert!(
            !marker.exists(),
            "guarded command ran without a ready helper"
        );
    }

    #[test]
    fn supervised_spawn_errors_preserve_direct_command_io_error_kind_and_os_code() {
        use std::os::unix::fs::PermissionsExt as _;

        let (_repository, _storage, mut broker) = fixture();
        let session = broker.start_worktree("spawn error fidelity", None).unwrap();
        let candidates = tempfile::tempdir().unwrap();
        let missing = candidates.path().join("missing-command");
        let non_executable = candidates.path().join("not-executable");
        std::fs::write(&non_executable, "#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = std::fs::metadata(&non_executable).unwrap().permissions();
        permissions.set_mode(0o644);
        std::fs::set_permissions(&non_executable, permissions).unwrap();

        let cases = [
            (
                "missing executable",
                vec![missing.to_string_lossy().into_owned()],
            ),
            (
                "non-executable file",
                vec![non_executable.to_string_lossy().into_owned()],
            ),
            (
                "NUL argument",
                vec![
                    "/bin/sh".into(),
                    "-c".into(),
                    "exit 0".into(),
                    "bad\0argument".into(),
                ],
            ),
        ];

        for (description, command) in cases {
            let direct = Command::new(&command[0])
                .args(&command[1..])
                .spawn()
                .expect_err("direct command unexpectedly started");
            let guarded = broker
                .guarded_exec_with_env_heartbeat_and_mode(
                    session.id,
                    &command,
                    &[],
                    || Ok(()),
                    || crate::inline_exec_supervisor::GenericExecMode::Supervised,
                )
                .expect_err("supervised command unexpectedly started");
            let BrokerOpError::Spawn { source, .. } = guarded else {
                panic!("{description} returned an unexpected error: {guarded}");
            };
            assert_eq!(source.kind(), direct.kind(), "{description}");
            assert_eq!(
                source.raw_os_error(),
                direct.raw_os_error(),
                "{description}"
            );
        }
    }

    #[test]
    fn v1_helper_refuses_v2_before_the_guarded_command_can_write() {
        use std::os::unix::fs::PermissionsExt as _;

        let (_repository, _storage, mut broker) = fixture();
        let session = broker
            .start_worktree("old helper readiness refusal", None)
            .unwrap();
        let old_helper = tempfile::tempdir().unwrap();
        let executable = old_helper.path().join("v1-helper");
        // A v1 helper's reserved-prefix dispatcher exits 125 for the v2 flag
        // before sending Ready or receiving a Run frame.
        let helper_arg = old_helper.path().join("received-flag");
        std::fs::write(
            &executable,
            format!(
                "#!/bin/sh\nprintf '%s' \"$1\" > '{}'\ncase \"$1\" in\n  --aethyme-inline-exec-supervisor-v1) exit 0 ;;\n  --aethyme-inline-exec-supervisor*) exit 125 ;;\n  *) exit 125 ;;\nesac\n",
                helper_arg.display()
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&executable, permissions).unwrap();
        let mut broker = broker
            .with_agent_supervisor_executable(&executable)
            .unwrap();
        let marker = Path::new(&session.worktree_path).join("v1-helper-must-not-run-command");

        let error = broker
            .guarded_exec_with_env_heartbeat_and_mode(
                session.id,
                &[
                    "/bin/sh".into(),
                    "-c".into(),
                    "printf launched > v1-helper-must-not-run-command".into(),
                ],
                &[],
                || Ok(()),
                || crate::inline_exec_supervisor::GenericExecMode::Supervised,
            )
            .unwrap_err();

        assert!(matches!(error, BrokerOpError::Spawn { .. }), "{error}");
        assert!(
            error.to_string().contains("helper did not become ready"),
            "old helper was not rejected during readiness: {error}"
        );
        assert_eq!(
            std::fs::read_to_string(helper_arg).unwrap(),
            "--aethyme-inline-exec-supervisor-v2"
        );
        assert!(!marker.exists(), "the command ran with an old v1 helper");
    }

    #[test]
    fn public_guarded_exec_uses_supervisor_without_a_controlling_terminal() {
        const CHILD_MODE: &str = "AETHYME_TEST_GUARDED_EXEC_NO_CONTROLLING_TERMINAL_CHILD";
        if std::env::var_os(CHILD_MODE).is_none() {
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "broker::leases::generic_guarded_exec_tests::public_guarded_exec_uses_supervisor_without_a_controlling_terminal",
                    "--nocapture",
                    "--quiet",
                ])
                .env(CHILD_MODE, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "detached child failed with {}:\nstdout:\n{}\nstderr:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                String::from_utf8_lossy(&output.stdout)
                    .contains("test result: ok. 1 passed; 0 failed"),
                "detached child did not execute exactly one test:\n{}",
                String::from_utf8_lossy(&output.stdout)
            );
            return;
        }

        // The isolated child is not a process-group leader, so setsid creates
        // a new session with no controlling terminal before the public call.
        // SAFETY: This detached child is not a process-group leader; `setsid`
        // takes no pointers and changes only this child process's session.
        let session_id = unsafe { libc::setsid() };
        // SAFETY: `getpid` has no pointer or memory-safety preconditions.
        assert_eq!(session_id, unsafe { libc::getpid() });
        let terminal_error = std::fs::File::open("/dev/tty").unwrap_err();
        assert_eq!(terminal_error.raw_os_error(), Some(libc::ENXIO));

        use std::os::unix::fs::PermissionsExt as _;
        let (_repository, _storage, mut broker) = fixture();
        let session = broker
            .start_worktree("public guarded exec selector", None)
            .unwrap();
        let wrong_helper = tempfile::tempdir().unwrap();
        let executable = wrong_helper.path().join("not-aethyme-helper");
        std::fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&executable, permissions).unwrap();
        let mut broker = broker
            .with_agent_supervisor_executable(&executable)
            .unwrap();
        let marker = Path::new(&session.worktree_path).join("production-selector-sentinel");

        let result = broker.guarded_exec(
            session.id,
            &[
                "/bin/sh".into(),
                "-c".into(),
                "printf launched > production-selector-sentinel".into(),
            ],
        );

        assert!(
            !marker.exists(),
            "public guarded exec bypassed helper supervision"
        );
        let error = result.expect_err("public guarded exec accepted the incompatible helper");
        assert!(matches!(error, BrokerOpError::Spawn { .. }), "{error}");
        assert!(
            error.to_string().contains("helper did not become ready"),
            "public call did not reject the incompatible helper: {error}"
        );
    }

    #[test]
    fn supervisor_outcomes_map_to_typed_errors() {
        let command = vec!["command".to_owned()];
        let outcome = map_guarded_exec_supervisor_error(
            42,
            &command,
            crate::inline_exec_supervisor::InlineExecError::CommandOutcomeUnknown(
                "lost result".into(),
            ),
        );
        assert!(matches!(
            outcome,
            BrokerOpError::GuardedExecOutcomeUnknown { session_id: 42, .. }
        ));
        let after_dispatch = map_guarded_exec_supervisor_error(
            42,
            &command,
            crate::inline_exec_supervisor::InlineExecError::OperationAfterDispatch(
                BrokerOpError::MissingExecCommand,
            ),
        );
        assert!(matches!(
            after_dispatch,
            BrokerOpError::GuardedExecOutcomeUnknown { session_id: 42, reason }
                if reason.contains("after command dispatch")
        ));
        let cleanup = map_guarded_exec_supervisor_error(
            42,
            &command,
            crate::inline_exec_supervisor::InlineExecError::CleanupUnknown(
                "writer may remain".into(),
            ),
        );
        assert!(matches!(
            cleanup,
            BrokerOpError::GuardedExecCleanupUnknown { session_id: 42, .. }
        ));

        let preparation_unknown = map_preparation_exec_supervisor_error(
            42,
            &command,
            crate::inline_exec_supervisor::InlineExecError::OperationAfterDispatch(
                BrokerOpError::MissingExecCommand,
            ),
        );
        assert!(matches!(
            preparation_unknown,
            BrokerOpError::Preparation(crate::PreparationError::CommandOutcomeUnknown {
                session_id: 42,
                reason
            }) if reason.contains("after command dispatch")
        ));

        let setup = map_preparation_exec_supervisor_error(
            42,
            &command,
            crate::inline_exec_supervisor::InlineExecError::Setup(std::io::Error::other(
                "helper setup failed",
            )),
        );
        assert!(matches!(
            setup,
            BrokerOpError::Preparation(crate::PreparationError::SupervisorUnavailable {
                reason
            }) if reason.contains("helper setup failed")
        ));
        let command_start = map_preparation_exec_supervisor_error(
            42,
            &command,
            crate::inline_exec_supervisor::InlineExecError::CommandStart(std::io::Error::other(
                "command start failed",
            )),
        );
        assert!(matches!(
            command_start,
            BrokerOpError::Spawn { command: ref name, .. } if name == "command"
        ));
    }
}
