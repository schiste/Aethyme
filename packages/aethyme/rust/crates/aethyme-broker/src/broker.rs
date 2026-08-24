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
use crate::graph_impact::{
    GRAPH_IMPACT_MAX_DEPTH, GRAPH_IMPACT_MAX_NODES, GRAPH_IMPACT_RESULT_LIMIT, GraphImpactProvider,
    GraphImpactQuery, GraphImpactStatus, GraphStoreImpactProvider,
};
use crate::store::BrokerStore;
use crate::types::{
    GateStatus, LeaseKind, MergeQueueEntry, MergeStatus, NewSession, Session, SessionOrigin,
    SessionStatus,
};
use crate::version::{VersionDriftReport, VersionDriftStatus};

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
    #[error(transparent)]
    Pr(#[from] crate::pr::PrError),
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
    #[error(
        "refusing repair for session {id}: recorded adoption baseline {baseline} is not an ancestor of session HEAD; adopt/reuse the session from its intended baseline before repairing"
    )]
    InvalidRepairBaseline { id: i64, baseline: String },
    #[error(
        "refusing repair for session {id}: target integration {target} does not contain recorded session baseline {baseline}; reconcile integration with upstream before repairing"
    )]
    RepairTargetBehindBaseline {
        id: i64,
        baseline: String,
        target: String,
    },
    #[error("cannot resolve upstream ref {upstream:?}; fetch it explicitly, then retry")]
    UpstreamRefNotFound { upstream: String },
    #[error("invalid integration reconciliation resolution file {path:?}: {reason}")]
    InvalidReconciliationResolution { path: String, reason: String },
    #[error("integration reconciliation failed and ref rollback also failed: {reason}")]
    ReconciliationRollbackFailed { reason: String },
    #[error(
        "cannot recover prepared reconciliation for {branch}: ref is {actual}, expected either old {old} or new {new}; inspect the ref and broker database before continuing"
    )]
    ReconciliationRecoveryRequired {
        branch: String,
        actual: String,
        old: String,
        new: String,
    },
    #[error("failed to spawn agent command {command:?}: {source}")]
    Spawn {
        command: String,
        source: std::io::Error,
    },
    #[error(
        "lease claim for {path} by session {session_id} overlaps {blocker_count} active lease(s) held by other sessions"
    )]
    LeaseClaimConflict {
        session_id: i64,
        path: String,
        blocker_count: usize,
        blockers: Vec<LeaseBlocker>,
    },
    #[error("invalid lease path {path:?}: {reason}")]
    InvalidLeasePath { path: String, reason: String },
    #[error("{summary}")]
    OwnershipViolation {
        summary: String,
        report: Box<OwnershipAuditReport>,
    },
    #[error("guarded exec requires a command after --")]
    MissingExecCommand,
    #[error("invalid coordinated operation: {reason}")]
    InvalidCoordinatedOperation { reason: String },
    #[error(
        "coordinated operation blocked for {repository}: operation {operation_id} has an unknown outcome; inspect it and run `aethyme broker operations reconcile --operation {operation_id} --outcome succeeded --reason \"...\"` or use `--outcome failed`"
    )]
    CoordinatedOperationBlocked {
        repository: String,
        operation_id: i64,
    },
    #[error("coordinated operation lock at {path}: {source}")]
    OperationIo {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to spawn coordinated {executable} command: {source}")]
    OperationSpawn {
        executable: String,
        source: std::io::Error,
    },
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("session {session_id} has no completed handoff")]
    HandoffNotFoundForSession { session_id: i64 },
    #[error("worktree {worktree:?} has no completed handoff")]
    HandoffNotFoundForWorktree { worktree: String },
    #[error("session.finished event {event_id} is invalid: {reason}")]
    InvalidHandoffEvent { event_id: i64, reason: String },
    #[error(transparent)]
    GateConfig(#[from] crate::gates::GateConfigError),
    #[error(transparent)]
    PrePush(#[from] crate::gates::PrePushValidationError),
    #[error("queue entry {entry} is not verified (status: {status}) — submit/simulate first")]
    NotVerified { entry: i64, status: &'static str },
    #[error("refusing submission for session {session_id}: unsafe submission plan: {reason}")]
    UnsafeSubmissionPlan { session_id: i64, reason: String },
    #[error(
        "refusing submission for session {session_id}: owned commit {commit} has {parent_count} parents; normalized replay supports only single-parent commits"
    )]
    UnsupportedSubmissionCommit {
        session_id: i64,
        commit: String,
        parent_count: usize,
    },
    #[error("ship queue entry {entry} was not found")]
    ShipEntryNotFound { entry: i64 },
    #[error("ship requires a promoted queue entry; entry {entry} is {status}")]
    ShipEntryNotPromoted { entry: i64, status: &'static str },
    #[error(
        "ship entry {entry} promotion {promotion} is not reachable from integration {integration} at {head}"
    )]
    ShipEntryNotOnIntegration {
        entry: i64,
        promotion: String,
        integration: String,
        head: String,
    },
    #[error("ship cannot resolve {what}: {reason}")]
    ShipPlanUnavailable { what: &'static str, reason: String },
    #[error("ship confirmation must be the full 40-character integration SHA")]
    ShipConfirmationNotFullSha,
    #[error("ship confirmation mismatch: expected integration {expected}, received {actual}")]
    ShipConfirmationMismatch { expected: String, actual: String },
    #[error(
        "integration reconciliation apply requires --confirm {expected}; review the dry-run plan first"
    )]
    ReconciliationConfirmationRequired { expected: String },
    #[error("integration reconciliation confirmation must be a full 64-character SHA-256 digest")]
    ReconciliationConfirmationNotSha256,
    #[error(
        "integration reconciliation confirmation mismatch: expected {expected}, received {actual}"
    )]
    ReconciliationConfirmationMismatch { expected: String, actual: String },
    #[error("ship cannot execute without a fetched remote base for {tracking_ref}")]
    ShipRemoteBaseUnavailable { tracking_ref: String },
    #[error(
        "ship remote moved since planning: expected {expected} at {remote_ref}, fetched {actual}"
    )]
    ShipRemoteMoved {
        remote_ref: String,
        expected: String,
        actual: String,
    },
    #[error(
        "ship would not fast-forward {remote_ref}: remote {remote_sha} is not an ancestor of confirmed integration {integration_sha}"
    )]
    ShipNonFastForward {
        remote_ref: String,
        remote_sha: String,
        integration_sha: String,
    },
    #[error("ship {phase} operation {operation_id} ended {status}")]
    ShipOperationFailed {
        phase: &'static str,
        operation_id: i64,
        status: &'static str,
    },
    #[error("ship verification failed for {remote_ref}: expected {expected}, observed {actual}")]
    ShipVerificationMismatch {
        remote_ref: String,
        expected: String,
        actual: String,
    },
    #[error("ship local-main synchronization is unsafe: {reason}")]
    ShipLocalMainUnsafe { reason: String },
    #[error(
        "remote {published_sha} was published, but local-main synchronization was refused after revalidation: {reason}"
    )]
    ShipLocalMainMovedAfterPublish {
        published_sha: String,
        reason: String,
    },
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
    #[error("--sync-integration is valid only with adoption mode reuse")]
    ReuseSyncRequiresReuse,
    #[error("reuse synchronization requires a clean worktree; dirty paths: {paths:?}")]
    ReuseSyncDirty { paths: Vec<String> },
    #[error(
        "reuse synchronization requires a fast-forward, but session HEAD {session_head} is {relation} relative to integration HEAD {integration_head}"
    )]
    ReuseSyncNotFastForward {
        session_head: String,
        integration_head: String,
        relation: &'static str,
    },
    #[error(
        "reuse synchronization verification failed: expected HEAD {expected}, observed {actual}"
    )]
    ReuseSyncVerification { expected: String, actual: String },
}

/// Policy for `adopt` when the worktree already has a live session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdoptMode {
    /// Fail with guidance (default).
    New,
    /// Return the existing session, pointed at a follow-up task.
    Reuse,
    /// Close the existing session (state only) and register fresh.
    ReplaceStale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdoptOptions {
    pub mode: AdoptMode,
    pub sync_integration: bool,
}

impl AdoptOptions {
    pub fn new(mode: AdoptMode) -> Self {
        Self {
            mode,
            sync_integration: false,
        }
    }
}

/// The lifecycle transition actually performed by `adopt_with`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdoptOutcome {
    Created,
    Reused,
    Replaced,
}

impl AdoptOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Reused => "reused",
            Self::Replaced => "replaced",
        }
    }
}

/// Relationship between the adopted checkout and the integration tip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdoptIntegrationRelation {
    Current,
    Behind,
    Ahead,
    Diverged,
}

impl AdoptIntegrationRelation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Behind => "behind",
            Self::Ahead => "ahead",
            Self::Diverged => "diverged",
        }
    }
}

/// Structured integration drift observed while adopting with `--reuse`.
#[derive(Debug, serde::Serialize)]
pub struct AdoptIntegrationDrift {
    pub session_head: String,
    pub integration_branch: String,
    pub integration_head: String,
    pub relation: AdoptIntegrationRelation,
    pub ahead_commits: u64,
    pub behind_commits: u64,
    pub overlapping_changed_paths: Vec<String>,
    pub warning: Option<String>,
    pub safe_next_action: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdoptIntegrationSyncOutcome {
    AlreadyCurrent,
    FastForwarded,
}

#[derive(Debug, serde::Serialize)]
pub struct AdoptIntegrationSync {
    pub outcome: AdoptIntegrationSyncOutcome,
    pub integration_branch: String,
    pub integration_head: String,
    pub before_head: String,
    pub after_head: String,
}

/// Adoption result with the session fields kept at the JSON top level.
#[derive(Debug, serde::Serialize)]
pub struct AdoptReport {
    #[serde(flatten)]
    pub session: Session,
    pub outcome: AdoptOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub integration_drift: Option<AdoptIntegrationDrift>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub integration_sync: Option<AdoptIntegrationSync>,
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
    /// Present only when `doctor --fix-version` was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_repair: Option<VersionRepairReport>,
    /// Live sessions whose worktree path no longer exists on disk.
    pub missing_worktrees: Vec<i64>,
    /// Stale gate pidfiles found (and removed) whose process is gone.
    pub orphaned_pidfiles: Vec<String>,
    /// Lease rows of already-cleaned sessions found (and removed) —
    /// retention for databases written before leases were purged on clean.
    pub purged_stale_leases: usize,
    /// Present when live sessions can still submit and move integration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub integration_movement: Option<IntegrationMovementNotice>,
}

impl DoctorReport {
    pub fn healthy(&self) -> bool {
        let version_ok = !self.version.status.is_drift()
            || self
                .version_repair
                .as_ref()
                .is_some_and(VersionRepairReport::repaired);
        self.integrity == "ok" && self.missing_worktrees.is_empty() && version_ok
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorRepairStatus {
    NotNeeded,
    Skipped,
    Pass,
    Fail,
}

impl DoctorRepairStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotNeeded => "not needed",
            Self::Skipped => "skipped",
            Self::Pass => "pass",
            Self::Fail => "fail",
        }
    }
}

/// Result of the explicit local product-binary repair path.
#[derive(Debug, serde::Serialize)]
pub struct VersionRepairReport {
    pub status: DoctorRepairStatus,
    pub attempted: bool,
    pub command: Vec<String>,
    pub install_source: Option<String>,
    pub integration_head: Option<String>,
    pub exit_code: Option<i32>,
    pub duration_ms: i64,
    pub message: String,
    pub stdout_tail: Vec<String>,
    pub stderr_tail: Vec<String>,
    /// Exact install and verification commands, in execution order.
    pub commands: Vec<Vec<String>>,
    /// Per-component outcomes. Overall pass requires every step to pass.
    pub steps: Vec<VersionRepairStep>,
}

impl VersionRepairReport {
    pub fn repaired(&self) -> bool {
        self.status == DoctorRepairStatus::Pass
    }
}

#[derive(Debug, serde::Serialize)]
pub struct VersionRepairStep {
    pub component: String,
    pub action: String,
    pub command: Vec<String>,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout_tail: Vec<String>,
    pub stderr_tail: Vec<String>,
}

/// Everything `broker status` renders, in one serializable shape.
#[derive(Debug, serde::Serialize)]
pub struct StatusView {
    pub summary: StatusSummary,
    pub advice: Vec<StatusAdvice>,
    pub agents: Vec<AgentView>,
    pub overlaps: Vec<crate::leases::Overlap>,
    pub promoted_conflicts: Vec<PromotedConflict>,
    pub queue: Vec<crate::types::MergeQueueEntry>,
    pub integration_branch: String,
    pub integration_head: String,
    pub main_head: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_head: Option<String>,
    pub main_behind_upstream_commits: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StatusSummary {
    pub message: String,
    pub live_sessions: usize,
    pub active_sessions: usize,
    pub idle_sessions: usize,
    pub stale_sessions: usize,
    pub dirty_sessions: usize,
    pub overlap_count: usize,
    pub promoted_conflict_count: usize,
    pub integration_relation: StatusIntegrationRelation,
    pub integration_ahead_main_commits: u64,
    pub may_move_integration: bool,
    pub commands: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusIntegrationRelation {
    CurrentWithMain,
    AheadOfMain,
    DivergedFromMain,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct IntegrationLiveSession {
    pub id: i64,
    pub status: SessionStatus,
    pub branch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
}

/// Advisory context when integration may move after this command exits.
#[derive(Debug, Clone, serde::Serialize)]
pub struct IntegrationMovementNotice {
    pub branch: String,
    pub head: String,
    pub live_sessions: Vec<IntegrationLiveSession>,
    pub message: String,
    pub commands: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LeaseBlocker {
    pub session_id: i64,
    pub path: String,
    pub kind: LeaseKind,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LeaseClaimReport {
    pub session_id: i64,
    pub path: String,
    pub accepted: bool,
    pub blockers: Vec<LeaseBlocker>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseOverlapRelation {
    Exact,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LeasePlanOverlap {
    pub relation: LeaseOverlapRelation,
    pub session_id: i64,
    pub path: String,
    pub kind: LeaseKind,
    /// Unix epoch milliseconds; `None` means the lease does not expire.
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LeasePathPlan {
    pub path: String,
    pub owned: Vec<LeasePlanOverlap>,
    pub conflicts: Vec<LeasePlanOverlap>,
    pub would_conflict: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LeasePlan {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<i64>,
    pub paths: Vec<LeasePathPlan>,
    pub would_conflict: bool,
}

fn normalize_lease_path(path: &str) -> Result<String, BrokerOpError> {
    let invalid = |reason: &str| BrokerOpError::InvalidLeasePath {
        path: path.to_string(),
        reason: reason.to_string(),
    };
    if path.is_empty() {
        return Err(invalid("path must not be empty"));
    }
    if path.contains('\0') {
        return Err(invalid("path must not contain NUL"));
    }
    if Path::new(path).is_absolute() {
        return Err(invalid("path must be repository-relative"));
    }

    let directory = path.ends_with('/');
    let segments = path.split('/').collect::<Vec<_>>();
    for (index, segment) in segments.iter().enumerate() {
        let final_directory_marker = directory && index + 1 == segments.len();
        if segment.is_empty() && !final_directory_marker {
            return Err(invalid("empty path segments are ambiguous"));
        }
        if matches!(*segment, "." | "..") {
            return Err(invalid("`.` and `..` path segments are ambiguous"));
        }
    }
    Ok(path.to_string())
}

fn lease_plan_overlap_order(a: &LeasePlanOverlap, b: &LeasePlanOverlap) -> std::cmp::Ordering {
    (
        a.session_id,
        a.path.as_str(),
        a.kind.as_str(),
        a.relation,
        a.expires_at,
    )
        .cmp(&(
            b.session_id,
            b.path.as_str(),
            b.kind.as_str(),
            b.relation,
            b.expires_at,
        ))
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct OwnershipAuditReport {
    pub session_id: i64,
    pub base_commit: String,
    pub head_commit: String,
    pub changed_paths: Vec<String>,
    pub missing_lease_paths: Vec<String>,
    pub conflicting_leases: Vec<LeaseBlocker>,
    pub foreign_paths: Vec<String>,
    pub ok: bool,
}

impl OwnershipAuditReport {
    pub fn failure_summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.missing_lease_paths.is_empty() {
            parts.push(format!(
                "{} path(s) without a session lease",
                self.missing_lease_paths.len()
            ));
        }
        if !self.conflicting_leases.is_empty() {
            parts.push(format!(
                "{} overlapping lease(s) held by other sessions",
                self.conflicting_leases.len()
            ));
        }
        if !self.foreign_paths.is_empty() {
            parts.push(format!(
                "{} adoption-time foreign path(s)",
                self.foreign_paths.len()
            ));
        }
        if parts.is_empty() {
            format!("ownership audit failed for session {}", self.session_id)
        } else {
            format!(
                "ownership audit failed for session {}: {}",
                self.session_id,
                parts.join(", ")
            )
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GuardedExecReport {
    pub session_id: i64,
    pub command: Vec<String>,
    pub exit_code: Option<i32>,
    pub command_success: bool,
    pub before_dirty_paths: Vec<String>,
    pub after_dirty_paths: Vec<String>,
    pub touched_paths: Vec<String>,
    pub outside_lease_paths: Vec<String>,
    pub foreign_paths: Vec<String>,
    pub ok: bool,
}

/// Result of `broker integration wait-stable`.
#[derive(Debug, serde::Serialize)]
pub struct IntegrationStabilityReport {
    pub branch: String,
    pub start_head: String,
    pub end_head: String,
    pub stable: bool,
    pub requested_seconds: u64,
    pub observed_ms: i64,
    pub live_sessions: Vec<IntegrationLiveSession>,
    pub message: String,
    pub commands: Vec<String>,
}

/// Focused view of the local integration branch as a pending layer above
/// the main checkout.
#[derive(Debug, serde::Serialize)]
pub struct IntegrationStatusView {
    pub branch: String,
    pub head: String,
    pub main_head: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_head: Option<String>,
    pub main_behind_upstream_commits: u64,
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
    pub state: IntegrationDeliveryState,
    pub summary: String,
    pub commands: Vec<String>,
}

/// Delivery stage of the current integration tip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationDeliveryState {
    Promoted,
    Published,
    LocallySynchronized,
    Blocked,
    Untracked,
}

impl IntegrationDeliveryState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Promoted => "promoted",
            Self::Published => "published",
            Self::LocallySynchronized => "locally_synchronized",
            Self::Blocked => "blocked",
            Self::Untracked => "untracked",
        }
    }
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

/// Outcome class for `broker finish --session`: a human lifecycle helper
/// that only mutates state when the session is safe to close.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishStatus {
    Blocked,
    Closed,
    AlreadyClosed,
}

impl FinishStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Closed => "closed",
            Self::AlreadyClosed => "already_closed",
        }
    }
}

/// Report from `broker finish --session`: close when safe, otherwise
/// explain exactly what must happen first.
#[derive(Debug, serde::Serialize)]
pub struct FinishReport {
    pub session_id: i64,
    pub worktree_path: String,
    pub status: FinishStatus,
    pub closed: bool,
    pub dirty_paths: Vec<String>,
    pub unsubmitted_commits: u64,
    pub latest_queue_entry_id: Option<i64>,
    pub latest_queue_status: Option<MergeStatus>,
    pub delivery: FinishDelivery,
    pub pending_work: FinishPendingWork,
    pub leases_held: Vec<FinishLease>,
    pub last_gate: Option<FinishGateRun>,
    pub cleanup_safe: bool,
    pub recommended_next_action: Option<String>,
    pub summary: String,
    pub warnings: Vec<String>,
    pub next_commands: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct FinishDelivery {
    pub submitted: bool,
    pub promoted: bool,
    pub published: bool,
    pub promotion_commit: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct FinishPendingWork {
    pub present: bool,
    pub dirty_path_count: usize,
    pub unsubmitted_commits: u64,
    pub worktree_missing: bool,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum FinishLeaseState {
    Active,
    Released,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct FinishLease {
    pub path: String,
    pub kind: LeaseKind,
    pub state: FinishLeaseState,
    pub expires_at: Option<i64>,
    pub released_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishGateCacheSource {
    Executed,
    CacheHit,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct FinishGateRun {
    pub gate: String,
    pub status: GateStatus,
    pub tree_hash: String,
    /// Unix epoch milliseconds from the event ledger.
    pub recorded_at: i64,
    pub cache_source: FinishGateCacheSource,
}

/// Redacted durable projection written to a `session.finished` event.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct FinishHandoff {
    pub session_id: i64,
    pub status: FinishStatus,
    pub latest_queue_entry_id: Option<i64>,
    pub latest_queue_status: Option<MergeStatus>,
    pub delivery: FinishDelivery,
    pub pending_work: FinishPendingWork,
    pub leases_held: Vec<FinishLease>,
    pub last_gate: Option<FinishGateRun>,
    pub cleanup_safe: bool,
    pub recommended_next_action: Option<String>,
}

impl From<&FinishReport> for FinishHandoff {
    fn from(report: &FinishReport) -> Self {
        Self {
            session_id: report.session_id,
            status: report.status,
            latest_queue_entry_id: report.latest_queue_entry_id,
            latest_queue_status: report.latest_queue_status,
            delivery: report.delivery.clone(),
            pending_work: report.pending_work.clone(),
            leases_held: report.leases_held.clone(),
            last_gate: report.last_gate.clone(),
            cleanup_safe: report.cleanup_safe,
            recommended_next_action: report.recommended_next_action.clone(),
        }
    }
}

/// Latest persisted handoff plus its append-only event provenance.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionHandoffReport {
    pub event_id: i64,
    /// Unix epoch milliseconds from the event ledger.
    pub recorded_at: i64,
    #[serde(flatten)]
    pub handoff: FinishHandoff,
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

/// Advisory semantic gate-selection report. Path-triggered gate
/// selection remains the only enforced broker behavior; this report is
/// a read surface for graph/caller-edge hints once that provider is
/// proven.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SemanticGateAdvice {
    pub session_id: i64,
    pub mode: String,
    pub enforced: bool,
    pub changed_files: Vec<String>,
    pub path_selected_gates: Vec<SemanticGateSelection>,
    pub semantic_suggested_gates: Vec<SemanticGateSelection>,
    pub semantic: SemanticGateSource,
    pub next_action: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SemanticGateSelection {
    pub gate: String,
    pub triggered_by: Option<String>,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chain: Option<SemanticGateSuggestionChain>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SemanticGateSuggestionChain {
    pub changed_file: String,
    pub caller_file: String,
    pub suggested_gate: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SemanticGateSource {
    pub provider: String,
    pub status: GraphImpactStatus,
    pub reason: String,
    pub graph_store_path: String,
    pub graph_fragments_path: String,
    pub impacted_paths: Vec<String>,
    pub chains: Vec<crate::GraphImpactChain>,
    pub result_limit: usize,
    pub frontier_max_depth: usize,
    pub frontier_max_nodes: usize,
    pub frontier_visited_nodes: usize,
    pub truncated: bool,
}

/// Backwards-compatible name for the status nested in semantic gate reports.
pub type SemanticGateSourceStatus = GraphImpactStatus;

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
    graph_impact_provider: Box<dyn GraphImpactProvider>,
}

impl Broker {
    pub fn open(path_inside_repo: &Path) -> Result<Self, BrokerOpError> {
        Self::open_with_graph_impact_provider(path_inside_repo, GraphStoreImpactProvider)
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
        };
        broker.recover_prepared_reconciliation()?;
        Ok(broker)
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
        Ok(self.adopt_with(worktree, task, AdoptMode::New)?.session)
    }

    /// `adopt` with an explicit policy for the "this worktree already has
    /// a session" case (dogfood feedback 2026-07-14: the bare constraint
    /// error left no obvious follow-up path).
    pub fn adopt_with(
        &mut self,
        worktree: &Path,
        task: Option<&str>,
        mode: AdoptMode,
    ) -> Result<AdoptReport, BrokerOpError> {
        self.adopt_with_options(worktree, task, AdoptOptions::new(mode))
    }

    pub fn adopt_with_options(
        &mut self,
        worktree: &Path,
        task: Option<&str>,
        options: AdoptOptions,
    ) -> Result<AdoptReport, BrokerOpError> {
        if options.sync_integration && options.mode != AdoptMode::Reuse {
            return Err(BrokerOpError::ReuseSyncRequiresReuse);
        }
        let checkout = GitRepo::discover(worktree)?;
        let branch = checkout.current_branch()?;
        let integration_sync = if options.sync_integration {
            Some(self.synchronize_reuse_checkout(&checkout)?)
        } else {
            None
        };
        let diff_base = checkout.head_commit().ok();
        let worktree_path = checkout.root().to_string_lossy().into_owned();
        let foreign_files = checkout.untracked_paths()?;
        let mut outcome = AdoptOutcome::Created;

        if let Some(existing) = self.store.session_for_worktree(&worktree_path)? {
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
                    let session = self
                        .store
                        .reuse_session(existing.id, task, refreshed_base)?;
                    self.store
                        .set_session_foreign_files(session.id, &foreign_files)?;
                    let integration_drift =
                        Some(self.adopt_integration_drift(&checkout, session.id)?);
                    return Ok(AdoptReport {
                        session,
                        outcome: AdoptOutcome::Reused,
                        integration_drift,
                        integration_sync,
                    });
                }
                AdoptMode::ReplaceStale => {
                    // State-only close (never touches the filesystem),
                    // then a fresh registration.
                    self.store
                        .set_session_status(existing.id, SessionStatus::Cleaned, None)?;
                    outcome = AdoptOutcome::Replaced;
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
        self.store
            .set_session_foreign_files(session.id, &foreign_files)?;
        let integration_drift = if options.mode == AdoptMode::Reuse {
            Some(self.adopt_integration_drift(&checkout, session.id)?)
        } else {
            None
        };
        Ok(AdoptReport {
            session,
            outcome,
            integration_drift,
            integration_sync,
        })
    }

    fn synchronize_reuse_checkout(
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

    fn adopt_integration_drift(
        &mut self,
        checkout: &GitRepo,
        session_id: i64,
    ) -> Result<AdoptIntegrationDrift, BrokerOpError> {
        let session_head = checkout.head_commit()?;
        let (integration_branch, integration_head) = self.integration_head()?;
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
        let pending_owned_commits = submission_plan.as_ref().map(|plan| {
            plan.commits
                .iter()
                .filter(|commit| {
                    commit.ownership == crate::SubmissionCommitOwnership::SessionOwned
                        && commit.integration_state == crate::SubmissionIntegrationState::Pending
                })
                .count()
        });
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
                "aethyme broker integration status".into(),
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
                    "session HEAD is {ahead_commits} commit(s) ahead of {integration_branch}, but the submission plan has no safe pending session-owned commits; do not submit until ownership is reconciled"
                )),
                "aethyme broker integration status".into(),
            ),
            AdoptIntegrationRelation::Diverged => (
                Some(format!(
                    "session HEAD and {integration_branch} have diverged ({ahead_commits} ahead, {behind_commits} behind); reconcile before editing"
                )),
                "aethyme broker integration status".into(),
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

    /// Mark a session finished without touching its worktree — the right
    /// verb for adopted sessions, whose checkout the broker never owns
    /// (`cleanup` removes worktrees and refuses on the main checkout).
    pub fn close(&mut self, session_id: i64) -> Result<(), BrokerOpError> {
        self.store
            .set_session_status(session_id, SessionStatus::Cleaned, None)?;
        Ok(())
    }

    // ── start-agent (spawn convenience) ───────────────────────────────

    /// Create a broker-owned worktree + branch for `task` without
    /// spawning a process. This is the preferred entrypoint for agents
    /// already running in an existing shell: the caller can `cd` into the
    /// returned path and continue with an isolated index and checkout.
    pub fn start_worktree(&mut self, task: &str) -> Result<Session, BrokerOpError> {
        let (_slug, branch, base, worktree) = self.create_session_worktree(task)?;
        let session = self.store.register_session(&NewSession {
            worktree_path: worktree.root().to_string_lossy().into_owned(),
            branch,
            origin: SessionOrigin::Spawned,
            task: Some(task.to_string()),
            diff_base: Some(base),
            pid: None,
            command: None,
            log_path: None,
        })?;
        self.store.set_session_foreign_files(session.id, &[])?;
        Ok(session)
    }

    /// Create a worktree + branch for `task` and spawn `command` in it via
    /// `sh -c` with stdout/stderr teed to a log file. Returns the session;
    /// the child runs detached (the broker never owns the process beyond
    /// recording its PID).
    pub fn start_agent(&mut self, task: &str, command: &str) -> Result<Session, BrokerOpError> {
        let (slug, branch, base, worktree) = self.create_session_worktree(task)?;

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
        self.store.set_session_foreign_files(session.id, &[])?;
        Ok(session)
    }

    fn create_session_worktree(
        &mut self,
        task: &str,
    ) -> Result<(String, String, String, GitRepo), BrokerOpError> {
        let slug = self.next_worktree_slug(task);
        let worktree_path = self.main_root.join(".aethyme/worktrees").join(&slug);
        let branch = format!("agent/{slug}");
        let base = self.repo.head_commit()?;
        let worktree = self.repo.worktree_add(&worktree_path, &branch, &base)?;
        Ok((slug, branch, base, worktree))
    }

    fn next_worktree_slug(&self, task: &str) -> String {
        let base = slugify(task);
        for attempt in 0..1000 {
            let slug = if attempt == 0 {
                base.clone()
            } else {
                format!("{base}-{}", attempt + 1)
            };
            let branch = format!("refs/heads/agent/{slug}");
            let worktree_path = self.main_root.join(".aethyme/worktrees").join(&slug);
            if !worktree_path.exists() && self.repo.resolve_ref(&branch).is_none() {
                return slug;
            }
        }
        format!("{base}-{}", now_ms())
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

    /// Claim an explicit write lease after checking active ownership from
    /// other live sessions. Implicit leases remain advisory conflict
    /// telemetry; explicit leases are the boundary used by guarded exec.
    pub fn claim_lease(
        &mut self,
        session_id: i64,
        path: &str,
        ttl_ms: Option<i64>,
    ) -> Result<LeaseClaimReport, BrokerOpError> {
        self.store.session(session_id)?;
        let path = normalize_lease_path(path)?;
        self.refresh_leases()?;
        let blockers = self.lease_blockers(session_id, &path, false)?;
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
        })
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
        let mut planned = Vec::with_capacity(normalized.len());
        for path in normalized {
            let mut owned = Vec::new();
            let mut conflicts = Vec::new();
            for lease in leases
                .iter()
                .filter(|lease| crate::leases::paths_overlap(&path, &lease.path))
            {
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
                };
                if Some(lease.session_id) == session_id {
                    owned.push(overlap);
                } else {
                    conflicts.push(overlap);
                }
            }
            owned.sort_by(lease_plan_overlap_order);
            conflicts.sort_by(lease_plan_overlap_order);
            let would_conflict = !conflicts.is_empty();
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
        self.audit_ownership_for_paths(session_id, &base, &head, true)
    }

    fn audit_ownership_for_paths(
        &mut self,
        session_id: i64,
        base: &str,
        head: &str,
        allow_implicit: bool,
    ) -> Result<OwnershipAuditReport, BrokerOpError> {
        self.refresh_leases()?;
        let changed = self.repo.changed_between(base, head)?;
        self.audit_paths(session_id, base, head, changed, allow_implicit)
    }

    fn audit_paths(
        &mut self,
        session_id: i64,
        base: &str,
        head: &str,
        mut changed: Vec<String>,
        allow_implicit: bool,
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
            foreign_paths,
            ok,
        })
    }

    fn lease_blockers(
        &self,
        session_id: i64,
        path: &str,
        explicit_only: bool,
    ) -> Result<Vec<LeaseBlocker>, BrokerOpError> {
        use crate::leases::paths_overlap;

        let mut blockers: Vec<LeaseBlocker> = self
            .store
            .active_leases()?
            .into_iter()
            .filter(|lease| lease.session_id != session_id)
            .filter(|lease| !explicit_only || lease.kind == LeaseKind::Explicit)
            .filter(|lease| paths_overlap(&lease.path, path))
            .map(|lease| LeaseBlocker {
                session_id: lease.session_id,
                path: lease.path,
                kind: lease.kind,
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
    /// leaves new dirty paths outside explicit leases. The command's own
    /// exit status is preserved in the report; guard failure is separate
    /// so callers can distinguish test failure from ownership failure.
    pub fn guarded_exec(
        &mut self,
        session_id: i64,
        command: &[String],
    ) -> Result<GuardedExecReport, BrokerOpError> {
        if command.is_empty() {
            return Err(BrokerOpError::MissingExecCommand);
        }
        let session = self.store.session(session_id)?;
        self.refresh_leases()?;
        let checkout = GitRepo::discover(Path::new(&session.worktree_path))?;
        let mut before_dirty = checkout.dirty_paths()?;
        before_dirty.sort();
        before_dirty.dedup();

        let status = Command::new(&command[0])
            .args(&command[1..])
            .current_dir(&session.worktree_path)
            .env("AETHYME_BROKER_SESSION_ID", session_id.to_string())
            .env("AETHYME_GATE_WORKER_ID", format!("s{session_id}-exec"))
            .env("AETHYME_TEST_DB_SUFFIX", format!("s{session_id}-exec"))
            .status()
            .map_err(|source| BrokerOpError::Spawn {
                command: command.join(" "),
                source,
            })?;

        let mut after_dirty = checkout.dirty_paths()?;
        after_dirty.sort();
        after_dirty.dedup();
        let before_set: std::collections::BTreeSet<String> = before_dirty.iter().cloned().collect();
        let touched: Vec<String> = after_dirty
            .iter()
            .filter(|path| !before_set.contains(*path))
            .cloned()
            .collect();
        let audit = self.audit_paths(
            session_id,
            "GUARDED_EXEC_BEFORE",
            "GUARDED_EXEC_AFTER",
            touched.clone(),
            false,
        )?;
        let command_success = status.success();
        let ok = command_success && audit.ok;
        Ok(GuardedExecReport {
            session_id,
            command: command.to_vec(),
            exit_code: status.code(),
            command_success,
            before_dirty_paths: before_dirty,
            after_dirty_paths: after_dirty,
            touched_paths: touched,
            outside_lease_paths: audit.missing_lease_paths,
            foreign_paths: audit.foreign_paths,
            ok,
        })
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
            let baseline = session.diff_base.as_deref().ok_or_else(|| {
                BrokerOpError::InvalidRepairBaseline {
                    id: session_id,
                    baseline: "<missing>".into(),
                }
            })?;
            if !checkout.is_ancestor(baseline, "HEAD") {
                return Err(BrokerOpError::InvalidRepairBaseline {
                    id: session_id,
                    baseline: baseline.to_string(),
                });
            }
            checkout.fetch_local_commit(base)?;
            if !checkout.is_ancestor(baseline, base) {
                return Err(BrokerOpError::RepairTargetBehindBaseline {
                    id: session_id,
                    baseline: baseline.to_string(),
                    target: base.to_string(),
                });
            }
            checkout.rebase_onto_range(base, baseline).map_err(|err| {
                BrokerOpError::RepairRebaseFailed {
                    id: session_id,
                    base: base.to_string(),
                    message: err.to_string(),
                }
            })?;
            self.store.set_session_diff_base(session_id, base)?;
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

    /// Advisory semantic gate-selection surface. The returned semantic
    /// suggestions are not used by [`Self::run_gates`], submit, or CI.
    pub fn semantic_gate_advice(
        &mut self,
        session_id: i64,
    ) -> Result<SemanticGateAdvice, BrokerOpError> {
        let (_, gates, changed) = self.gate_inputs(session_id)?;
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
                        reason: "incoming Calls frontier".into(),
                        chain,
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        let semantic = SemanticGateSource {
            provider: self.graph_impact_provider.name().into(),
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
                "Run `aethyme broker gates run --session {session_id}` to execute the enforced path-triggered gates; treat semantic suggestions as hints only."
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
        self.run_all_gates_with_policy(dir, crate::gates::CachePolicy::Use)
    }

    /// Run every configured gate with an explicit cache lookup policy.
    pub fn run_all_gates_with_policy(
        &mut self,
        dir: &Path,
        cache_policy: crate::gates::CachePolicy,
    ) -> Result<Vec<crate::gates::GateRunOutcome>, BrokerOpError> {
        let checkout = GitRepo::discover(dir)?;
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
                resources_json: serde_json::to_string(&gate.resources)?,
                resource_ttl_seconds: gate.resource_ttl_seconds as i64,
                definition_hash: gate.definition_hash.clone(),
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
        let (upstream_ref, upstream_head) = self
            .repo
            .tracking_upstream()
            .map(|(name, commit)| (Some(name), Some(commit)))
            .unwrap_or((None, None));
        let main_behind_upstream_commits = upstream_head
            .as_deref()
            .filter(|upstream| self.repo.is_ancestor(&main_head, upstream))
            .map(|upstream| self.repo.commit_count_between(&main_head, upstream))
            .transpose()?
            .unwrap_or(0);
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
        if main_behind_upstream_commits > 0 && !integration_contains_upstream {
            let upstream = upstream_ref.as_deref().unwrap_or("@{upstream}");
            next_action = IntegrationNextAction {
                state: IntegrationDeliveryState::Blocked,
                summary: format!(
                    "external main movement detected: local main is {main_behind_upstream_commits} commits behind {upstream}; plan reconciliation before repair or submit"
                ),
                commands: vec![format!(
                    "aethyme broker integration reconcile --upstream {upstream} --dry-run"
                )],
            };
        }

        Ok(IntegrationStatusView {
            branch,
            head,
            main_head,
            upstream_ref,
            upstream_head,
            main_behind_upstream_commits,
            main_is_ancestor,
            commits_ahead_main,
            changed_files,
            promoted_entries,
            conflicts,
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
                commands.push("aethyme broker agents".into());
            }
        } else {
            commands.push(format!(
                "aethyme broker integration wait-stable --seconds {seconds}"
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
        let overlaps = self.refresh_leases()?;
        let agents = self.agents(now_ms)?;
        let promoted_conflicts = self.promoted_conflicts()?;
        let queue = self.store.merge_queue()?;
        let (integration_branch, integration_head) = self.integration_head()?;
        let main_head = self.repo.head_commit()?;
        let (upstream_ref, upstream_head) = self
            .repo
            .tracking_upstream()
            .map(|(name, commit)| (Some(name), Some(commit)))
            .unwrap_or((None, None));
        let main_behind_upstream_commits = upstream_head
            .as_deref()
            .filter(|upstream| self.repo.is_ancestor(&main_head, upstream))
            .map(|upstream| self.repo.commit_count_between(&main_head, upstream))
            .transpose()?
            .unwrap_or(0);
        let (integration_relation, integration_ahead_main_commits) =
            if integration_head == main_head {
                (StatusIntegrationRelation::CurrentWithMain, 0)
            } else if self.repo.is_ancestor(&main_head, &integration_head) {
                (
                    StatusIntegrationRelation::AheadOfMain,
                    self.repo
                        .commit_count_between(&main_head, &integration_head)?,
                )
            } else {
                (StatusIntegrationRelation::DivergedFromMain, 0)
            };
        let dirty_sessions = dirty_session_count(&agents);
        let summary = status_summary(
            &agents,
            overlaps.len(),
            promoted_conflicts.len(),
            dirty_sessions,
            &integration_branch,
            integration_relation,
            integration_ahead_main_commits,
        );
        let mut advice = self.status_advice(
            &agents,
            &promoted_conflicts,
            &queue,
            &integration_branch,
            &integration_head,
        );
        let integration_contains_upstream = upstream_head
            .as_deref()
            .is_some_and(|upstream| self.repo.is_ancestor(upstream, &integration_head));
        if main_behind_upstream_commits > 0 {
            let upstream = upstream_ref.as_deref().unwrap_or("@{upstream}");
            advice.insert(
                0,
                StatusAdvice {
                    id: "integration.upstream-main-ahead",
                    severity: if integration_contains_upstream {
                        StatusAdviceSeverity::Notice
                    } else {
                        StatusAdviceSeverity::Blocked
                    },
                    reason: "configured upstream moved outside broker-managed integration",
                    summary: if integration_contains_upstream {
                        format!(
                            "local main is {main_behind_upstream_commits} commits behind {upstream}; integration already contains upstream, so broker operations remain safe"
                        )
                    } else {
                        format!(
                            "external main movement detected: local main is {main_behind_upstream_commits} commits behind {upstream}; repair and submit are unsafe until reconciliation is planned"
                        )
                    },
                    session_id: None,
                    queue_entry_id: None,
                    evidence: vec![
                        format!("local main: {}", short_commit(&main_head)),
                        format!("{upstream}: {}", short_commit(upstream_head.as_deref().unwrap_or(""))),
                    ],
                    commands: if integration_contains_upstream {
                        Vec::new()
                    } else {
                        vec![format!(
                            "aethyme broker integration reconcile --upstream {upstream} --dry-run"
                        )]
                    },
                },
            );
        }
        Ok(StatusView {
            summary,
            advice,
            agents,
            overlaps,
            promoted_conflicts,
            queue,
            integration_branch,
            integration_head,
            main_head,
            upstream_ref,
            upstream_head,
            main_behind_upstream_commits,
        })
    }

    fn status_advice(
        &self,
        agents: &[AgentView],
        promoted_conflicts: &[PromotedConflict],
        queue: &[MergeQueueEntry],
        integration_branch: &str,
        integration_head: &str,
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
                continue;
            }
            let Ok(head) = checkout.head_commit() else {
                continue;
            };
            if let Some(entry) = queue.iter().rev().find(|entry| {
                entry.session_id == agent.session.id
                    && entry.head_commit == head
                    && matches!(
                        entry.status,
                        MergeStatus::Promoted | MergeStatus::ExternallyLanded
                    )
            }) {
                advice.push(promoted_clean_finish_advice(agent, entry));
            }
        }

        if !agents.is_empty() {
            advice.push(integration_movement_advice(
                integration_branch,
                integration_head,
                agents,
            ));
        }

        advice
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

    fn doctor_inner(&mut self, fix_version: bool) -> Result<DoctorReport, BrokerOpError> {
        let integrity = self.store.integrity_check()?;

        let live_sessions = self.store.live_sessions()?;
        let mut missing_worktrees = Vec::new();
        for session in &live_sessions {
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
        let version = crate::version::inspect_version(&self.main_root);
        let version_repair = fix_version.then(|| self.repair_local_cli_version(&version));
        let integration_movement =
            self.integration_movement_notice_from_sessions(&live_sessions)?;

        Ok(DoctorReport {
            integrity,
            version,
            version_repair,
            missing_worktrees,
            orphaned_pidfiles,
            purged_stale_leases,
            integration_movement,
        })
    }

    fn integration_movement_notice_from_sessions(
        &mut self,
        sessions: &[Session],
    ) -> Result<Option<IntegrationMovementNotice>, BrokerOpError> {
        if sessions.is_empty() {
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
                "aethyme broker integration wait-stable --seconds 30".into(),
                "aethyme broker status".into(),
            ],
        }))
    }

    fn repair_local_cli_version(&mut self, version: &VersionDriftReport) -> VersionRepairReport {
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
            Ok(RepairCommandOutput {
                success: output.status.success(),
                exit_code: output.status.code(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
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

    fn handoff_report(event: crate::types::Event) -> Result<SessionHandoffReport, BrokerOpError> {
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

    fn finish_delivery(&self, entry: Option<&MergeQueueEntry>) -> FinishDelivery {
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

    fn finish_leases(
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

    fn finish_last_gate(&self, session_id: i64) -> Result<Option<FinishGateRun>, BrokerOpError> {
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

    fn finalize_finish_report(&self, report: &mut FinishReport) {
        report.pending_work = FinishPendingWork {
            present: !report.dirty_paths.is_empty() || report.unsubmitted_commits > 0,
            dirty_path_count: report.dirty_paths.len(),
            unsubmitted_commits: report.unsubmitted_commits,
            worktree_missing: report.pending_work.worktree_missing,
        };
        report.recommended_next_action = report.next_commands.first().cloned().or_else(|| {
            if report.delivery.promoted && !report.delivery.published {
                report
                    .latest_queue_entry_id
                    .map(|entry| format!("aethyme broker ship plan --entry {entry}"))
            } else if report.delivery.published && !report.cleanup_safe {
                Some("aethyme broker integration status".into())
            } else {
                None
            }
        });
    }

    fn persist_finish_report(&mut self, report: &FinishReport) -> Result<(), BrokerOpError> {
        let payload = crate::events::session_finished_payload(report);
        self.store.finish_session(report.session_id, &payload)?;
        Ok(())
    }

    /// Finish a session at the operator level: close it when there is no
    /// dirty work and no committed work waiting for submit/promotion;
    /// otherwise return actionable guidance without mutating state.
    pub fn finish(&mut self, session_id: i64) -> Result<FinishReport, BrokerOpError> {
        let session = self.store.session(session_id)?;
        let worktree_path = PathBuf::from(&session.worktree_path);
        let at_ms = now_ms();
        let queue = self.store.merge_queue()?;
        let latest_for_session = queue
            .iter()
            .rev()
            .find(|entry| entry.session_id == session_id);
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
            pending_work: FinishPendingWork::default(),
            leases_held: self.finish_leases(session_id, at_ms)?,
            last_gate: self.finish_last_gate(session_id)?,
            cleanup_safe: false,
            recommended_next_action: None,
            summary: format!("session {session_id} is not finished yet"),
            warnings: Vec::new(),
            next_commands: Vec::new(),
        };

        if !worktree_path.exists() {
            report.pending_work.worktree_missing = true;
            if session.status == SessionStatus::Cleaned {
                report.status = FinishStatus::AlreadyClosed;
                report.summary = format!("session {session_id} is already closed");
            } else {
                report.status = FinishStatus::Closed;
                report.closed = true;
                report.summary =
                    format!("session {session_id} closed in broker state; worktree is missing");
            }
            report
                .warnings
                .push("worktree path does not exist; cleanup is not applicable".into());
            self.finalize_finish_report(&mut report);
            if report.status == FinishStatus::Closed {
                self.persist_finish_report(&report)?;
            }
            return Ok(report);
        }

        let checkout = GitRepo::discover(&worktree_path)?;
        let head = checkout.head_commit()?;
        report.dirty_paths = checkout.dirty_paths()?;

        let latest_for_head = queue
            .iter()
            .rev()
            .find(|entry| entry.session_id == session_id && entry.head_commit == head);
        let latest_for_session = queue
            .iter()
            .rev()
            .find(|entry| entry.session_id == session_id);
        let visible_entry = latest_for_head.or(latest_for_session);
        if let Some(entry) = visible_entry {
            report.latest_queue_entry_id = Some(entry.id);
            report.latest_queue_status = Some(entry.status);
            report.delivery = self.finish_delivery(Some(entry));
        }

        let submitted_head_is_delivered = latest_for_head.is_some_and(|entry| {
            matches!(
                entry.status,
                MergeStatus::Promoted | MergeStatus::ExternallyLanded
            )
        });
        report.unsubmitted_commits = if submitted_head_is_delivered {
            0
        } else {
            let pending_from_plan = self.integration_tip().and_then(|integration_head| {
                self.build_submission_plan(&session, &head, &integration_head)
                    .ok()
                    .filter(|plan| plan.safe)
                    .map(|plan| {
                        plan.commits
                            .iter()
                            .filter(|commit| {
                                commit.ownership == crate::SubmissionCommitOwnership::SessionOwned
                                    && commit.integration_state
                                        == crate::SubmissionIntegrationState::Pending
                            })
                            .count() as u64
                    })
            });
            if let Some(pending) = pending_from_plan {
                pending
            } else {
                let base = session
                    .diff_base
                    .clone()
                    .or_else(|| self.session_change_base(&checkout))
                    .unwrap_or_else(|| "HEAD".to_string());
                checkout.commit_count_between(&base, "HEAD")?
            }
        };

        if session.status == SessionStatus::Cleaned {
            report.status = FinishStatus::AlreadyClosed;
            report.summary = format!("session {session_id} is already closed");
            report.cleanup_safe =
                self.finish_cleanup_safe(session_id, &worktree_path, &report.dirty_paths)?;
            if report.cleanup_safe {
                report
                    .next_commands
                    .push(format!("aethyme broker cleanup {session_id}"));
            } else if !report.dirty_paths.is_empty() {
                report.warnings.push(format!(
                    "worktree still has {} uncommitted or untracked {}; cleanup is not safe",
                    report.dirty_paths.len(),
                    plural_word(report.dirty_paths.len(), "path", "paths")
                ));
            }
            self.finalize_finish_report(&mut report);
            return Ok(report);
        }

        if !report.dirty_paths.is_empty() {
            report.warnings.push(format!(
                "worktree has {} uncommitted or untracked {}; commit or stash before finish",
                report.dirty_paths.len(),
                plural_word(report.dirty_paths.len(), "path", "paths")
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
            report
                .next_commands
                .push(format!("git -C {} stash push", session.worktree_path));
            self.finalize_finish_report(&mut report);
            return Ok(report);
        }

        if let Some(entry) = latest_for_head {
            match entry.status {
                MergeStatus::Promoted | MergeStatus::ExternallyLanded => {}
                MergeStatus::Verified => {
                    report.warnings.push(format!(
                        "queue entry {} is verified but not promoted; promote it before finish",
                        entry.id
                    ));
                    report
                        .next_commands
                        .push(format!("aethyme broker promote --entry {}", entry.id));
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
                    report
                        .next_commands
                        .push(format!("aethyme broker repair --session {session_id}"));
                    report
                        .next_commands
                        .push(format!("aethyme broker submit --session {session_id}"));
                    self.finalize_finish_report(&mut report);
                    return Ok(report);
                }
                MergeStatus::Rejected => {
                    report.warnings.push(format!(
                        "latest submit qid {} was rejected; commit a fix and resubmit before finish",
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
                    report.next_commands.push("aethyme broker queue".into());
                    self.finalize_finish_report(&mut report);
                    return Ok(report);
                }
                MergeStatus::Superseded => {}
            }
        }

        if report.unsubmitted_commits > 0 {
            report.warnings.push(format!(
                "HEAD has {} committed {} not yet represented in promoted integration; submit before finish",
                report.unsubmitted_commits,
                plural_word(report.unsubmitted_commits as usize, "change", "changes")
            ));
            report
                .next_commands
                .push(format!("aethyme broker submit --session {session_id}"));
            self.finalize_finish_report(&mut report);
            return Ok(report);
        }

        report.status = FinishStatus::Closed;
        report.closed = true;
        report.summary = format!("session {session_id} closed; worktree untouched");
        report.cleanup_safe =
            self.finish_cleanup_safe(session_id, &worktree_path, &report.dirty_paths)?;
        if report.cleanup_safe {
            report
                .next_commands
                .push(format!("aethyme broker cleanup {session_id}"));
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

    fn finish_cleanup_safe(
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
        let checkout = GitRepo::discover(worktree_path)?;
        let main_head = self.repo.head_commit()?;
        let session_head = checkout.head_commit()?;
        Ok(checkout.unmerged_commit_count(&main_head)? == 0
            || self.submitted_head_is_represented_on(session_id, &session_head, &main_head)?)
    }

    fn submitted_head_is_represented_on(
        &self,
        session_id: i64,
        session_head: &str,
        target_head: &str,
    ) -> Result<bool, BrokerOpError> {
        let represented = self.store.merge_queue()?.into_iter().rev().any(|entry| {
            entry.session_id == session_id
                && entry.head_commit == session_head
                && matches!(
                    entry.status,
                    MergeStatus::Promoted | MergeStatus::ExternallyLanded
                )
                && details_string_value(entry.details_json.as_deref(), "commit")
                    .is_some_and(|promotion| self.repo.is_ancestor(&promotion, target_head))
        });
        Ok(represented)
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
                let session_head = checkout.head_commit()?;
                if unmerged > 0
                    && !self.submitted_head_is_represented_on(
                        session_id,
                        &session_head,
                        &main_head,
                    )?
                {
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

fn plural_word(count: usize, singular: &'static str, plural: &'static str) -> &'static str {
    if count == 1 { singular } else { plural }
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

fn promoted_clean_finish_advice(agent: &AgentView, entry: &MergeQueueEntry) -> StatusAdvice {
    StatusAdvice {
        id: "session.promoted-clean-finish",
        severity: StatusAdviceSeverity::Notice,
        reason: "promoted_clean_finish",
        summary: format!(
            "session {} is promoted and clean; run aethyme broker finish --session {}",
            agent.session.id, agent.session.id
        ),
        session_id: Some(agent.session.id),
        queue_entry_id: Some(entry.id),
        evidence: vec![
            format!("qid {} promoted", entry.id),
            format!("head {}", short_commit(&entry.head_commit)),
        ],
        commands: vec![format!(
            "aethyme broker finish --session {}",
            agent.session.id
        )],
    }
}

fn dirty_session_count(agents: &[AgentView]) -> usize {
    agents
        .iter()
        .filter(|agent| {
            let Ok(checkout) = GitRepo::discover(Path::new(&agent.session.worktree_path)) else {
                return false;
            };
            checkout
                .dirty_paths()
                .map(|dirty| !dirty.is_empty())
                .unwrap_or(false)
        })
        .count()
}

fn status_summary(
    agents: &[AgentView],
    overlap_count: usize,
    promoted_conflict_count: usize,
    dirty_sessions: usize,
    integration_branch: &str,
    integration_relation: StatusIntegrationRelation,
    integration_ahead_main_commits: u64,
) -> StatusSummary {
    let live_sessions = agents.len();
    let active_sessions = agents
        .iter()
        .filter(|agent| agent.derived_status == SessionStatus::Active)
        .count();
    let idle_sessions = agents
        .iter()
        .filter(|agent| agent.derived_status == SessionStatus::Idle)
        .count();
    let stale_sessions = agents
        .iter()
        .filter(|agent| agent.derived_status == SessionStatus::Stale)
        .count();
    let may_move_integration = live_sessions > 0;

    let sessions = session_summary_phrase(
        live_sessions,
        active_sessions,
        idle_sessions,
        stale_sessions,
    );
    let overlaps = overlap_summary_phrase(overlap_count, promoted_conflict_count);
    let integration = integration_summary_phrase(
        integration_branch,
        integration_relation,
        integration_ahead_main_commits,
    );
    let mut notes = Vec::new();
    if dirty_sessions > 0 {
        notes.push(format!(
            "{} dirty {} need commit/stash before submit",
            dirty_sessions,
            plural_word(dirty_sessions, "session", "sessions")
        ));
    }
    notes.push(if active_sessions > 0 {
        "active session may promote new integration work".to_string()
    } else if live_sessions > 0 {
        "live session may promote new integration work".to_string()
    } else {
        "no active submitters".to_string()
    });

    let mut commands = Vec::new();
    if may_move_integration {
        commands.push("aethyme broker integration wait-stable --seconds 30".into());
    }
    if integration_relation != StatusIntegrationRelation::CurrentWithMain {
        commands.push("aethyme broker integration status".into());
    }

    StatusSummary {
        message: format!(
            "{sessions}; {overlaps}; {integration}; {}",
            notes.join("; ")
        ),
        live_sessions,
        active_sessions,
        idle_sessions,
        stale_sessions,
        dirty_sessions,
        overlap_count,
        promoted_conflict_count,
        integration_relation,
        integration_ahead_main_commits,
        may_move_integration,
        commands,
    }
}

fn session_summary_phrase(
    live_sessions: usize,
    active_sessions: usize,
    idle_sessions: usize,
    stale_sessions: usize,
) -> String {
    if live_sessions == 0 {
        return "no live sessions".into();
    }
    if live_sessions == 1 {
        if active_sessions == 1 {
            return "1 active session".into();
        }
        if idle_sessions == 1 {
            return "1 idle session".into();
        }
        if stale_sessions == 1 {
            return "1 stale session".into();
        }
        return "1 live session".into();
    }

    let mut parts = Vec::new();
    if active_sessions > 0 {
        parts.push(format!("{active_sessions} active"));
    }
    if idle_sessions > 0 {
        parts.push(format!("{idle_sessions} idle"));
    }
    if stale_sessions > 0 {
        parts.push(format!("{stale_sessions} stale"));
    }
    if parts.is_empty() {
        format!("{live_sessions} live sessions")
    } else {
        format!("{live_sessions} live sessions ({})", parts.join(", "))
    }
}

fn overlap_summary_phrase(overlap_count: usize, promoted_conflict_count: usize) -> String {
    match (overlap_count, promoted_conflict_count) {
        (0, 0) => "no overlaps".into(),
        (overlaps, 0) => format!(
            "{} live {}",
            overlaps,
            plural_word(overlaps, "overlap", "overlaps")
        ),
        (0, promoted) => format!(
            "{} promoted {}",
            promoted,
            plural_word(promoted, "conflict", "conflicts")
        ),
        (overlaps, promoted) => format!(
            "{} live {}, {} promoted {}",
            overlaps,
            plural_word(overlaps, "overlap", "overlaps"),
            promoted,
            plural_word(promoted, "conflict", "conflicts")
        ),
    }
}

fn integration_summary_phrase(
    integration_branch: &str,
    integration_relation: StatusIntegrationRelation,
    commits_ahead_main: u64,
) -> String {
    match integration_relation {
        StatusIntegrationRelation::CurrentWithMain => {
            format!("{integration_branch} current with main")
        }
        StatusIntegrationRelation::AheadOfMain => format!(
            "{integration_branch} ahead of main by {} {}",
            commits_ahead_main,
            plural_word(commits_ahead_main as usize, "commit", "commits")
        ),
        StatusIntegrationRelation::DivergedFromMain => {
            format!("{integration_branch} diverged from main")
        }
    }
}

fn integration_movement_advice(
    integration_branch: &str,
    integration_head: &str,
    agents: &[AgentView],
) -> StatusAdvice {
    let count = agents.len();
    let summary = format!(
        "{} live {} may submit and move {integration_branch}; wait for a stable integration window before treating long checks as current-tip proof",
        count,
        plural_word(count, "session", "sessions")
    );
    let mut evidence: Vec<String> = agents
        .iter()
        .take(5)
        .map(|agent| {
            format!(
                "session {} {} {}",
                agent.session.id,
                agent.derived_status.as_str(),
                agent.session.branch
            )
        })
        .collect();
    if count > evidence.len() {
        evidence.push(format!(
            "and {} more {}",
            count - evidence.len(),
            plural_word(count - evidence.len(), "session", "sessions")
        ));
    }
    evidence.push(format!(
        "integration head {}",
        short_commit(integration_head)
    ));

    StatusAdvice {
        id: "integration.may-move",
        severity: StatusAdviceSeverity::Notice,
        reason: "live_sessions_can_submit",
        summary,
        session_id: None,
        queue_entry_id: None,
        evidence,
        commands: vec![
            "aethyme broker integration wait-stable --seconds 30".into(),
            "aethyme broker agents".into(),
        ],
    }
}

fn integration_next_action(
    branch: &str,
    integration_head: &str,
    main_head: &str,
    upstream_head: Option<&str>,
    main_is_ancestor: bool,
    latest_delivery_entry_id: Option<i64>,
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
        let count = conflict_sessions.len();
        let noun = if count == 1 { "session" } else { "sessions" };
        let verb = if count == 1 { "overlaps" } else { "overlap" };
        let commands = conflict_sessions
            .iter()
            .take(5)
            .map(|session_id| format!("aethyme broker repair --session {session_id}"))
            .collect();
        return IntegrationNextAction {
            state: IntegrationDeliveryState::Blocked,
            summary: format!(
                "{count} {noun} {verb} the pending integration layer; repair or rebase before submit"
            ),
            commands,
        };
    }

    if main_head == integration_head {
        return IntegrationNextAction {
            state: IntegrationDeliveryState::LocallySynchronized,
            summary: format!("local main is synchronized with {branch}"),
            commands: Vec::new(),
        };
    }

    if upstream_head == Some(integration_head) {
        if let Some(entry_id) = latest_delivery_entry_id {
            return IntegrationNextAction {
                state: IntegrationDeliveryState::Published,
                summary: format!(
                    "{branch} is published at {integration_head}; local main is not synchronized"
                ),
                commands: vec![format!(
                    "aethyme broker ship execute --entry {entry_id} --confirm {integration_head} --sync-main"
                )],
            };
        }
    }

    if !promoted_entries.is_empty() {
        let count = promoted_entries.len();
        let noun = if count == 1 {
            "promoted entry"
        } else {
            "promoted entries"
        };
        let verb = if count == 1 { "is" } else { "are" };
        if main_is_ancestor {
            let entry_id = latest_delivery_entry_id
                .expect("visible promoted entries have a delivery queue entry");
            return IntegrationNextAction {
                state: IntegrationDeliveryState::Promoted,
                summary: format!(
                    "{count} {noun} {verb} promoted on {branch} and ready for a ship plan"
                ),
                commands: vec![format!("aethyme broker ship plan --entry {entry_id}")],
            };
        }
        let entry_id =
            latest_delivery_entry_id.expect("visible promoted entries have a delivery queue entry");
        return IntegrationNextAction {
            state: IntegrationDeliveryState::Blocked,
            summary: format!(
                "{count} {noun} {verb} pending, but main and {branch} have diverged; inspect the blocked ship plan"
            ),
            commands: vec![format!("aethyme broker ship plan --entry {entry_id}")],
        };
    }

    if !changed_files.is_empty() {
        return IntegrationNextAction {
            state: IntegrationDeliveryState::Untracked,
            summary: format!(
                "{branch} differs from main, but no promoted queue entries describe the pending commits; inspect branch history"
            ),
            commands: vec![format!("git log --oneline --left-right HEAD...{branch}")],
        };
    }

    IntegrationNextAction {
        state: IntegrationDeliveryState::Untracked,
        summary: "no promoted work pending outside main".into(),
        commands: Vec::new(),
    }
}

fn integration_live_sessions(sessions: Vec<Session>) -> Vec<IntegrationLiveSession> {
    sessions
        .into_iter()
        .map(|session| IntegrationLiveSession {
            id: session.id,
            status: session.status,
            branch: session.branch,
            task: session.task,
        })
        .collect()
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
    tree_hash: Option<String>,
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
        let tree = self
            .tree_hash
            .as_deref()
            .map(|tree_hash| format!(" tree {}", short_commit(tree_hash)))
            .unwrap_or_default();
        format!(
            "gate {} status {}{}{}{}",
            self.name,
            self.status,
            class,
            if self.cached { " (cached)" } else { "" },
            tree,
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
                tree_hash: gate
                    .get("tree_hash")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
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

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn tail_lines(text: &str, limit: usize) -> Vec<String> {
    let mut lines: Vec<String> = text
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    if lines.len() > limit {
        lines = lines.split_off(lines.len() - limit);
    }
    lines
}

#[derive(Clone)]
struct RepairStepSpec {
    component: &'static str,
    action: &'static str,
    command: Vec<String>,
}

struct RepairCommandOutput {
    success: bool,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn cargo_install_bin_dir() -> PathBuf {
    if let Some(root) = std::env::var_os("CARGO_INSTALL_ROOT") {
        return PathBuf::from(root).join("bin");
    }
    if let Some(home) = std::env::var_os("CARGO_HOME") {
        return PathBuf::from(home).join("bin");
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".cargo/bin")
}

fn local_cli_repair_step_specs(
    source_root: Option<&Path>,
    install_bin: Option<&Path>,
) -> Vec<RepairStepSpec> {
    let source_root = source_root
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| "<integration-worktree>".into());
    let install_bin = install_bin
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| "<cargo-install-root>/bin".into());
    [
        ("router", "aethyme-cli", "aethyme"),
        ("engine", "aethyme-engine", "aethyme-engine-cli"),
    ]
    .into_iter()
    .flat_map(|(component, crate_name, binary)| {
        let install = RepairStepSpec {
            component,
            action: "install",
            command: vec![
                "cargo".into(),
                "install".into(),
                "--path".into(),
                format!("{source_root}/packages/aethyme/rust/crates/{crate_name}"),
                "--force".into(),
                "--locked".into(),
            ],
        };
        let verify = RepairStepSpec {
            component,
            action: "verify",
            command: vec![format!("{install_bin}/{binary}"), "--version".into()],
        };
        [install, verify]
    })
    .collect()
}

fn local_cli_repair_commands(
    source_root: Option<&Path>,
    install_bin: Option<&Path>,
) -> Vec<Vec<String>> {
    local_cli_repair_step_specs(source_root, install_bin)
        .into_iter()
        .map(|step| step.command)
        .collect()
}

fn execute_version_repair_steps(
    specs: &[RepairStepSpec],
    mut run: impl FnMut(&[String]) -> Result<RepairCommandOutput, String>,
) -> Vec<VersionRepairStep> {
    specs
        .iter()
        .map(|spec| match run(&spec.command) {
            Ok(output) => VersionRepairStep {
                component: spec.component.into(),
                action: spec.action.into(),
                command: spec.command.clone(),
                success: output.success,
                exit_code: output.exit_code,
                stdout_tail: tail_lines(&output.stdout, 12),
                stderr_tail: tail_lines(&output.stderr, 12),
            },
            Err(error) => VersionRepairStep {
                component: spec.component.into(),
                action: spec.action.into(),
                command: spec.command.clone(),
                success: false,
                exit_code: None,
                stdout_tail: Vec::new(),
                stderr_tail: vec![error],
            },
        })
        .collect()
}

fn combined_repair_tail(steps: &[VersionRepairStep], stdout: bool) -> Vec<String> {
    let mut lines = steps
        .iter()
        .flat_map(|step| {
            let source = if stdout {
                &step.stdout_tail
            } else {
                &step.stderr_tail
            };
            source
                .iter()
                .map(|line| format!("{} {}: {line}", step.component, step.action))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    if lines.len() > 12 {
        lines = lines.split_off(lines.len() - 12);
    }
    lines
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
    use super::{
        DoctorRepairStatus, DoctorReport, RepairCommandOutput, VersionRepairReport,
        execute_version_repair_steps, local_cli_repair_step_specs, slugify,
    };
    use crate::types::{MergeQueueEntry, Session, SessionOrigin, SessionStatus};
    use crate::version::{BinaryBuild, VersionDriftReport, VersionDriftStatus};

    #[test]
    fn slugify_is_safe_for_branches_and_paths() {
        assert_eq!(slugify("Fix auth bug!"), "fix-auth-bug");
        assert_eq!(slugify("  weird///name  "), "weird-name");
        assert_eq!(slugify("émojis 🎉 stripped"), "mojis-stripped");
        assert_eq!(slugify(""), "task");
        assert!(slugify(&"x".repeat(100)).len() <= 40);
    }

    #[test]
    fn doctor_healthy_accepts_successful_explicit_version_repair() {
        let report = doctor_report(
            VersionDriftStatus::BehindIntegration,
            Some(repair_report(DoctorRepairStatus::Pass)),
        );

        assert!(report.healthy());
    }

    #[test]
    fn doctor_healthy_rejects_failed_explicit_version_repair() {
        let report = doctor_report(
            VersionDriftStatus::BehindIntegration,
            Some(repair_report(DoctorRepairStatus::Fail)),
        );

        assert!(!report.healthy());
    }

    #[test]
    fn version_repair_targets_and_verifies_both_required_binaries() {
        let source = std::path::Path::new("/tmp/aethyme-release-source");
        let install_bin = std::path::Path::new("/tmp/cargo-root/bin");

        let specs = local_cli_repair_step_specs(Some(source), Some(install_bin));

        assert_eq!(specs.len(), 4);
        assert_eq!(
            specs
                .iter()
                .map(|spec| (spec.component, spec.action))
                .collect::<Vec<_>>(),
            vec![
                ("router", "install"),
                ("router", "verify"),
                ("engine", "install"),
                ("engine", "verify"),
            ]
        );
        assert!(specs[0].command.join(" ").contains("aethyme-cli"));
        assert_eq!(
            specs[1].command,
            vec!["/tmp/cargo-root/bin/aethyme", "--version"]
        );
        assert!(specs[2].command.join(" ").contains("aethyme-engine"));
        assert_eq!(
            specs[3].command,
            vec!["/tmp/cargo-root/bin/aethyme-engine-cli", "--version"]
        );
    }

    #[test]
    fn version_repair_requires_every_install_and_verification_step() {
        let specs = local_cli_repair_step_specs(None, None);

        for failed_index in 0..specs.len() {
            let mut observed = 0;
            let steps = execute_version_repair_steps(&specs, |_| {
                let current = observed;
                observed += 1;
                Ok(RepairCommandOutput {
                    success: current != failed_index,
                    exit_code: Some(if current == failed_index { 7 } else { 0 }),
                    stdout: String::new(),
                    stderr: String::new(),
                })
            });

            assert_eq!(observed, 4, "all outcomes must remain observable");
            assert!(!steps.iter().all(|step| step.success));
            assert_eq!(steps.iter().filter(|step| !step.success).count(), 1);
            assert_eq!(steps[failed_index].exit_code, Some(7));
        }
    }

    #[test]
    fn integration_movement_advice_points_to_wait_stable() {
        let agent = super::AgentView {
            session: session(56),
            activity_at: 0,
            derived_status: SessionStatus::Active,
            pid_alive: None,
        };

        let advice =
            super::integration_movement_advice("aethyme/integration", "abcdef1234567890", &[agent]);

        assert_eq!(advice.id, "integration.may-move");
        assert_eq!(advice.severity, super::StatusAdviceSeverity::Notice);
        assert!(
            advice
                .commands
                .contains(&"aethyme broker integration wait-stable --seconds 30".into())
        );
    }

    #[test]
    fn promoted_clean_finish_advice_points_to_finish() {
        let agent = super::AgentView {
            session: session(69),
            activity_at: 0,
            derived_status: SessionStatus::Idle,
            pid_alive: None,
        };
        let entry = queue_entry(104, 69, crate::MergeStatus::Promoted);

        let advice = super::promoted_clean_finish_advice(&agent, &entry);

        assert_eq!(advice.id, "session.promoted-clean-finish");
        assert_eq!(advice.severity, super::StatusAdviceSeverity::Notice);
        assert_eq!(advice.queue_entry_id, Some(104));
        assert_eq!(
            advice.commands,
            vec!["aethyme broker finish --session 69".to_string()]
        );
        assert!(advice.summary.contains("session 69 is promoted and clean"));
    }

    #[test]
    fn status_summary_explains_single_active_session_risk() {
        let agent = super::AgentView {
            session: session(70),
            activity_at: 0,
            derived_status: SessionStatus::Active,
            pid_alive: None,
        };

        let summary = super::status_summary(
            &[agent],
            0,
            0,
            0,
            "aethyme/integration",
            super::StatusIntegrationRelation::CurrentWithMain,
            0,
        );

        assert_eq!(summary.live_sessions, 1);
        assert_eq!(summary.active_sessions, 1);
        assert!(summary.may_move_integration);
        assert_eq!(
            summary.message,
            "1 active session; no overlaps; aethyme/integration current with main; active session may promote new integration work"
        );
        assert_eq!(
            summary.commands,
            vec!["aethyme broker integration wait-stable --seconds 30"]
        );
    }

    #[test]
    fn status_summary_explains_no_active_submitters() {
        let summary = super::status_summary(
            &[],
            0,
            0,
            0,
            "aethyme/integration",
            super::StatusIntegrationRelation::CurrentWithMain,
            0,
        );

        assert_eq!(summary.live_sessions, 0);
        assert_eq!(summary.active_sessions, 0);
        assert!(!summary.may_move_integration);
        assert_eq!(
            summary.message,
            "no live sessions; no overlaps; aethyme/integration current with main; no active submitters"
        );
        assert!(summary.commands.is_empty());
    }

    fn doctor_report(
        status: VersionDriftStatus,
        version_repair: Option<VersionRepairReport>,
    ) -> DoctorReport {
        DoctorReport {
            integrity: "ok".into(),
            version: VersionDriftReport {
                binary: BinaryBuild {
                    version: "0.1.1".into(),
                    describe: Some("v0.1.1".into()),
                    commit: Some("aaaaaaaaaaaa".into()),
                    path: Some("/tmp/aethyme".into()),
                },
                repo_is_aethyme_source: true,
                integration_branch: "aethyme/integration".into(),
                integration_head: Some("bbbbbbbbbbbb".into()),
                integration_describe: Some("v0.1.1-1-gbbbbbbbbbbbb".into()),
                release_tag: Some("v0.1.1".into()),
                status,
                message: "test report".into(),
            },
            version_repair,
            missing_worktrees: Vec::new(),
            orphaned_pidfiles: Vec::new(),
            purged_stale_leases: 0,
            integration_movement: None,
        }
    }

    fn repair_report(status: DoctorRepairStatus) -> VersionRepairReport {
        VersionRepairReport {
            status,
            attempted: true,
            command: vec![
                "cargo".into(),
                "install".into(),
                "--path".into(),
                "/tmp/source".into(),
                "--force".into(),
                "--locked".into(),
            ],
            install_source: Some("/tmp/source".into()),
            integration_head: Some("bbbbbbbbbbbb".into()),
            exit_code: Some(if status == DoctorRepairStatus::Pass {
                0
            } else {
                1
            }),
            duration_ms: 1,
            message: "test repair".into(),
            stdout_tail: Vec::new(),
            stderr_tail: Vec::new(),
            commands: Vec::new(),
            steps: Vec::new(),
        }
    }

    fn session(id: i64) -> Session {
        Session {
            id,
            worktree_path: format!("/tmp/session-{id}"),
            branch: format!("agent/session-{id}"),
            origin: SessionOrigin::Adopted,
            status: SessionStatus::Active,
            task: Some("test".into()),
            diff_base: Some("HEAD".into()),
            pid: None,
            command: None,
            log_path: None,
            exit_code: None,
            created_at: 0,
            updated_at: 0,
            last_activity_at: 0,
        }
    }

    fn queue_entry(id: i64, session_id: i64, status: crate::MergeStatus) -> MergeQueueEntry {
        MergeQueueEntry {
            id,
            session_id,
            head_commit: "abcdef1234567890".into(),
            base_commit: "0123456789abcdef".into(),
            status,
            merged_tree: Some("tree".into()),
            details_json: None,
            created_at: 0,
            updated_at: 0,
        }
    }
}
