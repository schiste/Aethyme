//! Session-facing broker operations: the API `aethyme broker ...` wraps.
//!
//! Combines the git service layer with the store. Attach-first: `adopt`
//! is the primary registration path; `start_agent` layers worktree
//! creation + subprocess spawn on the same session model. No code here
//! may assume the broker owns the agent process.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::error::BrokerError;
use crate::git::{GitError, GitRepo};
use crate::store::BrokerStore;
use crate::types::{
    MergeQueueEntry, MergeStatus, NewSession, Session, SessionOrigin, SessionStatus,
};
use crate::version::VersionDriftReport;

/// Idle/stale thresholds for activity-derived liveness (issue #9).
/// Configurable via `.aethyme/config.toml` in a later phase; constants
/// for now, chosen so an agent "thinking" for a few minutes stays active.
const IDLE_AFTER_MS: i64 = 10 * 60 * 1000;
const STALE_AFTER_MS: i64 = 2 * 60 * 60 * 1000;

#[derive(Debug, thiserror::Error)]
pub enum BrokerOpError {
    #[error(transparent)]
    Git(#[from] GitError),
    #[error(transparent)]
    Store(#[from] BrokerError),
    #[error("refusing to clean session {id}: {reason} (use --force to discard)")]
    DirtyWorktree { id: i64, reason: String },
    #[error(
        "repair paused during rebase for session {id} onto {base}: {message}\n\
         Resolve conflicts in the session worktree, then run \
         `GIT_EDITOR=true git rebase --continue` and resubmit."
    )]
    RepairRebaseFailed {
        id: i64,
        base: String,
        message: String,
    },
    #[error("failed to spawn agent command {command:?}: {source}")]
    Spawn {
        command: String,
        source: std::io::Error,
    },
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    GateConfig(#[from] crate::gates::GateConfigError),
    #[error("queue entry {entry} is not verified (status: {status}) — submit/simulate first")]
    NotVerified { entry: i64, status: &'static str },
    #[error(
        "session {id} ({status}) already exists for this worktree{task}. Options:\n  \
         aethyme broker submit --session {id}        submit its committed work\n  \
         aethyme broker adopt --reuse --task \"...\"   point it at a follow-up task\n  \
         aethyme broker close --session {id}         mark it finished (state only)\n  \
         aethyme broker adopt --replace-stale        close it and register fresh"
    )]
    SessionExistsForWorktree {
        id: i64,
        status: &'static str,
        task: String,
    },
}

/// Policy for `adopt` when the worktree already has a live session.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AdoptMode {
    /// Fail with guidance (default).
    New,
    /// Return the existing session, pointed at a follow-up task.
    Reuse,
    /// Close the existing session (state only) and register fresh.
    ReplaceStale,
}

/// A session enriched with liveness derived at read time — what
/// `broker agents` renders. Serializes as the session's fields plus the
/// derived ones.
#[derive(Debug, serde::Serialize)]
pub struct AgentView {
    #[serde(flatten)]
    pub session: Session,
    /// Best-known activity timestamp: max of the store's value and
    /// filesystem signals from the worktree's git metadata.
    pub activity_at: i64,
    /// Status after applying activity thresholds and (for spawned
    /// sessions) PID liveness. This is the field to display.
    pub derived_status: SessionStatus,
    /// Only meaningful for spawned sessions with a recorded PID.
    pub pid_alive: Option<bool>,
}

/// `broker doctor` findings, serializable for the --json contract.
#[derive(Debug, serde::Serialize)]
pub struct DoctorReport {
    /// SQLite PRAGMA integrity_check result ("ok" when healthy).
    pub integrity: String,
    /// Running CLI build compared with this checkout's integration head
    /// when the checkout is Aethyme itself.
    pub version: VersionDriftReport,
    /// Live sessions whose worktree path no longer exists on disk.
    pub missing_worktrees: Vec<i64>,
    /// Stale gate pidfiles found (and removed) whose process is gone.
    pub orphaned_pidfiles: Vec<String>,
    /// Lease rows of already-cleaned sessions found (and removed) —
    /// retention for databases written before leases were purged on clean.
    pub purged_stale_leases: usize,
}

impl DoctorReport {
    pub fn healthy(&self) -> bool {
        self.integrity == "ok"
            && self.missing_worktrees.is_empty()
            && !self.version.status.is_drift()
    }
}

/// Everything `broker status` renders, in one serializable shape.
#[derive(Debug, serde::Serialize)]
pub struct StatusView {
    pub advice: Vec<StatusAdvice>,
    pub agents: Vec<AgentView>,
    pub overlaps: Vec<crate::leases::Overlap>,
    pub promoted_conflicts: Vec<PromotedConflict>,
    pub queue: Vec<crate::types::MergeQueueEntry>,
    pub integration_branch: String,
    pub integration_head: String,
}

/// Focused view of the local integration branch as a pending layer above
/// the main checkout.
#[derive(Debug, serde::Serialize)]
pub struct IntegrationStatusView {
    pub branch: String,
    pub head: String,
    pub main_head: String,
    pub main_is_ancestor: bool,
    pub commits_ahead_main: u64,
    pub changed_files: Vec<String>,
    pub promoted_entries: Vec<PromotedIntegrationEntry>,
    pub conflicts: Vec<PromotedConflict>,
    pub next_action: IntegrationNextAction,
}

/// A promoted queue entry whose merge commit is still reachable from
/// integration and not yet reachable from main.
#[derive(Debug, serde::Serialize)]
pub struct PromotedIntegrationEntry {
    pub queue_entry_id: i64,
    pub session_id: i64,
    pub branch: Option<String>,
    pub task: Option<String>,
    pub base_commit: String,
    pub head_commit: String,
    pub merge_commit: String,
    pub files: Vec<String>,
}

/// Deterministic operator guidance for the focused integration view.
#[derive(Debug, serde::Serialize)]
pub struct IntegrationNextAction {
    pub summary: String,
    pub commands: Vec<String>,
}

/// Result of `broker repair --session`: a conservative recovery action
/// plus the refreshed gate-selection surface.
#[derive(Debug, serde::Serialize)]
pub struct RepairReport {
    pub session_id: i64,
    pub worktree_path: String,
    pub source: RepairSource,
    pub action: RepairAction,
    pub base: Option<String>,
    pub leases_refreshed: bool,
    pub affected_gates: Vec<RepairGateSelection>,
    pub next_command: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairSource {
    LatestSubmitConflict,
    PromotedConflict,
    None,
}

impl RepairSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LatestSubmitConflict => "latest submit conflict",
            Self::PromotedConflict => "promoted conflict",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairAction {
    Rebased,
    None,
}

impl RepairAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rebased => "rebased",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RepairGateSelection {
    pub gate: String,
    pub triggered_by: Option<String>,
}

/// Operator guidance derived from `broker status` facts. This is
/// deliberately local and deterministic: no model-generated prose, no
/// hidden lookups, and no state mutation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StatusAdvice {
    pub id: &'static str,
    pub severity: StatusAdviceSeverity,
    pub reason: &'static str,
    pub summary: String,
    pub session_id: Option<i64>,
    pub queue_entry_id: Option<i64>,
    pub evidence: Vec<String>,
    pub commands: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StatusAdviceSeverity {
    Blocked,
    Warning,
    Notice,
    Info,
}

impl StatusAdviceSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Warning => "warning",
            Self::Notice => "notice",
            Self::Info => "info",
        }
    }
}

/// A live session lease overlapping work that has already promoted to the
/// local integration branch but has not necessarily reached main. Separate
/// from live/live lease overlaps: the blocking work may belong to a closed
/// session whose leases were correctly purged.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub struct PromotedConflict {
    pub session_id: i64,
    pub path: String,
    pub session_path: String,
    pub promoted_path: String,
}

/// One broker instance per repository: git handle on the main checkout +
/// the shared store. Open from ANY path inside the repo (including a
/// linked worktree) — state always resolves to the main checkout.
pub struct Broker {
    repo: GitRepo,
    store: BrokerStore,
    main_root: PathBuf,
}

impl Broker {
    pub fn open(path_inside_repo: &Path) -> Result<Self, BrokerOpError> {
        let here = GitRepo::discover(path_inside_repo)?;
        let main_root = here.main_root()?;
        let repo = GitRepo::discover(&main_root)?;
        let store = BrokerStore::open_in_repo(&main_root)?;
        Ok(Self {
            repo,
            store,
            main_root,
        })
    }

    pub fn main_root(&self) -> &Path {
        &self.main_root
    }

    pub fn store(&mut self) -> &mut BrokerStore {
        &mut self.store
    }

    pub(crate) fn main_root_path(&self) -> PathBuf {
        self.main_root.clone()
    }

    pub(crate) fn repo_handle(&self) -> &GitRepo {
        &self.repo
    }

    // ── adopt (attach-first) ──────────────────────────────────────────

    /// Register an existing worktree the user already launched an agent
    /// in. `worktree` may be any path inside it.
    pub fn adopt(&mut self, worktree: &Path, task: Option<&str>) -> Result<Session, BrokerOpError> {
        self.adopt_with(worktree, task, AdoptMode::New)
    }

    /// `adopt` with an explicit policy for the "this worktree already has
    /// a session" case (dogfood feedback 2026-07-14: the bare constraint
    /// error left no obvious follow-up path).
    pub fn adopt_with(
        &mut self,
        worktree: &Path,
        task: Option<&str>,
        mode: AdoptMode,
    ) -> Result<Session, BrokerOpError> {
        let checkout = GitRepo::discover(worktree)?;
        let branch = checkout.current_branch()?;
        let diff_base = checkout.head_commit().ok();
        let worktree_path = checkout.root().to_string_lossy().into_owned();

        if let Some(existing) = self.store.session_for_worktree(&worktree_path)? {
            match mode {
                AdoptMode::Reuse => {
                    // Follow-up task on the same worktree: same identity,
                    // fresh baseline so leases and submit scope reflect
                    // work from *now*, not the previous task.
                    return Ok(self.store.reuse_session(
                        existing.id,
                        task,
                        diff_base.as_deref(),
                    )?);
                }
                AdoptMode::ReplaceStale => {
                    // State-only close (never touches the filesystem),
                    // then a fresh registration.
                    self.store
                        .set_session_status(existing.id, SessionStatus::Cleaned, None)?;
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

        let session = self.store.register_session(&NewSession {
            worktree_path,
            branch,
            origin: SessionOrigin::Adopted,
            task: task.map(str::to_string),
            diff_base,
            pid: None,
            command: None,
            log_path: None,
        })?;
        Ok(session)
    }

    /// Mark a session finished without touching its worktree — the right
    /// verb for adopted sessions, whose checkout the broker never owns
    /// (`cleanup` removes worktrees and refuses on the main checkout).
    pub fn close(&mut self, session_id: i64) -> Result<(), BrokerOpError> {
        self.store
            .set_session_status(session_id, SessionStatus::Cleaned, None)?;
        Ok(())
    }

    // ── start-agent (spawn convenience) ───────────────────────────────

    /// Create a worktree + branch for `task` and spawn `command` in it via
    /// `sh -c` with stdout/stderr teed to a log file. Returns the session;
    /// the child runs detached (the broker never owns the process beyond
    /// recording its PID).
    pub fn start_agent(&mut self, task: &str, command: &str) -> Result<Session, BrokerOpError> {
        let slug = slugify(task);
        let worktree_path = self.main_root.join(".aethyme/worktrees").join(&slug);
        let branch = format!("agent/{slug}");
        let base = self.repo.head_commit()?;
        let worktree = self.repo.worktree_add(&worktree_path, &branch, &base)?;

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

        let session = self.store.register_session(&NewSession {
            worktree_path: worktree.root().to_string_lossy().into_owned(),
            branch,
            origin: SessionOrigin::Spawned,
            task: Some(task.to_string()),
            diff_base: Some(base),
            pid: Some(child.id() as i64),
            command: Some(command.to_string()),
            log_path: Some(log_path.to_string_lossy().into_owned()),
        })?;
        Ok(session)
    }

    // ── agents (liveness) ─────────────────────────────────────────────

    /// Live sessions with liveness derived from what the broker can
    /// actually observe: store activity, worktree git metadata mtimes,
    /// and PID liveness for spawned sessions. Reconciles dead spawned
    /// processes to `exited` in the store as a side effect.
    pub fn agents(&mut self, now_ms: i64) -> Result<Vec<AgentView>, BrokerOpError> {
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
            views.push(AgentView {
                session,
                activity_at,
                derived_status,
                pid_alive,
            });
        }
        Ok(views)
    }

    // ── leases (Phase 3) ──────────────────────────────────────────────

    /// Recompute every live session's implicit leases from its diff
    /// against its recorded base (ignore rules applied), then return the
    /// current overlap set. Emits one `lease.overlap` event per pair that
    /// is NEW relative to before the refresh — repeated status calls do
    /// not re-announce known overlaps.
    ///
    /// Sessions whose worktree is gone or whose base no longer resolves
    /// are skipped, never fatal: lease freshness must not take the broker
    /// down.
    pub fn refresh_leases(&mut self) -> Result<Vec<crate::leases::Overlap>, BrokerOpError> {
        use crate::leases::{LeaseIgnoreRules, detect_overlaps};

        let before: std::collections::HashSet<crate::leases::Overlap> =
            detect_overlaps(&self.store.active_leases()?)
                .into_iter()
                .collect();

        let rules = LeaseIgnoreRules::load(&self.main_root);
        // The integration tip is the same for every session; resolving it
        // inside the loop cost one git subprocess per live session.
        let integration = self.integration_tip();
        for session in self.store.live_sessions()? {
            if !matches!(
                session.status,
                SessionStatus::Active | SessionStatus::Idle | SessionStatus::Stale
            ) {
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
                .and_then(|tip| checkout.merge_base(tip, "HEAD").ok())
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

        let after = detect_overlaps(&self.store.active_leases()?);
        for overlap in &after {
            if !before.contains(overlap) {
                self.store.append_event(
                    crate::events::LEASE_OVERLAP,
                    Some(overlap.session_a),
                    Some(&serde_json::to_string(overlap)?),
                )?;
            }
        }
        Ok(after)
    }

    /// Detect live-session leases that overlap already-promoted work on
    /// the integration branch. This intentionally does NOT keep leases for
    /// cleaned sessions alive: closed-session rows remain purged, and the
    /// promoted branch is its own conflict surface.
    fn promoted_conflicts(&self) -> Result<Vec<PromotedConflict>, BrokerOpError> {
        use std::collections::{BTreeMap, BTreeSet};

        use crate::leases::{LeaseIgnoreRules, paths_overlap};

        let Some(integration) = self.integration_tip() else {
            return Ok(Vec::new());
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
            return Ok(Vec::new());
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
            let Ok(checkout) = GitRepo::discover(Path::new(&session.worktree_path)) else {
                continue;
            };
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
        Ok(conflicts.into_iter().collect())
    }

    // ── repair (one-command recovery) ─────────────────────────────────

    /// Recover a blocked session by applying the documented local rebase
    /// path when there is an actionable conflict, then refresh leases and
    /// return the affected gate selection. Does not submit or promote.
    pub fn repair(&mut self, session_id: i64) -> Result<RepairReport, BrokerOpError> {
        let session = self.store.session(session_id)?;
        let worktree_path = session.worktree_path.clone();
        let checkout = GitRepo::discover(Path::new(&worktree_path))?;
        let (source, base) = self.repair_target(session_id)?;
        let action = if let Some(base) = base.as_deref() {
            let dirty = checkout.dirty_paths()?;
            if !dirty.is_empty() {
                return Err(BrokerOpError::DirtyWorktree {
                    id: session_id,
                    reason: format!(
                        "worktree has uncommitted changes; commit or stash before repair, e.g. {}",
                        dirty.first().map(String::as_str).unwrap_or("-")
                    ),
                });
            }
            checkout.fetch_local_commit(base)?;
            checkout
                .rebase_onto(base)
                .map_err(|err| BrokerOpError::RepairRebaseFailed {
                    id: session_id,
                    base: base.to_string(),
                    message: err.to_string(),
                })?;
            let _ = std::fs::remove_file(
                Path::new(&worktree_path).join(crate::ACTION_REQUIRED_RELPATH),
            );
            RepairAction::Rebased
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
            leases_refreshed: true,
            affected_gates,
            next_command: format!("aethyme broker submit --session {session_id}"),
        })
    }

    fn repair_target(
        &mut self,
        session_id: i64,
    ) -> Result<(RepairSource, Option<String>), BrokerOpError> {
        let latest = self
            .store
            .merge_queue()?
            .into_iter()
            .filter(|entry| entry.session_id == session_id)
            .last();
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
        let (_, gates, changed) = self.gate_inputs(session_id)?;
        Ok(crate::gates::select_gates(&gates, &changed)
            .into_iter()
            .map(|s| (s.gate.name.clone(), s.triggered_by))
            .collect())
    }

    /// Run the affected gates for a session's worktree: cheap-first,
    /// tree-hash cached, cancelling this session's obsolete in-flight
    /// runs first. Stops at the first failure.
    pub fn run_gates(
        &mut self,
        session_id: i64,
    ) -> Result<Vec<crate::gates::GateRunOutcome>, BrokerOpError> {
        let (checkout, gates, changed) = self.gate_inputs(session_id)?;
        crate::gates::run_affected(
            &mut self.store,
            &self.main_root,
            &checkout,
            &gates,
            &changed,
            Some(session_id),
        )
    }

    /// Test/non-CLI entrypoint for gate runs with injectable progress
    /// reporting. The default [`Self::run_gates`] sink writes to stderr.
    pub fn run_gates_with_progress(
        &mut self,
        session_id: i64,
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
            progress,
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
        ))
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
        let checkout = GitRepo::discover(dir)?;
        let config_root = checkout.root().to_path_buf();
        let gates = self.load_and_sync_gates_from(&config_root)?;
        crate::gates::run_all(&mut self.store, &self.main_root, &checkout, &gates, None)
    }

    /// Test/non-CLI entrypoint for [`Self::run_all_gates`] with injectable
    /// progress reporting.
    pub fn run_all_gates_with_progress(
        &mut self,
        dir: &Path,
        progress: &dyn crate::gates::GateProgressSink,
    ) -> Result<Vec<crate::gates::GateRunOutcome>, BrokerOpError> {
        let checkout = GitRepo::discover(dir)?;
        let config_root = checkout.root().to_path_buf();
        let gates = self.load_and_sync_gates_from(&config_root)?;
        crate::gates::run_all_with_progress(
            &mut self.store,
            &self.main_root,
            &checkout,
            &gates,
            None,
            progress,
        )
    }

    fn gate_inputs(
        &mut self,
        session_id: i64,
    ) -> Result<(GitRepo, Vec<crate::gates::Gate>, Vec<String>), BrokerOpError> {
        let session = self.store.session(session_id)?;
        let checkout = GitRepo::discover(Path::new(&session.worktree_path))?;
        let config_root = checkout.root().to_path_buf();
        let gates = self.load_and_sync_gates_from(&config_root)?;
        let base = self
            .session_change_base(&checkout)
            .or(session.diff_base)
            .unwrap_or_else(|| "HEAD".to_string());
        let changed = checkout.changed_files(&base)?;
        Ok((checkout, gates, changed))
    }

    /// Load gates.toml and sync the definition snapshot so recorded
    /// results stay interpretable after config edits.
    pub(crate) fn load_and_sync_gates_from(
        &mut self,
        config_root: &Path,
    ) -> Result<Vec<crate::gates::Gate>, BrokerOpError> {
        let gates = crate::gates::load_gates(config_root)?;
        for gate in &gates {
            self.store.upsert_gate(&crate::types::GateDef {
                name: gate.name.clone(),
                command: gate.command.clone(),
                cost_tier: gate.cost,
                triggers_json: serde_json::to_string(&gate.triggers)?,
                updated_at: 0,
            })?;
        }
        Ok(gates)
    }

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

        let (branch, head) = self.integration_head()?;
        let main_head = self.repo.head_commit()?;
        let main_is_ancestor = self.repo.is_ancestor(&main_head, &head);
        let commits_ahead_main = self.repo.commit_count_between(&main_head, &head)?;
        let changed_files = if head == main_head {
            Vec::new()
        } else {
            self.repo.changed_between(&main_head, &head)?
        };

        let mut promoted_entries = Vec::new();
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
            let session = self.store.session(entry.session_id).ok();
            let files = self
                .repo
                .changed_between(&entry.base_commit, &entry.head_commit)
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
        let next_action = integration_next_action(
            &branch,
            main_is_ancestor,
            &promoted_entries,
            &changed_files,
            &conflicts,
        );

        Ok(IntegrationStatusView {
            branch,
            head,
            main_head,
            main_is_ancestor,
            commits_ahead_main,
            changed_files,
            promoted_entries,
            conflicts,
            next_action,
        })
    }

    /// The whole picture in one call: refreshed leases + overlaps, agent
    /// views, promoted/unmerged conflicts, the merge queue, and the
    /// integration branch head.
    pub fn status(&mut self, now_ms: i64) -> Result<StatusView, BrokerOpError> {
        let overlaps = self.refresh_leases()?;
        let agents = self.agents(now_ms)?;
        let promoted_conflicts = self.promoted_conflicts()?;
        let queue = self.store.merge_queue()?;
        let (integration_branch, integration_head) = self.integration_head()?;
        let advice = self.status_advice(&agents, &promoted_conflicts, &queue, &integration_branch);
        Ok(StatusView {
            advice,
            agents,
            overlaps,
            promoted_conflicts,
            queue,
            integration_branch,
            integration_head,
        })
    }

    fn status_advice(
        &self,
        agents: &[AgentView],
        promoted_conflicts: &[PromotedConflict],
        queue: &[MergeQueueEntry],
        integration_branch: &str,
    ) -> Vec<StatusAdvice> {
        use std::collections::BTreeMap;

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
                MergeStatus::Conflict => advice.push(conflict_submit_advice(agent, entry)),
                _ => {}
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

        for agent in agents {
            let Ok(checkout) = GitRepo::discover(Path::new(&agent.session.worktree_path)) else {
                continue;
            };
            let Ok(dirty) = checkout.dirty_paths() else {
                continue;
            };
            if !dirty.is_empty() {
                advice.push(dirty_worktree_advice(agent, &dirty));
            }
        }

        advice
    }

    // ── doctor (operational health) ───────────────────────────────────

    /// Health checks an operator (or CI) can run cheaply: database
    /// integrity, live sessions whose worktree no longer exists, and
    /// orphaned gate pidfiles (whose process group is gone) — the latter
    /// are removed as part of the check.
    pub fn doctor(&mut self) -> Result<DoctorReport, BrokerOpError> {
        let integrity = self.store.integrity_check()?;

        let mut missing_worktrees = Vec::new();
        for session in self.store.live_sessions()? {
            if matches!(
                session.status,
                SessionStatus::Active | SessionStatus::Idle | SessionStatus::Stale
            ) && !Path::new(&session.worktree_path).exists()
            {
                missing_worktrees.push(session.id);
            }
        }

        let mut orphaned_pidfiles = Vec::new();
        let run_dir = self.main_root.join(".aethyme/run/gates");
        if let Ok(entries) = std::fs::read_dir(&run_dir) {
            for entry in entries.flatten() {
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

        let purged_stale_leases = self.store.purge_leases_of_cleaned_sessions()?;

        Ok(DoctorReport {
            integrity,
            version: crate::version::inspect_version(&self.main_root),
            missing_worktrees,
            orphaned_pidfiles,
            purged_stale_leases,
        })
    }

    // ── cleanup ───────────────────────────────────────────────────────

    /// Remove a session's worktree and mark it cleaned. Refuses when the
    /// worktree has uncommitted changes or commits not reachable from the
    /// main checkout's HEAD, unless `force`.
    pub fn cleanup(&mut self, session_id: i64, force: bool) -> Result<(), BrokerOpError> {
        let session = self.store.session(session_id)?;
        let worktree_path = PathBuf::from(&session.worktree_path);

        if worktree_path.exists() {
            let checkout = GitRepo::discover(&worktree_path)?;
            if !force {
                if checkout.is_dirty()? {
                    return Err(BrokerOpError::DirtyWorktree {
                        id: session_id,
                        reason: "worktree has uncommitted changes".into(),
                    });
                }
                let main_head = self.repo.head_commit()?;
                let unmerged = checkout.unmerged_commit_count(&main_head)?;
                if unmerged > 0 {
                    return Err(BrokerOpError::DirtyWorktree {
                        id: session_id,
                        reason: format!(
                            "worktree has {unmerged} commit(s) not reachable from the \
                             main checkout's HEAD"
                        ),
                    });
                }
            }
            self.repo.worktree_remove(&worktree_path, force)?;
        }
        self.store
            .set_session_status(session_id, SessionStatus::Cleaned, None)?;
        Ok(())
    }
}

fn rejected_submit_advice(agent: &AgentView, entry: &MergeQueueEntry) -> StatusAdvice {
    let failures = gate_failures(entry.details_json.as_deref());
    let gate_names: Vec<String> = failures
        .iter()
        .map(|failure| failure.name.clone())
        .collect();
    let summary = if gate_names.is_empty() {
        format!(
            "session {} latest submit qid {} was rejected; inspect the gate details, commit a fix, then resubmit",
            agent.session.id, entry.id
        )
    } else {
        format!(
            "session {} latest submit qid {} was rejected by {}; commit a fix, then resubmit",
            agent.session.id,
            entry.id,
            gate_names.join(", ")
        )
    };

    let mut evidence = queue_evidence(entry);
    if failures.is_empty() {
        evidence.push("gate details unavailable or did not include a failing gate".into());
    } else {
        evidence.extend(failures.iter().map(GateFailure::evidence));
    }

    let worktree = shell_quote(&agent.session.worktree_path);
    StatusAdvice {
        id: "session.latest-submit-rejected",
        severity: StatusAdviceSeverity::Blocked,
        reason: "submit_rejected",
        summary,
        session_id: Some(agent.session.id),
        queue_entry_id: Some(entry.id),
        evidence,
        commands: vec![
            format!("git -C {worktree} status --short"),
            format!("aethyme broker gates run --session {}", agent.session.id),
            format!("aethyme broker submit --session {}", agent.session.id),
        ],
    }
}

fn conflict_submit_advice(agent: &AgentView, entry: &MergeQueueEntry) -> StatusAdvice {
    let conflicts = details_string_array(entry.details_json.as_deref(), "conflicts");
    let blockers = details_i64_array(entry.details_json.as_deref(), "blocking_sessions");
    let conflict_count = conflicts.len();
    let summary = if conflict_count == 0 {
        format!(
            "session {} latest submit qid {} conflicted; read the action-required file, rebase, then resubmit",
            agent.session.id, entry.id
        )
    } else {
        format!(
            "session {} latest submit qid {} conflicted on {} path(s); rebase, resolve, then resubmit",
            agent.session.id, entry.id, conflict_count
        )
    };

    let mut evidence = queue_evidence(entry);
    evidence.extend(path_evidence("conflict", &conflicts));
    if !blockers.is_empty() {
        let labels: Vec<String> = blockers.iter().map(i64::to_string).collect();
        evidence.push(format!("blocking sessions {}", labels.join(", ")));
    }

    let worktree = shell_quote(&agent.session.worktree_path);
    StatusAdvice {
        id: "session.latest-submit-conflict",
        severity: StatusAdviceSeverity::Blocked,
        reason: "submit_conflict",
        summary,
        session_id: Some(agent.session.id),
        queue_entry_id: Some(entry.id),
        evidence,
        commands: vec![
            format!("aethyme broker repair --session {}", agent.session.id),
            format!("aethyme broker submit --session {}", agent.session.id),
            format!("cat {}/{}", worktree, crate::ACTION_REQUIRED_RELPATH),
        ],
    }
}

fn promoted_conflict_advice(
    session_id: i64,
    _worktree_path: &str,
    integration_branch: &str,
    conflicts: &[&PromotedConflict],
) -> StatusAdvice {
    let count = conflicts.len();
    let summary = format!(
        "session {session_id} overlaps promoted integration work on {count} path(s); rebase onto {integration_branch} before submit"
    );
    let mut evidence: Vec<String> = conflicts
        .iter()
        .take(5)
        .map(|conflict| {
            format!(
                "path {} (session {}, integration {})",
                conflict.path, conflict.session_path, conflict.promoted_path
            )
        })
        .collect();
    if count > evidence.len() {
        evidence.push(format!("and {} more path(s)", count - evidence.len()));
    }

    let mut commands = Vec::new();
    commands.push(format!("aethyme broker repair --session {session_id}"));
    commands.push(format!("aethyme broker submit --session {session_id}"));

    StatusAdvice {
        id: "session.promoted-conflict",
        severity: StatusAdviceSeverity::Blocked,
        reason: "promoted_conflict",
        summary,
        session_id: Some(session_id),
        queue_entry_id: None,
        evidence,
        commands,
    }
}

fn integration_next_action(
    branch: &str,
    main_is_ancestor: bool,
    promoted_entries: &[PromotedIntegrationEntry],
    changed_files: &[String],
    conflicts: &[PromotedConflict],
) -> IntegrationNextAction {
    use std::collections::BTreeSet;

    let conflict_sessions: BTreeSet<i64> = conflicts
        .iter()
        .map(|conflict| conflict.session_id)
        .collect();
    if !conflict_sessions.is_empty() {
        let commands = conflict_sessions
            .iter()
            .take(5)
            .map(|session_id| format!("aethyme broker repair --session {session_id}"))
            .collect();
        return IntegrationNextAction {
            summary: format!(
                "{} session(s) overlap the pending integration layer; repair or rebase them before submit",
                conflict_sessions.len()
            ),
            commands,
        };
    }

    if !promoted_entries.is_empty() {
        if main_is_ancestor {
            return IntegrationNextAction {
                summary: format!(
                    "{} promoted entry(s) are pending; fast-forward main from {branch} when ready",
                    promoted_entries.len()
                ),
                commands: vec![
                    format!("git merge --ff-only {branch}"),
                    "aethyme broker integration status".into(),
                ],
            };
        }
        return IntegrationNextAction {
            summary: format!(
                "{} promoted entry(s) are pending, but main and {branch} have diverged; inspect before merging",
                promoted_entries.len()
            ),
            commands: vec![format!("git log --oneline --left-right HEAD...{branch}")],
        };
    }

    if !changed_files.is_empty() {
        return IntegrationNextAction {
            summary: format!(
                "{branch} differs from main, but no promoted queue entries describe the pending commits; inspect branch history"
            ),
            commands: vec![format!("git log --oneline --left-right HEAD...{branch}")],
        };
    }

    IntegrationNextAction {
        summary: "no promoted work pending outside main".into(),
        commands: Vec::new(),
    }
}

fn dirty_worktree_advice(agent: &AgentView, dirty: &[String]) -> StatusAdvice {
    let summary = format!(
        "session {} has {} uncommitted change(s); commit or stash before submit because only committed work integrates",
        agent.session.id,
        dirty.len()
    );
    let worktree = shell_quote(&agent.session.worktree_path);
    StatusAdvice {
        id: "session.dirty-worktree",
        severity: StatusAdviceSeverity::Warning,
        reason: "dirty_worktree",
        summary,
        session_id: Some(agent.session.id),
        queue_entry_id: None,
        evidence: path_evidence("dirty", dirty),
        commands: vec![
            format!("git -C {worktree} status --short"),
            format!("git -C {worktree} add ..."),
            format!("git -C {worktree} commit"),
            format!("git -C {worktree} stash push"),
        ],
    }
}

#[derive(Debug)]
struct GateFailure {
    name: String,
    status: String,
    failure_class: Option<String>,
    cached: bool,
}

impl GateFailure {
    fn evidence(&self) -> String {
        let class = self
            .failure_class
            .as_deref()
            .map(|class| format!("/{class}"))
            .unwrap_or_default();
        format!(
            "gate {} status {}{}{}",
            self.name,
            self.status,
            class,
            if self.cached { " (cached)" } else { "" }
        )
    }
}

fn gate_failures(details_json: Option<&str>) -> Vec<GateFailure> {
    let Some(details_json) = details_json else {
        return Vec::new();
    };
    let Ok(details) = serde_json::from_str::<serde_json::Value>(details_json) else {
        return Vec::new();
    };
    let Some(gates) = details.get("gates").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    gates
        .iter()
        .filter_map(|gate| {
            let name = gate.get("gate")?.as_str()?;
            let status = gate.get("status")?.as_str()?;
            if status == "pass" {
                return None;
            }
            Some(GateFailure {
                name: name.to_string(),
                status: status.to_string(),
                failure_class: gate
                    .get("failure_class")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                cached: gate
                    .get("cached")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect()
}

fn details_string_array(details_json: Option<&str>, key: &str) -> Vec<String> {
    let Some(details_json) = details_json else {
        return Vec::new();
    };
    let Ok(details) = serde_json::from_str::<serde_json::Value>(details_json) else {
        return Vec::new();
    };
    details
        .get(key)
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str().map(str::to_string))
        .collect()
}

fn details_string_value(details_json: Option<&str>, key: &str) -> Option<String> {
    let details_json = details_json?;
    let details = serde_json::from_str::<serde_json::Value>(details_json).ok()?;
    details.get(key)?.as_str().map(str::to_string)
}

fn details_i64_array(details_json: Option<&str>, key: &str) -> Vec<i64> {
    let Some(details_json) = details_json else {
        return Vec::new();
    };
    let Ok(details) = serde_json::from_str::<serde_json::Value>(details_json) else {
        return Vec::new();
    };
    details
        .get(key)
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_i64)
        .collect()
}

fn queue_evidence(entry: &MergeQueueEntry) -> Vec<String> {
    vec![
        format!("head {}", short_commit(&entry.head_commit)),
        format!("base {}", short_commit(&entry.base_commit)),
    ]
}

fn path_evidence(label: &str, paths: &[String]) -> Vec<String> {
    let mut evidence: Vec<String> = paths
        .iter()
        .take(5)
        .map(|path| format!("{label} {path}"))
        .collect();
    if paths.len() > evidence.len() {
        evidence.push(format!("and {} more path(s)", paths.len() - evidence.len()));
    }
    evidence
}

fn short_commit(commit: &str) -> &str {
    &commit[..12.min(commit.len())]
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-' | b':' | b'@')
        })
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

/// Newest mtime among the worktree's per-worktree git metadata (`index`,
/// `HEAD` under `<main>/.git/worktrees/<name>/`) — updated by any staging,
/// commit, or checkout the agent performs. Content-free by design (the
/// vendor-artifact decision allows metadata only).
fn worktree_activity_ms(main_root: &Path, session: &Session) -> Option<i64> {
    let name = Path::new(&session.worktree_path).file_name()?;
    let meta_dir = main_root.join(".git/worktrees").join(name);
    let mut newest: Option<i64> = None;
    for file in ["index", "HEAD"] {
        if let Ok(meta) = std::fs::metadata(meta_dir.join(file))
            && let Ok(mtime) = meta.modified()
            && let Ok(dur) = mtime.duration_since(std::time::UNIX_EPOCH)
        {
            let ms = dur.as_millis() as i64;
            newest = Some(newest.map_or(ms, |n: i64| n.max(ms)));
        }
    }
    newest
}

/// True when the PID exists and is not a zombie (macOS/Linux — the v0
/// platforms). `kill -0` alone is wrong here: it succeeds on zombies,
/// and an exited-but-unreaped agent must read as dead.
fn pid_alive(pid: i64) -> bool {
    match Command::new("ps")
        .args(["-o", "state=", "-p", &pid.to_string()])
        .stderr(Stdio::null())
        .output()
    {
        Ok(output) if output.status.success() => {
            let state = String::from_utf8_lossy(&output.stdout).trim().to_string();
            !state.is_empty() && !state.starts_with('Z')
        }
        _ => false,
    }
}

/// Task → worktree/branch slug: lowercase alphanumerics with dashes,
/// capped at 40 chars, never empty.
fn slugify(task: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = true;
    for ch in task.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
        if slug.len() >= 40 {
            break;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() { "task".into() } else { slug }
}

#[cfg(test)]
mod tests {
    use super::slugify;

    #[test]
    fn slugify_is_safe_for_branches_and_paths() {
        assert_eq!(slugify("Fix auth bug!"), "fix-auth-bug");
        assert_eq!(slugify("  weird///name  "), "weird-name");
        assert_eq!(slugify("émojis 🎉 stripped"), "mojis-stripped");
        assert_eq!(slugify(""), "task");
        assert!(slugify(&"x".repeat(100)).len() <= 40);
    }
}
