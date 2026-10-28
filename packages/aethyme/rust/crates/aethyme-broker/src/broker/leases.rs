use super::*;

const DEFAULT_LEASE_CLAIM_PREPARATION_BUDGET_MS: u64 = 30_000;
const LEASE_CLAIM_PREPARATION_BUDGET_ENV: &str = "AETHYME_BROKER_LEASE_CLAIM_PREPARATION_BUDGET_MS";
const SESSION_COMMIT_STATUS_BUDGET: std::time::Duration = std::time::Duration::from_secs(15);
const SESSION_COMMIT_PATH_OUTPUT_CAP: usize = 8 * 1024 * 1024;
const SESSION_COMMIT_PATH_FIELD_CAP: usize = 16_384;
const SESSION_COMMIT_PARENT_OUTPUT_CAP: usize = 64 * 1024;

/// Validate arguments after the literal `git commit` command prefix. Global
/// routing switches are refused explicitly, and no option that skips a Git
/// commit hook may pass through this broker lane.
pub(crate) fn validate_session_commit_args(args: &[String]) -> Result<(), String> {
    let option_end = args
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(args.len());
    let mut index = 0;
    while index < option_end {
        let arg = args[index].as_str();
        let blocked_long = ["--no-verify", "--no-post-rewrite"];
        let hook_bypass = arg == "-n"
            || (arg.starts_with("--")
                && blocked_long.iter().any(|blocked| {
                    arg == *blocked
                        || arg.starts_with(&format!("{blocked}="))
                        || blocked.starts_with(arg)
                }))
            || (arg.starts_with('-')
                && !arg.starts_with("--")
                && short_option_cluster_has_hook_bypass(arg));
        if hook_bypass {
            return Err("hook-bypass options are not allowed".into());
        }
        let blocked_long = [
            "--git-dir",
            "--work-tree",
            "--namespace",
            "--config-env",
            "--config",
            "--exec-path",
            "--super-prefix",
            "--no-replace-objects",
            "--literal-pathspecs",
            "--no-optional-locks",
        ];
        if arg.starts_with("--") && ("--amend" == arg || "--amend".starts_with(arg)) {
            return Err("history-rewriting --amend is not supported by broker commit".into());
        }
        if arg.starts_with("--")
            && blocked_long.iter().any(|blocked| {
                arg == *blocked
                    || arg.starts_with(&format!("{blocked}="))
                    || blocked.starts_with(arg)
            })
        {
            return Err("repository-routing or Git configuration options are not allowed".into());
        }

        if matches!(
            arg,
            "-m" | "--message"
                | "-F"
                | "--file"
                | "-C"
                | "--reuse-message"
                | "-c"
                | "--reedit-message"
                | "-t"
                | "--template"
                | "--author"
                | "--date"
                | "--cleanup"
                | "--trailer"
                | "--fixup"
                | "--squash"
                | "--pathspec-from-file"
        ) {
            index = index.saturating_add(2);
        } else {
            index += 1;
        }
    }
    Ok(())
}

fn short_option_cluster_has_hook_bypass(arg: &str) -> bool {
    let Some(cluster) = arg.strip_prefix('-') else {
        return false;
    };
    for option in cluster.bytes() {
        if option == b'n' {
            return true;
        }
        // These options take an attached or following value. The remainder of
        // the cluster is their value, not more switches (so `-mn` is a
        // message whose contents are `n`, not the hook-bypass `-n`).
        if matches!(option, b'm' | b'F' | b'C' | b'c' | b't' | b'A') {
            return false;
        }
    }
    false
}

#[cfg(test)]
mod session_commit_argument_tests {
    use super::validate_session_commit_args;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn commit_args_refuse_hook_bypass_and_repository_routing_options() {
        for values in [
            &["-n"][..],
            &["-an"][..],
            &["--no-verify"][..],
            &["--no-ver"][..],
            &["--no-post-rewrite"][..],
            &["--no-post"][..],
            &["--amend"][..],
            &["--git-dir=/tmp/elsewhere"][..],
            &["--work-tree", "/tmp/elsewhere"][..],
            &["--namespace=other"][..],
            &["--config-env=core.hooksPath=EMPTY"][..],
            &["--config=core.hooksPath=EMPTY"][..],
            &["--exec-path=/tmp/elsewhere"][..],
        ] {
            assert!(
                validate_session_commit_args(&args(values)).is_err(),
                "{values:?}"
            );
        }
    }

    #[test]
    fn commit_args_allow_message_values_and_pathspecs_after_separator() {
        for values in [
            &["-m", "text with -n in the message"][..],
            &["-mn"][..],
            &["--message", "--no-verify"][..],
            &["--", "-n"][..],
            &["--", "--git-dir=literal-path"][..],
        ] {
            assert!(
                validate_session_commit_args(&args(values)).is_ok(),
                "{values:?}"
            );
        }
    }
}

fn lease_claim_preparation_budget_ms_from(value: Option<&str>) -> u64 {
    value
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|budget| *budget > 0)
        .map_or(DEFAULT_LEASE_CLAIM_PREPARATION_BUDGET_MS, |budget| {
            budget.min(DEFAULT_LEASE_CLAIM_PREPARATION_BUDGET_MS)
        })
}

fn lease_claim_preparation_budget_ms() -> u64 {
    lease_claim_preparation_budget_ms_from(
        std::env::var(LEASE_CLAIM_PREPARATION_BUDGET_ENV)
            .ok()
            .as_deref(),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorktreeDirectoryScan {
    Complete,
    Incomplete,
    DeadlineReached,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AbandonedSessionCloseOutcome {
    Closed,
    NotDue,
    Changed,
    ActiveProcess,
    HolderUnavailable,
    LeaseHeld,
    LeaseUnavailable,
    WorktreeActivityUnavailable,
    LifecycleBusy,
    DatabaseBusy,
}

fn collect_worktree_directory_entries<I>(
    entries: I,
    observed: &mut Vec<crate::worktree_reconcile::ObservedDirectory>,
    deadline: Option<std::time::Instant>,
) -> WorktreeDirectoryScan
where
    I: IntoIterator<Item = std::io::Result<std::fs::DirEntry>>,
{
    let mut complete = true;
    for entry in entries {
        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            return WorktreeDirectoryScan::DeadlineReached;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                // `read_dir` may open successfully and then fail for one
                // entry. Preserve what was observed, but never call that a
                // complete inventory.
                complete = false;
                continue;
            }
        };
        if is_worktree_root_infrastructure(&entry.file_name()) {
            continue;
        }
        let path = entry.path();
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => {
                // A listed path can disappear or become unreadable before its
                // type is checked; it is not safe to treat that as a
                // confirmed non-directory.
                complete = false;
                continue;
            }
        };
        if !metadata.is_dir() {
            continue;
        }
        let git_marker = match std::fs::metadata(path.join(".git")) {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(_) => {
                complete = false;
                false
            }
        };
        observed.push(crate::worktree_reconcile::ObservedDirectory {
            path: path.to_string_lossy().into_owned(),
            git_marker,
            estimated_bytes: None,
        });
    }
    if complete {
        WorktreeDirectoryScan::Complete
    } else {
        WorktreeDirectoryScan::Incomplete
    }
}

fn scan_worktree_roots<I, F>(
    roots: &[PathBuf],
    observed: &mut Vec<crate::worktree_reconcile::ObservedDirectory>,
    deadline: Option<std::time::Instant>,
    mut read_root: F,
) -> bool
where
    I: IntoIterator<Item = std::io::Result<std::fs::DirEntry>>,
    F: FnMut(&Path) -> std::io::Result<I>,
{
    let mut complete = true;
    for root in roots {
        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            return false;
        }
        let entries = match read_root(root) {
            Ok(entries) => entries,
            Err(_) => {
                // An unreadable root is not drift; claiming it held nothing
                // would understate the problem this sweep exists to surface.
                complete = false;
                continue;
            }
        };
        match collect_worktree_directory_entries(entries, observed, deadline) {
            WorktreeDirectoryScan::Complete => {}
            WorktreeDirectoryScan::Incomplete => complete = false,
            WorktreeDirectoryScan::DeadlineReached => return false,
        }
    }
    complete
}

fn size_unclaimed_directories<F>(
    entries: &mut [crate::worktree_reconcile::ReconciledDirectory],
    mut size_directory: F,
) -> bool
where
    F: FnMut(&Path) -> std::io::Result<u64>,
{
    let mut complete = true;
    for entry in entries.iter_mut().filter(|entry| entry.unclaimed()) {
        match size_directory(Path::new(&entry.path)) {
            Ok(bytes) => entry.estimated_bytes = Some(bytes),
            Err(_) => {
                entry.estimated_bytes = None;
                complete = false;
            }
        }
    }
    complete
}

/// The deadline for the read-only and Git-backed preparation before an
/// explicit lease claim is committed. The SQLite claim transaction itself is
/// not interrupted: the deadline is checked immediately before that write.
struct LeaseClaimPreparation {
    deadline: std::time::Instant,
    budget_ms: u64,
    git_timeout_generation: u64,
}

impl LeaseClaimPreparation {
    fn start(budget_ms: u64) -> Self {
        Self {
            deadline: std::time::Instant::now() + std::time::Duration::from_millis(budget_ms),
            budget_ms,
            git_timeout_generation: crate::git::git_timeout_generation(),
        }
    }

    fn check(&self) -> Result<(), BrokerOpError> {
        if std::time::Instant::now() >= self.deadline
            || crate::git::git_timeout_generation() != self.git_timeout_generation
        {
            return Err(BrokerOpError::LeaseClaimPreparationTimedOut {
                budget_ms: self.budget_ms,
            });
        }
        Ok(())
    }
}

struct GuardedExecOptions {
    stdout: std::process::Stdio,
    supervised_timeout: std::time::Duration,
    record_receipt: bool,
}

pub(crate) struct GuardedExecCliReport {
    pub report: GuardedExecReport,
    pub receipt_id: String,
    pub command_ran: Option<bool>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct GuardedCommitCliReport {
    pub execution: GuardedExecReport,
    pub execution_receipt_id: String,
    pub head_before: String,
    pub head_after: Option<String>,
    pub committed_paths: Option<Vec<String>>,
    pub committed_ownership: Option<OwnershipAuditReport>,
    pub post_commit_audit_error: Option<String>,
    pub post_commit_audit_complete: bool,
    pub ok: bool,
    pub command_ran: Option<bool>,
}

#[derive(Debug, Clone, Copy)]
enum GuardedExecDispatchEvent {
    Started,
    NotStarted,
    OutcomeUnknown(&'static str),
}

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
        let mut holder_process_table = crate::session_holder::RecordedHolderProcessTable::default();
        let retirement_candidates = sessions
            .iter()
            .filter(|session| {
                !session.status.is_closed()
                    && crate::decide_abandonment(
                        &SessionActivity {
                            session_id: session.id,
                            last_activity_at: session.last_activity_at,
                            created_at: session.created_at,
                            agent_alive: None,
                            closed: false,
                        },
                        now_ms,
                        abandon_after_ms,
                    )
                    .is_abandoned()
            })
            .map(|session| session.id)
            .collect::<Vec<_>>();
        holder_process_table.prime(self.store_ref(), &retirement_candidates, None);
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
            if !derived_status.is_closed()
                && matches!(
                    self.try_close_abandoned_session(
                        session.id,
                        now_ms,
                        abandon_after_ms,
                        &mut holder_process_table,
                        true,
                    )?,
                    AbandonedSessionCloseOutcome::Closed
                )
            {
                derived_status = SessionStatus::Closed;
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
        let mut complete = scan_worktree_roots(&roots, &mut observed, deadline, |root| {
            std::fs::read_dir(root)
        });

        let mut reconciled = crate::worktree_reconcile::reconcile(&observed, &claims);
        if sized {
            if let Some(deadline) = deadline {
                for entry in reconciled.iter_mut().filter(|entry| entry.unclaimed()) {
                    entry.estimated_bytes = crate::disk_headroom::directory_usage_bounded(
                        Path::new(&entry.path),
                        deadline,
                    )
                    .map(|usage| usage.bytes);
                    if entry.estimated_bytes.is_none() {
                        complete = false;
                        break;
                    }
                }
            } else if !size_unclaimed_directories(
                &mut reconciled,
                directory_size_without_following_links,
            ) {
                complete = false;
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

    /// Close one age-eligible session only after fresh lifecycle, activity,
    /// owner, and lease checks. The process snapshot is captured before the
    /// nonblocking lifecycle claim; its holder-event identity is then checked
    /// again under the claim and in the store transaction.
    pub(crate) fn try_close_abandoned_session(
        &mut self,
        session_id: i64,
        decision_now_ms: i64,
        abandon_after_ms: i64,
        process_table: &mut crate::session_holder::RecordedHolderProcessTable,
        allow_process_snapshot: bool,
    ) -> Result<AbandonedSessionCloseOutcome, BrokerOpError> {
        if abandon_after_ms <= 0 {
            return Ok(AbandonedSessionCloseOutcome::NotDue);
        }
        let candidate = match self.store_ref().session(session_id) {
            Ok(session) => session,
            Err(BrokerError::SessionNotFound(_)) => {
                return Ok(AbandonedSessionCloseOutcome::Changed);
            }
            Err(error) => return Err(error.into()),
        };
        if candidate.cleanup_state != crate::SessionCleanupState::Open
            || candidate.status.is_closed()
        {
            return Ok(AbandonedSessionCloseOutcome::Changed);
        }
        if candidate.pid.is_some_and(crate::broker::session_pid_alive) {
            return Ok(AbandonedSessionCloseOutcome::ActiveProcess);
        }
        let fs_activity = match checked_worktree_activity_ms(self.main_root(), &candidate) {
            Ok(activity) => activity.unwrap_or(0),
            Err(_) => return Ok(AbandonedSessionCloseOutcome::WorktreeActivityUnavailable),
        };
        let database_activity = fs_activity.max(candidate.last_activity_at);
        if !matches!(
            crate::decide_abandonment(
                &SessionActivity {
                    session_id,
                    last_activity_at: database_activity,
                    created_at: candidate.created_at,
                    agent_alive: None,
                    closed: false,
                },
                decision_now_ms,
                abandon_after_ms,
            ),
            crate::AbandonmentVerdict::Abandoned { .. }
        ) {
            return Ok(AbandonedSessionCloseOutcome::NotDue);
        }

        let holder = crate::session_holder::recorded_holder_liveness(
            self.store_ref(),
            session_id,
            process_table,
            allow_process_snapshot,
        );
        match holder.liveness {
            crate::session_holder::RecordedHolderLiveness::Unknown => {
                return Ok(AbandonedSessionCloseOutcome::HolderUnavailable);
            }
            crate::session_holder::RecordedHolderLiveness::Alive => {
                return Ok(AbandonedSessionCloseOutcome::ActiveProcess);
            }
            crate::session_holder::RecordedHolderLiveness::Gone
            | crate::session_holder::RecordedHolderLiveness::Unbound => {}
        }
        if candidate.pid.is_some_and(crate::broker::session_pid_alive) {
            return Ok(AbandonedSessionCloseOutcome::ActiveProcess);
        }

        // A maintenance pass must keep moving when another lifecycle operation
        // is active; status callers also leave the row open and retry later.
        let Some(_lifecycle_claim) = self.try_claim_worktree_lifecycle()? else {
            return Ok(AbandonedSessionCloseOutcome::LifecycleBusy);
        };

        let current = match self.store_ref().session(session_id) {
            Ok(session) => session,
            Err(BrokerError::SessionNotFound(_)) => {
                return Ok(AbandonedSessionCloseOutcome::Changed);
            }
            Err(error) => return Err(error.into()),
        };
        if current.cleanup_state != crate::SessionCleanupState::Open || current.status.is_closed() {
            return Ok(AbandonedSessionCloseOutcome::Changed);
        }
        let fs_activity = match checked_worktree_activity_ms(self.main_root(), &current) {
            Ok(activity) => activity.unwrap_or(0),
            Err(_) => return Ok(AbandonedSessionCloseOutcome::WorktreeActivityUnavailable),
        };
        let activity_at = fs_activity.max(current.last_activity_at);
        let observed_at = decision_now_ms;
        let crate::AbandonmentVerdict::Abandoned { idle_ms } = crate::decide_abandonment(
            &SessionActivity {
                session_id,
                last_activity_at: activity_at,
                created_at: current.created_at,
                agent_alive: None,
                closed: false,
            },
            observed_at,
            abandon_after_ms,
        ) else {
            return Ok(AbandonedSessionCloseOutcome::NotDue);
        };

        // Recheck the spawned PID after taking the lifecycle claim. PID reuse
        // can only make this conservative: any occupant keeps the session open.
        if current.pid.is_some_and(crate::broker::session_pid_alive) {
            return Ok(AbandonedSessionCloseOutcome::ActiveProcess);
        }
        let current_holder_event_id = self
            .store_ref()
            .latest_session_holder_event(session_id)?
            .map(|event| event.id);
        if current_holder_event_id != holder.binding_event_id {
            return Ok(AbandonedSessionCloseOutcome::Changed);
        }

        let leases = self
            .store_ref()
            .unreleased_unexpired_leases_for_sessions_at(&[session_id], observed_at)?;
        // Implicit leases are a projection of the session's diff, not an
        // independent activity signal. Once its holder and activity checks
        // permit retirement, those leases must close with the session; an
        // unbound legacy session otherwise holds them forever. Explicit
        // claims retain their own conservative liveness guard.
        let explicit_leases = leases
            .iter()
            .filter(|lease| lease.kind == LeaseKind::Explicit)
            .cloned()
            .collect::<Vec<_>>();
        if !explicit_leases.is_empty() {
            let lease_table = holder
                .binding_event_id
                .and_then(|event_id| process_table.table_for(session_id, event_id));
            let liveness = match crate::lease_liveness::assess(
                self.store_ref(),
                &explicit_leases,
                lease_table,
                observed_at,
                crate::lease_liveness::LeaseLivenessPolicy::load(self.main_root()),
            ) {
                Ok(liveness) => liveness,
                Err(_) => return Ok(AbandonedSessionCloseOutcome::LeaseUnavailable),
            };
            if liveness.iter().any(|lease| lease.liveness_evidence.holds) {
                return Ok(AbandonedSessionCloseOutcome::LeaseHeld);
            }
        }
        if self
            .store_ref()
            .latest_session_holder_event(session_id)?
            .map(|event| event.id)
            != holder.binding_event_id
        {
            return Ok(AbandonedSessionCloseOutcome::Changed);
        }

        let abandoned_payload = crate::events::session_abandoned_payload(
            idle_ms,
            abandon_after_ms,
            current.pid.map(crate::broker::session_pid_alive),
        );
        match self.store().abandon_session_if_unchanged(
            &current,
            holder.binding_event_id,
            &leases,
            observed_at,
            &abandoned_payload,
        ) {
            Ok(true) => Ok(AbandonedSessionCloseOutcome::Closed),
            Ok(false) => Ok(AbandonedSessionCloseOutcome::Changed),
            Err(BrokerError::SessionRetirementDatabaseBusy) => {
                Ok(AbandonedSessionCloseOutcome::DatabaseBusy)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Read-only diagnostics for explicit requests and final admission refusal.
    /// Never called by routine status, successful acquisition or wait retries.
    pub(crate) fn resource_pressure_report(
        &self,
    ) -> Result<crate::resource_pressure::Report, BrokerOpError> {
        let sessions = self.store.live_sessions()?;
        let count = sessions.len();
        if count > crate::resource_pressure::SESSION_LIMIT {
            return Ok(crate::resource_pressure::Report::over_budget(count));
        }
        let now = now_ms();
        let mut owners = Vec::new();
        for session in sessions {
            let (inactive, idle_ms) = crate::resource_pressure::activity(
                session.status,
                session.last_activity_at,
                worktree_activity_ms(&self.main_root, &session),
                now,
            );
            owners.push(crate::resource_pressure::Owner {
                session_id: session.id,
                inactive,
                idle_ms,
                holder: crate::session_holder::recorded_holder(&self.store, session.id)?,
            });
        }
        Ok(crate::resource_pressure::Report::capture(owners))
    }

    /// Derive current liveness without persisting status transitions. This is
    /// deliberately allowed to inspect PIDs and worktree metadata, but never
    /// changes a session row or appends an event.
    pub fn agents_snapshot(&self, now_ms: i64) -> Result<Vec<AgentView>, BrokerOpError> {
        self.agent_views(self.store.live_sessions()?, now_ms)
    }

    fn agent_views(
        &self,
        sessions: Vec<Session>,
        now_ms: i64,
    ) -> Result<Vec<AgentView>, BrokerOpError> {
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

    /// Reuse the status call's liveness sample after `agents()` persisted its
    /// derived statuses. The returned `AgentView` retains the original stored
    /// status, so pairing must use `derived_status` to match the store state
    /// that a second snapshot would observe.
    pub(super) fn lease_overlaps_after_agent_reconciliation(
        &self,
        agents: &[AgentView],
    ) -> Result<Vec<crate::Overlap>, BrokerOpError> {
        self.lease_overlaps_for_agent_views(agents, true)
    }

    /// Reuse a read-only status sample while retaining its stored-status rule:
    /// a dead PID with recent activity still holds its recorded lease until a
    /// writable liveness pass persists an exit.
    pub(super) fn lease_overlaps_from_stored_status_views(
        &self,
        agents: &[AgentView],
    ) -> Result<Vec<crate::Overlap>, BrokerOpError> {
        self.lease_overlaps_for_agent_views(agents, false)
    }

    fn lease_overlaps_for_agent_views(
        &self,
        agents: &[AgentView],
        use_derived_status: bool,
    ) -> Result<Vec<crate::Overlap>, BrokerOpError> {
        // Match `coordinating_session_ids`: pairing uses current wall time,
        // independently of the caller-supplied time used to derive agents.
        let now = now_ms();
        let coordinating = agents
            .iter()
            .filter(|agent| {
                let status = if use_derived_status {
                    agent.derived_status
                } else {
                    agent.session.status
                };
                matches!(
                    status,
                    SessionStatus::Active | SessionStatus::Idle | SessionStatus::Stale
                ) && now.saturating_sub(agent.activity_at) <= STALE_AFTER_MS
            })
            .map(|agent| agent.session.id)
            .collect();
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
        self.refresh_leases_including_with_claim_preparation(acting, None, true)
    }

    fn refresh_leases_including_with_claim_preparation(
        &mut self,
        acting: Option<i64>,
        preparation: Option<&LeaseClaimPreparation>,
        classify_overlaps: bool,
    ) -> Result<Vec<crate::leases::Overlap>, BrokerOpError> {
        use crate::leases::{LeaseIgnoreRules, detect_overlaps};

        let rules = LeaseIgnoreRules::load(&self.main_root);
        // The integration tip is the same for every session; resolving it
        // inside the loop cost one git subprocess per live session.
        let integration = self.integration_tip();
        if let Some(preparation) = preparation {
            preparation.check()?;
        }
        let upstream = self.repo.upstream_default().map(|(_, commit)| commit);
        if let Some(preparation) = preparation {
            preparation.check()?;
        }
        let mut coordinating = self.coordinating_session_ids()?;
        if let Some(preparation) = preparation {
            preparation.check()?;
        }
        coordinating.extend(acting);
        let mut pending_implicit_leases = Vec::new();
        for session in self.store.live_sessions()? {
            if let Some(preparation) = preparation {
                preparation.check()?;
            }
            if !coordinating.contains(&session.id) {
                continue;
            }
            let Some(paths) = implicit_lease_paths(
                &session,
                &rules,
                integration.as_deref(),
                upstream.as_deref(),
            ) else {
                if let Some(preparation) = preparation {
                    // `integration_tip`, `upstream_default`, baseline and
                    // changed-file helpers deliberately hide ordinary Git
                    // failures behind `Option`. A timeout is different: a
                    // lease claim must stop rather than treating that
                    // incomplete scan as an empty or absent worktree.
                    preparation.check()?;
                }
                continue;
            };
            if let Some(preparation) = preparation {
                preparation.check()?;
                pending_implicit_leases.push((session.id, paths));
            } else {
                self.store.set_implicit_leases(session.id, &paths)?;
            }
        }
        if let Some(preparation) = preparation {
            preparation.check()?;
            for (session_id, paths) in pending_implicit_leases {
                preparation.check()?;
                self.store.set_implicit_leases(session_id, &paths)?;
            }
        }

        let after = detect_overlaps(&self.coordinating_leases(&coordinating)?);
        if let Some(preparation) = preparation {
            preparation.check()?;
        }
        if classify_overlaps {
            self.classify_and_announce_overlaps(&after)?;
        } else {
            self.defer_overlap_classification(&after)?;
        }
        if let Some(preparation) = preparation {
            preparation.check()?;
        }
        self.store
            .meta_set("leases.refreshed_at_ms", &now_ms().to_string())?;
        if let Some(preparation) = preparation {
            preparation.check()?;
        }
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
        let preparation = LeaseClaimPreparation::start(lease_claim_preparation_budget_ms());
        preparation.check()?;
        let session = self.store.session(session_id)?;
        preparation.check()?;
        if session.status.is_closed() {
            return Err(BrokerOpError::ClosedSessionOperation {
                session_id,
                repository_root: self.main_root().display().to_string(),
            });
        }
        let path = normalize_lease_path(path)?;
        preparation.check()?;
        let _git_deadline = crate::git::limit_git_until(preparation.deadline);
        // Explicit leases are authoritative records, not inferred Git edits.
        // The current holder policy and overlapping records below decide this
        // claim; refreshing every working checkout and classifying unrelated
        // overlaps is diagnostic work, not a prerequisite for authorization.
        // Keep implicit warnings at their recorded snapshot and do not advance
        // the global refresh timestamp here.
        preparation.check()?;
        let policy = self.lease_refusal_policy()?;
        preparation.check()?;
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
        preparation.check()?;
        if !blockers.is_empty() {
            return Err(BrokerOpError::LeaseClaimConflict {
                session_id,
                path,
                blocker_count: blockers.len(),
                blockers,
            });
        }
        // Keep the deadline immediately adjacent to the only explicit-claim
        // write. A timeout in preparation never inserts a claim row or emits
        // `lease.claimed`; the storage transaction itself is not interrupted.
        preparation.check()?;
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

    /// Run CLI guarded exec with its existing automatic mode by default. The
    /// explicit non-interactive opt-in forces process-group supervision; JSON
    /// mode moves child stdout to stderr so the final report remains one JSON
    /// document. All other child stdio behavior is preserved.
    pub(crate) fn guarded_exec_for_cli(
        &mut self,
        session_id: i64,
        command: &[String],
        force_supervised: bool,
        json_mode: bool,
        requested_timeout: Option<std::time::Duration>,
    ) -> Result<GuardedExecCliReport, BrokerOpError> {
        let mode = select_guarded_exec_mode(
            force_supervised,
            crate::inline_exec_supervisor::generic_exec_mode,
        );
        let supervised_timeout =
            crate::inline_exec_supervisor::select_guarded_exec_timeout(&mode, requested_timeout)
                .map_err(|reason| BrokerOpError::GuardedExecTimeoutRequiresSupervision {
                    session_id,
                    reason: format!(
                        "{reason}; add --non-interactive only when the command does not need terminal interaction"
                    ),
                })?
                .unwrap_or(crate::inline_exec_supervisor::DEFAULT_GUARDED_EXEC_TIMEOUT);
        let stdout = if json_mode {
            std::process::Stdio::from(std::io::stderr())
        } else {
            std::process::Stdio::inherit()
        };
        let (report, receipt_id, command_ran) = self
            .guarded_exec_with_env_heartbeat_mode_and_output(
                session_id,
                command,
                &[],
                || Ok(()),
                move || mode,
                GuardedExecOptions {
                    stdout,
                    supervised_timeout,
                    record_receipt: true,
                },
            )?;
        let receipt_id = receipt_id.expect("CLI guarded exec always records a receipt");
        Ok(GuardedExecCliReport {
            report,
            receipt_id,
            command_ran,
        })
    }

    /// Run one local Git commit under the session-local cooperative lane.
    /// The command still uses guarded-exec receipts, process supervision and
    /// dirty-path auditing; complete status and committed-tree audits bracket
    /// it so a successful commit cannot make an unauthorized path disappear
    /// from the post-command dirty list.
    pub(crate) fn guarded_commit_for_cli(
        &mut self,
        session_id: i64,
        caller_cwd: &Path,
        commit_args: &[String],
        force_supervised: bool,
        json_mode: bool,
        requested_timeout: Option<std::time::Duration>,
    ) -> Result<GuardedCommitCliReport, BrokerOpError> {
        if session_id <= 0 {
            return Err(BrokerOpError::SessionCommitRefused {
                reason: "session id must be positive; no command ran".into(),
            });
        }
        if commit_args.is_empty() {
            return Err(BrokerOpError::MissingExecCommand);
        }
        validate_session_commit_args(commit_args).map_err(|reason| {
            BrokerOpError::SessionCommitRefused {
                reason: format!("{reason}; no command ran"),
            }
        })?;
        if let Some(reason) = crate::git::session_commit_git_environment_refusal() {
            return Err(BrokerOpError::SessionCommitRefused {
                reason: format!("{reason}; no command ran"),
            });
        }

        let _session_commit_lock =
            crate::file_lock::try_acquire_session_commit_lock(&self.main_root, session_id)
                .map_err(|error| BrokerOpError::SessionCommitRefused {
                    reason: format!(
                        "could not acquire the non-queueing session-local commit lane: {error}; no command ran"
                    ),
                })?;

        if let Some(reason) = crate::git::session_commit_git_environment_refusal() {
            return Err(BrokerOpError::SessionCommitRefused {
                reason: format!("{reason}; no command ran"),
            });
        }
        let preflight_deadline = std::time::Instant::now() + SESSION_COMMIT_STATUS_BUDGET;
        let preflight_git_deadline = crate::git::limit_git_until(preflight_deadline);
        let (_session, checkout, head_before) = self
            .session_commit_context(session_id, caller_cwd)
            .map_err(|error| BrokerOpError::SessionCommitRefused {
                reason: format!("pre-commit session verification failed: {error}; no command ran"),
            })?;
        let (dirty, _) = checkout
            .dirty_and_untracked_paths_until_with_limits(
                preflight_deadline,
                SESSION_COMMIT_PATH_OUTPUT_CAP,
                SESSION_COMMIT_PATH_FIELD_CAP,
            )
            .map_err(|error| BrokerOpError::SessionCommitRefused {
                reason: format!(
                    "could not prove a complete pre-commit path census ({error}); no command ran"
                ),
            })?;
        let before_audit = self
            .audit_paths(session_id, &head_before, &head_before, dirty, true, false)
            .map_err(|error| BrokerOpError::SessionCommitRefused {
                reason: format!(
                    "pre-commit ownership verification failed: {error}; no command ran"
                ),
            })?;
        if !before_audit.ok {
            return Err(BrokerOpError::OwnershipViolation {
                summary: format!(
                    "broker commit refused before running: {}",
                    before_audit.failure_summary()
                ),
                report: Box::new(before_audit),
            });
        }
        if let Some(reason) = crate::git::session_commit_git_environment_refusal() {
            return Err(BrokerOpError::SessionCommitRefused {
                reason: format!("{reason}; no command ran"),
            });
        }
        let (_session_before_dispatch, _checkout_before_dispatch, dispatch_head) = self
            .session_commit_context(session_id, caller_cwd)
            .map_err(|error| BrokerOpError::SessionCommitRefused {
                reason: format!(
                    "final pre-commit session verification failed: {error}; no command ran"
                ),
            })?;
        if dispatch_head != head_before {
            return Err(BrokerOpError::SessionCommitRefused {
                reason: "the session branch moved during commit preflight; no command ran".into(),
            });
        }
        drop(preflight_git_deadline);

        let mut command = Vec::with_capacity(commit_args.len() + 2);
        command.push("git".to_owned());
        command.push("commit".to_owned());
        command.extend(commit_args.iter().cloned());
        let execution = self.guarded_exec_for_cli(
            session_id,
            &command,
            force_supervised,
            json_mode,
            requested_timeout,
        )?;
        let command_ran = execution.command_ran;

        let mut head_after = None;
        let mut committed_paths = None;
        let mut committed_ownership = None;
        let mut post_commit_audit_error = None;
        let post_commit_deadline = std::time::Instant::now() + SESSION_COMMIT_STATUS_BUDGET;
        let post_commit_git_deadline = crate::git::limit_git_until(post_commit_deadline);
        let post_context = self.session_commit_context(session_id, caller_cwd);
        match post_context {
            Ok((_session_after, checkout_after, committed_head)) => {
                head_after = Some(committed_head.clone());
                let history_is_one_commit = if committed_head == head_before {
                    true
                } else {
                    match checkout_after.commit_parents_until_with_limit(
                        &committed_head,
                        post_commit_deadline,
                        SESSION_COMMIT_PARENT_OUTPUT_CAP,
                    ) {
                        Ok(parents) if parents == [head_before.clone()] => true,
                        Ok(_) => {
                            post_commit_audit_error = Some(
                                "commit history changed by more than one direct child; complete committed-path audit is unknown".into(),
                            );
                            false
                        }
                        Err(error) => {
                            post_commit_audit_error = Some(format!(
                                "could not prove the committed history shape: {error}"
                            ));
                            false
                        }
                    }
                };
                match checkout_after.changed_between_until_with_limits(
                    &head_before,
                    &committed_head,
                    post_commit_deadline,
                    SESSION_COMMIT_PATH_OUTPUT_CAP,
                    SESSION_COMMIT_PATH_FIELD_CAP,
                ) {
                    Ok(paths) => {
                        committed_paths = Some(paths.clone());
                        match self.audit_paths(
                            session_id,
                            &head_before,
                            &committed_head,
                            paths,
                            true,
                            false,
                        ) {
                            Ok(audit) => committed_ownership = Some(audit),
                            Err(error) => {
                                post_commit_audit_error.get_or_insert_with(|| {
                                    format!(
                                        "could not complete committed-path ownership audit: {error}"
                                    )
                                });
                            }
                        }
                    }
                    Err(error) => {
                        post_commit_audit_error.get_or_insert_with(|| {
                            format!("could not obtain a complete committed-path census: {error}")
                        });
                    }
                }
                if !history_is_one_commit {
                    post_commit_audit_error.get_or_insert_with(|| {
                        "the hook changed commit history outside one direct child; inspect the worktree before retrying".into()
                    });
                }
            }
            Err(error) => {
                post_commit_audit_error = Some(format!(
                    "the command ran, but post-commit session and checkout verification failed: {error}"
                ));
            }
        }
        drop(post_commit_git_deadline);

        let post_commit_audit_complete = post_commit_audit_error.is_none()
            && committed_paths.is_some()
            && committed_ownership.is_some();
        let ownership_ok = execution.report.ownership_ok
            && post_commit_audit_complete
            && committed_ownership.as_ref().is_some_and(|audit| audit.ok);
        let ok = execution.report.command_success && ownership_ok;
        Ok(GuardedCommitCliReport {
            execution: execution.report,
            execution_receipt_id: execution.receipt_id,
            head_before,
            head_after,
            committed_paths,
            committed_ownership,
            post_commit_audit_error,
            post_commit_audit_complete,
            ok,
            command_ran,
        })
    }

    fn session_commit_context(
        &self,
        session_id: i64,
        caller_cwd: &Path,
    ) -> Result<(crate::Session, GitRepo, String), BrokerOpError> {
        let session = self.store.session(session_id)?;
        if session.status.is_closed() {
            return Err(BrokerOpError::ClosedSessionOperation {
                session_id,
                repository_root: self.main_root.display().to_string(),
            });
        }
        let caller =
            std::fs::canonicalize(caller_cwd).map_err(|source| crate::BrokerError::Io {
                path: caller_cwd.to_path_buf(),
                source,
            })?;
        let registered_path = Path::new(&session.worktree_path);
        let registered =
            std::fs::canonicalize(registered_path).map_err(|source| crate::BrokerError::Io {
                path: registered_path.to_path_buf(),
                source,
            })?;
        if caller != registered {
            return Err(BrokerOpError::SessionCommitRefused {
                reason: format!(
                    "current directory is not session {session_id}'s registered worktree; expected {}",
                    registered.display()
                ),
            });
        }
        let checkout = GitRepo::discover(&registered)?;
        let checkout_root =
            std::fs::canonicalize(checkout.root()).map_err(|source| crate::BrokerError::Io {
                path: checkout.root().to_path_buf(),
                source,
            })?;
        if checkout_root != registered {
            return Err(BrokerOpError::SessionCommitRefused {
                reason: format!(
                    "Git discovered a different worktree root for session {session_id}"
                ),
            });
        }
        let primary_common = self.repo.git_common_dir()?;
        let checkout_common = checkout.git_common_dir()?;
        if primary_common != checkout_common {
            return Err(BrokerOpError::SessionCommitRefused {
                reason: format!(
                    "session {session_id}'s worktree belongs to a different Git common directory"
                ),
            });
        }
        let head_ref = checkout.symbolic_ref("HEAD");
        let expected_head_ref = format!("refs/heads/{}", session.branch);
        if head_ref.as_deref() != Some(expected_head_ref.as_str()) {
            return Err(BrokerOpError::SessionCommitRefused {
                reason: format!("session {session_id} is not attached to its recorded branch"),
            });
        }
        let branch = checkout.current_branch()?;
        if branch != session.branch {
            return Err(BrokerOpError::SessionCommitRefused {
                reason: format!(
                    "session {session_id}'s checked-out branch {branch:?} differs from its recorded branch {:?}",
                    session.branch
                ),
            });
        }
        if crate::hooks::protected_branches(&checkout).contains(&branch) {
            return Err(BrokerOpError::SessionCommitRefused {
                reason: format!("session {session_id}'s branch {branch:?} is protected"),
            });
        }
        let head = checkout.head_commit()?;
        Ok((session, checkout, head))
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
        heartbeat: F,
        select_mode: M,
    ) -> Result<GuardedExecReport, BrokerOpError>
    where
        F: FnMut() -> Result<(), BrokerOpError>,
        M: FnOnce() -> crate::inline_exec_supervisor::GenericExecMode,
    {
        self.guarded_exec_with_env_heartbeat_mode_and_output(
            session_id,
            command,
            environment,
            heartbeat,
            select_mode,
            GuardedExecOptions {
                stdout: std::process::Stdio::inherit(),
                supervised_timeout: crate::inline_exec_supervisor::DEFAULT_GUARDED_EXEC_TIMEOUT,
                record_receipt: false,
            },
        )
        .map(|(report, _, _)| report)
    }

    fn guarded_exec_with_env_heartbeat_mode_and_output<F, M>(
        &mut self,
        session_id: i64,
        command: &[String],
        environment: &[(String, String)],
        mut heartbeat: F,
        select_mode: M,
        output: GuardedExecOptions,
    ) -> Result<(GuardedExecReport, Option<String>, Option<bool>), BrokerOpError>
    where
        F: FnMut() -> Result<(), BrokerOpError>,
        M: FnOnce() -> crate::inline_exec_supervisor::GenericExecMode,
    {
        let GuardedExecOptions {
            stdout,
            supervised_timeout,
            record_receipt,
        } = output;
        let supervisor_executable = self.agent_supervisor_executable.clone();
        self.guarded_exec_with_runner(
            session_id,
            command,
            record_receipt,
            move |session, observation, artifact_use, dispatch| {
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
                        let executable = match crate::agent_supervisor::resolve_executable(
                            supervisor_executable.as_deref(),
                        ) {
                            Ok(executable) => executable,
                            Err(source) => {
                                dispatch(GuardedExecDispatchEvent::NotStarted);
                                return Err(BrokerOpError::Spawn {
                                    command: command.join(" "),
                                    source,
                                });
                            }
                        };
                        let supervisor = match crate::inline_exec_supervisor::InlineExecSupervisor::start_inheriting_stdin(
                            &executable,
                            stdout,
                        ) {
                            Ok(supervisor) => supervisor,
                            Err(error) => {
                                dispatch(GuardedExecDispatchEvent::NotStarted);
                                return Err(map_guarded_exec_supervisor_error(
                                    session_id, command, error,
                                ));
                            }
                        };
                        let result = match supervisor
                            .with_observation(observation)
                            .with_artifact_use(artifact_use.take())
                            .run_with_timeout_and_dispatch(
                            Path::new(&session.worktree_path),
                            command,
                            &execution_environment,
                            Some(supervised_timeout),
                            &mut heartbeat,
                            |state| match state {
                                crate::inline_exec_supervisor::CommandDispatchState::Started => {
                                    dispatch(GuardedExecDispatchEvent::Started)
                                }
                                crate::inline_exec_supervisor::CommandDispatchState::NotStarted => {
                                    dispatch(GuardedExecDispatchEvent::NotStarted)
                                }
                            },
                        ) {
                            Ok(result) => result,
                            Err(error) => {
                                match &error {
                                    crate::inline_exec_supervisor::InlineExecError::CleanupUnknown(_) => {
                                        dispatch(GuardedExecDispatchEvent::OutcomeUnknown("unknown"));
                                    }
                                    crate::inline_exec_supervisor::InlineExecError::CommandOutcomeUnknown(_)
                                    | crate::inline_exec_supervisor::InlineExecError::OperationAfterDispatch(_) => {
                                        dispatch(GuardedExecDispatchEvent::OutcomeUnknown("verified"));
                                    }
                                    crate::inline_exec_supervisor::InlineExecError::Setup(_)
                                    | crate::inline_exec_supervisor::InlineExecError::CommandStart(_) => {}
                                }
                                return Err(map_guarded_exec_supervisor_error(
                                    session_id, command, error,
                                ));
                            }
                        };
                        GuardedCommandResult {
                            exit_code: result.exit_code,
                            succeeded: result.succeeded,
                            timed_out: result.termination
                                == crate::inline_exec_supervisor::CommandTermination::TimedOut,
                            command_started: result.command_started,
                            process_group_cleanup: "verified",
                        }
                    }
                    crate::inline_exec_supervisor::GenericExecMode::Legacy(reason) => {
                        eprintln!(
                            "warning: guarded exec for session {session_id} is using the legacy unsupervised runner ({reason}); descendant cleanup is not verified"
                        );
                        let mut child = Command::new(&command[0]);
                        child
                            .args(&command[1..])
                            .current_dir(&session.worktree_path)
                            .stdout(stdout);
                        for (name, value) in &execution_environment {
                            child.env(name, value);
                        }
                        let mut child = match child.spawn() {
                            Ok(child) => child,
                            Err(source) => {
                                dispatch(GuardedExecDispatchEvent::NotStarted);
                                return Err(BrokerOpError::Spawn {
                                    command: command.join(" "),
                                    source,
                                });
                            }
                        };
                        dispatch(GuardedExecDispatchEvent::Started);
                        let status = loop {
                            match child.try_wait() {
                                Ok(Some(status)) => break status,
                                Ok(None) => {}
                                Err(source) => {
                                    dispatch(GuardedExecDispatchEvent::OutcomeUnknown(
                                        "unverified",
                                    ));
                                    return Err(BrokerOpError::Spawn {
                                        command: command.join(" "),
                                        source,
                                    });
                                }
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
                                dispatch(GuardedExecDispatchEvent::OutcomeUnknown(
                                    "unverified",
                                ));
                                return Err(error);
                            }
                            std::thread::sleep(std::time::Duration::from_millis(200));
                        };
                        GuardedCommandResult {
                            exit_code: status.code(),
                            succeeded: status.success(),
                            timed_out: false,
                            command_started: true,
                            process_group_cleanup: "unverified",
                        }
                    }
                };
                Ok(result)
            },
        )
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
        self.guarded_exec_with_runner(
            session_id,
            command,
            false,
            move |session, _observation, artifact_use, _dispatch| {
                let result = supervisor
                    .with_artifact_use(artifact_use.take())
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
                    timed_out: result.termination
                        == crate::inline_exec_supervisor::CommandTermination::TimedOut,
                    command_started: result.command_started,
                    process_group_cleanup: "verified",
                })
            },
        )
        .map(|(report, _, _)| report)
    }

    fn guarded_exec_with_runner<F>(
        &mut self,
        session_id: i64,
        command: &[String],
        record_receipt: bool,
        run: F,
    ) -> Result<(GuardedExecReport, Option<String>, Option<bool>), BrokerOpError>
    where
        F: FnOnce(
            &crate::types::Session,
            Option<crate::store::guarded_exec::HelperObservationTarget>,
            &mut Option<crate::file_lock::artifact_use::ArtifactUseProducer>,
            &mut dyn FnMut(GuardedExecDispatchEvent),
        ) -> Result<GuardedCommandResult, BrokerOpError>,
    {
        if command.is_empty() {
            return Err(BrokerOpError::MissingExecCommand);
        }
        let session = self.store.session(session_id)?;
        // Global implicit leases and overlap classification are diagnostic
        // snapshots maintained by explicit refresh/submit paths. Guarded
        // authorization below reads current explicit leases, applies current
        // holder policy, and checks the foreign-file snapshot directly. Avoid
        // rescanning every live checkout here; leave the refresh timestamp
        // unchanged so it continues to describe the diagnostic snapshot.
        let checkout = GitRepo::discover(Path::new(&session.worktree_path))?;
        let (mut before_dirty, before_untracked) = checkout.dirty_and_untracked_paths()?;
        before_dirty.sort();
        before_dirty.dedup();
        let before_untracked = before_untracked
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        let before_identities = snapshot_path_identities(checkout.root(), &before_dirty)?;

        let receipt_id = if record_receipt {
            Some(self.store.begin_guarded_exec_receipt(session_id)?)
        } else {
            None
        };
        let observation = receipt_id
            .as_deref()
            .map(|id| self.store.helper_observation_target(session_id, id))
            .transpose()?
            .flatten();
        let mut artifact_use = match crate::file_lock::artifact_use::begin_artifact_use(
            self.main_root(),
            checkout.root(),
            session_id,
            &crate::file_lock::artifact_use::invocation_id("guarded-exec", session_id),
        ) {
            Ok(crate::file_lock::artifact_use::ArtifactUseAdmission::Admitted(producer)) => {
                Some(*producer)
            }
            Ok(crate::file_lock::artifact_use::ArtifactUseAdmission::ProtocolUnavailable) => None,
            Err(source) => {
                if let Some(receipt_id) = receipt_id.as_deref() {
                    crate::warn_unrecorded(
                        "mark guarded exec not started after artifact admission failure",
                        self.store
                            .mark_guarded_exec_not_started(session_id, receipt_id),
                    );
                }
                return Err(crate::BrokerError::Io {
                    path: checkout.root().to_path_buf(),
                    source,
                }
                .into());
            }
        };
        let mut pending_not_started_dispatch = false;
        let mut terminal_dispatch_recorded = false;
        let run_result = {
            let mut dispatch = |event| {
                match event {
                    GuardedExecDispatchEvent::NotStarted => {
                        pending_not_started_dispatch = true;
                        return;
                    }
                    GuardedExecDispatchEvent::Started
                    | GuardedExecDispatchEvent::OutcomeUnknown(_) => {
                        pending_not_started_dispatch = false;
                    }
                }
                let Some(receipt_id) = receipt_id.as_deref() else {
                    return;
                };
                let result = match event {
                    GuardedExecDispatchEvent::NotStarted => {
                        unreachable!("not-started dispatch returns before receipt persistence")
                    }
                    GuardedExecDispatchEvent::Started => {
                        self.store.mark_guarded_exec_running(session_id, receipt_id)
                    }
                    GuardedExecDispatchEvent::OutcomeUnknown(cleanup) => self
                        .store
                        .mark_guarded_exec_outcome_unknown(session_id, receipt_id, cleanup),
                };
                match result {
                    Ok(()) => {
                        if matches!(event, GuardedExecDispatchEvent::OutcomeUnknown(_)) {
                            terminal_dispatch_recorded = true;
                        }
                    }
                    Err(error) => crate::warn_unrecorded(
                        "persist guarded exec dispatch status",
                        Err::<(), _>(error),
                    ),
                }
            };
            run(&session, observation, &mut artifact_use, &mut dispatch)
        };
        let status = match run_result {
            Ok(status) => status,
            Err(error) => {
                let known_not_started =
                    pending_not_started_dispatch && matches!(&error, BrokerOpError::Spawn { .. });
                if known_not_started
                    && let Some(producer) = artifact_use.as_mut()
                    && producer.is_pre_dispatch()
                {
                    crate::warn_unrecorded(
                        "clear guarded exec artifact-use marker after confirmed no-start",
                        producer.finish_not_started(),
                    );
                }
                if let Some(receipt_id) = receipt_id.as_deref()
                    && !terminal_dispatch_recorded
                {
                    let result = if known_not_started {
                        self.store
                            .mark_guarded_exec_not_started(session_id, receipt_id)
                    } else {
                        self.store
                            .mark_guarded_exec_outcome_unknown(session_id, receipt_id, "unknown")
                    };
                    if let Err(persist_error) = result {
                        crate::warn_unrecorded(
                            "persist guarded exec dispatch status",
                            Err::<(), _>(persist_error),
                        );
                        if known_not_started {
                            crate::warn_unrecorded(
                                "persist guarded exec unknown outcome",
                                self.store.mark_guarded_exec_outcome_unknown(
                                    session_id, receipt_id, "unknown",
                                ),
                            );
                        }
                    }
                }
                return Err(error);
            }
        };

        if let Some(receipt_id) = receipt_id.as_deref() {
            if status.command_started {
                let command_outcome = if status.timed_out {
                    "timed_out"
                } else if status.succeeded {
                    "succeeded"
                } else {
                    "failed"
                };
                crate::warn_unrecorded(
                    "persist guarded exec command outcome",
                    self.store.complete_guarded_exec_command(
                        session_id,
                        receipt_id,
                        command_outcome,
                        status.exit_code,
                        status.process_group_cleanup,
                    ),
                );
            } else {
                crate::warn_unrecorded(
                    "persist guarded exec unconfirmed dispatch outcome",
                    self.store.mark_guarded_exec_outcome_unknown(
                        session_id,
                        receipt_id,
                        status.process_group_cleanup,
                    ),
                );
            }
        }

        let (mut after_dirty, after_untracked) = checkout.dirty_and_untracked_paths()?;
        after_dirty.sort();
        after_dirty.dedup();
        let before_set: std::collections::BTreeSet<String> = before_dirty.iter().cloned().collect();
        let newly_dirty_paths: Vec<String> = after_dirty
            .iter()
            .filter(|path| !before_set.contains(*path))
            .cloned()
            .collect();
        let mut new_untracked_paths = after_untracked
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
        if let Some(receipt_id) = receipt_id.as_deref() {
            crate::warn_unrecorded(
                "persist guarded exec ownership audit",
                self.store.finish_guarded_exec_audit(
                    session_id,
                    receipt_id,
                    if audit.ok { "passed" } else { "failed" },
                ),
            );
        }
        let command_success = status.succeeded;
        let ok = command_success && audit.ok;
        let report = GuardedExecReport {
            session_id,
            command: command.to_vec(),
            exit_code: status.exit_code,
            command_success,
            command_timed_out: status.timed_out,
            ownership_ok: audit.ok,
            conflicting_leases: audit.conflicting_leases,
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
        Ok((report, receipt_id, status.command_started.then_some(true)))
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

#[cfg(test)]
mod worktree_inventory_scan_tests {
    use super::*;

    #[test]
    fn iterator_error_preserves_later_directories_and_marks_inventory_incomplete() {
        let root = tempfile::tempdir().unwrap();
        let first_path = root.path().join("first");
        let later_path = root.path().join("later");
        std::fs::create_dir(&first_path).unwrap();
        std::fs::create_dir(&later_path).unwrap();

        let read_entries = || {
            std::fs::read_dir(root.path())
                .unwrap()
                .map(Result::unwrap)
                .collect::<Vec<_>>()
        };
        let roots = [root.path().to_path_buf()];
        let mut complete_observations = Vec::new();
        assert!(scan_worktree_roots(
            &roots,
            &mut complete_observations,
            None,
            |root| std::fs::read_dir(root)
        ));
        assert_eq!(complete_observations.len(), 2);

        let mut entries = read_entries().into_iter();
        let first = entries.next().unwrap();
        let later = entries.next().unwrap();
        let injected: Vec<std::io::Result<std::fs::DirEntry>> = vec![
            Ok(first),
            Err(std::io::Error::other("injected directory-entry failure")),
            Ok(later),
        ];
        let mut injected = Some(injected);
        let mut partial_observations = Vec::new();
        let complete = scan_worktree_roots(&roots, &mut partial_observations, None, |_| {
            Ok(injected.take().unwrap().into_iter())
        });
        assert!(!complete);
        let observed_paths: std::collections::BTreeSet<_> = partial_observations
            .into_iter()
            .map(|entry| entry.path)
            .collect();
        let expected_paths: std::collections::BTreeSet<_> = [first_path, later_path]
            .into_iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        assert_eq!(observed_paths, expected_paths);
    }

    #[test]
    fn metadata_error_for_a_listed_entry_marks_inventory_incomplete() {
        let root = tempfile::tempdir().unwrap();
        let removed_path = root.path().join("removed");
        std::fs::create_dir(&removed_path).unwrap();
        let entry = std::fs::read_dir(root.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        std::fs::remove_dir(&removed_path).unwrap();

        let roots = [root.path().to_path_buf()];
        let mut injected = Some(vec![Ok(entry)]);
        let mut observed = Vec::new();
        let complete = scan_worktree_roots(&roots, &mut observed, None, |_| {
            Ok(injected.take().unwrap().into_iter())
        });
        assert!(!complete);
        assert!(observed.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn git_marker_metadata_error_marks_inventory_incomplete() {
        let root = tempfile::tempdir().unwrap();
        let worktree = root.path().join("worktree");
        std::fs::create_dir(&worktree).unwrap();
        std::os::unix::fs::symlink(".git", worktree.join(".git")).unwrap();

        let roots = [root.path().to_path_buf()];
        let mut observed = Vec::new();
        let complete =
            scan_worktree_roots(&roots, &mut observed, None, |root| std::fs::read_dir(root));

        assert!(!complete);
        assert_eq!(observed.len(), 1);
        assert!(!observed[0].git_marker);
    }

    #[test]
    fn expired_deadline_stops_before_a_stream_of_entry_errors() {
        let mut observed = Vec::new();
        let errors: [std::io::Result<std::fs::DirEntry>; 2] = [
            Err(std::io::Error::other("injected directory-entry failure")),
            Err(std::io::Error::other("injected directory-entry failure")),
        ];
        let expired = std::time::Instant::now() - std::time::Duration::from_secs(1);

        assert_eq!(
            collect_worktree_directory_entries(errors, &mut observed, Some(expired)),
            WorktreeDirectoryScan::DeadlineReached
        );
        assert!(observed.is_empty());
    }

    #[test]
    fn failed_unbounded_size_is_unknown_not_zero_and_does_not_hide_later_sizes() {
        let mut entries = vec![
            crate::worktree_reconcile::ReconciledDirectory {
                path: "/unreadable".into(),
                claim: crate::worktree_reconcile::DirectoryClaim::Unclaimed,
                git_marker: false,
                estimated_bytes: None,
            },
            crate::worktree_reconcile::ReconciledDirectory {
                path: "/readable".into(),
                claim: crate::worktree_reconcile::DirectoryClaim::Unclaimed,
                git_marker: false,
                estimated_bytes: None,
            },
        ];
        let mut calls = 0;
        let complete = size_unclaimed_directories(&mut entries, |_| {
            calls += 1;
            if calls == 1 {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected sizing failure",
                ))
            } else {
                Ok(7)
            }
        });

        assert!(!complete);
        assert_eq!(entries[0].estimated_bytes, None);
        assert_eq!(entries[1].estimated_bytes, Some(7));
    }
}

fn select_guarded_exec_mode(
    force_supervised: bool,
    automatic_mode: impl FnOnce() -> crate::inline_exec_supervisor::GenericExecMode,
) -> crate::inline_exec_supervisor::GenericExecMode {
    if force_supervised {
        crate::inline_exec_supervisor::GenericExecMode::Supervised
    } else {
        automatic_mode()
    }
}

#[cfg(test)]
mod guarded_exec_mode_selection_tests {
    use super::select_guarded_exec_mode;
    use crate::inline_exec_supervisor::GenericExecMode;

    #[test]
    fn non_interactive_opt_in_forces_supervision_without_tty_probe() {
        let mode = select_guarded_exec_mode(true, || {
            panic!("opt-in must not probe the controlling terminal")
        });
        assert_eq!(mode, GenericExecMode::Supervised);
    }

    #[test]
    fn default_exec_mode_preserves_automatic_selection() {
        let expected = GenericExecMode::Legacy("test controlling terminal".into());
        let mode = select_guarded_exec_mode(false, || expected.clone());
        assert_eq!(mode, expected);
    }
}

#[cfg(all(test, not(any(target_os = "linux", target_os = "macos"))))]
mod guarded_exec_unsupported_platform_tests {
    use crate::inline_exec_supervisor::{InlineExecError, InlineExecSupervisor};
    use std::path::Path;

    #[test]
    fn supervised_runner_refuses_unsupported_platform_before_dispatch() {
        let error = match InlineExecSupervisor::start_inheriting_stdin(
            Path::new("unused-supervisor-path"),
            std::process::Stdio::inherit(),
        ) {
            Err(error) => error,
            Ok(_) => panic!("unsupported platform started the supervisor"),
        };
        assert!(matches!(
            error,
            InlineExecError::Setup(ref source)
                if source.kind() == std::io::ErrorKind::Unsupported
        ));
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
    timed_out: bool,
    command_started: bool,
    process_group_cleanup: &'static str,
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
    use std::path::{Path, PathBuf};
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

    struct ReleaseHookOnDrop(PathBuf);

    impl Drop for ReleaseHookOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::write(&self.0, b"continue");
        }
    }

    #[test]
    fn local_commit_hook_does_not_hold_the_repository_lock_needed_by_fetch() {
        use crate::operations::{CoordinatedCommand, QueueWait};
        use crate::types::{OperationEffect, OperationProvider};

        let (repository, storage, mut commit_broker) = fixture();
        git(repository.path(), &["config", "user.name", "Fixture"]);
        git(
            repository.path(),
            &["config", "user.email", "fixture@example.invalid"],
        );
        let session = commit_broker
            .start_worktree("session-local commit with a waiting hook", None)
            .unwrap();
        let worktree = PathBuf::from(&session.worktree_path)
            .canonicalize()
            .unwrap();
        commit_broker
            .claim_lease(session.id, "tracked.txt", None)
            .unwrap();
        std::fs::write(worktree.join("tracked.txt"), "committed by the session\n").unwrap();
        git(&worktree, &["add", "tracked.txt"]);

        let bare = tempfile::tempdir().unwrap();
        git(bare.path(), &["init", "--bare", "-q"]);
        let bare_path = bare.path().to_string_lossy().into_owned();
        git(repository.path(), &["push", &bare_path, "main"]);

        let ssh_proxy = tempfile::tempdir().unwrap();
        let ssh_proxy_path = ssh_proxy.path().join("ssh-proxy");
        let quoted_bare_path = crate::broker::shell_quote(&bare_path);
        std::fs::write(
            &ssh_proxy_path,
            format!("#!/bin/sh\nexec git upload-pack {quoted_bare_path}\n"),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut permissions = std::fs::metadata(&ssh_proxy_path).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&ssh_proxy_path, permissions).unwrap();
        }
        let ssh_proxy_command = crate::broker::shell_quote(&ssh_proxy_path.to_string_lossy());
        git(
            repository.path(),
            &["config", "core.sshCommand", &ssh_proxy_command],
        );
        git(repository.path(), &["config", "ssh.variant", "ssh"]);
        git(
            repository.path(),
            &["remote", "add", "origin", "git@github.com:example/repo.git"],
        );

        let hooks = tempfile::tempdir().unwrap();
        let entered = hooks.path().join("pre-commit-entered");
        let release = hooks.path().join("release-pre-commit");
        let script = format!(
            "#!/bin/sh\nprintf entered > '{}'\nwhile [ ! -f '{}' ]; do sleep 0.02; done\n",
            entered.display(),
            release.display()
        );
        let hook = hooks.path().join("pre-commit");
        std::fs::write(&hook, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut permissions = std::fs::metadata(&hook).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&hook, permissions).unwrap();
        }
        let hooks_path = hooks.path().to_string_lossy().into_owned();
        git(&worktree, &["config", "core.hooksPath", &hooks_path]);

        let session_id = session.id;
        let commit_worktree = worktree.clone();
        let commit = std::thread::spawn(move || {
            commit_broker.guarded_commit_for_cli(
                session_id,
                &commit_worktree,
                &["-m".into(), "session commit".into()],
                true,
                false,
                None,
            )
        });
        let _release_hook = ReleaseHookOnDrop(release.clone());
        let hook_deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !entered.exists() && std::time::Instant::now() < hook_deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if !entered.exists() {
            let _ = std::fs::write(&release, b"continue");
            let result = commit.join().unwrap();
            panic!("pre-commit hook never entered; commit result: {result:?}");
        }

        let mut fetch_broker = storage.open(repository.path()).unwrap();
        let cleanup = fetch_broker.cleanup(session_id, true);
        assert!(
            matches!(&cleanup, Err(BrokerOpError::DirtyWorktree { .. })),
            "cleanup must leave a session worktree alone while its commit hook is running: {cleanup:?}"
        );
        assert!(worktree.is_dir(), "the active commit worktree was removed");
        let unrelated_session = fetch_broker
            .start_worktree("unrelated session while another commit hook waits", None)
            .unwrap();
        let fetch = fetch_broker.run_coordinated_operation_with_wait(
            CoordinatedCommand {
                session_id,
                provider: OperationProvider::Git,
                repository: Some("example/repo".into()),
                resolved_target: None,
                scope: None,
                declared_effect: Some(OperationEffect::Write),
                destructive_confirmed: false,
                cross_session: None,
                ref_write_acknowledged: false,
                authorization_reason: Some(
                    "verify a coordinated fetch can proceed during a session-local commit hook"
                        .into(),
                ),
                args: vec!["fetch".into(), "origin".into()],
            },
            QueueWait::Seconds(4),
        );
        std::fs::write(&release, b"continue").unwrap();
        let committed = commit.join().unwrap().unwrap();
        let fetched = fetch.expect("coordinated fetch should finish while the hook is waiting");
        assert!(fetched.ok(), "{fetched:?}");
        assert!(committed.ok, "{committed:?}");
        assert!(committed.post_commit_audit_complete, "{committed:?}");
        assert_eq!(
            committed.committed_paths.as_deref().unwrap(),
            &["tracked.txt".to_owned()]
        );
        fetch_broker.cleanup(unrelated_session.id, true).unwrap();
    }

    #[test]
    fn local_commit_refuses_a_staged_path_without_session_ownership_before_dispatch() {
        let (_repository, _storage, mut broker) = fixture();
        let session = broker
            .start_worktree("session-local commit preflight", None)
            .unwrap();
        let worktree = PathBuf::from(&session.worktree_path)
            .canonicalize()
            .unwrap();
        std::fs::write(worktree.join("tracked.txt"), "unleased staged change\n").unwrap();
        git(&worktree, &["add", "tracked.txt"]);
        let head_before = GitRepo::discover(&worktree).unwrap().head_commit().unwrap();

        let error = broker
            .guarded_commit_for_cli(
                session.id,
                &worktree,
                &["-m".into(), "must not run".into()],
                true,
                false,
                None,
            )
            .unwrap_err();

        assert!(
            matches!(error, BrokerOpError::OwnershipViolation { .. }),
            "{error}"
        );
        assert_eq!(
            GitRepo::discover(&worktree).unwrap().head_commit().unwrap(),
            head_before
        );
        assert!(
            broker
                .store
                .guarded_exec_receipts(session.id, 10)
                .unwrap()
                .is_empty(),
            "a preflight refusal must happen before a guarded-exec receipt is created"
        );
        assert!(
            GitRepo::discover(&worktree)
                .unwrap()
                .dirty_and_untracked_paths()
                .unwrap()
                .0
                .contains(&"tracked.txt".to_owned()),
            "the staged edit must remain available for the user"
        );
    }

    #[test]
    fn local_commit_refuses_foreign_cwd_and_closed_session_before_dispatch() {
        let (repository, _storage, mut broker) = fixture();
        let session = broker
            .start_worktree("session-local commit scope refusal", None)
            .unwrap();
        let worktree = PathBuf::from(&session.worktree_path)
            .canonicalize()
            .unwrap();
        let head_before = GitRepo::discover(&worktree).unwrap().head_commit().unwrap();

        let foreign_cwd = broker
            .guarded_commit_for_cli(
                session.id,
                repository.path(),
                &["-m".into(), "must not run".into()],
                true,
                false,
                None,
            )
            .unwrap_err();
        assert!(
            matches!(foreign_cwd, BrokerOpError::SessionCommitRefused { .. }),
            "{foreign_cwd}"
        );

        broker
            .store
            .set_session_status(session.id, crate::SessionStatus::Closed, None)
            .unwrap();
        let closed = broker
            .guarded_commit_for_cli(
                session.id,
                &worktree,
                &["-m".into(), "must not run".into()],
                true,
                false,
                None,
            )
            .unwrap_err();
        assert!(
            matches!(closed, BrokerOpError::SessionCommitRefused { .. }),
            "{closed}"
        );
        assert_eq!(
            GitRepo::discover(&worktree).unwrap().head_commit().unwrap(),
            head_before
        );
        assert!(
            broker
                .store
                .guarded_exec_receipts(session.id, 10)
                .unwrap()
                .is_empty(),
            "scope refusals must happen before a guarded-exec receipt is created"
        );
    }

    #[test]
    fn local_commit_refuses_inherited_git_routing_environment_before_dispatch() {
        const CHILD_MODE: &str = "AETHYME_SESSION_COMMIT_ENV_TEST_CHILD";
        const TEST: &str = "broker::leases::generic_guarded_exec_tests::local_commit_refuses_inherited_git_routing_environment_before_dispatch";
        if std::env::var_os(CHILD_MODE).is_some() {
            let (_repository, _storage, mut broker) = fixture();
            let session = broker
                .start_worktree("session-local commit environment refusal", None)
                .unwrap();
            let worktree = PathBuf::from(&session.worktree_path)
                .canonicalize()
                .unwrap();
            let head_before = GitRepo::discover(&worktree).unwrap().head_commit().unwrap();

            // SAFETY: this test re-executes only itself in a child process;
            // no other test or concurrent environment reader runs there.
            unsafe { std::env::set_var("GIT_DIR", "/untrusted/git-dir") };
            let refusal = broker
                .guarded_commit_for_cli(
                    session.id,
                    &worktree,
                    &["-m".into(), "must not run".into()],
                    true,
                    false,
                    None,
                )
                .unwrap_err();
            // SAFETY: the same isolated test process is still the only code
            // running here, and restoring precedes any later Git use.
            unsafe { std::env::remove_var("GIT_DIR") };
            assert!(
                matches!(refusal, BrokerOpError::SessionCommitRefused { .. }),
                "{refusal}"
            );
            assert_eq!(
                GitRepo::discover(&worktree).unwrap().head_commit().unwrap(),
                head_before
            );
            assert!(
                broker
                    .store
                    .guarded_exec_receipts(session.id, 10)
                    .unwrap()
                    .is_empty(),
                "environment refusal must happen before a receipt is created"
            );
            println!("AETHYME_SESSION_COMMIT_ENV_ASSERTIONS_OK");
            return;
        }

        let mut child = Command::new(std::env::current_exe().unwrap());
        child.args(["--exact", TEST, "--nocapture"]);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                child.env_remove(key);
            }
        }
        let output = child.env(CHILD_MODE, "1").output().unwrap();
        assert!(
            output.status.success(),
            "child test failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .contains("AETHYME_SESSION_COMMIT_ENV_ASSERTIONS_OK"),
            "the child must execute the assertions, not only exit successfully:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    #[test]
    fn local_commit_audits_a_foreign_path_added_by_a_hook_after_dispatch() {
        let (repository, _storage, mut broker) = fixture();
        git(repository.path(), &["config", "user.name", "Fixture"]);
        git(
            repository.path(),
            &["config", "user.email", "fixture@example.invalid"],
        );
        let session = broker
            .start_worktree("session-local commit hook ownership audit", None)
            .unwrap();
        let worktree = PathBuf::from(&session.worktree_path)
            .canonicalize()
            .unwrap();
        broker.claim_lease(session.id, "tracked.txt", None).unwrap();
        std::fs::write(worktree.join("tracked.txt"), "leased edit\n").unwrap();
        git(&worktree, &["add", "tracked.txt"]);

        let hooks = tempfile::tempdir().unwrap();
        let hook = hooks.path().join("pre-commit");
        std::fs::write(
            &hook,
            "#!/bin/sh\nprintf 'hook edit\\n' > hook-added.txt\ngit add -- hook-added.txt\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut permissions = std::fs::metadata(&hook).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&hook, permissions).unwrap();
        }
        let hooks_path = hooks.path().to_string_lossy().into_owned();
        git(&worktree, &["config", "core.hooksPath", &hooks_path]);

        let head_before = GitRepo::discover(&worktree).unwrap().head_commit().unwrap();
        let report = broker
            .guarded_commit_for_cli(
                session.id,
                &worktree,
                &["-m".into(), "hook adds an unowned path".into()],
                true,
                false,
                None,
            )
            .unwrap();

        assert!(report.execution.command_success, "{report:?}");
        assert!(
            !report.ok,
            "the commit must not pass ownership audit: {report:?}"
        );
        assert!(report.post_commit_audit_complete, "{report:?}");
        assert_ne!(report.head_after.as_deref(), Some(head_before.as_str()));
        let paths = report
            .committed_paths
            .as_ref()
            .expect("complete path census");
        assert!(paths.iter().any(|path| path == "tracked.txt"), "{paths:?}");
        assert!(
            paths.iter().any(|path| path == "hook-added.txt"),
            "{paths:?}"
        );
        assert!(!report.committed_ownership.as_ref().unwrap().ok);
        assert!(worktree.join("hook-added.txt").is_file());
        assert!(
            !report.execution_receipt_id.is_empty(),
            "the command's durable receipt remains available for investigation"
        );
    }

    #[cfg(unix)]
    #[test]
    fn guarded_exec_and_claim_do_not_probe_unrelated_live_worktrees() {
        const CHILD_MODE: &str = "AETHYME_GUARDED_EXEC_TRACE_CHILD";
        const TEST: &str = "broker::leases::generic_guarded_exec_tests::guarded_exec_and_claim_do_not_probe_unrelated_live_worktrees";
        if std::env::var_os(CHILD_MODE).is_some() {
            let (_repository, _storage, mut broker) = fixture();
            let actor = broker
                .start_worktree("guarded exec trace actor", None)
                .unwrap();
            let peer = broker
                .start_worktree("guarded exec trace peer", None)
                .unwrap();
            let actor_root = PathBuf::from(&actor.worktree_path).canonicalize().unwrap();
            let peer_root = PathBuf::from(&peer.worktree_path).canonicalize().unwrap();
            std::fs::write(peer_root.join("tracked.txt"), "peer edit\n").unwrap();
            broker.claim_lease(actor.id, "tracked.txt", None).unwrap();
            assert!(
                broker
                    .coordinating_session_ids()
                    .unwrap()
                    .contains(&peer.id),
                "the peer must be a live coordinator so the old global refresh would scan it"
            );
            broker
                .store
                .meta_set("leases.refreshed_at_ms", "12345")
                .unwrap();
            let trace = PathBuf::from(
                std::env::var_os("GIT_TRACE2_EVENT").expect("parent supplies a private trace path"),
            );
            std::fs::write(&trace, b"").unwrap();

            let claim = broker.claim_lease(actor.id, "tracked.txt", None).unwrap();
            assert!(claim.accepted);
            assert_eq!(
                broker
                    .store
                    .meta_get("leases.refreshed_at_ms")
                    .unwrap()
                    .as_deref(),
                Some("12345"),
                "an explicit claim must not refresh the global diagnostic snapshot"
            );

            let report = broker
                .guarded_exec_with_env_heartbeat_and_mode(
                    actor.id,
                    &[
                        "/bin/sh".into(),
                        "-c".into(),
                        "printf guarded > tracked.txt".into(),
                    ],
                    &[],
                    || Ok(()),
                    || crate::inline_exec_supervisor::GenericExecMode::Supervised,
                )
                .unwrap();
            assert!(report.command_success);
            assert!(report.ownership_ok);
            assert_eq!(
                broker
                    .store
                    .meta_get("leases.refreshed_at_ms")
                    .unwrap()
                    .as_deref(),
                Some("12345"),
                "guarded exec must not claim the global diagnostic snapshot was refreshed"
            );

            let worktrees: Vec<PathBuf> = std::fs::read_to_string(&trace)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                .filter(|event| event["event"] == "def_repo")
                .filter_map(|event| event["worktree"].as_str().map(PathBuf::from))
                .map(|path| path.canonicalize().unwrap_or(path))
                .collect();
            assert!(
                worktrees.iter().any(|path| path == &actor_root),
                "the trace must observe Git in the guarded checkout: {worktrees:?}"
            );
            assert!(
                !worktrees.iter().any(|path| path == &peer_root),
                "claim and guarded exec must not start Git in an unrelated live checkout: {worktrees:?}"
            );
            println!("AETHYME_GUARDED_EXEC_TRACE_ASSERTIONS_OK");
            return;
        }

        let parent = tempfile::tempdir().unwrap();
        let parent_path = parent.path().canonicalize().unwrap();
        let trace = parent_path.join("git-trace.jsonl");
        let home = parent_path.join("home");
        std::fs::create_dir(&home).unwrap();
        let global_config = parent_path.join("empty-git-config");
        std::fs::write(
            &global_config,
            b"[maintenance]\n\tauto = false\n[gc]\n\tauto = 0\n",
        )
        .unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap());
        child.args(["--exact", TEST, "--nocapture"]);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                child.env_remove(key);
            }
        }
        let output = child
            .env(CHILD_MODE, "1")
            .env("HOME", &home)
            .env("GIT_CONFIG_GLOBAL", &global_config)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TRACE2_EVENT", &trace)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child test failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .contains("AETHYME_GUARDED_EXEC_TRACE_ASSERTIONS_OK"),
            "the child must execute the assertions, not only exit successfully:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    #[test]
    fn guarded_exec_preserves_out_of_lease_foreign_and_holder_policy_checks() {
        let (_repository, _storage, mut broker) = fixture();
        let actor = broker
            .start_worktree("guarded ownership actor", None)
            .unwrap();
        let holder = broker
            .start_worktree("guarded ownership holder", None)
            .unwrap();
        broker
            .store
            .connection()
            .execute(
                "UPDATE sessions SET pid = NULL, status = 'active', last_activity_at = ?1 WHERE id = ?2",
                rusqlite::params![crate::clock::epoch_ms(), holder.id],
            )
            .unwrap();
        broker.claim_lease(holder.id, "tracked.txt", None).unwrap();
        let holder_view = broker
            .agents_snapshot(crate::clock::epoch_ms())
            .unwrap()
            .into_iter()
            .find(|agent| agent.session.id == holder.id)
            .unwrap();
        assert_eq!(
            holder_view.pid_alive, None,
            "the holder has unknown process evidence"
        );
        assert_eq!(holder_view.derived_status, crate::SessionStatus::Active);
        let expected_holder_block = !broker.lease_refusal_policy().unwrap().is_verify_only();

        let guarded_write = |broker: &mut Broker, target: &str, value: &str| {
            broker
                .guarded_exec_with_env_heartbeat_and_mode(
                    actor.id,
                    &[
                        "/bin/sh".into(),
                        "-c".into(),
                        "printf '%s' \"$VALUE\" > \"$TARGET\"".into(),
                    ],
                    &[
                        ("TARGET".into(), target.into()),
                        ("VALUE".into(), value.into()),
                    ],
                    || Ok(()),
                    || crate::inline_exec_supervisor::GenericExecMode::Supervised,
                )
                .unwrap()
        };

        let outside = guarded_write(&mut broker, "tracked.txt", "unclaimed change\n");
        assert!(!outside.ownership_ok);
        assert!(
            outside
                .outside_lease_paths
                .iter()
                .any(|path| path == "tracked.txt")
        );
        assert_eq!(
            outside
                .conflicting_leases
                .iter()
                .any(|lease| lease.session_id == holder.id && lease.path == "tracked.txt"),
            expected_holder_block,
            "the holder has unknown PID evidence but remains active; conflict reporting follows the current repository policy"
        );

        let actor_root = PathBuf::from(&actor.worktree_path);
        std::fs::write(actor_root.join("foreign.txt"), "before\n").unwrap();
        broker
            .store
            .set_session_foreign_files(actor.id, &["foreign.txt".to_owned()])
            .unwrap();
        let foreign = guarded_write(&mut broker, "foreign.txt", "changed foreign content\n");
        assert!(!foreign.ownership_ok);
        assert!(
            foreign
                .foreign_paths
                .iter()
                .any(|path| path == "foreign.txt")
        );
        assert!(
            foreign
                .outside_lease_paths
                .iter()
                .any(|path| path == "foreign.txt")
        );

        broker.claim_lease(actor.id, "foreign.txt", None).unwrap();
        let explicitly_owned = guarded_write(&mut broker, "foreign.txt", "claimed change\n");
        assert!(explicitly_owned.ownership_ok);
        assert!(
            explicitly_owned
                .outside_lease_paths
                .iter()
                .all(|path| path != "foreign.txt")
        );
        assert!(
            explicitly_owned
                .foreign_paths
                .iter()
                .all(|path| path != "foreign.txt")
        );
    }

    #[test]
    fn lease_claim_preparation_budget_is_positive_and_capped() {
        assert_eq!(
            lease_claim_preparation_budget_ms_from(None),
            DEFAULT_LEASE_CLAIM_PREPARATION_BUDGET_MS
        );
        assert_eq!(
            lease_claim_preparation_budget_ms_from(Some(" 1250 ")),
            1_250
        );
        assert_eq!(
            lease_claim_preparation_budget_ms_from(Some("90000")),
            DEFAULT_LEASE_CLAIM_PREPARATION_BUDGET_MS
        );
        assert_eq!(
            lease_claim_preparation_budget_ms_from(Some("0")),
            DEFAULT_LEASE_CLAIM_PREPARATION_BUDGET_MS
        );
        assert_eq!(
            lease_claim_preparation_budget_ms_from(Some("invalid")),
            DEFAULT_LEASE_CLAIM_PREPARATION_BUDGET_MS
        );
    }

    #[test]
    fn stale_path_lease_is_informational_but_live_claim_still_refuses() {
        let (repository, _storage, mut broker) = fixture();
        let holder = broker.start_worktree("lease holder", None).unwrap();
        let claimant = broker.start_worktree("lease claimant", None).unwrap();
        broker.claim_lease(holder.id, "shared.txt", None).unwrap();

        let active_refusal = broker
            .claim_lease(claimant.id, "shared.txt", None)
            .expect_err("an active explicit lease must still refuse an overlapping claim");
        assert!(matches!(
            active_refusal,
            BrokerOpError::LeaseClaimConflict { .. }
        ));

        let missing_worktree = repository
            .path()
            .join("missing-stale-owner-worktree")
            .display()
            .to_string();
        broker
            .store
            .connection()
            .execute(
                "UPDATE sessions
                 SET status = 'stale', worktree_path = ?1, pid = NULL, last_activity_at = 0
                 WHERE id = ?2",
                rusqlite::params![missing_worktree, holder.id],
            )
            .unwrap();

        let report = broker.blockers();
        assert!(
            report
                .blockers
                .iter()
                .all(|blocker| blocker.kind != crate::BlockerKind::PathLease),
            "a stale path lease does not become a repository-wide blocker: {report:#?}"
        );

        let stale_claim = broker
            .claim_lease(claimant.id, "shared.txt", None)
            .expect("the shared policy treats a stale lease as informational");
        assert!(stale_claim.accepted);
        assert!(stale_claim.blockers.is_empty());
        assert!(
            stale_claim
                .warnings
                .iter()
                .any(|warning| warning.session_id == holder.id)
        );
    }

    #[test]
    fn timed_out_git_during_claim_preparation_records_no_explicit_claim() {
        use std::time::{Duration, Instant};

        let (_repository, _storage, mut broker) = fixture();
        let session = broker.start_worktree("bounded lease claim", None).unwrap();
        let explicit_before: i64 = broker
            .store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM leases
                 WHERE session_id = ?1 AND path = ?2 AND kind = 'explicit'",
                rusqlite::params![session.id, "claim-target.txt"],
                |row| row.get(0),
            )
            .unwrap();
        let claimed_events_before: i64 = broker
            .store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM events
                 WHERE session_id = ?1 AND kind = ?2",
                rusqlite::params![session.id, crate::events::LEASE_CLAIMED],
                |row| row.get(0),
            )
            .unwrap();
        let git_timeout_generation_before = crate::git::git_timeout_generation();

        let error = {
            // Simulate a Git command whose ordinary timeout is shorter than
            // the claim's overall budget. The ref helper returns `None`, but
            // the timeout generation must make the claim refuse instead of
            // treating it as a missing integration ref.
            let _expired_git_deadline =
                crate::git::limit_git_until(Instant::now() - Duration::from_millis(1));
            broker
                .claim_lease(session.id, "claim-target.txt", None)
                .unwrap_err()
        };

        assert!(
            matches!(error, BrokerOpError::LeaseClaimPreparationTimedOut { .. }),
            "unexpected error: {error}"
        );
        assert_ne!(
            crate::git::git_timeout_generation(),
            git_timeout_generation_before,
            "the test must exercise a timed-out Git read, not only an elapsed claim budget"
        );
        assert_eq!(
            crate::exit_status::for_broker_error(&error),
            crate::exit_status::ENVIRONMENT
        );
        let explicit_after: i64 = broker
            .store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM leases
                 WHERE session_id = ?1 AND path = ?2 AND kind = 'explicit'",
                rusqlite::params![session.id, "claim-target.txt"],
                |row| row.get(0),
            )
            .unwrap();
        let claimed_events_after: i64 = broker
            .store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM events
                 WHERE session_id = ?1 AND kind = ?2",
                rusqlite::params![session.id, crate::events::LEASE_CLAIMED],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(explicit_after, explicit_before);
        assert_eq!(claimed_events_after, claimed_events_before);
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
    fn real_guarded_exec_holds_artifact_admission_until_child_settlement() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::{Duration, Instant};

        let (_repository, _storage, mut broker) = fixture();
        let session = broker
            .start_worktree("artifact admission guarded exec", None)
            .unwrap();
        let main_root = broker.main_root().to_path_buf();
        let checkout = PathBuf::from(&session.worktree_path);
        let rendezvous = tempfile::tempdir().unwrap();
        let ready = rendezvous.path().join("ready");
        let release = rendezvous.path().join("release");
        let saw_ready = std::sync::Arc::new(AtomicBool::new(false));
        let was_busy = std::sync::Arc::new(AtomicBool::new(false));
        let heartbeat_ready = std::sync::Arc::clone(&saw_ready);
        let heartbeat_busy = std::sync::Arc::clone(&was_busy);
        let command = vec![
            "/bin/sh".into(),
            "-c".into(),
            "touch \"$READY_FILE\"; i=0; while [ ! -e \"$RELEASE_FILE\" ] && [ \"$i\" -lt 1000 ]; do i=$((i + 1)); /bin/sleep 0.01; done; [ -e \"$RELEASE_FILE\" ]".into(),
        ];
        let environment = vec![
            ("READY_FILE".into(), ready.to_string_lossy().into_owned()),
            (
                "RELEASE_FILE".into(),
                release.to_string_lossy().into_owned(),
            ),
        ];
        let report = broker
            .guarded_exec_with_env_heartbeat_and_mode(
                session.id,
                &command,
                &environment,
                move || {
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while !ready.exists() && Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    let ready_seen = ready.exists();
                    let busy = matches!(
                        crate::file_lock::artifact_use::try_artifact_use_reclaimer(
                            &main_root, &checkout,
                        ),
                        Ok(
                            crate::file_lock::artifact_use::ArtifactUseReclaimTry::Blocked(
                                crate::file_lock::artifact_use::ArtifactUseReclaimBlock::Contended
                            )
                        )
                    );
                    heartbeat_ready.store(ready_seen, Ordering::Release);
                    heartbeat_busy.store(busy, Ordering::Release);
                    std::fs::write(&release, b"continue").map_err(|source| {
                        BrokerOpError::Spawn {
                            command: "artifact admission test release signal".into(),
                            source,
                        }
                    })?;
                    Ok(())
                },
                || crate::inline_exec_supervisor::GenericExecMode::Supervised,
            )
            .unwrap();

        assert!(report.command_success);
        assert!(saw_ready.load(Ordering::Acquire));
        assert!(was_busy.load(Ordering::Acquire));
        assert!(matches!(
            crate::file_lock::artifact_use::try_artifact_use_reclaimer(
                broker.main_root(),
                Path::new(&session.worktree_path),
            )
            .unwrap(),
            crate::file_lock::artifact_use::ArtifactUseReclaimTry::Acquired(_)
        ));
    }

    #[test]
    fn supervised_exec_timeout_keeps_the_post_command_ownership_audit() {
        use std::time::Duration;

        let (_repository, _storage, mut broker) = fixture();
        let session = broker.start_worktree("timed guarded exec", None).unwrap();
        broker.claim_lease(session.id, "tracked.txt", None).unwrap();
        let report = broker
            .guarded_exec_for_cli(
                session.id,
                &[
                    "/bin/sh".into(),
                    "-c".into(),
                    "printf before-timeout > tracked.txt; sleep 30".into(),
                ],
                true,
                false,
                Some(Duration::from_millis(100)),
            )
            .unwrap()
            .report;

        assert!(report.command_timed_out);
        assert_eq!(report.exit_code, None);
        assert!(!report.command_success);
        assert!(report.ownership_ok);
        assert!(!report.ok);
        assert!(report.outside_lease_paths.is_empty());
        assert_eq!(report.touched_paths, vec!["tracked.txt".to_owned()]);
        assert_eq!(
            std::fs::read_to_string(Path::new(&session.worktree_path).join("tracked.txt")).unwrap(),
            "before-timeout"
        );
        assert!(matches!(
            crate::file_lock::artifact_use::try_artifact_use_reclaimer(
                broker.main_root(),
                Path::new(&session.worktree_path),
            )
            .unwrap(),
            crate::file_lock::artifact_use::ArtifactUseReclaimTry::Acquired(_)
        ));
    }

    #[test]
    fn timeout_before_start_confirmation_keeps_receipt_outcome_unknown() {
        let (_repository, _storage, mut broker) = fixture();
        let session = broker
            .start_worktree("unconfirmed timeout receipt", None)
            .unwrap();
        let (report, receipt_id, command_ran) = broker
            .guarded_exec_with_runner(
                session.id,
                &["/bin/sh".into(), "-c".into(), "exit 0".into()],
                true,
                |_session, _observation, _artifact_use, _dispatch| {
                    Ok(GuardedCommandResult {
                        exit_code: None,
                        succeeded: false,
                        timed_out: true,
                        command_started: false,
                        process_group_cleanup: "verified",
                    })
                },
            )
            .unwrap();

        assert!(report.command_timed_out);
        assert_eq!(command_ran, None);
        let receipt = broker
            .store
            .guarded_exec_receipt(session.id, receipt_id.as_deref().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(receipt.phase, "outcome_unknown");
        assert_eq!(receipt.command_outcome, "unknown");
        assert_eq!(receipt.process_group_cleanup, "verified");
        assert_eq!(receipt.process_revalidation, "not_revalidated");
    }

    #[test]
    fn not_started_dispatch_is_finalized_from_the_runner_result() {
        #[derive(Clone, Copy)]
        enum Failure {
            Setup,
            CleanupUnknown,
            CommandUnknown,
            CommandUnknownWithVerifiedCleanup,
        }

        let (_repository, _storage, mut broker) = fixture();
        let session = broker
            .start_worktree("deferred exec receipt", None)
            .unwrap();
        for failure in [
            Failure::Setup,
            Failure::CleanupUnknown,
            Failure::CommandUnknown,
            Failure::CommandUnknownWithVerifiedCleanup,
        ] {
            let result = broker.guarded_exec_with_runner(
                session.id,
                &["fake-command".into()],
                true,
                |_session, _observation, _artifact_use, dispatch| {
                    dispatch(GuardedExecDispatchEvent::NotStarted);
                    if matches!(failure, Failure::CommandUnknownWithVerifiedCleanup) {
                        dispatch(GuardedExecDispatchEvent::OutcomeUnknown("verified"));
                    }
                    Err(match failure {
                        Failure::Setup => BrokerOpError::Spawn {
                            command: "fake-command".into(),
                            source: std::io::Error::other("supervisor setup failed"),
                        },
                        Failure::CleanupUnknown => BrokerOpError::GuardedExecCleanupUnknown {
                            session_id: session.id,
                            reason: "helper cleanup could not be verified".into(),
                        },
                        Failure::CommandUnknown | Failure::CommandUnknownWithVerifiedCleanup => {
                            BrokerOpError::GuardedExecOutcomeUnknown {
                                session_id: session.id,
                                reason: "command result was not confirmed".into(),
                            }
                        }
                    })
                },
            );
            let error = match result {
                Err(error) => error,
                Ok(_) => panic!("simulated runner failure must be returned"),
            };

            let receipt = broker
                .store
                .guarded_exec_receipts(session.id, 100)
                .unwrap()
                .into_iter()
                .next()
                .expect("the pre-dispatch receipt should remain queryable");
            match failure {
                Failure::Setup => {
                    assert!(matches!(error, BrokerOpError::Spawn { .. }));
                    assert_eq!(receipt.phase, "not_started");
                    assert_eq!(receipt.command_outcome, "not_started");
                    assert_eq!(receipt.process_group_cleanup, "not_applicable");
                }
                Failure::CleanupUnknown => {
                    assert!(matches!(
                        error,
                        BrokerOpError::GuardedExecCleanupUnknown { .. }
                    ));
                    assert_eq!(receipt.phase, "outcome_unknown");
                    assert_eq!(receipt.command_outcome, "unknown");
                    assert_eq!(receipt.process_group_cleanup, "unknown");
                }
                Failure::CommandUnknown => {
                    assert!(matches!(
                        error,
                        BrokerOpError::GuardedExecOutcomeUnknown { .. }
                    ));
                    assert_eq!(receipt.phase, "outcome_unknown");
                    assert_eq!(receipt.command_outcome, "unknown");
                    assert_eq!(receipt.process_group_cleanup, "unknown");
                }
                Failure::CommandUnknownWithVerifiedCleanup => {
                    assert!(matches!(
                        error,
                        BrokerOpError::GuardedExecOutcomeUnknown { .. }
                    ));
                    assert_eq!(receipt.phase, "outcome_unknown");
                    assert_eq!(receipt.command_outcome, "unknown");
                    assert_eq!(receipt.process_group_cleanup, "verified");
                }
            }
        }
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
    fn non_interactive_exec_refuses_wrong_helper_before_command_dispatch() {
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

        let error = match broker.guarded_exec_for_cli(
            session.id,
            &[
                "/bin/sh".into(),
                "-c".into(),
                "printf launched > must-not-exist".into(),
            ],
            true,
            false,
            None,
        ) {
            Ok(_) => panic!("wrong helper must be refused"),
            Err(error) => error,
        };

        assert!(matches!(error, BrokerOpError::Spawn { .. }), "{error}");
        assert!(
            error.to_string().contains("helper did not become ready"),
            "wrong helper was not refused during readiness: {error}"
        );
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
    fn v1_helper_refuses_v3_before_the_guarded_command_can_write() {
        use std::os::unix::fs::PermissionsExt as _;

        let (_repository, _storage, mut broker) = fixture();
        let session = broker
            .start_worktree("old helper readiness refusal", None)
            .unwrap();
        let old_helper = tempfile::tempdir().unwrap();
        let executable = old_helper.path().join("v1-helper");
        // A v1 helper's reserved-prefix dispatcher exits 125 for the v3 flag
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
            "--aethyme-inline-exec-supervisor-v4"
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
