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
    OperationGuardBusy,
    OperationActive,
    OperationEvidenceUnavailable,
    /// Artifact-use markers saved before a device renumber keep the checkout
    /// (rule `legacy_marker_device_renumber`); the count is how many. They
    /// need review, so this stays actionable instead of reading as live work.
    LegacyArtifactMarkers(usize),
    /// The landed-session lane kept the session open for a recorded reason.
    LandedDeclined(LandedSessionDecline),
}

/// Why the landed-session lane kept one open session. Every reason keeps the
/// session and its checkout; `code` is the machine-readable name counted in
/// `AutoCleanupReport::landed_sessions_declined` and `broker status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum LandedSessionDecline {
    /// Session or worktree activity is newer than the landed window.
    RecentActivity,
    /// The session row changed, or was closed, while it was checked.
    Changed,
    /// The broker-spawned process, or a holder whose identity could not be
    /// re-read, is still running.
    ProcessAlive,
    /// The recorded holder runs, its task lease is current or absent, and it
    /// has not moved on: this is its latest session, or it was bound
    /// elsewhere only before this session's last activity
    /// (`holder_alive_current`).
    HolderAlive,
    /// No holder was recorded, or it is a shared agent service, so its
    /// absence from this session cannot be proved.
    HolderUnbound,
    /// The recorded holder could not be checked.
    HolderUnknown,
    /// An explicit lease still holds.
    LeaseHeld,
    /// Explicit lease liveness could not be checked.
    LeaseUnknown,
    /// The checkout, its Git metadata or its HEAD could not be read.
    WorktreeUnavailable,
    /// Another lifecycle operation, session command or database writer was busy.
    Busy,
    /// A gate, operation, preparation or process still uses the session.
    OperationActive,
    /// Operation or process-use evidence could not be established.
    OperationUnknown,
    /// Retention or cleanup policy could not be read.
    PolicyUnavailable,
    /// `[cleanup] auto_remove = false`: closing would strand the checkout.
    AutoRemoveDisabled,
    /// The checkout matches a `[cleanup]` keep pin.
    Pinned,
    /// A pull request linked to the session is not merged.
    PullRequestUnmerged,
    /// The session never moved past its base and nothing records a landing.
    NoSessionCommits,
    /// No merged pull request, representation or ancestry suggests a landing,
    /// so the content search is not attempted.
    NoLandingEvidence,
    /// The landing rule `finish` uses does not account for every commit.
    NotLanded,
    /// The landing rule could not be evaluated.
    LandingUnknown,
    /// Uncommitted, untracked, in-progress or protected ignored content.
    WorktreeUnclean,
    /// HEAD or the worktree's index/HEAD activity changed between the proof
    /// and the close: the agent committed or worked meanwhile.
    MovedBeforeClose,
    /// The fetched default branch moved after the proof; the next lap proves
    /// the landing again against the new tip.
    UpstreamMoved,
    /// The pass deadline ran out before the proof finished. The cursor stays
    /// on the session so a later pass, usually the background worker, takes it.
    BudgetExhausted,
    /// This one session's proof ran past [`LANDED_PROOF_SESSION_BUDGET`]
    /// while the pass still had time: the evidence is inconclusive, so the
    /// session is kept, and the lane moves on to the next session instead of
    /// spending its whole share on this one. A later lap asks again.
    ProofTimedOut,
    /// The merged-pull-request window closes only sessions the broker
    /// spawned; an adopted checkout is someone's own and waits for the
    /// ordinary landed window.
    Adopted,
    /// The live holder moved on to a newer session, but a process (the
    /// holder, a child, anything) has its working directory or an open file
    /// in this checkout.
    HolderAliveInUse,
}

impl LandedSessionDecline {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::RecentActivity => "recent_activity",
            Self::Changed => "changed",
            Self::ProcessAlive => "process_alive",
            Self::HolderAlive => "holder_alive_current",
            Self::HolderUnbound => "holder_unbound",
            Self::HolderUnknown => "holder_unknown",
            Self::LeaseHeld => "lease_held",
            Self::LeaseUnknown => "lease_unknown",
            Self::WorktreeUnavailable => "worktree_unavailable",
            Self::Busy => "busy",
            Self::OperationActive => "operation_active",
            Self::OperationUnknown => "operation_unknown",
            Self::PolicyUnavailable => "policy_unavailable",
            Self::AutoRemoveDisabled => "auto_remove_disabled",
            Self::Pinned => "pinned",
            Self::PullRequestUnmerged => "pull_request_unmerged",
            Self::NoSessionCommits => "no_session_commits",
            Self::NoLandingEvidence => "no_landing_evidence",
            Self::NotLanded => "not_landed",
            Self::LandingUnknown => "landing_unknown",
            Self::WorktreeUnclean => "worktree_unclean",
            Self::MovedBeforeClose => "moved_before_close",
            Self::UpstreamMoved => "upstream_moved",
            Self::BudgetExhausted => "budget_exhausted",
            Self::ProofTimedOut => "proof_timed_out",
            Self::Adopted => "adopted",
            Self::HolderAliveInUse => "holder_alive_in_use",
        }
    }

    /// The `phase_counts` name one decline adds to the command's metrics.
    pub(crate) const fn telemetry_name(self) -> &'static str {
        match self {
            Self::RecentActivity => "landed_sessions_declined_recent_activity",
            Self::Changed => "landed_sessions_declined_changed",
            Self::ProcessAlive => "landed_sessions_declined_process_alive",
            Self::HolderAlive => "landed_sessions_declined_holder_alive_current",
            Self::HolderUnbound => "landed_sessions_declined_holder_unbound",
            Self::HolderUnknown => "landed_sessions_declined_holder_unknown",
            Self::LeaseHeld => "landed_sessions_declined_lease_held",
            Self::LeaseUnknown => "landed_sessions_declined_lease_unknown",
            Self::WorktreeUnavailable => "landed_sessions_declined_worktree_unavailable",
            Self::Busy => "landed_sessions_declined_busy",
            Self::OperationActive => "landed_sessions_declined_operation_active",
            Self::OperationUnknown => "landed_sessions_declined_operation_unknown",
            Self::PolicyUnavailable => "landed_sessions_declined_policy_unavailable",
            Self::AutoRemoveDisabled => "landed_sessions_declined_auto_remove_disabled",
            Self::Pinned => "landed_sessions_declined_pinned",
            Self::PullRequestUnmerged => "landed_sessions_declined_pull_request_unmerged",
            Self::NoSessionCommits => "landed_sessions_declined_no_session_commits",
            Self::NoLandingEvidence => "landed_sessions_declined_no_landing_evidence",
            Self::NotLanded => "landed_sessions_declined_not_landed",
            Self::LandingUnknown => "landed_sessions_declined_landing_unknown",
            Self::WorktreeUnclean => "landed_sessions_declined_worktree_unclean",
            Self::MovedBeforeClose => "landed_sessions_declined_moved_before_close",
            Self::UpstreamMoved => "landed_sessions_declined_upstream_moved",
            Self::BudgetExhausted => "landed_sessions_declined_budget_exhausted",
            Self::ProofTimedOut => "landed_sessions_declined_proof_timed_out",
            Self::Adopted => "landed_sessions_declined_adopted",
            Self::HolderAliveInUse => "landed_sessions_declined_holder_alive_in_use",
        }
    }

    /// The reason a landed-lane outcome other than `Closed` kept the session.
    pub(crate) fn from_outcome(outcome: AbandonedSessionCloseOutcome) -> Option<Self> {
        Some(match outcome {
            AbandonedSessionCloseOutcome::Closed => return None,
            AbandonedSessionCloseOutcome::LandedDeclined(reason) => reason,
            AbandonedSessionCloseOutcome::NotDue => Self::RecentActivity,
            AbandonedSessionCloseOutcome::Changed => Self::Changed,
            AbandonedSessionCloseOutcome::ActiveProcess => Self::ProcessAlive,
            AbandonedSessionCloseOutcome::HolderUnavailable => Self::HolderUnknown,
            AbandonedSessionCloseOutcome::LeaseHeld => Self::LeaseHeld,
            AbandonedSessionCloseOutcome::LeaseUnavailable => Self::LeaseUnknown,
            AbandonedSessionCloseOutcome::WorktreeActivityUnavailable => Self::WorktreeUnavailable,
            AbandonedSessionCloseOutcome::LifecycleBusy
            | AbandonedSessionCloseOutcome::DatabaseBusy
            | AbandonedSessionCloseOutcome::OperationGuardBusy => Self::Busy,
            AbandonedSessionCloseOutcome::OperationActive => Self::OperationActive,
            AbandonedSessionCloseOutcome::OperationEvidenceUnavailable
            | AbandonedSessionCloseOutcome::LegacyArtifactMarkers(_) => Self::OperationUnknown,
        })
    }

    /// Whether the landed proof itself reached this answer, so a
    /// `proof_timed_out` streak ends: the landing proof concluded (not
    /// landed, no evidence or commits to search for) or the cleanliness
    /// listing finished (unclean, or work since the proof). A cheap decline
    /// decided before the proof ran (recent activity, a live process, a held
    /// lease, a changed row, a moved default branch), contention, running out
    /// of time and unreadable evidence leave the streak as it was.
    pub(crate) const fn is_conclusive(self) -> bool {
        matches!(
            self,
            Self::NotLanded
                | Self::NoLandingEvidence
                | Self::NoSessionCommits
                | Self::WorktreeUnclean
                | Self::MovedBeforeClose
        )
    }

    /// Whether the abandonment lane may still judge the session by its own
    /// rules after this decline. Contention and unknown or active process
    /// evidence apply to both lanes alike, so they are reported once instead
    /// of being asked twice. A missing checkout is not among them: the
    /// abandonment lane closes such a session without reading it.
    pub(crate) const fn allows_abandonment_fallback(self) -> bool {
        !matches!(
            self,
            Self::Busy
                | Self::HolderUnknown
                | Self::LeaseHeld
                | Self::LeaseUnknown
                | Self::OperationActive
                | Self::OperationUnknown
                | Self::MovedBeforeClose
                | Self::BudgetExhausted
                | Self::ProofTimedOut
        )
    }
}

/// Why one automatic close kept a session open: one reason per exit of the
/// close path other than the close itself. The abandonment window counts it
/// as `session_abandonment_kept_<code>` and `broker status` shows the counts
/// under `landed_session_retirement.abandonment.kept`, so a reader can tell
/// which check holds sessions that are far past the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum AbandonmentKeepReason {
    /// `session_abandoned_after_hours` is zero.
    WindowDisabled,
    /// The session row, its holder binding or its status changed while it
    /// was judged, or the close transaction found it changed.
    Changed,
    /// The broker-spawned process recorded on the session still runs.
    SessionPidAlive,
    /// A follow-up session the landed windows protect, while the abandonment
    /// window is the shorter one.
    FollowUpProtected,
    /// The checkout's Git activity metadata could not be read.
    WorktreeActivityUnavailable,
    /// Session or checkout activity is newer than the window.
    RecentActivity,
    /// Past the abandonment window, but not past the window of what closing
    /// would put at risk (`[cleanup] stale_*_close_after_hours`: landed,
    /// pushed but unmerged, unpushed or uncommitted work).
    WorkClassNotDue,
    /// The recorded holder could not be checked.
    HolderUnknown,
    /// This command was refused the process listing (a sandboxed caller), so
    /// no holder could be checked in it.
    HolderProcessTableDenied,
    /// The recorded holder runs and this is still its session.
    HolderAliveCurrent,
    /// A live holder whose task lease expired could not be re-read.
    HolderRecordUnavailable,
    /// A live holder whose task lease expired is a shared agent service.
    HolderSharedService,
    /// A live holder's task lease no longer reads as expired under the claims.
    TaskLeaseCurrent,
    /// An admitted session command holds the task operation guard.
    OperationGuardBusy,
    /// The task operation guard could not be tried.
    OperationGuardUnavailable,
    /// A running `broker start` has reserved the checkout's path.
    StartReserved,
    /// Another lifecycle operation holds the repository lifecycle claim.
    LifecycleBusy,
    /// The pass deadline ran out during the running-work checks.
    DeadlineReached,
    /// A gate runs for the session.
    GateRunning,
    /// The gate inventory could not be read or verified.
    GatesUnknown,
    /// A coordinated operation of the session is unresolved.
    OperationUnresolved,
    /// The coordinated-operation journal could not be read.
    OperationsUnknown,
    /// A submission is still being simulated, gated or promoted.
    MergeQueueActive,
    /// The merge queue could not be read.
    MergeQueueUnknown,
    /// Dependency preparation runs for the session.
    PreparationInProgress,
    /// The preparation configuration is invalid.
    PreparationInvalid,
    /// The preparation status could not be read.
    PreparationUnknown,
    /// The recorded checkout path is not a plain directory.
    CheckoutNotDirectory,
    /// An artifact producer uses the checkout, or its marker is contended.
    ArtifactProducerActive,
    /// The artifact-use protocol could not be read.
    ArtifactProtocolUnavailable,
    /// Artifact-use markers saved before a device renumber.
    LegacyArtifactMarkers,
    /// A process has its working directory or an open file in the checkout.
    OpenFilesInCheckout,
    /// The open-file observation failed or was incomplete.
    OpenFilesUnavailable,
    /// The pass may not take another open-file observation now.
    OpenFilesThrottled,
    /// An explicit lease still holds: its holder runs, or its stale grace has
    /// not ended.
    LeaseHeld,
    /// Explicit-lease liveness could not be assessed.
    LeaseUnavailable,
    /// A concurrent SQLite writer prevented the close.
    DatabaseBusy,
    /// Retention or cleanup policy could not be read.
    PolicyUnavailable,
    /// A landed-lane proof declined (never the abandonment window itself).
    LandedDeclined,
    /// No-work lane: an adopted session keeps the full window (its diff base
    /// is the adopted HEAD, which can carry earlier work).
    NoWorkAdopted,
    /// No-work lane: HEAD is not the session branch, the branch has commits
    /// past its diff base, or either could not be read.
    NoWorkSessionCommits,
    /// No-work lane: the checkout has uncommitted or untracked content, or
    /// its state could not be read.
    NoWorkWorktreeDirty,
    /// No-work lane: no holder was ever recorded; the full window applies.
    NoWorkHolderUnbound,
    /// No-work lane: the recorded holder runs.
    NoWorkHolderAlive,
    /// No-work lane: the holder is gone, but not yet for the stale grace.
    NoWorkHolderGoneRecently,
    /// No-work lane: the session holds an explicit claim.
    NoWorkExplicitClaim,
    /// No-work lane: the session has a merge-queue entry, or the queue could
    /// not be read.
    NoWorkMergeQueue,
    /// No-work lane: a pull request recorded for the session is not merged.
    NoWorkPullRequestOpen,
    /// No-work lane: the session's pull requests could not be read.
    NoWorkPullRequestUnknown,
    /// An exit that recorded no reason; a defect if it ever appears.
    Unrecorded,
}

impl AbandonmentKeepReason {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::WindowDisabled => "window_disabled",
            Self::Changed => "changed",
            Self::SessionPidAlive => "session_pid_alive",
            Self::FollowUpProtected => "follow_up_protected",
            Self::WorktreeActivityUnavailable => "worktree_activity_unavailable",
            Self::RecentActivity => "recent_activity",
            Self::WorkClassNotDue => "work_class_not_due",
            Self::HolderUnknown => "holder_unknown",
            Self::HolderProcessTableDenied => "holder_process_table_denied",
            Self::HolderAliveCurrent => "holder_alive_current",
            Self::HolderRecordUnavailable => "holder_record_unavailable",
            Self::HolderSharedService => "holder_shared_service",
            Self::TaskLeaseCurrent => "task_lease_current",
            Self::OperationGuardBusy => "operation_guard_busy",
            Self::OperationGuardUnavailable => "operation_guard_unavailable",
            Self::StartReserved => "start_reserved",
            Self::LifecycleBusy => "lifecycle_busy",
            Self::DeadlineReached => "deadline_reached",
            Self::GateRunning => "gate_running",
            Self::GatesUnknown => "gates_unknown",
            Self::OperationUnresolved => "operation_unresolved",
            Self::OperationsUnknown => "operations_unknown",
            Self::MergeQueueActive => "merge_queue_active",
            Self::MergeQueueUnknown => "merge_queue_unknown",
            Self::PreparationInProgress => "preparation_in_progress",
            Self::PreparationInvalid => "preparation_invalid",
            Self::PreparationUnknown => "preparation_unknown",
            Self::CheckoutNotDirectory => "checkout_not_directory",
            Self::ArtifactProducerActive => "artifact_producer_active",
            Self::ArtifactProtocolUnavailable => "artifact_protocol_unavailable",
            Self::LegacyArtifactMarkers => "legacy_artifact_markers",
            Self::OpenFilesInCheckout => "open_files_in_checkout",
            Self::OpenFilesUnavailable => "open_files_unavailable",
            Self::OpenFilesThrottled => "open_files_throttled",
            Self::LeaseHeld => "lease_held",
            Self::LeaseUnavailable => "lease_unavailable",
            Self::DatabaseBusy => "database_busy",
            Self::PolicyUnavailable => "policy_unavailable",
            Self::LandedDeclined => "landed_declined",
            Self::NoWorkAdopted => "no_work_adopted",
            Self::NoWorkSessionCommits => "no_work_session_commits",
            Self::NoWorkWorktreeDirty => "no_work_worktree_dirty",
            Self::NoWorkHolderUnbound => "no_work_holder_unbound",
            Self::NoWorkHolderAlive => "no_work_holder_alive",
            Self::NoWorkHolderGoneRecently => "no_work_holder_gone_recently",
            Self::NoWorkExplicitClaim => "no_work_explicit_claim",
            Self::NoWorkMergeQueue => "no_work_merge_queue",
            Self::NoWorkPullRequestOpen => "no_work_pull_request_open",
            Self::NoWorkPullRequestUnknown => "no_work_pull_request_unknown",
            Self::Unrecorded => "unrecorded",
        }
    }

    /// The `phase_counts` name one abandonment-window keep adds to the
    /// command's metrics.
    pub(crate) const fn telemetry_name(self) -> &'static str {
        match self {
            Self::WindowDisabled => "session_abandonment_kept_window_disabled",
            Self::Changed => "session_abandonment_kept_changed",
            Self::SessionPidAlive => "session_abandonment_kept_session_pid_alive",
            Self::FollowUpProtected => "session_abandonment_kept_follow_up_protected",
            Self::WorktreeActivityUnavailable => {
                "session_abandonment_kept_worktree_activity_unavailable"
            }
            Self::RecentActivity => "session_abandonment_kept_recent_activity",
            Self::WorkClassNotDue => "session_abandonment_kept_work_class_not_due",
            Self::HolderUnknown => "session_abandonment_kept_holder_unknown",
            Self::HolderProcessTableDenied => {
                "session_abandonment_kept_holder_process_table_denied"
            }
            Self::HolderAliveCurrent => "session_abandonment_kept_holder_alive_current",
            Self::HolderRecordUnavailable => "session_abandonment_kept_holder_record_unavailable",
            Self::HolderSharedService => "session_abandonment_kept_holder_shared_service",
            Self::TaskLeaseCurrent => "session_abandonment_kept_task_lease_current",
            Self::OperationGuardBusy => "session_abandonment_kept_operation_guard_busy",
            Self::OperationGuardUnavailable => {
                "session_abandonment_kept_operation_guard_unavailable"
            }
            Self::StartReserved => "session_abandonment_kept_start_reserved",
            Self::LifecycleBusy => "session_abandonment_kept_lifecycle_busy",
            Self::DeadlineReached => "session_abandonment_kept_deadline_reached",
            Self::GateRunning => "session_abandonment_kept_gate_running",
            Self::GatesUnknown => "session_abandonment_kept_gates_unknown",
            Self::OperationUnresolved => "session_abandonment_kept_operation_unresolved",
            Self::OperationsUnknown => "session_abandonment_kept_operations_unknown",
            Self::MergeQueueActive => "session_abandonment_kept_merge_queue_active",
            Self::MergeQueueUnknown => "session_abandonment_kept_merge_queue_unknown",
            Self::PreparationInProgress => "session_abandonment_kept_preparation_in_progress",
            Self::PreparationInvalid => "session_abandonment_kept_preparation_invalid",
            Self::PreparationUnknown => "session_abandonment_kept_preparation_unknown",
            Self::CheckoutNotDirectory => "session_abandonment_kept_checkout_not_directory",
            Self::ArtifactProducerActive => "session_abandonment_kept_artifact_producer_active",
            Self::ArtifactProtocolUnavailable => {
                "session_abandonment_kept_artifact_protocol_unavailable"
            }
            Self::LegacyArtifactMarkers => "session_abandonment_kept_legacy_artifact_markers",
            Self::OpenFilesInCheckout => "session_abandonment_kept_open_files_in_checkout",
            Self::OpenFilesUnavailable => "session_abandonment_kept_open_files_unavailable",
            Self::OpenFilesThrottled => "session_abandonment_kept_open_files_throttled",
            Self::LeaseHeld => "session_abandonment_kept_lease_held",
            Self::LeaseUnavailable => "session_abandonment_kept_lease_unavailable",
            Self::DatabaseBusy => "session_abandonment_kept_database_busy",
            Self::PolicyUnavailable => "session_abandonment_kept_policy_unavailable",
            Self::LandedDeclined => "session_abandonment_kept_landed_declined",
            Self::NoWorkAdopted => "session_abandonment_kept_no_work_adopted",
            Self::NoWorkSessionCommits => "session_abandonment_kept_no_work_session_commits",
            Self::NoWorkWorktreeDirty => "session_abandonment_kept_no_work_worktree_dirty",
            Self::NoWorkHolderUnbound => "session_abandonment_kept_no_work_holder_unbound",
            Self::NoWorkHolderAlive => "session_abandonment_kept_no_work_holder_alive",
            Self::NoWorkHolderGoneRecently => {
                "session_abandonment_kept_no_work_holder_gone_recently"
            }
            Self::NoWorkExplicitClaim => "session_abandonment_kept_no_work_explicit_claim",
            Self::NoWorkMergeQueue => "session_abandonment_kept_no_work_merge_queue",
            Self::NoWorkPullRequestOpen => "session_abandonment_kept_no_work_pull_request_open",
            Self::NoWorkPullRequestUnknown => {
                "session_abandonment_kept_no_work_pull_request_unknown"
            }
            Self::Unrecorded => "session_abandonment_kept_unrecorded",
        }
    }

    /// The `phase_counts` name one no-work-lane keep adds: its own counter
    /// for each no-work condition and for the shared running-work checks the
    /// lane relies on, one bucket for every other keep.
    pub(crate) const fn no_work_telemetry_name(self) -> &'static str {
        match self {
            Self::NoWorkAdopted => "session_no_work_kept_adopted",
            Self::NoWorkSessionCommits => "session_no_work_kept_session_commits",
            Self::NoWorkWorktreeDirty => "session_no_work_kept_worktree_dirty",
            Self::NoWorkHolderUnbound => "session_no_work_kept_holder_unbound",
            Self::NoWorkHolderAlive => "session_no_work_kept_holder_alive",
            Self::NoWorkHolderGoneRecently => "session_no_work_kept_holder_gone_recently",
            Self::NoWorkExplicitClaim => "session_no_work_kept_explicit_claim",
            Self::NoWorkMergeQueue => "session_no_work_kept_merge_queue",
            Self::NoWorkPullRequestOpen => "session_no_work_kept_pull_request_open",
            Self::NoWorkPullRequestUnknown => "session_no_work_kept_pull_request_unknown",
            Self::OperationUnresolved => "session_no_work_kept_operation_unresolved",
            Self::PreparationInProgress => "session_no_work_kept_preparation_in_progress",
            Self::RecentActivity => "session_no_work_kept_recent_activity",
            Self::HolderUnknown | Self::HolderProcessTableDenied => {
                "session_no_work_kept_holder_unknown"
            }
            _ => "session_no_work_kept_other",
        }
    }
}

/// Record why the close path keeps the session and return `outcome`. The
/// reason rides on the pass's process table (one per pass) so the caller can
/// count it without every outcome variant carrying a payload.
fn keep_with(
    process_table: &crate::session_holder::RecordedHolderProcessTable,
    reason: AbandonmentKeepReason,
    outcome: AbandonedSessionCloseOutcome,
) -> AbandonedSessionCloseOutcome {
    process_table.note_keep_reason(reason);
    outcome
}

/// What a landed proof established, re-checked under the claims and again
/// immediately before the close.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LandedProof {
    /// The session HEAD the landing was proved for.
    pub head: String,
    /// The fetched default-branch tip the proof used.
    pub upstream_tip: String,
    /// Which landing rule held: `representation` (recorded against the
    /// default branch), `ancestry`, or `content` (every commit accounted for
    /// by the landing search after a squash or rebase).
    pub evidence: &'static str,
    /// The landing came from the remembered proof for this head, not a fresh
    /// search; a close resting on it searches again under the claims.
    pub memoized: bool,
}

/// Sessions created before this instant (2026-10-05T00:00:00Z, a day before
/// the 0.8.21 release that started recording holders, #542) were created by
/// binaries that never recorded one. Only such a session, with no holder
/// event of any kind, is legacy.
const HOLDER_RECORDING_SHIPPED_MS: i64 = 1_791_158_400_000;

/// The base budget of one session's landed judgement inside a budgeted
/// pass: the proof, a memoized re-proof and the re-checks under the claims
/// share it. Past it the session is declined `proof_timed_out` (kept) and
/// the lane moves on: one checkout whose `git status` crawls on a loaded
/// host used to hold the whole retirement share, and with it the host slot,
/// for minutes. Each consecutive `proof_timed_out` doubles that session's
/// budget (up to what is left of the pass), and once per pass the oldest
/// timed-out session gets the rest of the lane's share, so every session is
/// eventually judged conclusively.
pub(crate) const LANDED_PROOF_SESSION_BUDGET: std::time::Duration =
    std::time::Duration::from_secs(10);

/// Meta key of the consecutive `proof_timed_out` streaks: a JSON object
/// from session id to `[streak, first timed out at (epoch ms)]`.
pub(crate) const LANDED_PROOF_TIMEOUTS_KEY: &str = "landed_lane.proof_timeouts";
/// Streaks remembered at most; the oldest first-timeouts go first.
const LANDED_PROOF_TIMEOUTS_MAX: usize = 4_096;
/// Meta key of the sessions that still timed out with a long-budget grant:
/// a JSON array of session ids, shown by `broker status`.
pub(crate) const LANDED_PROOF_UNPROVABLE_KEY: &str = "landed_lane.proof_unprovable";
/// Meta key counting likely-landed passes that had a session to grant.
const LANDED_PROOF_GRANT_PASSES_KEY: &str = "landed_lane.grant_passes";
/// Every this many such passes the grant runs first instead of last.
pub(crate) const LANDED_PROOF_GRANT_FIRST_EVERY: u64 = 4;
/// Doublings at most: the pass deadline caps the budget long before.
const LANDED_PROOF_MAX_DOUBLINGS: u32 = 16;

thread_local! {
    /// The session this thread's lane granted the rest of its share to.
    static LONG_PROOF_GRANT: std::cell::Cell<Option<i64>> = const { std::cell::Cell::new(None) };
}

/// While held, `session_id`'s landed judgements on this thread are bounded
/// by the pass deadline only (the long-budget grant).
pub(crate) struct LongProofGrant(Option<i64>);

impl Drop for LongProofGrant {
    fn drop(&mut self) {
        LONG_PROOF_GRANT.with(|cell| cell.set(self.0));
    }
}

/// Grant `session_id` the rest of the lane's share until the guard drops.
pub(crate) fn long_proof_grant(session_id: i64) -> LongProofGrant {
    crate::phase_telemetry::add(
        "landed_proof_long_budget_granted",
        std::time::Duration::ZERO,
    );
    LongProofGrant(LONG_PROOF_GRANT.with(|cell| cell.replace(Some(session_id))))
}

#[cfg(test)]
thread_local! {
    /// Test seam: the base per-session budget, for one session id or (with
    /// `None`) for every session, instead of [`LANDED_PROOF_SESSION_BUDGET`].
    static LANDED_PROOF_SESSION_BUDGET_FOR_TEST:
        std::cell::Cell<Option<(Option<i64>, std::time::Duration)>> =
        const { std::cell::Cell::new(None) };
}

/// Install a base per-session budget for this test thread until the guard
/// drops: for `session_id` only, or for every session with `None`.
#[cfg(test)]
pub(crate) fn landed_proof_session_budget_for_test(
    session_id: Option<i64>,
    budget: std::time::Duration,
) -> LandedProofSessionBudgetRestore {
    LandedProofSessionBudgetRestore(
        LANDED_PROOF_SESSION_BUDGET_FOR_TEST.with(|cell| cell.replace(Some((session_id, budget)))),
    )
}

/// Restores the previous test budget when dropped.
#[cfg(test)]
pub(crate) struct LandedProofSessionBudgetRestore(Option<(Option<i64>, std::time::Duration)>);

#[cfg(test)]
impl Drop for LandedProofSessionBudgetRestore {
    fn drop(&mut self) {
        LANDED_PROOF_SESSION_BUDGET_FOR_TEST.with(|cell| cell.set(self.0));
    }
}

/// The base per-session budget for `session_id`.
#[cfg(not(test))]
fn landed_proof_base_budget(_session_id: i64) -> std::time::Duration {
    LANDED_PROOF_SESSION_BUDGET
}

/// The base per-session budget for `session_id`, or the test override.
#[cfg(test)]
fn landed_proof_base_budget(session_id: i64) -> std::time::Duration {
    match LANDED_PROOF_SESSION_BUDGET_FOR_TEST.with(std::cell::Cell::get) {
        Some((only, budget)) if only.is_none_or(|only| only == session_id) => budget,
        _ => LANDED_PROOF_SESSION_BUDGET,
    }
}

/// `base` doubled once per consecutive `proof_timed_out`.
pub(crate) fn doubled_proof_budget(base: std::time::Duration, streak: u32) -> std::time::Duration {
    base.saturating_mul(1_u32 << streak.min(LANDED_PROOF_MAX_DOUBLINGS))
}

/// The phase counter one timeout streak of `streak` adds.
const fn proof_timeout_streak_counter(streak: u32) -> &'static str {
    match streak {
        0 | 1 => "landed_proof_timeout_streak_1",
        2 => "landed_proof_timeout_streak_2",
        3 | 4 => "landed_proof_timeout_streak_3_4",
        _ => "landed_proof_timeout_streak_5_plus",
    }
}

/// The session deadline of one judgement: the earlier of the pass deadline
/// and `budget` from now; unbudgeted callers stay unbounded.
fn session_deadline_within(
    pass: Option<std::time::Instant>,
    budget: std::time::Duration,
) -> Option<std::time::Instant> {
    pass.map(|pass| {
        std::time::Instant::now()
            .checked_add(budget)
            .map_or(pass, |end| pass.min(end))
    })
}

/// The decline of a listing that failed: the pass ran out
/// (`budget_exhausted`), only this session's budget did (`proof_timed_out`),
/// or neither (`otherwise`).
fn stopped_listing_decline(
    pass: Option<std::time::Instant>,
    session: Option<std::time::Instant>,
    otherwise: LandedSessionDecline,
) -> LandedSessionDecline {
    let now = std::time::Instant::now();
    if pass.is_some_and(|pass| now >= pass) {
        LandedSessionDecline::BudgetExhausted
    } else if session.is_some_and(|session| now >= session) {
        LandedSessionDecline::ProofTimedOut
    } else {
        otherwise
    }
}

/// Which window and which proofs an automatic close uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionCloseLane {
    /// `session_abandoned_after_hours`: idleness and a gone or unbound holder.
    Abandonment,
    /// `session_landed_after_hours`: the landed proof as well.
    Landed,
    /// `session_merged_after_hours`: the landed proof, a spawned session,
    /// every recorded pull request merged, idleness counted from the later of
    /// the last activity and the last merge, and no gate, queue entry,
    /// operation, preparation or artifact producer running.
    MergedPullRequest,
    /// `session_no_work_abandoned_after_hours`: the abandonment window's
    /// checks for a session that never did any work, plus, re-checked under
    /// the claims: HEAD still its diff base, a clean checkout, a recorded
    /// holder gone past the stale grace, and no explicit claim, unresolved
    /// operation, running preparation, merge-queue entry or open pull
    /// request.
    NoWork,
}

/// The phase counter one `worktree_unclean` landed decline adds, per kind.
fn worktree_unclean_counter(code: &str) -> &'static str {
    match code {
        "uncommitted_changes" => "landed_worktree_unclean_uncommitted_changes",
        "unmarked_tracked_deletions" => "landed_worktree_unclean_unmarked_tracked_deletions",
        "strict_tracked_deletions" => "landed_worktree_unclean_strict_tracked_deletions",
        "ignored_content" => "landed_worktree_unclean_ignored_content",
        "ignored_content_unverified" => "landed_worktree_unclean_ignored_content_unverified",
        "operation_in_progress" => "landed_worktree_unclean_operation_in_progress",
        "stash" => "landed_worktree_unclean_stash",
        _ => "landed_worktree_unclean_other",
    }
}

/// Why the open-file observation keeps the session, or `None` when it shows
/// no process using `worktree` (besides `exempt_holder`). A landed-lane
/// session that cannot get a fresh observation this pass because of the
/// per-pass throttle is `budget_exhausted` and retried next pass.
fn process_use_verdict(
    process_table: &mut crate::session_holder::RecordedHolderProcessTable,
    root: &std::path::Path,
    worktree: &std::path::Path,
    deadline: Option<std::time::Instant>,
    exempt_holder: Option<i64>,
    require_landed: bool,
) -> Option<AbandonedSessionCloseOutcome> {
    let Some(open_files) = process_table.process_use_snapshot_for_retirement(root, deadline) else {
        let throttled = process_table.process_use_throttled();
        process_table.note_keep_reason(if throttled {
            AbandonmentKeepReason::OpenFilesThrottled
        } else {
            AbandonmentKeepReason::OpenFilesUnavailable
        });
        return Some(if require_landed && throttled {
            AbandonedSessionCloseOutcome::LandedDeclined(LandedSessionDecline::BudgetExhausted)
        } else {
            AbandonedSessionCloseOutcome::OperationEvidenceUnavailable
        });
    };
    // The broker's own state files under the primary checkout's `.aethyme/`
    // (its database, locks, logs and runtime records, open in every broker
    // process) are not anyone using the checkout: without this, a session
    // adopted on the primary checkout could never close. Nothing else there
    // is excluded (review checkouts live under `.aethyme/reviews/`), never a
    // working directory, and nothing at all when the session's own checkout
    // is under the state directory.
    let state_dir = root.join(".aethyme");
    let checkout_in_state_dir = worktree.starts_with(&state_dir)
        || std::fs::canonicalize(worktree).is_ok_and(|real| real.starts_with(&state_dir));
    let without_broker_state;
    let open_files = match open_files {
        crate::reclaim::OpenFileSnapshot::Complete(uses)
            if !checkout_in_state_dir
                && uses
                    .iter()
                    .any(|open| is_broker_state_file(open, &state_dir)) =>
        {
            without_broker_state = crate::reclaim::OpenFileSnapshot::Complete(
                uses.iter()
                    .filter(|open| !is_broker_state_file(open, &state_dir))
                    .cloned()
                    .collect(),
            );
            &without_broker_state
        }
        other => other,
    };
    let verdict = match open_files {
        crate::reclaim::OpenFileSnapshot::Complete(_) => {
            crate::reclaim::open_file_snapshot_blocker_for_session_worktree(
                open_files,
                worktree,
                exempt_holder,
            )
            .map(|_| {
                (
                    AbandonmentKeepReason::OpenFilesInCheckout,
                    AbandonedSessionCloseOutcome::OperationActive,
                )
            })
        }
        crate::reclaim::OpenFileSnapshot::Incomplete { .. } => Some((
            AbandonmentKeepReason::OpenFilesUnavailable,
            AbandonedSessionCloseOutcome::OperationEvidenceUnavailable,
        )),
    };
    verdict.map(|(reason, outcome)| keep_with(process_table, reason, outcome))
}

/// Whether `open` is one of the broker's own state files under `state_dir`
/// (the primary checkout's `.aethyme/`): the database and its siblings
/// (`broker.db`, `-wal`, `-shm`, `-journal`, its bootstrap lock), a lock file
/// directly in it, or anything under `run/`, `logs/` or `locks/`. A working
/// directory never is.
pub(crate) fn is_broker_state_file(
    open: &crate::reclaim::OpenFileUse,
    state_dir: &std::path::Path,
) -> bool {
    if open.descriptor == "cwd" {
        return false;
    }
    let Ok(relative) = open.path.strip_prefix(state_dir) else {
        return false;
    };
    let mut components = relative.components();
    let Some(first) = components.next() else {
        return false;
    };
    let first = first.as_os_str().to_string_lossy();
    if components.next().is_some() {
        return matches!(first.as_ref(), "run" | "logs" | "locks");
    }
    first.starts_with("broker.db") || first.ends_with(".lock")
}

/// The checkout's HEAD commit, or `None` when it cannot be read.
fn current_checkout_head(worktree: &std::path::Path) -> Option<String> {
    crate::git::GitRepo::discover(worktree)
        .ok()?
        .head_commit()
        .ok()
}

/// Meta key holding `"<head> <default-branch tip>"` for the last HEAD the
/// landing search found unlanded, so the search is not repeated every lap
/// until either moves.
fn landed_search_memo_key(session_id: i64) -> String {
    format!("landed_lane.not_landed.{session_id}")
}

/// Meta key holding `"<head> <default-branch tip> <evidence>"` for the last
/// HEAD the landing search proved landed, so a later lap answers with one
/// ancestry check of the recorded tip instead of searching again.
fn landed_proof_memo_key(session_id: i64) -> String {
    format!("landed_lane.landed.{session_id}")
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

fn index_active_lease_paths_by_holder<'lease>(
    lease_paths: impl IntoIterator<Item = (i64, &'lease str)>,
) -> std::collections::HashMap<i64, Vec<&'lease str>> {
    let mut paths_by_holder: std::collections::HashMap<i64, Vec<&'lease str>> =
        std::collections::HashMap::new();
    for (holder_session_id, path) in lease_paths {
        paths_by_holder
            .entry(holder_session_id)
            .or_default()
            .push(path);
    }
    paths_by_holder
}

fn holder_has_overlapping_active_lease(
    paths_by_holder: &std::collections::HashMap<i64, Vec<&str>>,
    holder_session_id: i64,
    path: &str,
) -> bool {
    paths_by_holder
        .get(&holder_session_id)
        .is_some_and(|paths| {
            paths
                .iter()
                .any(|lease_path| crate::leases::paths_overlap(path, lease_path))
        })
}

impl Broker {
    // ── agents (liveness) ─────────────────────────────────────────────

    /// Live sessions with liveness derived from what the broker can
    /// actually observe: store activity, worktree git metadata mtimes,
    /// and PID liveness for spawned sessions. Reconciles dead spawned
    /// processes to `exited` in the store as a side effect.
    pub fn agents(&mut self, now_ms: i64) -> Result<Vec<AgentView>, BrokerOpError> {
        self.agents_where(now_ms, |_| true)
    }

    /// [`Broker::agents`] for only the live sessions `keep` accepts.
    ///
    /// Each view is derived exactly as `agents` derives it, side effects
    /// included, so a caller that needs a few sessions' liveness gets the same
    /// answer for them without paying for, or reconciling, every other one.
    /// Each session left out is counted as `agents_view_sessions_skipped`.
    pub(crate) fn agents_where(
        &mut self,
        now_ms: i64,
        keep: impl Fn(&Session) -> bool,
    ) -> Result<Vec<AgentView>, BrokerOpError> {
        let _span = crate::phase_telemetry::span("agents_view");
        let mut sessions = self.store.live_sessions()?;
        sessions.retain(|session| {
            let kept = keep(session);
            if !kept {
                crate::phase_telemetry::add(
                    "agents_view_sessions_skipped",
                    std::time::Duration::ZERO,
                );
            }
            kept
        });
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
        // Sessions past the shorter no-work window by their row, not yet past
        // the full one: judged by the no-work lane only. No not-due memo
        // applies to them, so a session that can qualify at that age is
        // judged at that age.
        let no_work_after_ms = self.no_work_window_ms();
        let no_work_candidates = sessions
            .iter()
            .filter(|session| {
                no_work_after_ms > 0
                    && !session.status.is_closed()
                    && !retirement_candidates.contains(&session.id)
                    && crate::decide_abandonment(
                        &SessionActivity {
                            session_id: session.id,
                            last_activity_at: session.last_activity_at,
                            created_at: session.created_at,
                            agent_alive: None,
                            closed: false,
                        },
                        now_ms,
                        no_work_after_ms,
                    )
                    .is_abandoned()
            })
            .map(|session| session.id)
            .collect::<std::collections::HashSet<_>>();
        // A foreground command only records that sessions are due and queues
        // the shared detached worker: judging them here meant a nonblocking
        // lifecycle claim such a command lost on every busy repository, and a
        // sandboxed caller's refused process listing read as a keep.
        let judge_inline = crate::file_lock::inline_session_retirement_allowed();
        let mut due_deferred = 0_usize;
        if judge_inline {
            let primed = retirement_candidates
                .iter()
                .copied()
                .chain(no_work_candidates.iter().copied())
                .collect::<Vec<_>>();
            holder_process_table.prime(self.store_ref(), &primed, None);
        }
        // Only a session past the window by its row can be closed here (the
        // close path keeps any session whose row or checkout activity is
        // newer), and one the retirement memo knows is not due yet would be
        // kept again: neither is judged. Reading the memo writes nothing.
        let due_by_row = retirement_candidates
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        let not_due_memo = crate::auto_cleanup::NotDueMemo::load(self, abandon_after_ms, now_ms);
        let enqueue_closed_checkout_cleanup = crate::load_retention_policy(&self.main_root)
            .is_ok_and(|policy| policy.auto_cleanup_worktrees_on_finish)
            && crate::AutoCleanupPolicy::load(&self.main_root)
                .is_ok_and(|policy| policy.auto_remove);
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
            let judge =
                due_by_row.contains(&session.id) && !not_due_memo.not_due(session.id, now_ms);
            if !derived_status.is_closed() && !judge {
                crate::phase_telemetry::add(
                    "agents_view_abandonment_not_due",
                    std::time::Duration::ZERO,
                );
            }
            if !judge_inline
                && !derived_status.is_closed()
                && (judge || no_work_candidates.contains(&session.id))
            {
                due_deferred += 1;
                crate::phase_telemetry::add(
                    "agents_view_abandonment_due_deferred",
                    std::time::Duration::ZERO,
                );
            }
            let abandoned_now = judge_inline
                && !derived_status.is_closed()
                && judge
                && matches!(
                    self.try_close_abandoned_session(
                        session.id,
                        now_ms,
                        abandon_after_ms,
                        &mut holder_process_table,
                        true,
                        None,
                    )?,
                    AbandonedSessionCloseOutcome::Closed
                );
            let abandoned_now = abandoned_now
                || (judge_inline
                    && !derived_status.is_closed()
                    && no_work_candidates.contains(&session.id)
                    && matches!(
                        self.try_close_no_work_session(
                            session.id,
                            now_ms,
                            no_work_after_ms,
                            &mut holder_process_table,
                            true,
                            None,
                        )?,
                        AbandonedSessionCloseOutcome::Closed
                    ));
            if abandoned_now {
                derived_status = SessionStatus::Closed;
                if enqueue_closed_checkout_cleanup
                    && session.origin == crate::SessionOrigin::Spawned
                {
                    crate::warn_unrecorded(
                        "wake post-close checkout cleanup",
                        self.request_checkout_cleanup_after_close(session.id)
                            .map(|_| ()),
                    );
                }
            }
            views.push(AgentView {
                session,
                activity_at,
                derived_status,
                pid_alive,
            });
        }
        if due_deferred > 0 {
            self.request_session_retirement_for_due_sessions();
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
    /// Whether the session still owns an unresolved coordinated operation or
    /// guarded command, not counting guarded-exec receipts whose command
    /// provably died with an earlier boot. Read-only: such receipts are
    /// settled only once the session actually closes
    /// ([`Self::settle_dead_receipts_after_close`]).
    ///
    /// Only the live `exec` command moves a receipt on. The per-session
    /// operation guard does not prove a receipt of the current boot dead:
    /// the shared lock is close-on-exec so a detached child never pins it
    /// (`file_lock::SessionOperationGuard`), so a command's children can
    /// outlive the command that owns the receipt. A receipt that names
    /// another boot is the proof; one written without a boot id by an older
    /// version counts only when its last write predates the boot by
    /// [`crate::host_boot::PRE_BOOT_MARGIN_MS`]. `outcome_unknown` receipts
    /// stay blocking whatever their age: they mean "never retry blindly",
    /// not "still running".
    pub(crate) fn session_operations_unresolved_since_boot(
        &self,
        session_id: i64,
    ) -> Result<bool, crate::BrokerError> {
        self.store_ref()
            .session_has_unresolved_operations_since_boot(
                session_id,
                &crate::host_boot::BootEvidence::current(),
            )
    }

    /// After a close, settle the session's receipts whose command died with
    /// an earlier boot, so they stop reading as running.
    fn settle_dead_receipts_after_close(&mut self, session_id: i64) {
        match self.store().abandon_dead_guarded_exec_receipts(
            session_id,
            &crate::host_boot::BootEvidence::current(),
        ) {
            Ok(settled) => {
                for _ in 0..settled {
                    crate::phase_telemetry::add(
                        "guarded_exec_receipts_abandoned_pre_boot",
                        std::time::Duration::ZERO,
                    );
                }
            }
            Err(error) => crate::warn_unrecorded(
                "settle guarded exec receipts of a closed session",
                Err::<(), _>(error),
            ),
        }
    }

    pub(crate) fn try_close_abandoned_session(
        &mut self,
        session_id: i64,
        decision_now_ms: i64,
        abandon_after_ms: i64,
        process_table: &mut crate::session_holder::RecordedHolderProcessTable,
        allow_process_snapshot: bool,
        deadline: Option<std::time::Instant>,
    ) -> Result<AbandonedSessionCloseOutcome, BrokerOpError> {
        process_table.clear_keep_reason();
        let outcome = self.try_close_session_inner(
            session_id,
            decision_now_ms,
            abandon_after_ms,
            process_table,
            allow_process_snapshot,
            deadline,
            SessionCloseLane::Abandonment,
        )?;
        if outcome == AbandonedSessionCloseOutcome::Closed {
            process_table.clear_keep_reason();
            self.retire_inline_after_close(session_id, deadline);
        } else {
            // Every keep is counted per reason, so the metrics log says why
            // sessions far past the window stay open.
            let reason = process_table
                .keep_reason()
                .unwrap_or(AbandonmentKeepReason::Unrecorded);
            process_table.note_keep_reason(reason);
            crate::phase_telemetry::add(reason.telemetry_name(), std::time::Duration::ZERO);
        }
        Ok(outcome)
    }

    pub(crate) fn try_close_landed_session(
        &mut self,
        session_id: i64,
        decision_now_ms: i64,
        landed_after_ms: i64,
        process_table: &mut crate::session_holder::RecordedHolderProcessTable,
        allow_process_snapshot: bool,
        deadline: Option<std::time::Instant>,
    ) -> Result<AbandonedSessionCloseOutcome, BrokerOpError> {
        let outcome = self.try_close_session_inner(
            session_id,
            decision_now_ms,
            landed_after_ms,
            process_table,
            allow_process_snapshot,
            deadline,
            SessionCloseLane::Landed,
        )?;
        self.record_landed_proof_outcome(session_id, LandedSessionDecline::from_outcome(outcome));
        if outcome == AbandonedSessionCloseOutcome::Closed {
            self.retire_inline_after_close(session_id, deadline);
        }
        Ok(outcome)
    }

    /// The landed close for a spawned session whose every recorded pull
    /// request merged, after `merged_after_ms` of inactivity counted from the
    /// later of its last activity and its last merge. Every proof of
    /// [`Self::try_close_landed_session`] applies, plus the running-work
    /// checks listed on `SessionCloseLane::MergedPullRequest`.
    pub(crate) fn try_close_merged_pull_request_session(
        &mut self,
        session_id: i64,
        decision_now_ms: i64,
        merged_after_ms: i64,
        process_table: &mut crate::session_holder::RecordedHolderProcessTable,
        allow_process_snapshot: bool,
        deadline: Option<std::time::Instant>,
    ) -> Result<AbandonedSessionCloseOutcome, BrokerOpError> {
        let outcome = self.try_close_session_inner(
            session_id,
            decision_now_ms,
            merged_after_ms,
            process_table,
            allow_process_snapshot,
            deadline,
            SessionCloseLane::MergedPullRequest,
        )?;
        self.record_landed_proof_outcome(session_id, LandedSessionDecline::from_outcome(outcome));
        if outcome == AbandonedSessionCloseOutcome::Closed {
            self.retire_inline_after_close(session_id, deadline);
        }
        Ok(outcome)
    }

    /// The early close for a session that never did any work, after
    /// `no_work_after_ms` of inactivity (`session_no_work_abandoned_after_hours`,
    /// never longer than the abandonment window). Every check of the
    /// abandonment window applies; the lane's own conditions are listed on
    /// `SessionCloseLane::NoWork`. Each keep is counted under
    /// `session_no_work_kept_*`, each close as `session_closed_no_work`.
    pub(crate) fn try_close_no_work_session(
        &mut self,
        session_id: i64,
        decision_now_ms: i64,
        no_work_after_ms: i64,
        process_table: &mut crate::session_holder::RecordedHolderProcessTable,
        allow_process_snapshot: bool,
        deadline: Option<std::time::Instant>,
    ) -> Result<AbandonedSessionCloseOutcome, BrokerOpError> {
        process_table.clear_keep_reason();
        let outcome = self.try_close_session_inner(
            session_id,
            decision_now_ms,
            no_work_after_ms,
            process_table,
            allow_process_snapshot,
            deadline,
            SessionCloseLane::NoWork,
        )?;
        if outcome == AbandonedSessionCloseOutcome::Closed {
            process_table.clear_keep_reason();
            crate::phase_telemetry::add("session_closed_no_work", std::time::Duration::ZERO);
        } else {
            let reason = process_table
                .keep_reason()
                .unwrap_or(AbandonmentKeepReason::Unrecorded);
            crate::phase_telemetry::add(reason.no_work_telemetry_name(), std::time::Duration::ZERO);
        }
        Ok(outcome)
    }

    /// The abandonment window's per-class gate (`[cleanup]
    /// stale_*_close_after_hours`): the session's class when it may close,
    /// otherwise the keep. A class that holds work closes only after a
    /// closing-soon note left since its last activity has stood for the
    /// notice period; the note is left under the claims, once the holder is
    /// known not to be live (the first look, `under_claims` false, lets such
    /// a session through to those checks), and a note that cannot be written
    /// keeps the session without advancing. An unread note older than the
    /// latest activity is acknowledged and replaced. Every judgement is
    /// remembered for `broker status`.
    fn stale_class_gate(
        &mut self,
        session: &Session,
        activity_ms: i64,
        now_ms: i64,
        process_table: &crate::session_holder::RecordedHolderProcessTable,
        under_claims: bool,
    ) -> Result<crate::stale_session_policy::StaleWorkClass, AbandonedSessionCloseOutcome> {
        use crate::stale_session_policy as stale;
        let unavailable = || {
            keep_with(
                process_table,
                AbandonmentKeepReason::PolicyUnavailable,
                AbandonedSessionCloseOutcome::OperationEvidenceUnavailable,
            )
        };
        let Ok(policy) = stale::StaleSessionPolicy::load(self.main_root()) else {
            return Err(unavailable());
        };
        let Ok(latest_note) = stale::latest_closing_soon_note(self.store_ref(), session.id) else {
            return Err(unavailable());
        };
        let class = self.stale_work_class(session);
        let activity_ms = activity_ms.max(session.created_at);
        let not_due = |revisit_at_ms: Option<i64>| {
            if let Some(revisit_at_ms) = revisit_at_ms {
                process_table.note_not_due_before(revisit_at_ms);
            }
            keep_with(
                process_table,
                AbandonmentKeepReason::WorkClassNotDue,
                AbandonedSessionCloseOutcome::NotDue,
            )
        };
        let verdict = policy.judge(
            class,
            activity_ms,
            now_ms,
            latest_note.map(|(noted_at, _)| noted_at),
        );
        let due_at_ms = match verdict {
            stale::StaleClassVerdict::Due { due_at_ms }
            | stale::StaleClassVerdict::NotDue { due_at_ms, .. }
            | stale::StaleClassVerdict::NeedsNotice { due_at_ms } => due_at_ms,
        };
        stale::remember_class(
            self.store(),
            session.id,
            stale::ClassRecord { class, due_at_ms },
        );
        match verdict {
            stale::StaleClassVerdict::Due { .. } => Ok(class),
            stale::StaleClassVerdict::NotDue { revisit_at_ms, .. } => {
                Err(not_due(Some(revisit_at_ms)))
            }
            stale::StaleClassVerdict::NeedsNotice { .. } if !under_claims => Ok(class),
            stale::StaleClassVerdict::NeedsNotice { due_at_ms } => {
                // An unread note left before the latest activity is stale:
                // acknowledged, so the new note is written, not deduplicated.
                if let Some((noted_at, Some(note_id))) = latest_note
                    && noted_at < activity_ms
                    && self.store().acknowledge_automation_note(note_id).is_err()
                {
                    crate::phase_telemetry::add(
                        "stale_closing_soon_note_failed",
                        std::time::Duration::ZERO,
                    );
                    return Err(not_due(None));
                }
                let written = if stale::closing_soon_note_write_fails_for_test() {
                    None
                } else {
                    self.store()
                        .record_automation_note(
                            crate::session_auto_close::NOTE_SENDER,
                            session.id,
                            &stale::closing_soon_note(session.id, class, due_at_ms, now_ms),
                            Some(stale::CLOSING_SOON_NOTE_FINGERPRINT),
                        )
                        .ok()
                };
                let Some((note, _)) = written else {
                    // No note, no close: the next pass tries again.
                    crate::phase_telemetry::add(
                        "stale_closing_soon_note_failed",
                        std::time::Duration::ZERO,
                    );
                    return Err(not_due(None));
                };
                crate::phase_telemetry::add("stale_closing_soon_noted", std::time::Duration::ZERO);
                Err(not_due(Some(
                    due_at_ms.max(note.created_at.saturating_add(policy.notice_ms)),
                )))
            }
        }
    }

    /// The no-work window from the retention policy, in milliseconds; `0`
    /// (disabled) when the policy cannot be read.
    pub(crate) fn no_work_window_ms(&self) -> i64 {
        crate::load_retention_policy(&self.main_root)
            .map(|policy| policy.no_work_abandonment_window_ms())
            .unwrap_or(0)
    }

    /// The no-work lane's own conditions, read under the session operation
    /// guard and the repository lifecycle claim immediately before the close:
    /// HEAD equals the recorded diff base (read from Git now), the checkout
    /// is clean under the shared cleanup classifier, no pull request recorded
    /// for the session is unmerged (unreadable state does not qualify), and
    /// the session has no merge-queue entry. `None` when all hold.
    fn no_work_decline(&self, session: &crate::Session) -> Option<AbandonmentKeepReason> {
        if session.origin != crate::SessionOrigin::Spawned {
            return Some(AbandonmentKeepReason::NoWorkAdopted);
        }
        let worktree = std::path::Path::new(&session.worktree_path);
        // HEAD must be the session branch itself, and that branch must still
        // be its diff base: a detached HEAD or another branch at the base
        // can hide commits on the session branch.
        let Ok(checkout) = crate::git::GitRepo::discover(worktree) else {
            return Some(AbandonmentKeepReason::NoWorkSessionCommits);
        };
        let branch_ref = format!("refs/heads/{}", session.branch);
        if checkout.symbolic_ref("HEAD").as_deref() != Some(branch_ref.as_str()) {
            return Some(AbandonmentKeepReason::NoWorkSessionCommits);
        }
        match (
            checkout.resolve_ref(&branch_ref),
            session.diff_base.as_deref(),
        ) {
            (Some(tip), Some(base)) if tip == base => {}
            _ => return Some(AbandonmentKeepReason::NoWorkSessionCommits),
        }
        let Ok(policy) = crate::auto_cleanup::AutoCleanupPolicy::load(self.main_root()) else {
            return Some(AbandonmentKeepReason::NoWorkWorktreeDirty);
        };
        match crate::auto_cleanup::uncommitted_reason(
            worktree,
            Some(session.branch.as_str()),
            self.main_root(),
            &policy,
        ) {
            Ok(None) => {}
            Ok(Some(_)) | Err(_) => return Some(AbandonmentKeepReason::NoWorkWorktreeDirty),
        }
        match crate::session_auto_close::PullRequestMerges::read(self.store_ref(), session.id) {
            Some(merges) if merges.links.iter().all(|(_, _, merged)| merged.is_some()) => {}
            Some(_) => return Some(AbandonmentKeepReason::NoWorkPullRequestOpen),
            None => return Some(AbandonmentKeepReason::NoWorkPullRequestUnknown),
        }
        match self.store_ref().latest_merge_queue_for_session(session.id) {
            Ok(None) => None,
            Ok(Some(_)) | Err(_) => Some(AbandonmentKeepReason::NoWorkMergeQueue),
        }
    }

    /// The newer session a live holder was bound to after `activity_at`, when
    /// it has moved on from `session_id`; `None` when this is still its
    /// latest session, the binding cannot be read, or it is a shared agent
    /// service (whose binding names no single task). Database only.
    fn holder_moved_on(
        &self,
        session_id: i64,
        holder_event: &crate::Event,
        activity_at: i64,
    ) -> Result<Option<i64>, BrokerError> {
        let Some(agent) = holder_event.payload_json.as_deref().and_then(|payload| {
            serde_json::from_str::<crate::session_holder::AgentProcess>(payload).ok()
        }) else {
            return Ok(None);
        };
        if crate::session_holder::is_shared_agent_service(&agent.command) {
            return Ok(None);
        }
        self.store_ref().newer_holder_binding_elsewhere(
            session_id,
            holder_event.id,
            activity_at,
            agent.pid,
            &agent.started,
        )
    }

    /// A whitespace-free fingerprint of the session's recorded pull requests
    /// and their merge times, or `unknown` when they cannot be read.
    fn pull_request_fingerprint(&self, session_id: i64) -> String {
        crate::session_auto_close::PullRequestMerges::read(self.store_ref(), session_id)
            .map_or_else(|| "unknown".to_string(), |merges| merges.fingerprint())
    }

    /// Whether a follow-up task (its latest `session.reused` event, or the
    /// creation of an adopted follow-up on a checkout the broker spawned)
    /// started less than the longer of `session_landed_after_hours` and
    /// `session_abandoned_after_hours` ago.
    fn follow_up_protected(
        &self,
        session: &crate::Session,
        follow_up_on_spawned_checkout: bool,
    ) -> Result<bool, LandedSessionDecline> {
        let follow_up_at = if follow_up_on_spawned_checkout {
            Some(session.created_at)
        } else {
            self.store_ref()
                .session_last_reused_at(session.id)
                .map_err(|_| LandedSessionDecline::LandingUnknown)?
        };
        let Some(follow_up_at) = follow_up_at else {
            return Ok(false);
        };
        let policy = crate::load_retention_policy(self.main_root())
            .map_err(|_| LandedSessionDecline::PolicyUnavailable)?;
        let window_ms = i64::from(
            policy
                .session_landed_after_hours
                .max(policy.session_abandoned_after_hours),
        ) * 3_600_000;
        Ok(crate::clock::epoch_ms().saturating_sub(follow_up_at) < window_ms)
    }

    /// The pull requests of a session the merged-pull-request lane may close,
    /// and when the last of them merged; the decline otherwise.
    fn merged_pull_request_evidence(
        &self,
        session: &crate::Session,
    ) -> Result<(crate::session_auto_close::PullRequestMerges, i64), LandedSessionDecline> {
        // A follow-up task started with `start --reuse` on a checkout the
        // broker created after its first session closed is adopted by
        // construction, yet the checkout is provably the broker's: same path
        // and branch as an earlier spawned session.
        let follow_up_on_spawned_checkout = session.origin == crate::SessionOrigin::Adopted
            && self
                .store_ref()
                .open_adopted_sessions_on_spawned_checkouts()
                .map_err(|_| LandedSessionDecline::LandingUnknown)?
                .contains(&session.id);
        if session.origin != crate::SessionOrigin::Spawned && !follow_up_on_spawned_checkout {
            return Err(LandedSessionDecline::Adopted);
        }
        // A follow-up task keeps its session for the longer of the landed and
        // abandonment windows, whoever holds it: the caller may be
        // unidentifiable and the previous holder gone, so neither holder
        // evidence nor two quiet hours prove that the new task is over.
        match self.follow_up_protected(session, follow_up_on_spawned_checkout) {
            Ok(false) => {}
            Ok(true) => return Err(LandedSessionDecline::RecentActivity),
            Err(reason) => return Err(reason),
        }
        let merges =
            crate::session_auto_close::PullRequestMerges::read(self.store_ref(), session.id)
                .ok_or(LandedSessionDecline::LandingUnknown)?;
        if merges.links.is_empty() {
            return Err(LandedSessionDecline::NoLandingEvidence);
        }
        let Some(merged_at) = merges.latest_merged_at() else {
            return Err(LandedSessionDecline::PullRequestUnmerged);
        };
        Ok((merges, merged_at))
    }

    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    fn landed_session_is_safe_to_retire(&self, session: &crate::Session) -> bool {
        self.landed_session_decline(session, None).is_ok()
    }

    /// The landing proof for `session`, or why it cannot be retired as
    /// landed. The proof holds when no linked pull request is unmerged,
    /// automatic removal applies to the checkout, the fetched default branch
    /// holds every commit of its HEAD by the landing rule `finish` uses (a
    /// representation recorded against that branch, ancestry, or
    /// `default_branch_landing`, whose `landing_covers` keeps a follow-up
    /// commit after a squash unlanded), and the worktree is clean under the
    /// shared cleanup classifier.
    ///
    /// The content search runs only when a merged pull request, a
    /// representation or ancestry already suggests a landing, stops at
    /// `deadline` (`BudgetExhausted`), and is not repeated for a HEAD it
    /// already found unlanded against the same default-branch tip. The close
    /// path computes its one session deadline itself; this entry point is
    /// the proof alone, for tests.
    #[cfg(test)]
    pub(crate) fn landed_session_decline(
        &self,
        session: &crate::Session,
        deadline: Option<std::time::Instant>,
    ) -> Result<LandedProof, LandedSessionDecline> {
        let session_deadline = self.landed_proof_session_deadline(session.id, deadline);
        self.landed_session_decline_with_memo(session, deadline, session_deadline, true)
    }

    /// The consecutive `proof_timed_out` streaks, by session id, as
    /// `(streak, first timed out at)`. Unreadable state reads as none.
    pub(crate) fn landed_proof_timeout_streaks(
        &self,
    ) -> std::collections::BTreeMap<i64, (u32, i64)> {
        self.store_ref()
            .meta_get(LANDED_PROOF_TIMEOUTS_KEY)
            .ok()
            .flatten()
            .and_then(|value| {
                serde_json::from_str::<std::collections::BTreeMap<i64, (u32, i64)>>(&value).ok()
            })
            .unwrap_or_default()
    }

    /// This session's budget for one landed judgement: the base budget
    /// doubled once per consecutive `proof_timed_out`, or (a long-budget
    /// grant) unbounded but for the pass deadline.
    pub(crate) fn landed_proof_session_budget(&self, session_id: i64) -> std::time::Duration {
        if LONG_PROOF_GRANT.with(std::cell::Cell::get) == Some(session_id) {
            return std::time::Duration::MAX;
        }
        let streak = self
            .landed_proof_timeout_streaks()
            .get(&session_id)
            .map_or(0, |(streak, _)| *streak);
        let budget = doubled_proof_budget(landed_proof_base_budget(session_id), streak);
        if streak > 0 {
            crate::phase_telemetry::add("landed_proof_session_budget_doubled", budget);
        }
        budget
    }

    /// Record how one landed judgement of `session_id` ended: a
    /// `proof_timed_out` extends its streak, any conclusive outcome (a close
    /// included, `decline` `None`) ends it.
    pub(crate) fn record_landed_proof_outcome(
        &self,
        session_id: i64,
        decline: Option<LandedSessionDecline>,
    ) {
        let mut streaks = self.landed_proof_timeout_streaks();
        let changed = match decline {
            Some(LandedSessionDecline::ProofTimedOut) => {
                let entry = streaks
                    .entry(session_id)
                    .or_insert((0, crate::clock::epoch_ms()));
                entry.0 = entry.0.saturating_add(1);
                crate::phase_telemetry::add(
                    proof_timeout_streak_counter(entry.0),
                    std::time::Duration::ZERO,
                );
                true
            }
            Some(reason) if !reason.is_conclusive() => false,
            _ => {
                let ended = streaks.remove(&session_id);
                if ended.is_some() {
                    crate::phase_telemetry::add(
                        "landed_proof_timeout_streak_ended",
                        std::time::Duration::ZERO,
                    );
                }
                ended.is_some()
            }
        };
        if decline.is_none_or(LandedSessionDecline::is_conclusive) {
            self.forget_landed_proof_unprovable(&[session_id]);
        }
        if changed {
            self.store_landed_proof_timeout_streaks(streaks);
        }
    }

    fn store_landed_proof_timeout_streaks(
        &self,
        mut streaks: std::collections::BTreeMap<i64, (u32, i64)>,
    ) {
        while streaks.len() > LANDED_PROOF_TIMEOUTS_MAX {
            let Some((&oldest, _)) = streaks
                .iter()
                .min_by_key(|(session_id, (_, first))| (*first, **session_id))
            else {
                break;
            };
            streaks.remove(&oldest);
        }
        crate::warn_unrecorded(
            "remember landed-proof timeout streaks",
            self.store_ref().meta_set(
                LANDED_PROOF_TIMEOUTS_KEY,
                &serde_json::to_string(&streaks).unwrap_or_default(),
            ),
        );
    }

    /// Drop the streaks and unprovable marks of sessions that are no longer
    /// open: once per likely-landed pass, bounded by the stored entries.
    pub(crate) fn prune_landed_proof_timeouts(&self, open: &std::collections::BTreeSet<i64>) {
        let mut streaks = self.landed_proof_timeout_streaks();
        let before = streaks.len();
        streaks.retain(|session_id, _| open.contains(session_id));
        for _ in streaks.len()..before {
            crate::phase_telemetry::add(
                "landed_proof_timeout_streaks_pruned",
                std::time::Duration::ZERO,
            );
        }
        if streaks.len() != before {
            self.store_landed_proof_timeout_streaks(streaks);
        }
        let gone = self
            .landed_proof_unprovable_sessions()
            .into_iter()
            .filter(|session_id| !open.contains(session_id))
            .collect::<Vec<_>>();
        self.forget_landed_proof_unprovable(&gone);
    }

    /// The sessions that still timed out with a long-budget grant.
    pub(crate) fn landed_proof_unprovable_sessions(&self) -> std::collections::BTreeSet<i64> {
        self.store_ref()
            .meta_get(LANDED_PROOF_UNPROVABLE_KEY)
            .ok()
            .flatten()
            .and_then(|value| serde_json::from_str::<std::collections::BTreeSet<i64>>(&value).ok())
            .unwrap_or_default()
    }

    fn store_landed_proof_unprovable(&self, sessions: &std::collections::BTreeSet<i64>) {
        crate::warn_unrecorded(
            "remember sessions the landed proof could not conclude",
            self.store_ref().meta_set(
                LANDED_PROOF_UNPROVABLE_KEY,
                &serde_json::to_string(sessions).unwrap_or_default(),
            ),
        );
    }

    /// Record that `session_id` timed out even with a long-budget grant.
    pub(crate) fn mark_landed_proof_unprovable(&self, session_id: i64) {
        crate::phase_telemetry::add("landed_proof_unprovable", std::time::Duration::ZERO);
        let mut sessions = self.landed_proof_unprovable_sessions();
        if sessions.insert(session_id) {
            while sessions.len() > LANDED_PROOF_TIMEOUTS_MAX {
                sessions.pop_first();
            }
            self.store_landed_proof_unprovable(&sessions);
        }
    }

    fn forget_landed_proof_unprovable(&self, session_ids: &[i64]) {
        if session_ids.is_empty() {
            return;
        }
        let mut sessions = self.landed_proof_unprovable_sessions();
        let before = sessions.len();
        for session_id in session_ids {
            sessions.remove(session_id);
        }
        if sessions.len() != before {
            self.store_landed_proof_unprovable(&sessions);
        }
    }

    /// Whether this likely-landed pass, which has a session to grant, runs
    /// the grant first: every [`LANDED_PROOF_GRANT_FIRST_EVERY`]th such pass.
    /// Otherwise it runs last, after the normal queue.
    pub(crate) fn landed_proof_grant_goes_first(&self) -> bool {
        let passes = self
            .store_ref()
            .meta_get(LANDED_PROOF_GRANT_PASSES_KEY)
            .ok()
            .flatten()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0)
            .wrapping_add(1);
        crate::warn_unrecorded(
            "count landed-proof grant passes",
            self.store_ref()
                .meta_set(LANDED_PROOF_GRANT_PASSES_KEY, &passes.to_string()),
        );
        passes.is_multiple_of(LANDED_PROOF_GRANT_FIRST_EVERY)
    }

    /// The one session deadline of a landed judgement of `session_id` in a
    /// pass ending at `pass`. A long-budget grant gets what is left of the
    /// pass less a tenth (at most a second), so a proof that still runs out
    /// is told apart from the pass running out (`proof_timed_out`, not
    /// `budget_exhausted`). Otherwise the session's budget from now, a
    /// doubled budget never more than half of what is left of the pass, so
    /// one slow session cannot take a whole pass outside the grant.
    pub(crate) fn landed_proof_session_deadline(
        &self,
        session_id: i64,
        pass: Option<std::time::Instant>,
    ) -> Option<std::time::Instant> {
        let budget = self.landed_proof_session_budget(session_id);
        if budget == std::time::Duration::MAX {
            return pass.map(|pass| {
                let remaining = pass.saturating_duration_since(std::time::Instant::now());
                pass.checked_sub((remaining / 10).min(std::time::Duration::from_secs(1)))
                    .unwrap_or(pass)
            });
        }
        let doubled = budget > landed_proof_base_budget(session_id);
        let budget = match pass {
            Some(pass) if doubled => {
                budget.min(pass.saturating_duration_since(std::time::Instant::now()) / 2)
            }
            _ => budget,
        };
        session_deadline_within(pass, budget)
    }

    /// Of `candidates`, the session that has been timing out longest: the
    /// one a pass grants the rest of its share to.
    pub(crate) fn oldest_timed_out_session(&self, candidates: &[i64]) -> Option<i64> {
        self.landed_proof_timeout_streaks()
            .into_iter()
            .filter(|(session_id, _)| candidates.contains(session_id))
            .min_by_key(|(session_id, (_, first))| (*first, *session_id))
            .map(|(session_id, _)| session_id)
    }

    /// [`Self::landed_session_decline`]; with `use_memo` false a remembered
    /// landing is not trusted and the search runs again, as it does under
    /// the claims before a close that rested on one. `session_deadline` is
    /// the one session deadline of this judgement (the earlier of the pass
    /// deadline and the session's budget); `deadline` the pass's own.
    fn landed_session_decline_with_memo(
        &self,
        session: &crate::Session,
        deadline: Option<std::time::Instant>,
        session_deadline: Option<std::time::Instant>,
        use_memo: bool,
    ) -> Result<LandedProof, LandedSessionDecline> {
        // Every Git child of the proof (the content search and the proof-3
        // listings alike) stops at the session deadline.
        let _git_deadline = session_deadline.map(crate::git::limit_git_until);
        let out_of_budget =
            || session_deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline);
        // Which bound ran out: the pass (stop the lane, resume here later) or
        // only this session (keep it and move on).
        let exhausted = || {
            if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                LandedSessionDecline::BudgetExhausted
            } else {
                LandedSessionDecline::ProofTimedOut
            }
        };
        let unknown = |reason| {
            if out_of_budget() { exhausted() } else { reason }
        };
        self.landed_session_preconditions(session)?;
        if out_of_budget() {
            return Err(exhausted());
        }
        let worktree = std::path::Path::new(&session.worktree_path);
        let (checkout, head) = {
            let _span = crate::phase_telemetry::span("landed_proof_checkout_read");
            let checkout = crate::git::GitRepo::discover(worktree)
                .map_err(|_| unknown(LandedSessionDecline::WorktreeUnavailable))?;
            let head = checkout
                .head_commit()
                .map_err(|_| unknown(LandedSessionDecline::WorktreeUnavailable))?;
            (checkout, head)
        };
        let Some((upstream_ref, upstream)) = self.repo_handle().upstream_default() else {
            return Err(LandedSessionDecline::NoLandingEvidence);
        };
        let representation = self
            .store_ref()
            .session_representation(session.id, &head)
            .map_err(|_| LandedSessionDecline::LandingUnknown)?;
        let represented_upstream = representation.as_ref().is_some_and(|record| {
            (record.representing_ref == upstream_ref
                || record.representing_ref == format!("refs/remotes/{upstream_ref}"))
                && record
                    .representing_commit
                    .as_deref()
                    .is_some_and(|commit| self.repo_handle().is_ancestor(commit, &upstream))
        });
        let mut evidence = "representation";
        let mut memoized = false;
        if !represented_upstream {
            let merged_pull_request = self
                .store_ref()
                .session_has_merged_pull_request_link(session.id)
                .map_err(|_| LandedSessionDecline::LandingUnknown)?;
            // A session that never committed and never merged a pull request
            // has no work of its own to have landed; retiring it is the
            // abandonment window's call.
            if !merged_pull_request && session.diff_base.as_deref() == Some(head.as_str()) {
                return Err(LandedSessionDecline::NoSessionCommits);
            }
            let on_upstream = self
                .repo_handle()
                .is_ancestor_checked(&head, &upstream)
                .map_err(|_| unknown(LandedSessionDecline::LandingUnknown))?;
            if !merged_pull_request && !on_upstream && representation.is_none() {
                return Err(LandedSessionDecline::NoLandingEvidence);
            }
            let memo_key = landed_search_memo_key(session.id);
            let memo_value = format!("{head} {upstream}");
            if !on_upstream
                && self
                    .store_ref()
                    .meta_get(&memo_key)
                    .ok()
                    .flatten()
                    .is_some_and(|recorded| recorded == memo_value)
            {
                crate::phase_telemetry::add(
                    "landed_landing_search_memo_skipped",
                    std::time::Duration::ZERO,
                );
                return Err(LandedSessionDecline::NotLanded);
            }
            if out_of_budget() {
                return Err(exhausted());
            }
            // A head already proved landed against an earlier default-branch
            // tip stays landed on any later tip: the default branch only moves
            // forward. One ancestry check replaces the content search.
            // The memo is bound to the head, the tip it was proved against
            // and the session's pull-request evidence (a new, reopened or
            // re-merged pull request changes the fingerprint).
            let landed_key = landed_proof_memo_key(session.id);
            let pull_request_fingerprint = self.pull_request_fingerprint(session.id);
            let remembered = self
                .store_ref()
                .meta_get(&landed_key)
                .ok()
                .flatten()
                .filter(|_| use_memo)
                .and_then(|value| {
                    let mut parts = value.split(' ');
                    let (recorded_head, recorded_tip, recorded_evidence, recorded_prs) =
                        (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
                    (recorded_head == head
                        && recorded_prs == pull_request_fingerprint
                        && self.repo_handle().is_ancestor(recorded_tip, &upstream))
                    .then_some(if recorded_evidence == "ancestry" {
                        "ancestry"
                    } else {
                        "content"
                    })
                });
            if let Some(recorded) = remembered {
                crate::phase_telemetry::add("landed_landing_memo_hit", std::time::Duration::ZERO);
                evidence = recorded;
                memoized = true;
            }
            let queue = self
                .store_ref()
                .latest_merge_queue_for_session_and_head(session.id, &head)
                .map_err(|_| LandedSessionDecline::LandingUnknown)?;
            let landed = remembered.is_some() || {
                let _span = crate::phase_telemetry::span("landed_landing_search");
                // A Git call the deadline stopped inside the search can read
                // as "not delivered" (an ancestry check that answers false
                // on error): any timeout makes the search inconclusive.
                let timeouts_before = crate::git::git_timeout_generation();
                let delivery = self
                    .head_delivery(session, &checkout, &head, &queue)
                    .map_err(|_| unknown(LandedSessionDecline::LandingUnknown))?;
                if crate::git::git_timeout_generation() != timeouts_before {
                    crate::phase_telemetry::add(
                        "landed_landing_search_timed_out",
                        std::time::Duration::ZERO,
                    );
                    return Err(stopped_listing_decline(
                        deadline,
                        session_deadline,
                        LandedSessionDecline::LandingUnknown,
                    ));
                }
                // Only the fetched default branch counts here: a local
                // integration commit is not a landing a pull-request
                // repository recognises.
                if delivery.on_remote_default {
                    evidence = "ancestry";
                } else {
                    evidence = "content";
                }
                delivery.on_remote_default || delivery.default_landing.is_some()
            };
            if landed && remembered.is_none() {
                crate::warn_unrecorded(
                    "remember a landed session head",
                    self.store_ref().meta_set(
                        &landed_key,
                        &format!("{head} {upstream} {evidence} {pull_request_fingerprint}"),
                    ),
                );
            }
            if !landed {
                if out_of_budget() {
                    return Err(exhausted());
                }
                crate::warn_unrecorded(
                    "remember an unlanded session head",
                    self.store_ref().meta_set(&memo_key, &memo_value),
                );
                return Err(LandedSessionDecline::NotLanded);
            }
        }
        let policy = crate::auto_cleanup::AutoCleanupPolicy::load(self.main_root())
            .map_err(|_| LandedSessionDecline::PolicyUnavailable)?;
        if out_of_budget() {
            return Err(exhausted());
        }
        let clean = {
            // Proof 3, timed on its own: an uncommitted-path listing that
            // crawled on a loaded host was the untimed part of `landed_proof`.
            let _span = crate::phase_telemetry::span("landed_proof_worktree_clean");
            crate::auto_cleanup::unclean_reason(
                worktree,
                Some(session.branch.as_str()),
                self.main_root(),
                &policy,
            )
        };
        if clean.is_err() && out_of_budget() {
            crate::phase_telemetry::add(
                "landed_proof_worktree_clean_timed_out",
                std::time::Duration::ZERO,
            );
        }
        match clean {
            Ok(None) => Ok(LandedProof {
                head,
                upstream_tip: upstream,
                evidence,
                memoized,
            }),
            Ok(Some(reason)) => {
                // Which kind of content keeps it: the next reading of the
                // worker metrics can tell untracked work from ignored leftovers.
                crate::phase_telemetry::add(
                    worktree_unclean_counter(crate::auto_cleanup::kept_reason_code(&reason)),
                    std::time::Duration::ZERO,
                );
                Err(LandedSessionDecline::WorktreeUnclean)
            }
            // A listing that failed is no verdict on the checkout.
            Err(_) => Err(stopped_listing_decline(
                deadline,
                session_deadline,
                LandedSessionDecline::WorktreeUnavailable,
            )),
        }
    }

    /// The cheap part of the landed proof: linked pull requests and cleanup
    /// policy, read from the database and the repository config.
    fn landed_session_preconditions(
        &self,
        session: &crate::Session,
    ) -> Result<(), LandedSessionDecline> {
        match self
            .store_ref()
            .session_has_unmerged_pull_request_link(session.id)
        {
            Ok(false) => {}
            Ok(true) => return Err(LandedSessionDecline::PullRequestUnmerged),
            Err(_) => return Err(LandedSessionDecline::LandingUnknown),
        }
        let policy = crate::auto_cleanup::AutoCleanupPolicy::load(self.main_root())
            .map_err(|_| LandedSessionDecline::PolicyUnavailable)?;
        if !policy.auto_remove {
            return Err(LandedSessionDecline::AutoRemoveDisabled);
        }
        if policy.is_pinned(std::path::Path::new(&session.worktree_path)) {
            return Err(LandedSessionDecline::Pinned);
        }
        Ok(())
    }

    /// Re-check a landed proof under the session and lifecycle claims without
    /// searching again: the preconditions still hold, HEAD is the proved
    /// head, the default branch has not moved, and nothing uncommitted,
    /// untracked or in progress appeared. Ignored content was classified by
    /// the proof and is classified again by the checkout-removal lane.
    fn landed_proof_still_holds(
        &self,
        session: &crate::Session,
        proof: &LandedProof,
        deadline: Option<std::time::Instant>,
        session_deadline: Option<std::time::Instant>,
    ) -> Option<LandedSessionDecline> {
        // Bounded by the judgement's one session deadline; a listing it
        // stops is `proof_timed_out`, never a change in the checkout.
        let _git_deadline = session_deadline.map(crate::git::limit_git_until);
        if let Err(reason) = self.landed_session_preconditions(session) {
            return Some(reason);
        }
        let worktree = std::path::Path::new(&session.worktree_path);
        match current_checkout_head(worktree) {
            Some(head) if head == proof.head => {}
            Some(_) => return Some(LandedSessionDecline::MovedBeforeClose),
            None => return Some(LandedSessionDecline::WorktreeUnavailable),
        }
        match self.repo_handle().upstream_default() {
            Some((_, tip)) if tip == proof.upstream_tip => {}
            Some(_) => return Some(LandedSessionDecline::UpstreamMoved),
            None => {
                return Some(stopped_listing_decline(
                    deadline,
                    session_deadline,
                    LandedSessionDecline::UpstreamMoved,
                ));
            }
        }
        let Ok(policy) = crate::auto_cleanup::AutoCleanupPolicy::load(self.main_root()) else {
            return Some(LandedSessionDecline::PolicyUnavailable);
        };
        match crate::auto_cleanup::uncommitted_reason(
            worktree,
            Some(session.branch.as_str()),
            self.main_root(),
            &policy,
        ) {
            Ok(None) => None,
            Ok(Some(_)) => Some(LandedSessionDecline::WorktreeUnclean),
            Err(_) => Some(stopped_listing_decline(
                deadline,
                session_deadline,
                LandedSessionDecline::WorktreeUnavailable,
            )),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn try_close_session_inner(
        &mut self,
        session_id: i64,
        decision_now_ms: i64,
        abandon_after_ms: i64,
        process_table: &mut crate::session_holder::RecordedHolderProcessTable,
        allow_process_snapshot: bool,
        deadline: Option<std::time::Instant>,
        lane: SessionCloseLane,
    ) -> Result<AbandonedSessionCloseOutcome, BrokerOpError> {
        // Every Git child of a budgeted close attempt stops at the pass
        // deadline, the cleanliness listings re-run under the session and
        // lifecycle claims included: a listing that outlives the pass would
        // hold the lane past its share and the lifecycle claim with it. All
        // Git here only reads, and a stopped listing keeps the session.
        let _git_deadline = deadline.map(crate::git::limit_git_until);
        let require_landed = matches!(
            lane,
            SessionCloseLane::Landed | SessionCloseLane::MergedPullRequest
        );
        // One session deadline for the whole landed judgement: the proof, a
        // memoized re-proof before the claims and the re-checks under them
        // share this session's budget.
        let session_deadline = if require_landed {
            self.landed_proof_session_deadline(session_id, deadline)
        } else {
            deadline
        };
        let merged_lane = lane == SessionCloseLane::MergedPullRequest;
        let no_work_lane = lane == SessionCloseLane::NoWork;
        if abandon_after_ms <= 0 {
            return Ok(keep_with(
                process_table,
                AbandonmentKeepReason::WindowDisabled,
                AbandonedSessionCloseOutcome::NotDue,
            ));
        }
        // `[cleanup] stale_session_auto_close = false` turns the idle windows
        // off; the landed lanes, which prove the work landed, still run.
        if matches!(
            lane,
            SessionCloseLane::Abandonment | SessionCloseLane::NoWork
        ) {
            match crate::stale_session_policy::StaleSessionPolicy::load(self.main_root()) {
                Ok(policy) if !policy.auto_close => {
                    return Ok(keep_with(
                        process_table,
                        AbandonmentKeepReason::WindowDisabled,
                        AbandonedSessionCloseOutcome::NotDue,
                    ));
                }
                Ok(_) => {}
                Err(_) => {
                    return Ok(keep_with(
                        process_table,
                        AbandonmentKeepReason::PolicyUnavailable,
                        AbandonedSessionCloseOutcome::OperationEvidenceUnavailable,
                    ));
                }
            }
        }
        let candidate = match self.store_ref().session(session_id) {
            Ok(session) => session,
            Err(BrokerError::SessionNotFound(_)) => {
                return Ok(keep_with(
                    process_table,
                    AbandonmentKeepReason::Changed,
                    AbandonedSessionCloseOutcome::Changed,
                ));
            }
            Err(error) => return Err(error.into()),
        };
        if candidate.cleanup_state != crate::SessionCleanupState::Open
            || candidate.status.is_closed()
        {
            return Ok(keep_with(
                process_table,
                AbandonmentKeepReason::Changed,
                AbandonedSessionCloseOutcome::Changed,
            ));
        }
        if candidate.pid.is_some_and(crate::broker::session_pid_alive) {
            return Ok(keep_with(
                process_table,
                AbandonmentKeepReason::SessionPidAlive,
                AbandonedSessionCloseOutcome::ActiveProcess,
            ));
        }
        // An adopted session keeps the full window: its diff base is the
        // adopted HEAD, which can carry work done before the adoption.
        if no_work_lane && candidate.origin != crate::SessionOrigin::Spawned {
            return Ok(keep_with(
                process_table,
                AbandonmentKeepReason::NoWorkAdopted,
                AbandonedSessionCloseOutcome::NotDue,
            ));
        }
        // Nothing makes `session_abandoned_after_hours` at least
        // `session_landed_after_hours`. When it is shorter, the abandonment
        // window must not close a follow-up task the landed lanes protect.
        if matches!(
            lane,
            SessionCloseLane::Abandonment | SessionCloseLane::NoWork
        ) && let Ok(policy) = crate::load_retention_policy(self.main_root())
            && (no_work_lane
                || policy.session_abandoned_after_hours < policy.session_landed_after_hours)
        {
            let follow_up_on_spawned_checkout = candidate.origin == crate::SessionOrigin::Adopted
                && self
                    .store_ref()
                    .open_adopted_sessions_on_spawned_checkouts()
                    .map(|ids| ids.contains(&candidate.id))
                    .unwrap_or(true);
            match self.follow_up_protected(&candidate, follow_up_on_spawned_checkout) {
                Ok(false) => {}
                Ok(true) | Err(_) => {
                    return Ok(keep_with(
                        process_table,
                        AbandonmentKeepReason::FollowUpProtected,
                        AbandonedSessionCloseOutcome::NotDue,
                    ));
                }
            }
        }
        // The merged-pull-request lane counts its window from the last merge
        // too: an agent that merged a minute ago may still be about to look.
        let merged_evidence = if merged_lane {
            match self.merged_pull_request_evidence(&candidate) {
                Ok(evidence) => Some(evidence),
                Err(reason) => {
                    return Ok(keep_with(
                        process_table,
                        AbandonmentKeepReason::LandedDeclined,
                        AbandonedSessionCloseOutcome::LandedDeclined(reason),
                    ));
                }
            }
        } else {
            None
        };
        let merged_at = merged_evidence.as_ref().map_or(0, |(_, at)| *at);
        let fs_activity = match checked_worktree_activity_ms(self.main_root(), &candidate) {
            Ok(activity) => activity.unwrap_or(0),
            Err(_) => {
                return Ok(keep_with(
                    process_table,
                    AbandonmentKeepReason::WorktreeActivityUnavailable,
                    AbandonedSessionCloseOutcome::WorktreeActivityUnavailable,
                ));
            }
        };
        let database_activity = fs_activity.max(candidate.last_activity_at).max(merged_at);
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
            process_table.note_not_due_before(
                database_activity
                    .max(candidate.created_at)
                    .saturating_add(abandon_after_ms),
            );
            return Ok(keep_with(
                process_table,
                AbandonmentKeepReason::RecentActivity,
                AbandonedSessionCloseOutcome::NotDue,
            ));
        }
        // Past the abandonment window, what closing would put at risk sets
        // how much longer the session waits (landed, pushed but unmerged,
        // unpushed or uncommitted work). Asked before any holder or process
        // evidence: it is the cheaper keep, and asked again under the claims.
        if lane == SessionCloseLane::Abandonment
            && let Err(kept) = self.stale_class_gate(
                &candidate,
                database_activity,
                decision_now_ms,
                process_table,
                false,
            )
        {
            return Ok(kept);
        }
        // The landing proof is asked before any holder, lease or process
        // evidence: most open sessions have not landed, and saying so costs
        // one ancestry check. It is asked again under both claims below.
        // The merged-pull-request lane is the exception: its sessions usually
        // have landed, so the proof is the content search that costs seconds,
        // and the holder (one process table per pass) is asked first.
        let mut landed_proof = if require_landed && !merged_lane {
            let _span = crate::phase_telemetry::span("landed_proof");
            match self.landed_session_decline_with_memo(
                &candidate,
                deadline,
                session_deadline,
                true,
            ) {
                Ok(proof) => Some(proof),
                Err(reason) => {
                    return Ok(keep_with(
                        process_table,
                        AbandonmentKeepReason::LandedDeclined,
                        AbandonedSessionCloseOutcome::LandedDeclined(reason),
                    ));
                }
            }
        } else {
            None
        };

        let holder = {
            let _span = crate::phase_telemetry::span("landed_holder_check");
            crate::session_holder::recorded_holder_liveness(
                self.store_ref(),
                session_id,
                process_table,
                allow_process_snapshot,
            )
        };
        let holder_event = self.store_ref().latest_session_holder_event(session_id)?;
        if holder_event.as_ref().map(|event| event.id) != holder.binding_event_id {
            return Ok(keep_with(
                process_table,
                AbandonmentKeepReason::Changed,
                AbandonedSessionCloseOutcome::Changed,
            ));
        }
        // The no-work lane needs a recorded holder that is provably gone, or
        // a live one that moved on to a newer session (the abandonment
        // window's rule): a session without one has only its age, which is
        // the full window's evidence, not this one's.
        if no_work_lane {
            let declined = match holder.liveness {
                crate::session_holder::RecordedHolderLiveness::Gone => None,
                crate::session_holder::RecordedHolderLiveness::Unbound => Some((
                    AbandonmentKeepReason::NoWorkHolderUnbound,
                    AbandonedSessionCloseOutcome::NotDue,
                )),
                crate::session_holder::RecordedHolderLiveness::Alive => {
                    let moved_on = match holder_event.as_ref() {
                        Some(event) => self.holder_moved_on(
                            session_id,
                            event,
                            fs_activity.max(candidate.last_activity_at),
                        )?,
                        None => None,
                    };
                    moved_on.is_none().then_some((
                        AbandonmentKeepReason::NoWorkHolderAlive,
                        AbandonedSessionCloseOutcome::ActiveProcess,
                    ))
                }
                crate::session_holder::RecordedHolderLiveness::Unknown
                    if process_table.process_listing_denied() =>
                {
                    Some((
                        AbandonmentKeepReason::HolderProcessTableDenied,
                        AbandonedSessionCloseOutcome::HolderUnavailable,
                    ))
                }
                crate::session_holder::RecordedHolderLiveness::Unknown => Some((
                    AbandonmentKeepReason::HolderUnknown,
                    AbandonedSessionCloseOutcome::HolderUnavailable,
                )),
            };
            if let Some((reason, outcome)) = declined {
                return Ok(keep_with(process_table, reason, outcome));
            }
        }
        let task_lease_binding_present = holder_event
            .as_ref()
            .is_some_and(crate::session_holder::task_lease_binding_present);
        let task_lease_expiry_enabled = crate::load_retention_policy(&self.main_root)
            .is_ok_and(|policy| policy.expire_abandoned_task_leases);
        // A landed session needs no opt-in to outlive a live holder: its work
        // is on the default branch and its worktree holds nothing else, so an
        // expired task lease plus the process-use checks below suffice. The
        // agent process itself routinely lives on, bound to later sessions.
        let holder_task_lease_expired = holder_event.as_ref().is_some_and(|event| {
            (task_lease_expiry_enabled || require_landed)
                && crate::session_holder::task_lease_expired(
                    event,
                    candidate.last_activity_at,
                    decision_now_ms,
                ) == Some(true)
        });
        let live_task_holder = if holder.liveness
            == crate::session_holder::RecordedHolderLiveness::Alive
            && holder_task_lease_expired
        {
            let Some(agent) = crate::session_holder::recorded_holder(self.store_ref(), session_id)?
            else {
                return Ok(keep_with(
                    process_table,
                    AbandonmentKeepReason::HolderRecordUnavailable,
                    AbandonedSessionCloseOutcome::ActiveProcess,
                ));
            };
            if crate::session_holder::is_shared_agent_service(&agent.command) {
                return Ok(keep_with(
                    process_table,
                    AbandonmentKeepReason::HolderSharedService,
                    AbandonedSessionCloseOutcome::ActiveProcess,
                ));
            }
            Some(agent)
        } else {
            None
        };
        // A live holder protects a session only while there is evidence it
        // uses that session. One orchestrating agent routinely holds dozens of
        // open sessions; when it was bound to a different session after this
        // one's last activity, it has moved on, and the running-work checks
        // below (open files with no holder exemption, gates, operations,
        // preparation, artifact producers, merge queue, leases) decide.
        let moved_on_to = if holder.liveness == crate::session_holder::RecordedHolderLiveness::Alive
            && live_task_holder.is_none()
        {
            match holder_event.as_ref() {
                Some(event) => self.holder_moved_on(
                    session_id,
                    event,
                    fs_activity.max(candidate.last_activity_at),
                )?,
                None => None,
            }
        } else {
            None
        };
        if moved_on_to.is_some() {
            crate::phase_telemetry::add("holder_moved_on_candidates", std::time::Duration::ZERO);
        }
        // A moved-on holder is no live holder of this session: the per-class
        // gate above (and again under the claims) decides how long its work
        // waits, uncommitted work included (the unpushed window, then the
        // archive path). Moved on means the same process (pid and start
        // time) was bound to another session of this repository after this
        // session's last activity and its latest binding here, so a bind
        // back, or any later activity here, makes it this session's holder
        // again. A process working in this checkout, a live explicit claim
        // and a shared agent service still keep it (below).
        if require_landed {
            match holder.liveness {
                crate::session_holder::RecordedHolderLiveness::Gone => {}
                crate::session_holder::RecordedHolderLiveness::Alive
                    if live_task_holder.is_some() || moved_on_to.is_some() => {}
                crate::session_holder::RecordedHolderLiveness::Alive => {
                    return Ok(keep_with(
                        process_table,
                        AbandonmentKeepReason::LandedDeclined,
                        AbandonedSessionCloseOutcome::LandedDeclined(
                            LandedSessionDecline::HolderAlive,
                        ),
                    ));
                }
                crate::session_holder::RecordedHolderLiveness::Unbound => {
                    return Ok(keep_with(
                        process_table,
                        AbandonmentKeepReason::LandedDeclined,
                        AbandonedSessionCloseOutcome::LandedDeclined(
                            LandedSessionDecline::HolderUnbound,
                        ),
                    ));
                }
                // No process table may be taken this late in the pass: that
                // is the pass running out of time, not a holder nobody can
                // check, and the next pass asks again.
                crate::session_holder::RecordedHolderLiveness::Unknown
                    if !allow_process_snapshot && holder.binding_event_id.is_some() =>
                {
                    return Ok(keep_with(
                        process_table,
                        AbandonmentKeepReason::LandedDeclined,
                        AbandonedSessionCloseOutcome::LandedDeclined(
                            LandedSessionDecline::BudgetExhausted,
                        ),
                    ));
                }
                crate::session_holder::RecordedHolderLiveness::Unknown => {
                    return Ok(keep_with(
                        process_table,
                        AbandonmentKeepReason::LandedDeclined,
                        AbandonedSessionCloseOutcome::LandedDeclined(
                            LandedSessionDecline::HolderUnknown,
                        ),
                    ));
                }
            }
        }
        if merged_lane {
            crate::phase_telemetry::add(
                "landed_merged_pr_reached_proof",
                std::time::Duration::ZERO,
            );
            let _span = crate::phase_telemetry::span("landed_proof");
            match self.landed_session_decline_with_memo(
                &candidate,
                deadline,
                session_deadline,
                true,
            ) {
                Ok(proof) => landed_proof = Some(proof),
                Err(reason) => {
                    return Ok(keep_with(
                        process_table,
                        AbandonmentKeepReason::LandedDeclined,
                        AbandonedSessionCloseOutcome::LandedDeclined(reason),
                    ));
                }
            }
        }
        match holder.liveness {
            // A command refused the process listing (sandboxed) is not
            // evidence about any holder: it is counted as its own reason, and
            // the pass took no further snapshot for it.
            crate::session_holder::RecordedHolderLiveness::Unknown
                if process_table.process_listing_denied() =>
            {
                return Ok(keep_with(
                    process_table,
                    AbandonmentKeepReason::HolderProcessTableDenied,
                    AbandonedSessionCloseOutcome::HolderUnavailable,
                ));
            }
            crate::session_holder::RecordedHolderLiveness::Unknown => {
                return Ok(keep_with(
                    process_table,
                    AbandonmentKeepReason::HolderUnknown,
                    AbandonedSessionCloseOutcome::HolderUnavailable,
                ));
            }
            crate::session_holder::RecordedHolderLiveness::Alive
                if live_task_holder.is_none() && moved_on_to.is_none() =>
            {
                return Ok(keep_with(
                    process_table,
                    AbandonmentKeepReason::HolderAliveCurrent,
                    AbandonedSessionCloseOutcome::ActiveProcess,
                ));
            }
            crate::session_holder::RecordedHolderLiveness::Gone
            | crate::session_holder::RecordedHolderLiveness::Unbound => {}
            crate::session_holder::RecordedHolderLiveness::Alive => {}
        }
        // No close without a complete process listing in this pass. A pass
        // refused the listing (a worker that inherited a caller's sandbox)
        // closes nothing, `Unbound` sessions included, which need no holder
        // listing of their own.
        if let Err(denied) = process_table.require_complete_listing(allow_process_snapshot) {
            let (reason, outcome) = if require_landed {
                (
                    AbandonmentKeepReason::LandedDeclined,
                    AbandonedSessionCloseOutcome::LandedDeclined(
                        LandedSessionDecline::HolderUnknown,
                    ),
                )
            } else if denied {
                (
                    AbandonmentKeepReason::HolderProcessTableDenied,
                    AbandonedSessionCloseOutcome::HolderUnavailable,
                )
            } else {
                (
                    AbandonmentKeepReason::HolderUnknown,
                    AbandonedSessionCloseOutcome::HolderUnavailable,
                )
            };
            return Ok(keep_with(process_table, reason, outcome));
        }

        // Every admitted session command holds the shared side from before
        // Broker::open (and its maintenance pass) through completion. The
        // exclusive side closes the startup-admission race and stays held
        // through the final lifecycle/CAS checks.
        // A remembered landing is a shortcut for judging, never for the
        // close. Before the claims are taken (so the lifecycle claim is never
        // held through a content search), search again without the memo; the
        // claims then re-check only that HEAD and the default-branch tip are
        // those this fresh search proved, and that nothing new is in the
        // checkout.
        if let Some(remembered) = landed_proof.as_ref().filter(|proof| proof.memoized) {
            let _span = crate::phase_telemetry::span("landed_reproof_before_claims");
            match self.landed_session_decline_with_memo(
                &candidate,
                deadline,
                session_deadline,
                false,
            ) {
                Ok(fresh) if fresh.head == remembered.head && !fresh.memoized => {
                    landed_proof = Some(fresh);
                }
                Ok(_) => {
                    return Ok(keep_with(
                        process_table,
                        AbandonmentKeepReason::LandedDeclined,
                        AbandonedSessionCloseOutcome::LandedDeclined(
                            LandedSessionDecline::MovedBeforeClose,
                        ),
                    ));
                }
                Err(reason) => {
                    return Ok(keep_with(
                        process_table,
                        AbandonmentKeepReason::LandedDeclined,
                        AbandonedSessionCloseOutcome::LandedDeclined(reason),
                    ));
                }
            }
        }
        // Both claims below are non-blocking: a busy one declines the session
        // (`busy`) instead of waiting, so this pass never waits on a lock here.
        let _operation_claim = match crate::file_lock::try_acquire_session_operation_guard(
            self.main_root(),
            session_id,
            crate::file_lock::SessionOperationLockMode::Exclusive,
        ) {
            Ok(Some(guard)) => guard,
            Ok(None) => {
                return Ok(keep_with(
                    process_table,
                    AbandonmentKeepReason::OperationGuardBusy,
                    AbandonedSessionCloseOutcome::OperationGuardBusy,
                ));
            }
            Err(_) => {
                return Ok(keep_with(
                    process_table,
                    AbandonmentKeepReason::OperationGuardUnavailable,
                    AbandonedSessionCloseOutcome::OperationEvidenceUnavailable,
                ));
            }
        };
        if candidate.pid.is_some_and(crate::broker::session_pid_alive) {
            return Ok(keep_with(
                process_table,
                AbandonmentKeepReason::SessionPidAlive,
                AbandonedSessionCloseOutcome::ActiveProcess,
            ));
        }

        // A maintenance pass must keep moving when another lifecycle operation
        // is active; status callers also leave the row open and retry later.
        let Some(_lifecycle_claim) = self.try_claim_worktree_lifecycle("abandoned_close")? else {
            return Ok(keep_with(
                process_table,
                AbandonmentKeepReason::LifecycleBusy,
                AbandonedSessionCloseOutcome::LifecycleBusy,
            ));
        };

        let current = match self.store_ref().session(session_id) {
            Ok(session) => session,
            Err(BrokerError::SessionNotFound(_)) => {
                return Ok(keep_with(
                    process_table,
                    AbandonmentKeepReason::Changed,
                    AbandonedSessionCloseOutcome::Changed,
                ));
            }
            Err(error) => return Err(error.into()),
        };
        if current.cleanup_state != crate::SessionCleanupState::Open || current.status.is_closed() {
            return Ok(keep_with(
                process_table,
                AbandonmentKeepReason::Changed,
                AbandonedSessionCloseOutcome::Changed,
            ));
        }
        // A running `broker start` (a `--reuse` of this checkout included)
        // reserves the path before it records anything: the checkout is in
        // use, and closing it now would race that start.
        if crate::start_reservation::path_is_reserved(
            self.main_root(),
            std::path::Path::new(&current.worktree_path),
        ) {
            return Ok(keep_with(
                process_table,
                AbandonmentKeepReason::StartReserved,
                AbandonedSessionCloseOutcome::OperationActive,
            ));
        }
        let fs_activity = match checked_worktree_activity_ms(self.main_root(), &current) {
            Ok(activity) => activity.unwrap_or(0),
            Err(_) => {
                return Ok(keep_with(
                    process_table,
                    AbandonmentKeepReason::WorktreeActivityUnavailable,
                    AbandonedSessionCloseOutcome::WorktreeActivityUnavailable,
                ));
            }
        };
        let claimed_fs_activity = fs_activity;
        let activity_at = fs_activity.max(current.last_activity_at).max(merged_at);
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
            process_table.note_not_due_before(
                activity_at
                    .max(current.created_at)
                    .saturating_add(abandon_after_ms),
            );
            return Ok(keep_with(
                process_table,
                AbandonmentKeepReason::RecentActivity,
                AbandonedSessionCloseOutcome::NotDue,
            ));
        };

        // Recheck the spawned PID after taking the lifecycle claim. PID reuse
        // can only make this conservative: any occupant keeps the session open.
        if current.pid.is_some_and(crate::broker::session_pid_alive) {
            return Ok(keep_with(
                process_table,
                AbandonmentKeepReason::SessionPidAlive,
                AbandonedSessionCloseOutcome::ActiveProcess,
            ));
        }
        let current_holder_event = self.store_ref().latest_session_holder_event(session_id)?;
        if current_holder_event.as_ref().map(|event| event.id) != holder.binding_event_id {
            return Ok(keep_with(
                process_table,
                AbandonmentKeepReason::Changed,
                AbandonedSessionCloseOutcome::Changed,
            ));
        }
        if live_task_holder.is_some()
            && !current_holder_event.as_ref().is_some_and(|event| {
                crate::session_holder::task_lease_expired(
                    event,
                    current.last_activity_at,
                    observed_at,
                ) == Some(true)
            })
        {
            return Ok(keep_with(
                process_table,
                AbandonmentKeepReason::TaskLeaseCurrent,
                AbandonedSessionCloseOutcome::ActiveProcess,
            ));
        }

        // A versioned task binding can outlive its broker parent: inspect the
        // native child protocols while holding both the per-session operation
        // claim and repository lifecycle claim. Unknown inventories never
        // authorize closure. Legacy gone/unbound sessions keep their existing
        // behavior and avoid a system-wide lsof scan.
        let mut _artifact_use_reclaimer = None;
        if task_lease_binding_present || merged_lane || no_work_lane || moved_on_to.is_some() {
            if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                return Ok(keep_with(
                    process_table,
                    AbandonmentKeepReason::DeadlineReached,
                    AbandonedSessionCloseOutcome::OperationEvidenceUnavailable,
                ));
            }
            let gates = crate::gates::checked_running_session_gates_until(
                self.main_root(),
                session_id,
                || deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline),
                256,
            );
            let Some(gates) = gates else {
                return Ok(keep_with(
                    process_table,
                    AbandonmentKeepReason::GatesUnknown,
                    AbandonedSessionCloseOutcome::OperationEvidenceUnavailable,
                ));
            };
            if !gates.is_empty() {
                return Ok(keep_with(
                    process_table,
                    AbandonmentKeepReason::GateRunning,
                    AbandonedSessionCloseOutcome::OperationActive,
                ));
            }
            match self.session_operations_unresolved_since_boot(session_id) {
                Ok(true) => {
                    return Ok(keep_with(
                        process_table,
                        AbandonmentKeepReason::OperationUnresolved,
                        AbandonedSessionCloseOutcome::OperationActive,
                    ));
                }
                Ok(false) => {}
                Err(_) => {
                    return Ok(keep_with(
                        process_table,
                        AbandonmentKeepReason::OperationsUnknown,
                        AbandonedSessionCloseOutcome::OperationEvidenceUnavailable,
                    ));
                }
            }
            // A submission still being simulated, gated or awaiting promotion
            // is work in flight for this session.
            if merged_lane || no_work_lane || moved_on_to.is_some() {
                match self.store_ref().latest_merge_queue_for_session(session_id) {
                    Ok(Some(entry))
                        if matches!(
                            entry.status,
                            crate::MergeStatus::Submitted
                                | crate::MergeStatus::Simulating
                                | crate::MergeStatus::Verified
                        ) =>
                    {
                        return Ok(keep_with(
                            process_table,
                            AbandonmentKeepReason::MergeQueueActive,
                            AbandonedSessionCloseOutcome::OperationActive,
                        ));
                    }
                    Ok(_) => {}
                    Err(_) => {
                        return Ok(keep_with(
                            process_table,
                            AbandonmentKeepReason::MergeQueueUnknown,
                            AbandonedSessionCloseOutcome::OperationEvidenceUnavailable,
                        ));
                    }
                }
            }
            match self.preparation_status(session_id) {
                Ok(status) if status.state == crate::PreparationState::InProgress => {
                    return Ok(keep_with(
                        process_table,
                        AbandonmentKeepReason::PreparationInProgress,
                        AbandonedSessionCloseOutcome::OperationActive,
                    ));
                }
                Ok(status) if status.state == crate::PreparationState::Invalid => {
                    return Ok(keep_with(
                        process_table,
                        AbandonmentKeepReason::PreparationInvalid,
                        AbandonedSessionCloseOutcome::OperationEvidenceUnavailable,
                    ));
                }
                Ok(_) => {}
                Err(_) => {
                    return Ok(keep_with(
                        process_table,
                        AbandonmentKeepReason::PreparationUnknown,
                        AbandonedSessionCloseOutcome::OperationEvidenceUnavailable,
                    ));
                }
            }
            let worktree = std::path::Path::new(&current.worktree_path);
            match std::fs::symlink_metadata(worktree) {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
                _ => {
                    return Ok(keep_with(
                        process_table,
                        AbandonmentKeepReason::CheckoutNotDirectory,
                        AbandonedSessionCloseOutcome::OperationEvidenceUnavailable,
                    ));
                }
            }
            match crate::file_lock::artifact_use::try_artifact_use_reclaimer(
                self.main_root(),
                worktree,
            ) {
                Ok(crate::file_lock::artifact_use::ArtifactUseReclaimTry::Acquired(reclaimer)) => {
                    _artifact_use_reclaimer = Some(reclaimer);
                }
                Ok(crate::file_lock::artifact_use::ArtifactUseReclaimTry::Blocked(
                    crate::file_lock::artifact_use::ArtifactUseReclaimBlock::UnknownProducer {
                        legacy_device_renumbered,
                        ..
                    },
                )) if legacy_device_renumbered > 0 => {
                    return Ok(keep_with(
                        process_table,
                        AbandonmentKeepReason::LegacyArtifactMarkers,
                        AbandonedSessionCloseOutcome::LegacyArtifactMarkers(
                            legacy_device_renumbered,
                        ),
                    ));
                }
                Ok(crate::file_lock::artifact_use::ArtifactUseReclaimTry::Blocked(
                    crate::file_lock::artifact_use::ArtifactUseReclaimBlock::Contended
                    | crate::file_lock::artifact_use::ArtifactUseReclaimBlock::UnknownProducer {
                        ..
                    },
                )) => {
                    return Ok(keep_with(
                        process_table,
                        AbandonmentKeepReason::ArtifactProducerActive,
                        AbandonedSessionCloseOutcome::OperationActive,
                    ));
                }
                Ok(crate::file_lock::artifact_use::ArtifactUseReclaimTry::Blocked(
                    crate::file_lock::artifact_use::ArtifactUseReclaimBlock::ProtocolUnavailable,
                ))
                | Err(_) => {
                    return Ok(keep_with(
                        process_table,
                        AbandonmentKeepReason::ArtifactProtocolUnavailable,
                        AbandonedSessionCloseOutcome::OperationEvidenceUnavailable,
                    ));
                }
            }
            if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                return Ok(keep_with(
                    process_table,
                    AbandonmentKeepReason::DeadlineReached,
                    AbandonedSessionCloseOutcome::OperationEvidenceUnavailable,
                ));
            }
        }
        // A live task holder is excused only after this open-file check. The
        // merged-pull-request lane runs it for every holder, a legacy binding
        // included: two quiet hours alone do not show nobody works there.
        // The opt-in abandonment lane tolerates the live holder's own working
        // directory; the landed lane does not: an agent sitting in its
        // worktree is using it.
        let exempt_holder = if require_landed || moved_on_to.is_some() {
            None
        } else {
            live_task_holder.as_ref().map(|agent| agent.pid)
        };
        // An `Unbound` session (no holder recorded, or a shared agent service)
        // has no process to observe, so the abandonment window requires the
        // complete open-file and working-directory observation before it
        // closes one.
        let process_use_checked = task_lease_binding_present
            || merged_lane
            || no_work_lane
            || moved_on_to.is_some()
            || (lane == SessionCloseLane::Abandonment
                && holder.liveness == crate::session_holder::RecordedHolderLiveness::Unbound);
        // For a moved-on holder a process in the checkout is the holder (or
        // anything) still using it.
        let in_use = |outcome: AbandonedSessionCloseOutcome| match outcome {
            AbandonedSessionCloseOutcome::OperationActive
                if moved_on_to.is_some() && require_landed =>
            {
                AbandonedSessionCloseOutcome::LandedDeclined(LandedSessionDecline::HolderAliveInUse)
            }
            other => other,
        };
        if process_use_checked
            && let Some(kept) = process_use_verdict(
                process_table,
                self.main_root(),
                std::path::Path::new(&current.worktree_path),
                deadline,
                exempt_holder,
                require_landed,
            )
        {
            return Ok(in_use(kept));
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
        if no_work_lane {
            if !explicit_leases.is_empty() {
                return Ok(keep_with(
                    process_table,
                    AbandonmentKeepReason::NoWorkExplicitClaim,
                    AbandonedSessionCloseOutcome::LeaseHeld,
                ));
            }
            // The holder must have been gone for the stale grace, counted
            // from the persisted first sighting (or the boot, for a holder
            // that started before it). The first sighting is persisted here,
            // so a later pass can see the grace end.
            let (Some(binding_event_id), Some(holder_process)) = (
                holder.binding_event_id,
                crate::session_holder::recorded_holder(self.store_ref(), session_id)?,
            ) else {
                return Ok(keep_with(
                    process_table,
                    AbandonmentKeepReason::NoWorkHolderUnbound,
                    AbandonedSessionCloseOutcome::NotDue,
                ));
            };
            // A holder that moved on is not gone; the open-file and
            // working-directory observation below decides instead.
            if moved_on_to.is_none() {
                let table = process_table.table_for(session_id, binding_event_id);
                match crate::lease_liveness::record_gone_holder_for_abandoned_session(
                    self.store(),
                    session_id,
                    binding_event_id,
                    &holder_process,
                    table,
                    deadline,
                )? {
                    crate::lease_liveness::GoneHolderWrite::Skipped
                    | crate::lease_liveness::GoneHolderWrite::Recorded => {}
                    crate::lease_liveness::GoneHolderWrite::DatabaseBusy => {
                        return Ok(keep_with(
                            process_table,
                            AbandonmentKeepReason::DatabaseBusy,
                            AbandonedSessionCloseOutcome::DatabaseBusy,
                        ));
                    }
                }
                let table = process_table.table_for(session_id, binding_event_id);
                let grace_ms = crate::lease_liveness::LeaseLivenessPolicy::load(self.main_root())
                    .stale_grace_ms;
                let gone_past_grace = matches!(
                    crate::lease_liveness::observe_holder(self.store_ref(), session_id, table, observed_at),
                    Ok((crate::lease_liveness::HolderObservation::Gone { since }, _))
                        if since.saturating_add(grace_ms) <= observed_at
                );
                if !gone_past_grace {
                    return Ok(keep_with(
                        process_table,
                        AbandonmentKeepReason::NoWorkHolderGoneRecently,
                        AbandonedSessionCloseOutcome::NotDue,
                    ));
                }
            }
        }
        if !explicit_leases.is_empty() {
            let bound_holder =
                crate::session_holder::recorded_holder(self.store_ref(), session_id)?;
            let shared_holder = bound_holder.as_ref().is_some_and(|holder| {
                crate::session_holder::is_shared_agent_service(&holder.command)
            });
            let lease_table = holder.binding_event_id.and_then(|event_id| {
                if shared_holder {
                    process_table.refresh_for_binding(session_id, event_id, allow_process_snapshot)
                } else {
                    process_table.table_for(session_id, event_id)
                }
            });
            if let (Some(binding_event_id), Some(holder_process)) =
                (holder.binding_event_id, bound_holder.as_ref())
            {
                match crate::lease_liveness::record_gone_holder_for_abandoned_session(
                    self.store(),
                    session_id,
                    binding_event_id,
                    holder_process,
                    lease_table,
                    deadline,
                )? {
                    crate::lease_liveness::GoneHolderWrite::Skipped
                    | crate::lease_liveness::GoneHolderWrite::Recorded => {}
                    crate::lease_liveness::GoneHolderWrite::DatabaseBusy => {
                        return Ok(keep_with(
                            process_table,
                            AbandonmentKeepReason::DatabaseBusy,
                            AbandonedSessionCloseOutcome::DatabaseBusy,
                        ));
                    }
                }
            }
            let lease_policy = crate::lease_liveness::LeaseLivenessPolicy::load(self.main_root());
            let liveness_result = match live_task_holder.as_ref().and_then(|_| {
                current_holder_event.as_ref().and_then(|event| {
                    crate::session_holder::task_lease(event).map(|lease| {
                        event
                            .ts
                            .max(current.last_activity_at)
                            .saturating_add(lease.duration_ms)
                    })
                })
            }) {
                Some(expired_at) => {
                    crate::lease_liveness::assess_for_abandonment_with_task_lease_expiry(
                        self.store_ref(),
                        &explicit_leases,
                        lease_table,
                        observed_at,
                        lease_policy,
                        session_id,
                        expired_at,
                    )
                }
                None => crate::lease_liveness::assess_for_abandonment(
                    self.store_ref(),
                    &explicit_leases,
                    lease_table,
                    observed_at,
                    lease_policy,
                ),
            };
            let liveness = match liveness_result {
                Ok(liveness) => liveness,
                Err(_) => {
                    return Ok(keep_with(
                        process_table,
                        AbandonmentKeepReason::LeaseUnavailable,
                        AbandonedSessionCloseOutcome::LeaseUnavailable,
                    ));
                }
            };
            // A legacy session, created before the broker recorded holders,
            // has no process to observe, so its claims would hold forever.
            // Age is the only evidence it can have: past the abandonment
            // window (judged above), with no unresolved operation, no
            // preparation in progress and no process using its checkout, its
            // claims stop holding. A session merely without a holder record
            // today (an unidentified caller: a person, an unrecognised agent,
            // a sandboxed start) is not legacy and keeps its claims. Every
            // other basis keeps its own rule.
            let legacy_unbound = holder.binding_event_id.is_none()
                && bound_holder.is_none()
                && current.created_at < HOLDER_RECORDING_SHIPPED_MS
                && matches!(
                    self.store_ref()
                        .latest_session_holder_gone_event(session_id),
                    Ok(None)
                );
            let legacy_claims_release_candidate = legacy_unbound
                && liveness.iter().any(|lease| {
                    lease.liveness_evidence.holds
                        && lease.liveness_evidence.basis == "no_recorded_holder"
                });
            if legacy_claims_release_candidate
                && let Some(kept) = process_use_verdict(
                    process_table,
                    self.main_root(),
                    std::path::Path::new(&current.worktree_path),
                    deadline,
                    None,
                    false,
                )
            {
                // A process in the checkout, or an observation that could not
                // be made, keeps the claim.
                return Ok(kept);
            }
            let legacy_claims_released = legacy_claims_release_candidate
                && matches!(
                    self.session_operations_unresolved_since_boot(session_id),
                    Ok(false)
                )
                && matches!(
                    self.preparation_status(session_id),
                    Ok(status) if !matches!(
                        status.state,
                        crate::PreparationState::InProgress | crate::PreparationState::Invalid
                    )
                );
            if legacy_claims_released {
                crate::phase_telemetry::add(
                    "legacy_unbound_claims_released",
                    std::time::Duration::ZERO,
                );
            }
            if liveness.iter().any(|lease| {
                lease.liveness_evidence.holds
                    && !(legacy_claims_released
                        && lease.liveness_evidence.basis == "no_recorded_holder")
            }) {
                return Ok(keep_with(
                    process_table,
                    AbandonmentKeepReason::LeaseHeld,
                    AbandonedSessionCloseOutcome::LeaseHeld,
                ));
            }
        }
        if self
            .store_ref()
            .latest_session_holder_event(session_id)?
            .map(|event| event.id)
            != holder.binding_event_id
        {
            return Ok(keep_with(
                process_table,
                AbandonmentKeepReason::Changed,
                AbandonedSessionCloseOutcome::Changed,
            ));
        }
        // The final look is real evidence: whatever observation it returns,
        // fresh or re-taken, is checked for processes in the checkout again.
        // (It used to check only that one existed, so a re-taken observation
        // that showed new work there was never read.)
        if process_use_checked {
            if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                return Ok(keep_with(
                    process_table,
                    AbandonmentKeepReason::DeadlineReached,
                    AbandonedSessionCloseOutcome::OperationEvidenceUnavailable,
                ));
            }
            if let Some(kept) = process_use_verdict(
                process_table,
                self.main_root(),
                std::path::Path::new(&current.worktree_path),
                deadline,
                exempt_holder,
                require_landed,
            ) {
                return Ok(in_use(kept));
            }
        }

        // Retention policy can be edited while the process and filesystem
        // witnesses are collected. Re-read it immediately before the close
        // CAS so disabling the opt-in revokes age-expiry authority at once.
        if live_task_holder.is_some() {
            let policy = match crate::load_retention_policy(&self.main_root) {
                Ok(policy) => policy,
                Err(_) => {
                    return Ok(keep_with(
                        process_table,
                        AbandonmentKeepReason::PolicyUnavailable,
                        AbandonedSessionCloseOutcome::OperationEvidenceUnavailable,
                    ));
                }
            };
            if !(policy.expire_abandoned_task_leases || require_landed)
                || !current_holder_event.as_ref().is_some_and(|event| {
                    crate::session_holder::task_lease_expired(
                        event,
                        current.last_activity_at,
                        observed_at,
                    ) == Some(true)
                })
            {
                return Ok(keep_with(
                    process_table,
                    AbandonmentKeepReason::TaskLeaseCurrent,
                    AbandonedSessionCloseOutcome::ActiveProcess,
                ));
            }
        }

        // A moved-on holder is asked again under the claims: the same later
        // binding elsewhere, after this session's current activity and its
        // own latest binding. What the checkout holds is left to the work
        // class, read again below.
        if moved_on_to.is_some() {
            let still_moved_on = match current_holder_event.as_ref() {
                Some(event) => self.holder_moved_on(
                    session_id,
                    event,
                    claimed_fs_activity.max(current.last_activity_at),
                )?,
                None => None,
            };
            if still_moved_on.is_none() {
                return Ok(keep_with(
                    process_table,
                    AbandonmentKeepReason::Changed,
                    AbandonedSessionCloseOutcome::Changed,
                ));
            }
        }

        // Recheck delivery and local changes while lifecycle and operation
        // guards are held. Unknown evidence preserves the session.
        if require_landed {
            let Ok(policy) = crate::load_retention_policy(self.main_root()) else {
                return Ok(keep_with(
                    process_table,
                    AbandonmentKeepReason::PolicyUnavailable,
                    AbandonedSessionCloseOutcome::OperationEvidenceUnavailable,
                ));
            };
            let configured_hours = if merged_lane {
                policy.session_merged_after_hours
            } else {
                policy.session_landed_after_hours
            };
            let configured_ms = i64::from(configured_hours).saturating_mul(3_600_000);
            if policy.session_abandoned_after_hours == 0
                || configured_ms <= 0
                || idle_ms < configured_ms
            {
                return Ok(keep_with(
                    process_table,
                    AbandonmentKeepReason::Changed,
                    AbandonedSessionCloseOutcome::Changed,
                ));
            }
            let Some(proof) = landed_proof.as_ref() else {
                return Ok(keep_with(
                    process_table,
                    AbandonmentKeepReason::Changed,
                    AbandonedSessionCloseOutcome::Changed,
                ));
            };
            if let Some(reason) =
                self.landed_proof_still_holds(&current, proof, deadline, session_deadline)
            {
                return Ok(keep_with(
                    process_table,
                    AbandonmentKeepReason::LandedDeclined,
                    AbandonedSessionCloseOutcome::LandedDeclined(reason),
                ));
            }
            // A memoised proof was replaced by a fresh search before the
            // claims were taken (see above); a close never rests on the memo.
            if proof.memoized {
                return Ok(keep_with(
                    process_table,
                    AbandonmentKeepReason::Changed,
                    AbandonedSessionCloseOutcome::Changed,
                ));
            }
            // Last look before the state transition: a commit or any index
            // write since the proof means the agent is still working here.
            #[cfg(test)]
            process_table.run_before_landed_close_for_test();
            let worktree = std::path::Path::new(&current.worktree_path);
            let head_now = current_checkout_head(worktree);
            let activity_now = checked_worktree_activity_ms(self.main_root(), &current)
                .ok()
                .map(|activity| activity.unwrap_or(0));
            if head_now.as_deref() != Some(proof.head.as_str())
                || activity_now != Some(claimed_fs_activity)
            {
                return Ok(keep_with(
                    process_table,
                    AbandonmentKeepReason::LandedDeclined,
                    AbandonedSessionCloseOutcome::LandedDeclined(
                        LandedSessionDecline::MovedBeforeClose,
                    ),
                ));
            }
            // The pull requests are read again under the claims, last: a new pull
            // request, a reopened one or a later merge changes the evidence.
            if let Some((merges, _)) = merged_evidence.as_ref() {
                match self.merged_pull_request_evidence(&current) {
                    Ok((current_merges, _)) if current_merges == *merges => {}
                    Ok(_) => {
                        return Ok(keep_with(
                            process_table,
                            AbandonmentKeepReason::Changed,
                            AbandonedSessionCloseOutcome::Changed,
                        ));
                    }
                    Err(reason) => {
                        return Ok(keep_with(
                            process_table,
                            AbandonmentKeepReason::LandedDeclined,
                            AbandonedSessionCloseOutcome::LandedDeclined(reason),
                        ));
                    }
                }
            }
            // A file written since the proof is work too: the shared
            // classifier looks once more, after every other check.
            let Ok(policy) = crate::auto_cleanup::AutoCleanupPolicy::load(self.main_root()) else {
                return Ok(keep_with(
                    process_table,
                    AbandonmentKeepReason::LandedDeclined,
                    AbandonedSessionCloseOutcome::LandedDeclined(
                        LandedSessionDecline::PolicyUnavailable,
                    ),
                ));
            };
            let last_look = {
                let _git_deadline = session_deadline.map(crate::git::limit_git_until);
                crate::auto_cleanup::uncommitted_reason(
                    worktree,
                    Some(current.branch.as_str()),
                    self.main_root(),
                    &policy,
                )
            };
            let changed = match last_look {
                Ok(None) => None,
                Ok(Some(_)) => Some(LandedSessionDecline::MovedBeforeClose),
                // A listing that failed or was stopped is no change by the
                // agent: it keeps the session under its own name.
                Err(_) => Some(stopped_listing_decline(
                    deadline,
                    session_deadline,
                    LandedSessionDecline::WorktreeUnavailable,
                )),
            };
            if let Some(reason) = changed {
                return Ok(keep_with(
                    process_table,
                    AbandonmentKeepReason::LandedDeclined,
                    AbandonedSessionCloseOutcome::LandedDeclined(reason),
                ));
            }
        }
        // The no-work conditions are read last, under both claims.
        if no_work_lane && let Some(reason) = self.no_work_decline(&current) {
            return Ok(keep_with(
                process_table,
                reason,
                AbandonedSessionCloseOutcome::NotDue,
            ));
        }
        // The work class is read again under both claims: a push, a merge or
        // new uncommitted work since the first look changes its window.
        let stale_class = if lane == SessionCloseLane::Abandonment {
            match self.stale_class_gate(&current, activity_at, observed_at, process_table, true) {
                Ok(class) => Some(class),
                Err(kept) => return Ok(kept),
            }
        } else {
            None
        };
        let pid_alive = current.pid.map(crate::broker::session_pid_alive);
        let holder_evidence = if live_task_holder.is_some() {
            "lease_expired"
        } else if moved_on_to.is_some() {
            "moved_on"
        } else if holder.liveness == crate::session_holder::RecordedHolderLiveness::Gone {
            "gone"
        } else {
            "unbound"
        };
        let abandoned_payload = match (landed_proof.as_ref(), merged_evidence.as_ref()) {
            (Some(proof), merged) => {
                let merges = match merged {
                    Some((merges, _)) => merges.clone(),
                    None => crate::session_auto_close::PullRequestMerges::read(
                        self.store_ref(),
                        session_id,
                    )
                    .unwrap_or_default(),
                };
                crate::session_auto_close::landed_close_payload(
                    if merged.is_some() {
                        crate::session_auto_close::CLOSE_REASON_MERGED_PULL_REQUEST
                    } else {
                        crate::session_auto_close::CLOSE_REASON_LANDED
                    },
                    idle_ms,
                    abandon_after_ms,
                    pid_alive,
                    if live_task_holder.is_some() {
                        "clean_landing_and_expired_task_lease"
                    } else {
                        "clean_recorded_landing_and_gone_holder"
                    },
                    proof,
                    &merges,
                    holder_evidence,
                    moved_on_to,
                )
            }
            (None, _) => {
                // Evidence only: whether the closed session still holds
                // commits the default branch lacks. Closing deletes nothing
                // either way; automatic removal keeps such a checkout as
                // `not_contained` until the work lands or a person decides.
                let head = current_checkout_head(std::path::Path::new(&current.worktree_path));
                let unlanded_commits = head.as_deref().and_then(|head| {
                    let (_, upstream) = self.repo_handle().upstream_default()?;
                    self.repo_handle()
                        .is_ancestor_checked(head, &upstream)
                        .ok()
                        .map(|landed| !landed)
                });
                let payload = crate::session_auto_close::abandonment_close_payload(
                    &crate::events::session_abandoned_payload(idle_ms, abandon_after_ms, pid_alive),
                    crate::session_auto_close::PullRequestMerges::read(
                        self.store_ref(),
                        session_id,
                    )
                    .as_ref(),
                    head.as_deref(),
                    unlanded_commits,
                    holder_evidence,
                    moved_on_to,
                );
                if no_work_lane {
                    crate::session_auto_close::with_close_reason(
                        &payload,
                        crate::session_auto_close::CLOSE_REASON_INACTIVE_NO_WORK,
                    )
                } else if let Some(class) = stale_class {
                    crate::session_auto_close::with_work_class(&payload, class.code())
                } else {
                    payload
                }
            }
        };
        let cleanup_request = self.checkout_cleanup_request_policy(&current);
        let _close_span = crate::phase_telemetry::span("session_close_cas");
        match self
            .store()
            .abandon_session_if_unchanged_with_cleanup_request(
                &current,
                holder.binding_event_id,
                &leases,
                observed_at,
                &abandoned_payload,
                cleanup_request.map(|request| request.grace_hours),
                cleanup_request.is_some_and(|request| request.compact_checkout),
            ) {
            Ok(true) => {
                self.settle_dead_receipts_after_close(session_id);
                // The checkout is retired by the worker's retirement lane:
                // archived when needed, renamed into the trash, deleted.
                self.request_checkout_retirement_run();
                crate::phase_telemetry::add(
                    match holder_evidence {
                        "lease_expired" => "session_closed_holder_lease_expired",
                        "moved_on" => "session_closed_holder_moved_on",
                        "gone" => "session_closed_holder_gone",
                        _ => "session_closed_holder_unbound",
                    },
                    std::time::Duration::ZERO,
                );
                // Tell the agent, should it return, why and how to continue.
                // The close stands whether or not the note is written.
                crate::warn_unrecorded(
                    "leave an automatic-close note",
                    self.store()
                        .record_automation_note(
                            crate::session_auto_close::NOTE_SENDER,
                            session_id,
                            &crate::session_auto_close::close_note(
                                session_id,
                                &current.worktree_path,
                                &abandoned_payload,
                            ),
                            Some(crate::session_auto_close::NOTE_FINGERPRINT),
                        )
                        .map(|_| ()),
                );
                Ok(AbandonedSessionCloseOutcome::Closed)
            }
            Ok(false) => Ok(keep_with(
                process_table,
                AbandonmentKeepReason::Changed,
                AbandonedSessionCloseOutcome::Changed,
            )),
            Err(BrokerError::SessionRetirementDatabaseBusy) => Ok(keep_with(
                process_table,
                AbandonmentKeepReason::DatabaseBusy,
                AbandonedSessionCloseOutcome::DatabaseBusy,
            )),
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
            let mut report = crate::resource_pressure::Report::over_budget(count);
            report.preparation_rotation_recovery = self.preparation_rotation_recoveries();
            return Ok(report);
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
        let mut report = crate::resource_pressure::Report::capture(owners);
        report.preparation_rotation_recovery = self.preparation_rotation_recoveries();
        Ok(report)
    }

    /// Derive current liveness without persisting status transitions. This is
    /// deliberately allowed to inspect PIDs and worktree metadata, but never
    /// changes a session row or appends an event.
    pub fn agents_snapshot(&self, now_ms: i64) -> Result<Vec<AgentView>, BrokerOpError> {
        self.agent_views(self.store.live_sessions()?, now_ms)
    }

    pub(super) fn agent_views(
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
        let _span = crate::phase_telemetry::span("lease_refresh");
        use crate::leases::{LeaseIgnoreRules, detect_overlaps};

        let rules = LeaseIgnoreRules::load(&self.main_root);
        // The baseline refs are the same for every session; resolving them
        // inside the loop cost git subprocesses per live session.
        let refs = self.baseline_refs();
        if let Some(preparation) = preparation {
            preparation.check()?;
        }
        let mut coordinating = self.coordinating_session_ids()?;
        if let Some(preparation) = preparation {
            preparation.check()?;
        }
        coordinating.extend(acting);
        let mut pending_implicit_leases = Vec::new();
        if preparation.is_none() {
            // Each scan stats one session's whole working tree, and the
            // sessions share nothing, so they are scanned side by side. One
            // after another, 32 sessions took 6.9 s of every status refresh
            // in a 300-session repository on 2026-11-11; four at a time
            // halved it. A lease claim keeps the serial loop below because
            // it must stop at its deadline between sessions.
            let sessions = self
                .store
                .live_sessions()?
                .into_iter()
                .filter(|session| coordinating.contains(&session.id))
                .collect::<Vec<_>>();
            let _span = crate::phase_telemetry::span("implicit_lease_scan_all");
            let scanned = crate::worktree_report::inspect_in_parallel(&sessions, |session| {
                implicit_lease_paths(session, &rules, &refs)
            });
            pending_implicit_leases.extend(
                sessions
                    .iter()
                    .zip(scanned)
                    .filter_map(|(session, paths)| Some((session.id, paths?))),
            );
        }
        for session in self.store.live_sessions()? {
            let Some(preparation) = preparation else {
                break;
            };
            preparation.check()?;
            if !coordinating.contains(&session.id) {
                continue;
            }
            let Some(paths) = implicit_lease_paths(&session, &rules, &refs) else {
                // `baseline_refs`, baseline and
                // changed-file helpers deliberately hide ordinary Git
                // failures behind `Option`. A timeout is different: a
                // lease claim must stop rather than treating that
                // incomplete scan as an empty or absent worktree.
                preparation.check()?;
                continue;
            };
            preparation.check()?;
            pending_implicit_leases.push((session.id, paths));
        }
        if preparation.is_none() {
            let _span = crate::phase_telemetry::span("implicit_lease_store");
            self.store
                .set_implicit_leases_batch(&pending_implicit_leases)?;
        } else if let Some(preparation) = preparation {
            preparation.check()?;
            for (session_id, paths) in pending_implicit_leases {
                preparation.check()?;
                self.store.set_implicit_leases(session_id, &paths)?;
            }
        }

        let after = {
            let _span = crate::phase_telemetry::span("lease_overlap_detection");
            detect_overlaps(&self.coordinating_leases(&coordinating)?)
        };
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

    /// Claim an explicit write lease. Another session's lease never refuses
    /// the claim: every overlapping lease of a live session, and every
    /// explicit lease, is returned as a warning with how to coordinate;
    /// leases implied by stale sessions' old edits are omitted.
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
        // Holder liveness only decides between overlapping leases, and working
        // it out visits every live session, so a claim nobody overlaps skips it.
        if self.lease_blockers(session_id, &path)?.is_empty() {
            self.store.claim_lease(session_id, &path, ttl_ms)?;
            return Ok(LeaseClaimReport {
                session_id,
                path,
                accepted: true,
                blockers: Vec::new(),
                warnings: Vec::new(),
            });
        }
        let holders = self.lease_holders()?;
        preparation.check()?;
        let mut warnings = Vec::new();
        // Read after the holders: assessing liveness updates the holder status
        // these rows report.
        for mut lease in self.lease_blockers(session_id, &path)? {
            if lease.kind == LeaseKind::Implicit && !holders.is_live(lease.session_id) {
                continue;
            }
            if let Some(status) = holders.status_of(lease.session_id) {
                lease.holder_status = Some(status.as_str().to_string());
            }
            lease.reason = Some(holders.overlap_reason(session_id, &lease));
            warnings.push(lease);
        }
        preparation.check()?;
        self.attach_overlap_changes(session_id, std::slice::from_ref(&path), &mut warnings);
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

    /// Each live session's derived status right now.
    pub(crate) fn lease_holders(&mut self) -> Result<LeaseHolders, BrokerOpError> {
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
        // A holder gone past its stale grace is no longer live (#360); its
        // leases are still listed.
        let mut live = live;
        for session_id in self.sessions_released_by_grace()? {
            live.remove(&session_id);
        }
        Ok(LeaseHolders::new(live))
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
        let paths_by_holder = index_active_lease_paths_by_holder(
            leases
                .iter()
                .map(|lease| (lease.session_id, lease.path.as_str())),
        );
        crate::lease_requests::requests(&self.store, |holder, path| {
            holder_has_overlapping_active_lease(&paths_by_holder, holder, path)
        })
    }

    /// Pending requests for routine status, excluding historical resolved work.
    pub(crate) fn pending_lease_release_requests(
        &self,
    ) -> Result<Vec<crate::lease_requests::LeaseReleaseRequest>, BrokerOpError> {
        let leases = self.store.active_leases()?;
        let paths_by_holder = index_active_lease_paths_by_holder(
            leases
                .iter()
                .map(|lease| (lease.session_id, lease.path.as_str())),
        );
        crate::lease_requests::pending_requests(&self.store, |holder, path| {
            holder_has_overlapping_active_lease(&paths_by_holder, holder, path)
        })
    }

    /// Pending release requests involving one session.
    pub(crate) fn session_pending_lease_release_requests(
        &self,
        session_id: i64,
    ) -> Result<Vec<crate::lease_requests::LeaseReleaseRequest>, BrokerOpError> {
        let leases = self
            .store
            .active_leases_for_release_session(session_id, now_ms())?;
        let paths_by_holder = index_active_lease_paths_by_holder(
            leases
                .iter()
                .map(|lease| (lease.session_id, lease.path.as_str())),
        );
        crate::lease_requests::pending_requests_for_session(
            &self.store,
            session_id,
            |holder, path| holder_has_overlapping_active_lease(&paths_by_holder, holder, path),
        )
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
            ids.push(self.store.append_release_request_with_note(
                requester,
                holder,
                &crate::lease_requests::request_payload(&path, requester, holder, reason),
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
    ) -> Result<bool, BrokerOpError> {
        Ok(self.store.resolve_lease_release_request(
            request.request_id,
            request.holder_session_id,
            kind,
            reason,
        )?)
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
            if gone_past_grace
                && self.release_for_request(
                    &request,
                    crate::lease_requests::GRANTED,
                    Some("holder_stale"),
                )?
            {
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
        let liveness = crate::lease_liveness::assess(
            &self.store,
            &leases,
            crate::session_holder::ProcessTable::snapshot().as_ref(),
            now_ms(),
            crate::lease_liveness::LeaseLivenessPolicy::load(&self.main_root),
        )?;
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
                    other_changes: None,
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
            planned.push(LeasePathPlan {
                path,
                owned,
                conflicts,
                would_conflict: false,
            });
        }

        Ok(LeasePlan {
            session_id,
            would_conflict: false,
            paths: planned,
        })
    }

    /// `leases plan`: what each holder changed in the requested paths. A plan is
    /// a question, so it answers in full and records nothing as shown.
    pub fn attach_plan_changes(&self, plan: &mut LeasePlan) {
        for path in &mut plan.paths {
            let mut rows: Vec<LeaseBlocker> = path
                .conflicts
                .iter()
                .map(|overlap| LeaseBlocker {
                    session_id: overlap.session_id,
                    path: overlap.path.clone(),
                    kind: overlap.kind,
                    holder_status: None,
                    holder_context: None,
                    severity: None,
                    reason: None,
                    other_changes: None,
                })
                .collect();
            self.attach_overlap_changes(0, std::slice::from_ref(&path.path), &mut rows);
            for (overlap, row) in path.conflicts.iter_mut().zip(rows) {
                overlap.other_changes = row.other_changes;
            }
        }
    }

    /// Other sessions' leases on the paths a new session just planned. They
    /// never refuse, so this is how the agent hears of them.
    pub(super) fn planned_lease_overlaps(
        &mut self,
        session_id: i64,
        paths: &[String],
    ) -> Vec<LeaseBlocker> {
        let Ok(holders) = self.lease_holders() else {
            return Vec::new();
        };
        let mut overlaps = Vec::new();
        for path in paths {
            let Ok(blockers) = self.lease_blockers(session_id, path) else {
                continue;
            };
            for mut lease in blockers {
                if !holders.is_live(lease.session_id) {
                    continue;
                }
                lease.reason = Some(holders.overlap_reason(session_id, &lease));
                overlaps.push(lease);
            }
        }
        overlaps.sort_by(|a, b| {
            (a.session_id, a.path.as_str(), a.kind.as_str()).cmp(&(
                b.session_id,
                b.path.as_str(),
                b.kind.as_str(),
            ))
        });
        overlaps
            .dedup_by(|a, b| a.session_id == b.session_id && a.path == b.path && a.kind == b.kind);
        self.attach_overlap_changes(session_id, paths, &mut overlaps);
        overlaps
    }

    /// Put what each holder changed in the overlapping files on the first row
    /// naming that holder. `session_id` 0 has no memory of what it was shown.
    /// Never fails: see [`crate::overlap_diff`].
    pub(crate) fn attach_overlap_changes(
        &self,
        session_id: i64,
        touched: &[String],
        rows: &mut [LeaseBlocker],
    ) {
        use crate::overlap_diff as diff;
        if rows.is_empty() || !diff::enabled(&self.main_root) {
            return;
        }
        let mut order: Vec<i64> = Vec::new();
        for row in rows.iter() {
            if !order.contains(&row.session_id) {
                order.push(row.session_id);
            }
        }
        let mut read = Vec::new();
        let mut reports: Vec<diff::OverlapChanges> = Vec::new();
        for (position, holder) in order.iter().enumerate() {
            let Ok(session) = self.store.session(*holder) else {
                continue;
            };
            let holder_rows = rows.iter().filter(|row| row.session_id == *holder);
            let explicit = rows
                .iter()
                .any(|row| row.session_id == *holder && row.kind == LeaseKind::Explicit);
            let mut paths: Vec<String> = holder_rows
                .flat_map(|row| {
                    touched
                        .iter()
                        .filter(|path| crate::leases::paths_overlap(&row.path, path))
                        .map(|path| diff::narrower(&row.path, path))
                        .collect::<Vec<_>>()
                })
                .collect();
            paths.sort();
            paths.dedup();
            if paths.is_empty() {
                continue;
            }
            let request = diff::HolderRequest::new(&session, explicit, paths);
            if position < diff::max_holders() {
                read.push(request);
            } else {
                reports.push(diff::over_limit(&request));
            }
        }
        reports.extend(diff::read_all(&self.main_root, read, diff::COMMAND_BUDGET));
        let mut seen = (session_id != 0)
            .then(|| crate::agent_hook::SeenState::load(&self.main_root, session_id));
        for mut report in reports {
            if let Some(seen) = seen.as_mut() {
                diff::mark_shown(&mut report, seen);
            }
            if let Some(row) = rows
                .iter_mut()
                .find(|row| row.session_id == report.session_id)
            {
                row.other_changes = Some(report);
            }
        }
        if let Some(seen) = seen {
            seen.save();
        }
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
    /// non-ignored changed path must be owned by this session and must not
    /// be an adoption-time foreign untracked path unless explicitly claimed.
    /// Another session's overlapping lease is reported, never refused.
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
        let base = crate::merge::session_baseline(&checkout, &self.baseline_refs())
            .or(session.diff_base.clone())
            .unwrap_or_else(|| "HEAD".to_string());
        let leases = self.leases_as_refreshed(session_id)?;
        let changed = self.repo.changed_between(&base, &head)?;
        let mut audit = self.audit_ownership_paths(session_id, &changed, &leases, true)?;
        if !audit.overlapping.is_empty() {
            let live_holders = self.lease_holders_snapshot()?;
            let holders: std::collections::BTreeSet<i64> = audit
                .overlapping
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
            audit.judge_on_conflicts(session_id, &live_holders, pair_with);
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
        let refs = self.baseline_refs();
        let mut coordinating = self.coordinating_session_ids()?;
        coordinating.insert(acting);
        let mut refreshed = std::collections::BTreeMap::new();
        for session in self.store.live_sessions()? {
            if !coordinating.contains(&session.id) {
                continue;
            }
            if let Some(paths) = implicit_lease_paths(&session, &rules, &refs) {
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

    /// [`Self::lease_holders`] from a liveness snapshot, persisting no
    /// status transition.
    pub(crate) fn lease_holders_snapshot(&self) -> Result<LeaseHolders, BrokerOpError> {
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
        Ok(LeaseHolders::new(live))
    }

    pub(super) fn audit_paths(
        &mut self,
        session_id: i64,
        base: &str,
        head: &str,
        changed: Vec<String>,
        allow_implicit: bool,
        // Submit only: classify whether this session's edits conflict with
        // each overlapping holder's, and report that severity. Guarded exec
        // reports the overlap alone, like a lease claim. Neither refuses.
        classify_conflicts: bool,
    ) -> Result<OwnershipAuditReport, BrokerOpError> {
        let leases = self.store.active_leases()?;
        let mut audit =
            self.audit_ownership_paths(session_id, &changed, &leases, allow_implicit)?;
        if !audit.overlapping.is_empty() {
            let live_holders = self.lease_holders()?;
            if classify_conflicts {
                // A lease refresh pairs only sessions still working, so a stale
                // holder's pair with this session may never have been classified.
                // Classify just this session's pairs with its blockers: bounded by
                // the blockers, and the verdict tells the agent whether the stale
                // work it overlaps would actually conflict.
                let holders: std::collections::BTreeSet<i64> = audit
                    .overlapping
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
                audit.judge_on_conflicts(session_id, &live_holders, |holder| {
                    self.overlap_pair_between(&overlaps, session_id, holder)
                });
            } else {
                // Guarded exec: reported like an overlap on a claim.
                audit.report_overlaps(session_id, &live_holders);
            }
        }
        Ok(audit.into_report(session_id, base, head, changed))
    }

    /// The path checks of an ownership audit against `leases`. Another
    /// session's overlapping explicit lease is collected in `overlapping`
    /// for the caller to report.
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
                audit.overlapping.push(LeaseBlocker {
                    session_id: blocker.session_id,
                    path: blocker.path.clone(),
                    kind: blocker.kind,
                    holder_status: self.lease_holder_status(blocker.session_id),
                    holder_context: self.lease_holder_context(blocker.session_id),
                    severity: None,
                    reason: None,
                    other_changes: None,
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

        audit.overlapping.sort_by(|a, b| {
            (a.session_id, a.path.as_str(), a.kind.as_str()).cmp(&(
                b.session_id,
                b.path.as_str(),
                b.kind.as_str(),
            ))
        });
        audit
            .overlapping
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
                other_changes: None,
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
        let precheck_started = std::time::Instant::now();
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
        let precheck_ms = precheck_started.elapsed().as_millis() as u64;
        let execution_started = std::time::Instant::now();
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
        let execution_ms = execution_started.elapsed().as_millis() as u64;
        let audit_started = std::time::Instant::now();
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
        let mut warned_leases = audit.warned_leases;
        self.attach_overlap_changes(session_id, &touched, &mut warned_leases);
        let phase_timings_ms = std::collections::BTreeMap::from([
            ("precheck".to_string(), precheck_ms),
            ("command_and_supervision".to_string(), execution_ms),
            (
                "postcheck".to_string(),
                audit_started.elapsed().as_millis() as u64,
            ),
        ]);
        let report = GuardedExecReport {
            session_id,
            phase_timings_ms,
            command: command.to_vec(),
            exit_code: status.exit_code,
            command_success,
            command_timed_out: status.timed_out,
            ownership_ok: audit.ok,
            conflicting_leases: Vec::new(),
            warned_leases,
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
    refs: &crate::merge::BaselineRefs,
) -> Option<Vec<String>> {
    let _span = crate::phase_telemetry::span("implicit_lease_scan");
    let checkout = {
        let _span = crate::phase_telemetry::span("implicit_lease_discover");
        GitRepo::discover(Path::new(&session.worktree_path)).ok()?
    };
    // #41: derive the baseline instead of trusting the stored
    // adoption-time diff_base — after a conflict-rebase the stored
    // value inflates the diff with everyone else's promoted work.
    let base = {
        let _span = crate::phase_telemetry::span("implicit_lease_baseline");
        crate::merge::session_baseline(&checkout, refs)
            .or_else(|| session.diff_base.clone())
            .unwrap_or_else(|| "HEAD".to_string())
    };
    let _span = crate::phase_telemetry::span("implicit_lease_changed_files");
    let changed = crate::worktree_watch::changed_files(&checkout, &base).ok()?;
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
    /// Other sessions' explicit leases on changed paths, before they are
    /// reported in `warned`.
    overlapping: Vec<LeaseBlocker>,
    warned: Vec<LeaseBlocker>,
    foreign: Vec<String>,
}

impl PathAudit {
    /// Guarded exec: each overlap is reported as for a lease claim.
    fn report_overlaps(&mut self, session_id: i64, holders: &LeaseHolders) {
        self.warned = std::mem::take(&mut self.overlapping)
            .into_iter()
            .map(|mut blocker| {
                blocker.reason = Some(holders.overlap_reason(session_id, &blocker));
                blocker
            })
            .collect();
    }

    /// Submit: each overlap is reported with whether Git reports a conflict
    /// between the two sessions' edits on it. `pair_with` gives the
    /// classified pair of this session with a holder.
    fn judge_on_conflicts(
        &mut self,
        session_id: i64,
        holders: &LeaseHolders,
        pair_with: impl Fn(i64) -> Option<crate::OverlapPair>,
    ) {
        use crate::leases::paths_overlap;

        self.warned = std::mem::take(&mut self.overlapping)
            .into_iter()
            .map(|mut blocker| {
                let pair = pair_with(blocker.session_id);
                let conflicting = pair.as_ref().is_some_and(|pair| {
                    pair.conflicting_paths
                        .iter()
                        .any(|path| paths_overlap(&blocker.path, path))
                });
                blocker.severity = Some(if conflicting { "high" } else { "low" }.to_string());
                let holder_active =
                    holders.status_of(blocker.session_id) == Some(SessionStatus::Active);
                blocker.reason = Some(match (conflicting, holder_active) {
                    (true, true) => format!(
                        "the holder is actively working and Git reports a conflict with this \
                         session's edits; this repository delivers through pull requests \
                         (verify-only), so the conflict is resolved when one of them merges. \
                         Coordinate: aethyme broker advanced note send --session {session_id} \
                         --to-session {} --message \"<who lands the shared change first>\", \
                         or follow the shared-edit advice in aethyme broker status",
                        blocker.session_id
                    ),
                    (true, false) => {
                        "Git reports a conflict, but the holder is not actively working".into()
                    }
                    (false, _) => pair
                        .map(|pair| pair.reason)
                        .unwrap_or_else(|| "the holder has not edited this path".into()),
                });
                blocker
            })
            .collect();
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
        let ok = self.missing.is_empty() && self.foreign.is_empty();
        OwnershipAuditReport {
            session_id,
            base_commit: base.to_string(),
            head_commit: head.to_string(),
            changed_paths: changed,
            missing_lease_paths: self.missing,
            conflicting_leases: Vec::new(),
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
        // These tests judge holder, lease and running-work evidence; the
        // per-class windows (tested in `session_auto_close`) stay out of it.
        std::fs::create_dir_all(repository.path().join(".aethyme")).unwrap();
        std::fs::write(
            repository.path().join(".aethyme/config.toml"),
            "[cleanup]\nstale_landed_close_after_hours = 1\n\
             stale_pushed_close_after_hours = 1\nstale_unpushed_close_after_hours = 1\n\
        stale_closing_notice_hours = 0\n",
        )
        .unwrap();
        (repository, storage, broker)
    }

    fn age_worktree_for_task_retirement(broker: &mut Broker, session: &crate::Session) -> i64 {
        let old_time = std::time::SystemTime::now() - std::time::Duration::from_secs(80 * 60 * 60);
        let old_ms = old_time
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let worktree = Path::new(&session.worktree_path);
        let git_dir = crate::git::checked_worktree_git_dir(worktree)
            .unwrap()
            .expect("fixture worktree has Git metadata");
        for name in ["index", "HEAD"] {
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(git_dir.join(name))
                .unwrap();
            file.set_modified(old_time).unwrap();
        }
        broker
            .store()
            .connection()
            .execute(
                "UPDATE sessions SET created_at = ?1, updated_at = ?1, last_activity_at = ?1 WHERE id = ?2",
                rusqlite::params![old_ms, session.id],
            )
            .unwrap();
        old_ms
    }

    #[test]
    fn only_broker_state_files_and_never_a_working_directory_are_broker_state() {
        let state = Path::new("/repo/.aethyme");
        let open = |descriptor: &str, path: &str| crate::reclaim::OpenFileUse {
            pid: 1,
            command: "x".into(),
            descriptor: descriptor.into(),
            file_type: None,
            path: path.into(),
        };
        for ignored in [
            "/repo/.aethyme/broker.db",
            "/repo/.aethyme/broker.db-wal",
            "/repo/.aethyme/broker.db-shm",
            "/repo/.aethyme/broker.db-journal",
            "/repo/.aethyme/gc.lock",
            "/repo/.aethyme/run/worktree-lifecycle.lock",
            "/repo/.aethyme/logs/command-metrics.jsonl",
            "/repo/.aethyme/locks/session-7.lock",
        ] {
            assert!(
                super::is_broker_state_file(&open("3u", ignored), state),
                "{ignored}"
            );
        }
        for kept in [
            "/repo/.aethyme/reviews/pr-7/code/src/lib.rs",
            "/repo/.aethyme/worktrees/task/file.rs",
            "/repo/.aethyme",
            "/repo/src/lib.rs",
        ] {
            assert!(
                !super::is_broker_state_file(&open("3u", kept), state),
                "{kept}"
            );
        }
        assert!(
            !super::is_broker_state_file(&open("cwd", "/repo/.aethyme/run"), state),
            "a working directory is never broker state"
        );
    }

    #[test]
    fn landed_retirement_requires_exact_delivery_cleanliness_and_a_gone_holder() {
        let (repository, _storage, mut broker) = fixture();
        let session = broker.start_worktree("landed retirement", None).unwrap();
        let checkout = Path::new(&session.worktree_path);
        let head = crate::git::GitRepo::discover(checkout)
            .unwrap()
            .head_commit()
            .unwrap();
        git(
            repository.path(),
            &["update-ref", "refs/remotes/origin/main", &head],
        );
        git(
            repository.path(),
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        assert!(!broker.landed_session_is_safe_to_retire(&session));
        broker
            .store()
            .record_session_representation(&crate::types::NewSessionRepresentation {
                session_id: session.id,
                session_head: head.clone(),
                representing_commit: Some(head.clone()),
                representing_ref: "refs/remotes/origin/main".into(),
                discovery: crate::types::RepresentationDiscovery::MergeTime,
                pr_number: None,
                paths_json: "[]".into(),
                evidence: "fixture landed".into(),
            })
            .unwrap();
        assert!(broker.landed_session_is_safe_to_retire(&session));
        broker.store().connection().execute(
            "INSERT INTO pull_request_session_links (repository, pr_number, session_id, linked_at, link_source) VALUES ('fixture', 1, ?1, 1, 'watch')",
            [session.id],
        ).unwrap();
        assert!(!broker.landed_session_is_safe_to_retire(&session));
        broker.store().connection().execute(
            "INSERT INTO pull_request_milestones (repository, pr_number, merged_at, first_seen_at, updated_at) VALUES ('fixture', 1, 1, 1, 1)",
            [],
        ).unwrap();
        assert!(broker.landed_session_is_safe_to_retire(&session));
        std::fs::write(checkout.join("untracked-user-work"), "keep").unwrap();
        assert!(!broker.landed_session_is_safe_to_retire(&session));
        std::fs::remove_file(checkout.join("untracked-user-work")).unwrap();
        let old = age_worktree_for_task_retirement(&mut broker, &session);
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        let decision = old + 25 * 60 * 60 * 1_000;
        assert!(matches!(
            broker
                .try_close_landed_session(
                    session.id,
                    decision,
                    24 * 60 * 60 * 1_000,
                    &mut table,
                    true,
                    None
                )
                .unwrap(),
            AbandonedSessionCloseOutcome::LandedDeclined(LandedSessionDecline::HolderUnbound)
        ));
        let holder = task_lease_holder(i64::MAX);
        broker
            .store()
            .append_event(
                crate::events::SESSION_HOLDER_BOUND,
                Some(session.id),
                Some(&crate::events::session_holder_bound_payload(
                    &holder, "fixture", None,
                )),
            )
            .unwrap();
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        assert!(matches!(
            broker
                .try_close_landed_session(
                    session.id,
                    decision,
                    24 * 60 * 60 * 1_000,
                    &mut table,
                    true,
                    None
                )
                .unwrap(),
            AbandonedSessionCloseOutcome::Closed
        ));
        std::fs::write(checkout.join("later-work"), "later").unwrap();
        git(checkout, &["add", "later-work"]);
        git(checkout, &["commit", "-qm", "new session work"]);
        assert!(!broker.landed_session_is_safe_to_retire(&session));
    }

    fn task_lease_holder(pid: i64) -> crate::session_holder::AgentProcess {
        crate::session_holder::AgentProcess {
            pid,
            started: "Mon Jan 1 00:00:00 2024".into(),
            command: "fixture-agent --task-lease".into(),
        }
    }

    /// A session with one commit landed on the fixture's `origin` the way a
    /// merged pull request lands it.
    fn landed_by_pull_request(
        broker: &mut Broker,
        repository: &Path,
        label: &str,
        style: aethyme_testkit::MergeStyle,
    ) -> crate::Session {
        aethyme_testkit::ensure_origin(repository);
        let session = broker.start_worktree(label, None).unwrap();
        let checkout = Path::new(&session.worktree_path);
        std::fs::write(checkout.join("landed.txt"), format!("{label}\n")).unwrap();
        git(checkout, &["add", "landed.txt"]);
        git(checkout, &["commit", "-qm", label]);
        aethyme_testkit::land_as_pull_request(repository, &session.branch, style);
        if style == aethyme_testkit::MergeStyle::Squash {
            // "Delete branch on merge": only the default branch's content
            // proves the work landed.
            aethyme_testkit::delete_remote_branch(repository, &session.branch);
        }
        session
    }

    /// The agent process that held a landed session usually lives on, bound
    /// to later sessions. Once its task lease on this one has expired, the
    /// landed proof closes the session without the abandonment opt-in; while
    /// the lease is current the session is kept with a reason.
    #[cfg(unix)]
    #[test]
    fn landed_session_outlives_a_live_holder_only_after_its_task_lease_expired() {
        for lease_current in [true, false] {
            let (repository, _storage, mut broker) = fixture();
            let session = landed_by_pull_request(
                &mut broker,
                repository.path(),
                "landed with a live holder",
                aethyme_testkit::MergeStyle::MergeCommit,
            );
            let old = age_worktree_for_task_retirement(&mut broker, &session);
            let holder = task_lease_holder(std::process::id() as i64);
            let bound_at = if lease_current {
                crate::clock::epoch_ms()
            } else {
                old
            };
            let binding_id = install_task_lease_binding(&mut broker, &session, &holder, bound_at);
            let mut table = synthetic_holder_process_table(session.id, binding_id, &holder, true);
            table.install_process_use_snapshot_for_test(
                crate::reclaim::OpenFileSnapshot::Complete(Vec::new()),
            );

            let outcome = broker
                .try_close_landed_session(
                    session.id,
                    crate::clock::epoch_ms(),
                    24 * 60 * 60 * 1_000,
                    &mut table,
                    true,
                    None,
                )
                .unwrap();

            let closed = broker
                .store()
                .session(session.id)
                .unwrap()
                .status
                .is_closed();
            if lease_current {
                assert_eq!(
                    outcome,
                    AbandonedSessionCloseOutcome::LandedDeclined(LandedSessionDecline::HolderAlive)
                );
                assert!(
                    !closed,
                    "a live holder with a current lease keeps the session"
                );
            } else {
                assert_eq!(outcome, AbandonedSessionCloseOutcome::Closed);
                assert!(closed);
            }
            assert!(
                Path::new(&session.worktree_path).exists(),
                "closing never removes the checkout itself"
            );
        }
    }

    /// One session's proof is bounded by its own budget inside a budgeted
    /// pass: past it the session is kept as `proof_timed_out` (the lane moves
    /// on), while a spent pass deadline is still `budget_exhausted` (the lane
    /// stops). An unbudgeted caller is not bounded per session. The budget
    /// is zero here, so no timing of the runner is involved.
    #[cfg(unix)]
    #[test]
    fn a_proof_past_its_session_budget_is_kept_as_timed_out() {
        let (repository, _storage, mut broker) = fixture();
        let session = landed_by_pull_request(
            &mut broker,
            repository.path(),
            "slow proof",
            aethyme_testkit::MergeStyle::Squash,
        );
        broker
            .store()
            .connection()
            .execute(
                "INSERT INTO pull_request_session_links (repository, pr_number, session_id, linked_at, link_source) VALUES ('fixture', 8, ?1, 1, 'watch')",
                [session.id],
            )
            .unwrap();
        broker
            .store()
            .connection()
            .execute(
                "INSERT INTO pull_request_milestones (repository, pr_number, merged_at, first_seen_at, updated_at) VALUES ('fixture', 8, 1, 1, 1)",
                [],
            )
            .unwrap();
        assert!(
            broker.landed_session_decline(&session, None).is_ok(),
            "the landing and the clean checkout prove"
        );

        let pass = std::time::Instant::now() + std::time::Duration::from_secs(3_600);
        let _budget =
            landed_proof_session_budget_for_test(Some(session.id), std::time::Duration::ZERO);
        assert_eq!(
            broker.landed_session_decline(&session, Some(pass)).err(),
            Some(LandedSessionDecline::ProofTimedOut),
            "a proof past its session budget is inconclusive and kept"
        );
        assert_eq!(
            broker
                .landed_session_decline(&session, Some(std::time::Instant::now()))
                .err(),
            Some(LandedSessionDecline::BudgetExhausted),
            "a spent pass deadline still stops the lane"
        );
        assert!(
            broker.landed_session_decline(&session, None).is_ok(),
            "an unbudgeted caller has no per-session bound"
        );
        assert!(!LandedSessionDecline::ProofTimedOut.allows_abandonment_fallback());
        assert_eq!(
            LandedSessionDecline::ProofTimedOut.code(),
            "proof_timed_out"
        );
        assert_eq!(
            LandedSessionDecline::ProofTimedOut.telemetry_name(),
            "landed_sessions_declined_proof_timed_out"
        );
    }

    /// A squash leaves only content behind. With a merged pull request on
    /// record the landing rule `finish` uses accepts it; a commit made after
    /// the squash is not on the default branch, and keeps the session.
    #[cfg(unix)]
    #[test]
    fn landed_decline_accepts_a_squash_but_not_a_follow_up_commit() {
        let (repository, _storage, mut broker) = fixture();
        let session = landed_by_pull_request(
            &mut broker,
            repository.path(),
            "squash landed",
            aethyme_testkit::MergeStyle::Squash,
        );
        assert_eq!(
            broker.landed_session_decline(&session, None).err(),
            Some(LandedSessionDecline::NoLandingEvidence),
            "without a merged pull request on record a squash is not searched for"
        );
        broker
            .store()
            .connection()
            .execute(
                "INSERT INTO pull_request_session_links (repository, pr_number, session_id, linked_at, link_source) VALUES ('fixture', 7, ?1, 1, 'watch')",
                [session.id],
            )
            .unwrap();
        broker
            .store()
            .connection()
            .execute(
                "INSERT INTO pull_request_milestones (repository, pr_number, merged_at, first_seen_at, updated_at) VALUES ('fixture', 7, 1, 1, 1)",
                [],
            )
            .unwrap();
        let proof = broker.landed_session_decline(&session, None).unwrap();
        let checkout_head = crate::git::GitRepo::discover(Path::new(&session.worktree_path))
            .unwrap()
            .head_commit()
            .unwrap();
        assert_eq!(
            proof.head, checkout_head,
            "the proof names the head it proved"
        );
        assert_eq!(
            broker
                .landed_session_decline(&session, Some(std::time::Instant::now()))
                .err(),
            Some(LandedSessionDecline::BudgetExhausted),
            "a spent pass deadline stops the proof instead of searching"
        );

        let checkout = Path::new(&session.worktree_path);
        std::fs::write(checkout.join("follow-up.txt"), "after the squash\n").unwrap();
        git(checkout, &["add", "follow-up.txt"]);
        git(checkout, &["commit", "-qm", "follow-up after the squash"]);
        assert_eq!(
            broker.landed_session_decline(&session, None).err(),
            Some(LandedSessionDecline::NotLanded)
        );
        // The unlanded head is remembered against the default-branch tip, so
        // the next lap does not search again until one of them moves.
        let head = crate::git::GitRepo::discover(checkout)
            .unwrap()
            .head_commit()
            .unwrap();
        let (_, tip) = broker.repo_handle().upstream_default().unwrap();
        assert_eq!(
            broker
                .store()
                .meta_get(&landed_search_memo_key(session.id))
                .unwrap(),
            Some(format!("{head} {tip}"))
        );
        assert_eq!(
            broker.landed_session_decline(&session, None).err(),
            Some(LandedSessionDecline::NotLanded)
        );
    }

    /// An agent's commit that lands between the proof and the close keeps
    /// the session open: HEAD and the worktree activity are read again
    /// immediately before the state transition.
    #[cfg(unix)]
    #[test]
    fn a_commit_between_the_landed_proof_and_the_close_keeps_the_session() {
        let (repository, _storage, mut broker) = fixture();
        let session = landed_by_pull_request(
            &mut broker,
            repository.path(),
            "committed during the close",
            aethyme_testkit::MergeStyle::MergeCommit,
        );
        age_worktree_for_task_retirement(&mut broker, &session);
        let holder = crate::session_holder::AgentProcess {
            pid: i64::MAX,
            started: "Mon Jan 1 00:00:00 2024".into(),
            command: "fixture-agent".into(),
        };
        broker
            .store()
            .append_event(
                crate::events::SESSION_HOLDER_BOUND,
                Some(session.id),
                Some(&crate::events::session_holder_bound_payload(
                    &holder, "fixture", None,
                )),
            )
            .unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        table.install_before_landed_close_for_test(Box::new(move || {
            std::fs::write(worktree.join("late.txt"), "late work\n").unwrap();
            git(&worktree, &["add", "late.txt"]);
            git(&worktree, &["commit", "-qm", "late work"]);
        }));

        let outcome = broker
            .try_close_landed_session(
                session.id,
                crate::clock::epoch_ms(),
                24 * 60 * 60 * 1_000,
                &mut table,
                true,
                None,
            )
            .unwrap();

        assert_eq!(
            outcome,
            AbandonedSessionCloseOutcome::LandedDeclined(LandedSessionDecline::MovedBeforeClose)
        );
        assert!(
            !broker
                .store()
                .session(session.id)
                .unwrap()
                .status
                .is_closed()
        );
    }

    /// The landed lane counts a live holder sitting in the worktree as use,
    /// while the same holder's other landed session, whose worktree it is not
    /// in, still closes.
    #[cfg(unix)]
    #[test]
    fn a_live_holder_working_in_the_worktree_keeps_only_that_landed_session() {
        let (repository, _storage, mut broker) = fixture();
        let occupied = landed_by_pull_request(
            &mut broker,
            repository.path(),
            "holder works here",
            aethyme_testkit::MergeStyle::MergeCommit,
        );
        let elsewhere = landed_by_pull_request(
            &mut broker,
            repository.path(),
            "holder moved on",
            aethyme_testkit::MergeStyle::MergeCommit,
        );
        let holder = task_lease_holder(std::process::id() as i64);
        let occupied_root = std::fs::canonicalize(&occupied.worktree_path).unwrap();
        for (session, expected) in [
            (&elsewhere, AbandonedSessionCloseOutcome::Closed),
            (&occupied, AbandonedSessionCloseOutcome::OperationActive),
        ] {
            let old = age_worktree_for_task_retirement(&mut broker, session);
            let binding_id = install_task_lease_binding(&mut broker, session, &holder, old);
            let mut table = synthetic_holder_process_table(session.id, binding_id, &holder, true);
            table.install_process_use_snapshot_for_test(
                crate::reclaim::OpenFileSnapshot::Complete(vec![crate::reclaim::OpenFileUse {
                    pid: std::process::id(),
                    command: "fixture-agent".into(),
                    descriptor: "cwd".into(),
                    file_type: Some("DIR".into()),
                    path: occupied_root.clone(),
                }]),
            );
            let outcome = broker
                .try_close_landed_session(
                    session.id,
                    crate::clock::epoch_ms(),
                    24 * 60 * 60 * 1_000,
                    &mut table,
                    true,
                    None,
                )
                .unwrap();
            assert_eq!(outcome, expected, "{:?}", session.task);
        }
        assert_eq!(
            crate::broker::LandedSessionDecline::from_outcome(
                AbandonedSessionCloseOutcome::OperationActive
            ),
            Some(LandedSessionDecline::OperationActive),
            "the kept session is counted as operation_active"
        );
    }

    fn install_task_lease_binding(
        broker: &mut Broker,
        session: &crate::Session,
        holder: &crate::session_holder::AgentProcess,
        ts: i64,
    ) -> i64 {
        broker
            .store()
            .append_event(
                crate::events::SESSION_HOLDER_BOUND,
                Some(session.id),
                Some(&crate::events::session_holder_task_lease_bound_payload(
                    holder,
                    "task_lease_enabled",
                    None,
                )),
            )
            .unwrap();
        let event = broker
            .store()
            .latest_session_holder_event(session.id)
            .unwrap()
            .unwrap();
        broker
            .store()
            .connection()
            .execute(
                "UPDATE events SET ts = ?1 WHERE id = ?2",
                rusqlite::params![ts, event.id],
            )
            .unwrap();
        event.id
    }

    fn synthetic_holder_process_table(
        session_id: i64,
        event_id: i64,
        holder: &crate::session_holder::AgentProcess,
        alive: bool,
    ) -> crate::session_holder::RecordedHolderProcessTable {
        let rows = if alive {
            format!("{} 1 {} {}\n", holder.pid, holder.started, holder.command)
        } else {
            String::new()
        };
        let mut table = crate::session_holder::RecordedHolderProcessTable::default();
        table.install_holder_process_table_for_test(
            session_id,
            event_id,
            crate::session_holder::ProcessTable::parse(&rows),
        );
        table
    }

    #[cfg(unix)]
    #[test]
    fn expired_opted_in_live_holder_can_close_metadata_without_holder_gone() {
        let (repository, _storage, mut broker) = fixture();
        std::fs::write(
            repository.path().join(".aethyme/broker.toml"),
            "[retention]\nexpire_abandoned_task_leases = true\nsession_abandoned_after_hours = 72\n",
        )
        .unwrap();
        let session = broker
            .start_worktree("expired live task holder", None)
            .unwrap();
        broker
            .store()
            .claim_lease(session.id, "tracked.txt", None)
            .unwrap();
        let old = age_worktree_for_task_retirement(&mut broker, &session);
        let holder = task_lease_holder(std::process::id() as i64);
        let binding_id = install_task_lease_binding(&mut broker, &session, &holder, old);
        let mut process_table =
            synthetic_holder_process_table(session.id, binding_id, &holder, true);
        process_table.install_process_use_snapshot_for_test(
            crate::reclaim::OpenFileSnapshot::Complete(Vec::new()),
        );

        let outcome = broker
            .try_close_abandoned_session(
                session.id,
                crate::clock::epoch_ms(),
                72 * 60 * 60 * 1_000,
                &mut process_table,
                true,
                None,
            )
            .unwrap();

        assert_eq!(outcome, AbandonedSessionCloseOutcome::Closed);
        assert!(
            broker
                .store()
                .session(session.id)
                .unwrap()
                .status
                .is_closed()
        );
        assert!(
            broker
                .store()
                .latest_session_holder_gone_event(session.id)
                .unwrap()
                .is_none(),
            "task-lease expiry never fabricates holder-gone evidence"
        );
        assert!(
            broker
                .store()
                .active_leases()
                .unwrap()
                .iter()
                .all(|lease| lease.session_id != session.id),
            "typed task-lease expiry releases the session's explicit and implicit leases"
        );
        assert!(Path::new(&session.worktree_path).exists());
    }

    /// A marker saved before a device renumber keeps the session open, but
    /// reports why (instead of reading as live work) so the user can act.
    #[cfg(unix)]
    #[test]
    fn a_renumbered_legacy_artifact_marker_defers_close_distinctly() {
        let (_repository, _storage, mut broker) = fixture();
        let session = broker
            .start_worktree("renumbered marker task", None)
            .unwrap();
        let old = age_worktree_for_task_retirement(&mut broker, &session);
        let holder = task_lease_holder(i64::MAX);
        let binding_id = install_task_lease_binding(&mut broker, &session, &holder, old);
        crate::file_lock::artifact_use::write_renumbered_legacy_marker_for_test(
            broker.main_root(),
            Path::new(&session.worktree_path),
            session.id,
        )
        .unwrap();
        let mut process_table =
            synthetic_holder_process_table(session.id, binding_id, &holder, false);

        let outcome = broker
            .try_close_abandoned_session(
                session.id,
                crate::clock::epoch_ms(),
                72 * 60 * 60 * 1_000,
                &mut process_table,
                true,
                None,
            )
            .unwrap();

        assert_eq!(
            outcome,
            AbandonedSessionCloseOutcome::LegacyArtifactMarkers(1)
        );
        assert_eq!(
            LandedSessionDecline::from_outcome(outcome),
            Some(LandedSessionDecline::OperationUnknown)
        );
    }

    #[cfg(unix)]
    #[test]
    fn finite_binding_with_orphan_artifact_marker_defers_close() {
        let (_repository, _storage, mut broker) = fixture();
        let session = broker.start_worktree("orphan task child", None).unwrap();
        let old = age_worktree_for_task_retirement(&mut broker, &session);
        let holder = task_lease_holder(i64::MAX);
        let binding_id = install_task_lease_binding(&mut broker, &session, &holder, old);
        let worktree = Path::new(&session.worktree_path);
        let mut producer = match crate::file_lock::artifact_use::begin_artifact_use(
            broker.main_root(),
            worktree,
            session.id,
            "retirement-test-child",
        )
        .unwrap()
        {
            crate::file_lock::artifact_use::ArtifactUseAdmission::Admitted(producer) => producer,
            crate::file_lock::artifact_use::ArtifactUseAdmission::ProtocolUnavailable => {
                panic!("Unix task-lease closure requires artifact-use support")
            }
        };
        let mut process_table =
            synthetic_holder_process_table(session.id, binding_id, &holder, false);

        let outcome = broker
            .try_close_abandoned_session(
                session.id,
                crate::clock::epoch_ms(),
                72 * 60 * 60 * 1_000,
                &mut process_table,
                true,
                None,
            )
            .unwrap();
        producer.finish_not_started().unwrap();

        assert_eq!(outcome, AbandonedSessionCloseOutcome::OperationActive);
        assert!(
            !broker
                .store()
                .session(session.id)
                .unwrap()
                .status
                .is_closed()
        );
    }

    #[cfg(unix)]
    #[test]
    fn finite_binding_with_in_progress_preparation_defers_close() {
        let (repository, _storage, mut broker) = fixture();
        let session = broker
            .start_worktree("active preparation witness", None)
            .unwrap();
        let prepare_config = "schema_version = 1\n\n[[steps]]\nname = \"deps\"\ncommand = [\"sh\", \"-c\", \"true\"]\ninputs = []\noutputs = [\"out/\"]\ncache = \"repository_shared\"\nrequired_for_hooks = false\n";
        std::fs::create_dir_all(Path::new(&session.worktree_path).join(".aethyme")).unwrap();
        std::fs::write(
            Path::new(&session.worktree_path).join(".aethyme/prepare.toml"),
            prepare_config,
        )
        .unwrap();
        let preparation_dir = repository.path().join(".aethyme/run/preparation");
        std::fs::create_dir_all(&preparation_dir).unwrap();
        std::fs::write(
            preparation_dir.join(format!("session-{}.json", session.id)),
            serde_json::json!({
                "schema_version": 1,
                "session_id": session.id,
                "state": "in_progress",
                "digest": "fixture-digest",
                "source_digest": "fixture-source",
                "started_at": crate::clock::epoch_ms(),
                "completed_at": null,
                "failed_step": null,
                "exit_code": null,
                "host_lease_id": null,
                "host_lease_generation": null,
            })
            .to_string(),
        )
        .unwrap();
        let old = age_worktree_for_task_retirement(&mut broker, &session);
        let holder = task_lease_holder(i64::MAX);
        install_task_lease_binding(&mut broker, &session, &holder, old);
        let mut process_table = crate::session_holder::RecordedHolderProcessTable::default();
        process_table.install_process_use_snapshot_for_test(
            crate::reclaim::OpenFileSnapshot::Complete(Vec::new()),
        );

        let outcome = broker
            .try_close_abandoned_session(
                session.id,
                crate::clock::epoch_ms(),
                72 * 60 * 60 * 1_000,
                &mut process_table,
                true,
                None,
            )
            .unwrap();

        assert_eq!(outcome, AbandonedSessionCloseOutcome::OperationActive);
        assert!(
            !broker
                .store()
                .session(session.id)
                .unwrap()
                .status
                .is_closed()
        );
    }

    #[cfg(unix)]
    #[test]
    fn finite_binding_with_incomplete_process_inventory_defers_close() {
        let (_repository, _storage, mut broker) = fixture();
        let session = broker.start_worktree("unknown process use", None).unwrap();
        let old = age_worktree_for_task_retirement(&mut broker, &session);
        let holder = task_lease_holder(i64::MAX);
        let binding_id = install_task_lease_binding(&mut broker, &session, &holder, old);
        let mut process_table =
            synthetic_holder_process_table(session.id, binding_id, &holder, false);
        process_table.install_process_use_snapshot_for_test(
            crate::reclaim::OpenFileSnapshot::Incomplete {
                reason: "fixture lsof was truncated".into(),
                observed: Vec::new(),
            },
        );

        let outcome = broker
            .try_close_abandoned_session(
                session.id,
                crate::clock::epoch_ms(),
                72 * 60 * 60 * 1_000,
                &mut process_table,
                true,
                None,
            )
            .unwrap();

        assert_eq!(
            outcome,
            AbandonedSessionCloseOutcome::OperationEvidenceUnavailable
        );
        assert!(
            !broker
                .store()
                .session(session.id)
                .unwrap()
                .status
                .is_closed()
        );
    }

    #[cfg(unix)]
    #[test]
    fn session_operation_guard_busy_defers_abandoned_close() {
        let (_repository, _storage, mut broker) = fixture();
        let session = broker
            .start_worktree("operation guard blocker", None)
            .unwrap();
        age_worktree_for_task_retirement(&mut broker, &session);
        let shared = crate::file_lock::try_acquire_session_operation_guard(
            broker.main_root(),
            session.id,
            crate::file_lock::SessionOperationLockMode::Shared,
        )
        .unwrap()
        .expect("admitted operation holds the shared side");
        let mut process_table = crate::session_holder::RecordedHolderProcessTable::default();

        let outcome = broker
            .try_close_abandoned_session(
                session.id,
                crate::clock::epoch_ms(),
                72 * 60 * 60 * 1_000,
                &mut process_table,
                true,
                None,
            )
            .unwrap();
        drop(shared);

        assert_eq!(outcome, AbandonedSessionCloseOutcome::OperationGuardBusy);
        assert!(
            !broker
                .store()
                .session(session.id)
                .unwrap()
                .status
                .is_closed()
        );
    }

    #[test]
    fn abandoned_shared_holder_explicit_lease_closes_only_after_recorded_grace() {
        for grace_elapsed in [false, true] {
            let (_repository, _storage, mut broker) = fixture();
            let session = broker
                .start_worktree("abandoned shared lease", None)
                .unwrap();
            let sentinel = Path::new(&session.worktree_path).join("unique-work.txt");
            std::fs::write(&sentinel, b"preserve this uncommitted work\n").unwrap();
            let holder = crate::session_holder::AgentProcess {
                pid: i32::MAX as i64,
                started: "Fri Oct 23 08:42:41 2026".into(),
                command: "/opt/codex app-server --listen local".into(),
            };
            broker
                .store
                .append_event(
                    crate::events::SESSION_HOLDER_BOUND,
                    Some(session.id),
                    Some(&crate::events::session_holder_bound_payload(
                        &holder,
                        "registered",
                        None,
                    )),
                )
                .unwrap();
            broker
                .store
                .claim_lease(session.id, "tracked.txt", None)
                .unwrap();
            broker
                .store
                .append_event(
                    crate::events::SESSION_HOLDER_GONE,
                    Some(session.id),
                    Some(&crate::events::session_holder_gone_payload(&holder)),
                )
                .unwrap();
            let decision = crate::clock::epoch_ms() + 3 * 24 * 60 * 60 * 1000;
            let policy = crate::lease_liveness::LeaseLivenessPolicy::load(broker.main_root());
            let since = if grace_elapsed {
                decision - policy.stale_grace_ms - 1
            } else {
                decision
            };
            broker.store.connection().execute(
                "UPDATE events SET ts = ?1 WHERE id = (SELECT MAX(id) FROM events WHERE session_id = ?2 AND kind = ?3)",
                rusqlite::params![since, session.id, crate::events::SESSION_HOLDER_GONE],
            ).unwrap();
            let mut table = crate::session_holder::RecordedHolderProcessTable::default();
            let outcome = broker
                .try_close_abandoned_session(session.id, decision, 60_000, &mut table, true, None)
                .unwrap();
            assert_eq!(
                outcome,
                if grace_elapsed {
                    AbandonedSessionCloseOutcome::Closed
                } else {
                    AbandonedSessionCloseOutcome::LeaseHeld
                }
            );
            assert_eq!(
                broker.store.session(session.id).unwrap().status.is_closed(),
                grace_elapsed
            );
            assert!(
                Path::new(&session.worktree_path).exists(),
                "closure preserves checkout content"
            );
            assert_eq!(
                std::fs::read(&sentinel).unwrap(),
                b"preserve this uncommitted work\n"
            );
            assert!(
                broker
                    .store
                    .session_leases(session.id)
                    .unwrap()
                    .iter()
                    .all(|lease| lease.released_at.is_some() == grace_elapsed)
            );
        }
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
            matches!(
                &cleanup,
                Err(BrokerOpError::CleanupSessionNotTerminal { .. })
            ),
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
        fetch_broker.close(unrelated_session.id).unwrap();
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
    fn guarded_exec_preserves_out_of_lease_and_foreign_checks_and_reports_holder() {
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
        assert_eq!(
            outside
                .phase_timings_ms
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["command_and_supervision", "postcheck", "precheck"]
        );
        assert!(!outside.ownership_ok);
        assert!(
            outside
                .outside_lease_paths
                .iter()
                .any(|path| path == "tracked.txt")
        );
        assert!(
            outside.conflicting_leases.is_empty(),
            "another session's lease never refuses a guarded exec"
        );
        assert!(
            outside
                .warned_leases
                .iter()
                .any(|lease| lease.session_id == holder.id && lease.path == "tracked.txt"),
            "the holder has unknown PID evidence but remains active, so its lease is reported"
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
    fn stale_and_live_path_leases_are_informational() {
        let (repository, _storage, mut broker) = fixture();
        let holder = broker.start_worktree("lease holder", None).unwrap();
        let claimant = broker.start_worktree("lease claimant", None).unwrap();
        broker.claim_lease(holder.id, "shared.txt", None).unwrap();

        let live_claim = broker
            .claim_lease(claimant.id, "shared.txt", None)
            .expect("an active explicit lease informs and never refuses a claim");
        assert!(live_claim.accepted);
        assert!(live_claim.blockers.is_empty());
        assert!(
            live_claim
                .warnings
                .iter()
                .any(|warning| warning.session_id == holder.id)
        );

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
        // Only an overlapping claim reads Git; one nothing overlaps has nothing
        // to time out.
        let holder = broker.start_worktree("overlapping holder", None).unwrap();
        broker
            .claim_lease(holder.id, "claim-target.txt", None)
            .unwrap();
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

#[cfg(test)]
mod active_lease_path_index_tests {
    use super::{holder_has_overlapping_active_lease, index_active_lease_paths_by_holder};

    #[test]
    fn indexed_holder_checks_match_linear_overlap_for_all_paths() {
        let leases = [
            (10, "src/lib.rs"),
            (10, "src/lib.rs"), // Duplicate rows preserve the any-match result.
            (10, "generated/"),
            (20, "src/"),
            (30, "assets/icon.svg"),
        ];
        let indexed = index_active_lease_paths_by_holder(leases.iter().copied());
        let probes = [
            (10, "src/lib.rs", true),
            (10, "generated/child.rs", true),
            (20, "src/lib.rs", true),
            (10, "src/", true),
            (10, "src/sibling.rs", false),
            (10, "generated-sibling/a", false),
            // Another holder's exact lease must not satisfy this query.
            (30, "src/lib.rs", false),
            (10, "assets/icon.svg", false),
            (30, "assets/icon.svg", true),
            (99, "generated/child.rs", false),
        ];

        for (holder, path, expected) in probes {
            let linear = leases.iter().any(|(lease_holder, lease_path)| {
                *lease_holder == holder && crate::leases::paths_overlap(path, lease_path)
            });
            let actual = holder_has_overlapping_active_lease(&indexed, holder, path);
            assert_eq!(
                linear, expected,
                "linear result for holder {holder} and {path}"
            );
            assert_eq!(
                actual, linear,
                "indexed result for holder {holder} and {path}"
            );
        }
    }
}
