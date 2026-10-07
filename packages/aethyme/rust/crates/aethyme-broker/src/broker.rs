//! Session-facing broker operations: the API `aethyme broker ...` wraps.
//!
//! Combines the git service layer with the store. Attach-first: `adopt`
//! is the primary registration path; `start_agent` layers worktree
//! creation + subprocess spawn on the same session model. No code here
//! may assume the broker owns the agent process.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::{
    fs::File,
    io::{Read, Write},
};

use sha2::{Digest, Sha256};

use crate::PromoteConfig;
use crate::error::BrokerError;
use crate::git::{GitError, GitRepo};
use crate::graph_impact::{
    GRAPH_IMPACT_MAX_DEPTH, GRAPH_IMPACT_MAX_NODES, GRAPH_IMPACT_RESULT_LIMIT, GraphImpactMode,
    GraphImpactProvider, GraphImpactQuery, GraphImpactReport, GraphImpactStatus,
    GraphStoreImpactProvider, revision_bound_impact_report,
};
use crate::session_abandonment::{
    AbandonmentVerdict, SessionActivity, decide as decide_abandonment,
};
use crate::store::BrokerStore;
use crate::types::{
    Advisory, AdvisoryList, GateStatus, LeaseKind, MergeQueueEntry, MergeStatus, NewAdvisory,
    NewSession, Session, SessionContext, SessionNote, SessionNoteList, SessionOrigin,
    SessionRepresentation, SessionStatus,
};
use crate::version::{VersionDriftReport, VersionDriftStatus};
use crate::worktree_reconcile::WorktreeReconciliation;

pub(crate) mod gate_trust;

/// Per-phase budget for the read-only `gates affected` inspection (#481).
pub(crate) const GATES_AFFECTED_PHASE_BUDGET_MS: u64 = 5_000;

/// Millisecond timings for each phase of `gates affected`.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub(crate) struct AffectedGatePhaseTimings {
    pub graph_read: u64,
    pub manifest: u64,
    pub selection: u64,
    /// The selection path deliberately does not acquire the graph-integrity
    /// slot, so this remains zero unless a future explicit lock is introduced.
    pub lock_wait: u64,
}

impl AffectedGatePhaseTimings {
    fn over_budget_phases(&self) -> Vec<&'static str> {
        [
            ("graph_read", self.graph_read),
            ("manifest", self.manifest),
            ("selection", self.selection),
            ("lock_wait", self.lock_wait),
        ]
        .into_iter()
        .filter_map(|(phase, elapsed_ms)| {
            (elapsed_ms > GATES_AFFECTED_PHASE_BUDGET_MS).then_some(phase)
        })
        .collect()
    }
}

#[derive(Debug)]
pub(crate) struct AffectedGatesReport {
    pub selected_gates: Vec<(String, Option<String>)>,
    pub phase_timings_ms: AffectedGatePhaseTimings,
    pub phase_budget_ms: u64,
    pub over_budget_phases: Vec<&'static str>,
}

/// Idle/stale thresholds for activity-derived liveness (issue #9).
/// Configurable via `.aethyme/config.toml` in a later phase; constants
/// for now, chosen so an agent "thinking" for a few minutes stays active.
pub(crate) const IDLE_AFTER_MS: i64 = 10 * 60 * 1000;
const STALE_AFTER_MS: i64 = 2 * 60 * 60 * 1000;
/// How long `status --refresh` spends on the Git work that grows with
/// sessions and history -- checkout reads, unpushed commits, promoted-path
/// conflicts and the integration drift assessment -- and `status doctor` on
/// unpushed commits (#460). Whatever the budget does not reach is named in
/// the output, never guessed. `AETHYME_STATUS_INSPECTION_BUDGET_MS` overrides
/// it, for tests and for an operator who would rather wait.
const STATUS_INSPECTION_BUDGET: std::time::Duration = std::time::Duration::from_secs(15);

fn status_inspection_budget() -> std::time::Duration {
    std::env::var("AETHYME_STATUS_INSPECTION_BUDGET_MS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map_or(STATUS_INSPECTION_BUDGET, std::time::Duration::from_millis)
}
pub const SESSION_NOTE_MAX_BYTES: usize = 1_000;
pub const WORKTREE_ROOT_SCHEMA_VERSION: u32 = 1;
pub(crate) const WORKTREE_ROOT_MARKER: &str = ".aethyme-worktree-root.json";

#[derive(Debug, thiserror::Error)]
pub enum BrokerOpError {
    /// The repository opted into the push lane and the session holds commits
    /// no remote has, so closing it would leave the only copy in a worktree
    /// the broker is about to stop tracking.
    #[error(
        "session {session_id} has {unpushed_commits} commit(s) on no remote ({branch} at {head}); \
         push them with `aethyme broker push --session {session_id}`, or close anyway with \
         `--abandon --reason \"<why>\"`"
    )]
    UnpushedSessionWork {
        session_id: i64,
        branch: String,
        head: String,
        unpushed_commits: u32,
    },
    /// Another live agent process holds the session (#393).
    #[error(
        "session {session_id} is held by another live agent process: pid {holder_pid} \
         ({holder_command}){holder_context}. Run this from that agent, or pass `--take-over` \
         to move the session to this agent; the transfer is recorded as a \
         `session.holder_bound` event"
    )]
    SessionHeldByAnotherAgent {
        session_id: i64,
        holder_pid: i64,
        holder_command: String,
        /// `, agent <identity>, tab <name>` for whichever are recorded.
        holder_context: String,
    },
    #[error("main reconciliation is unavailable: {reason}")]
    MainReconcileUnavailable { reason: String },
    /// Representation asks a different question from reconciliation -- whether
    /// a session's work is already on the default branch -- so its refusals
    /// must not read as a `main reconcile` failure.
    #[error("representation cannot be decided: {reason}")]
    RepresentationUnavailable { reason: String },
    #[error("main reconciliation is unsafe: {reason}")]
    MainReconcileUnsafe { reason: String },
    /// See [`BrokerOpError::CleanupConfirmationMismatch`] for why no digest is
    /// offered here (issue #142).
    #[error(
        "the reviewed main reconciliation plan no longer matches current state, so nothing was \
         moved; review a new plan with `aethyme broker advanced main reconcile plan` and confirm the \
         digest it prints (the digest passed, {actual}, is stale)"
    )]
    MainReconcileConfirmationMismatch { actual: String },

    /// An identical command from the same session is still pending. Queueing a
    /// second one would fire it against state the first already changed.
    #[error(
        "session already has an identical {status} coordinated operation {operation_id} pending; \
         current liveness: {liveness}; wait for it to finish or reconcile it rather than queueing a duplicate"
    )]
    DuplicatePendingOperation {
        operation_id: i64,
        status: &'static str,
        liveness: String,
    },
    /// The caller bounded its patience and the lock did not free in time.
    #[error(
        "repository {repository} write lock is busy ({waited}; caller operation {operation_id} did not start): {holder}"
    )]
    CoordinatedLockBusy {
        repository: String,
        holder: String,
        waited: String,
        operation_id: i64,
    },
    /// Admission spent its whole budget before it held the repository lane.
    ///
    /// Distinct from [`BrokerOpError::CoordinatedLockBusy`] on purpose: that one
    /// names a holder the caller can go look at, this one says the caller's own
    /// preparation ran out of time. Conflating them would send an operator
    /// hunting for a lock holder that never existed (#219).
    #[error(
        "admission for {repository} exceeded its {budget} budget while {stage}; nothing was queued and nothing ran -- retry, or allow more time with --queue-timeout"
    )]
    AdmissionTimedOut {
        repository: String,
        stage: String,
        budget: String,
    },
    /// A coordinated child was already running when its bounded operation
    /// budget expired. Remote writes are stored as `outcome_unknown` and use
    /// the durable recovery error below; this variant is for read-only and
    /// local commands whose timeout cannot have changed remote state.
    #[error(
        "coordinated {provider} operation {operation_id} for {repository} exceeded its {budget} budget while {stage}; inspect the recorded operation before retrying"
    )]
    CoordinatedOperationTimedOut {
        provider: &'static str,
        operation_id: i64,
        repository: String,
        stage: String,
        budget: String,
    },
    /// A read-only operation got no answer within its budget. It changed
    /// nothing, so unlike a timed-out write it needs no reconciliation; the
    /// phase split says whether the broker or the provider spent the time
    /// (#555).
    #[error(
        "read-only operation {operation_id} for {repository} got no answer within its {budget} budget: {} preparing, then {} waiting for `{provider}` to respond; it changed nothing, so retry it, or allow more time with --queue-timeout <seconds>",
        crate::operations::humanize_ms(*preparation_ms),
        crate::operations::humanize_ms(*provider_wait_ms)
    )]
    ReadOperationTimedOut {
        provider: &'static str,
        operation_id: i64,
        repository: String,
        budget: String,
        preparation_ms: u64,
        provider_wait_ms: u64,
    },

    #[error(transparent)]
    Git(#[from] GitError),
    #[error(transparent)]
    GithubTarget(#[from] crate::GithubTargetError),
    #[error(transparent)]
    Store(#[from] BrokerError),
    #[error(transparent)]
    Pr(#[from] crate::pr::PrError),
    #[error(transparent)]
    PullRequestWatch(#[from] crate::PullRequestWatchError),
    #[error(transparent)]
    Delivery(#[from] crate::DeliveryError),
    #[error("graph impact request invalid: {reason}")]
    GraphImpactInvalid { reason: String },
    #[error(transparent)]
    Preparation(#[from] crate::PreparationError),
    #[error(transparent)]
    RemoteTarget(#[from] crate::RemoteTargetError),
    #[error(transparent)]
    HostOperation(#[from] crate::HostOperationError),
    #[error("review lifecycle: {reason}")]
    ReviewLifecycle { reason: String },
    #[error("no configured gate named {name:?}")]
    UnknownGate { name: String },
    /// Repository-defined gate or prepare commands whose exact policy no human
    /// on this machine has approved. Nothing was run.
    #[error(
        "refusing to run repository-defined commands: the gate policy of {repository} \
         (sha256 {policy_sha256}) {state}. Gate and prepare commands come from the repository \
         and run as you, so a human must review and approve them on this machine: \
         {trust_command}"
    )]
    GatePolicyUntrusted {
        repository: String,
        policy_sha256: String,
        state: &'static str,
        trust_command: String,
    },
    #[error("refusing to clean session {id}: {reason} (use --force to discard)")]
    DirtyWorktree { id: i64, reason: String },
    #[error(
        "refusing to clean session {id}: its worktree {path} is the checkout of live session \
         {live_id}; finish that session first (--force does not override this)"
    )]
    WorktreeInUseByLiveSession { id: i64, live_id: i64, path: String },
    #[error("bulk cleanup confirmation must be a full SHA-256 digest")]
    CleanupConfirmationNotSha256,
    /// The freshly computed digest is deliberately withheld: it is an opaque
    /// token whose only use is to be pasted back, and pasting it confirms a plan
    /// nobody read (issue #142).
    #[error(
        "the reviewed cleanup plan no longer matches current state, so nothing was removed; \
         review a new plan with `aethyme broker finish cleanup --all-cleaned` and confirm the digest \
         it prints (the digest passed, {actual}, is stale)"
    )]
    CleanupConfirmationMismatch { actual: String },
    #[error("cannot resolve session {id}: {reason}")]
    CleanupResolveRefused { id: i64, reason: String },
    #[error("cleanup resolve confirmation must be a full SHA-256 digest")]
    CleanupResolveConfirmationNotSha256,
    /// See [`BrokerOpError::CleanupConfirmationMismatch`] for why no digest is
    /// offered here (issue #142).
    #[error(
        "the reviewed resolve plan for session {id} no longer matches its worktree, so nothing \
         was archived or removed; review a new plan with `aethyme broker finish cleanup resolve \
         {id} --archive` and confirm the digest it prints (the digest passed, {actual}, is stale)"
    )]
    CleanupResolveConfirmationMismatch { id: i64, actual: String },
    #[error(
        "the recovery archive for session {id} failed verification, so its worktree was left in \
         place: {reason} (incomplete archive: {path})"
    )]
    RecoveryArchiveUnverified {
        id: i64,
        path: String,
        reason: String,
    },
    #[error("GC confirmation must be a full SHA-256 digest")]
    GcConfirmationNotSha256,
    /// The reviewed plan no longer describes current state. The freshly computed
    /// digest is deliberately not offered as a value to pass: confirming a plan
    /// nobody reviewed is exactly what this pair exists to prevent (issue #140).
    #[error(
        "the reviewed GC plan no longer matches current state, so nothing was applied; \
         review a new plan with `aethyme broker gc plan` and confirm the digest it prints \
         (the digest passed, {actual}, is stale)"
    )]
    GcConfirmationMismatch { actual: String },
    /// A partially applied run is recorded in the journal and must be finished
    /// with its own digest. No fresh plan can reproduce it, so directing the
    /// operator to re-plan here would loop them (issue #140).
    #[error(
        "an interrupted GC run is pending and must be resumed on its own plan: confirm \
         {expected}, not {actual}. `aethyme broker gc plan` cannot reproduce that digest \
         because it belongs to the partially applied run recorded in \
         .aethyme/gc-journal.json; inspect it with `aethyme broker status doctor`"
    )]
    GcResumeConfirmationMismatch { expected: String, actual: String },
    /// See [`BrokerOpError::CleanupConfirmationMismatch`] for why no digest is
    /// offered here (issue #142).
    #[error(
        "the reviewed promotion record plan no longer matches current state, so nothing was \
         restored; review a new plan with `aethyme broker submit promotion-record plan` and confirm \
         the digest it prints (the digest passed, {actual}, is stale)"
    )]
    PromotionRecordConfirmationMismatch { actual: String },
    #[error("GC is already running under process {pid}; wait or inspect .aethyme/gc.lock")]
    GcLocked { pid: String },
    #[error("GC recovery journal is invalid: {reason}")]
    InvalidGcJournal { reason: String },
    #[error("GC reviewed artifact drifted at {path}: expected {expected}, observed {actual}")]
    GcArtifactDrift {
        path: String,
        expected: String,
        actual: String,
    },
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
        "refusing automatic repair for session {id}: {reason}\n\
         Commits requiring preservation or review:\n{commits}\n\
         Preservation-first recovery (do not reset before step 1):\n{guidance}"
    )]
    UnsafeRepairPlan {
        id: i64,
        reason: String,
        commits: String,
        guidance: String,
    },
    #[error(
        "broker repair is not applicable to session {id}: no conflicted submission or promoted-path conflict was found; repair does not rewrite checkpoint divergence\nnext: aethyme broker advanced checkpoint plan --session {id}"
    )]
    RepairNotApplicable { id: i64 },
    #[error("session checkpoint recovery is not safe: {reasons}")]
    UnsafeCheckpointRecovery { reasons: String },
    #[error("session checkpoint recovery confirmation must be a full SHA-256 digest")]
    CheckpointConfirmationNotSha256,
    /// See [`BrokerOpError::CleanupConfirmationMismatch`] for why no digest is
    /// offered here (issue #142).
    #[error(
        "the reviewed checkpoint recovery plan no longer matches current state, so nothing was \
         re-anchored; review a new plan with `aethyme broker advanced checkpoint plan --session <id>` \
         and confirm the digest it prints (the digest passed, {actual}, is stale)"
    )]
    CheckpointConfirmationMismatch { actual: String },
    #[error(
        "session checkpoint recovery ref {reference} already points to {actual}, expected {expected}"
    )]
    CheckpointPreservationRefConflict {
        reference: String,
        actual: String,
        expected: String,
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
    #[error("cannot select a safe base for broker start: {reason}")]
    StartBaseUnavailable { reason: String },
    #[error(
        "refusing an integration-tip worktree for pull-request review #{pull_request}; use the routed review adapter, which provisions a detached checkout at the exact PR head, or create and verify that checkout explicitly"
    )]
    ReviewRequiresPullRequestHead { pull_request: i64 },
    #[error("cannot prepare broker worktree root {path}: {reason}")]
    WorktreeRootUnavailable { path: PathBuf, reason: String },
    #[error("refusing nested broker worktree path {path}: it is inside linked worktree {owner}")]
    NestedWorktreePath { path: PathBuf, owner: PathBuf },
    #[error("cannot capture repository contract at {path}: {reason}")]
    RepositoryContract { path: String, reason: String },
    #[error(
        "lease claim for {path} by session {session_id} overlaps {blocker_count} active lease(s): {}",
        describe_lease_blockers(blockers)
    )]
    LeaseClaimConflict {
        session_id: i64,
        path: String,
        blocker_count: usize,
        blockers: Vec<LeaseBlocker>,
    },
    #[error("invalid lease path {path:?}: {reason}")]
    InvalidLeasePath { path: String, reason: String },
    #[error(
        "session {session_id} cannot claim {name:?}: {} holds it and is working (last active {}); coordinate first: aethyme broker advanced note send --session {session_id} --to-session {} --message \"…\"",
        crate::ownership::describe_holder(holder),
        crate::ownership::age_label(crate::clock::epoch_ms().saturating_sub(holder.last_active_at)) + " ago",
        holder.claim.session_id
    )]
    OwnershipClaimHeld {
        name: String,
        session_id: i64,
        holder: Box<crate::OwnershipClaimView>,
    },
    #[error("invalid ownership claim {name:?}: {reason}")]
    InvalidOwnershipClaim { name: String, reason: String },
    #[error("invalid advisory: {reason}")]
    InvalidAdvisory { reason: String },
    #[error("invalid broker note: {reason}")]
    InvalidSessionNote { reason: String },
    #[error(
        "session {session_id} cannot acknowledge note {note_id}, which belongs to session {recipient_session_id}"
    )]
    SessionNoteRecipientMismatch {
        session_id: i64,
        note_id: i64,
        recipient_session_id: i64,
    },
    #[error("cannot project broker advisories at {path}: {source}")]
    AdvisoryProjectionIo {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{summary}")]
    OwnershipViolation {
        summary: String,
        report: Box<OwnershipAuditReport>,
    },
    #[error("guarded exec requires a command after --")]
    MissingExecCommand,
    #[error("invalid coordinated operation: {reason}")]
    InvalidCoordinatedOperation { reason: String },
    /// Session ids are numbered per repository and resolved against the
    /// broker of the directory the command runs in, so the repository is
    /// named: "closed" is otherwise indistinguishable from "you meant the
    /// session with this id in a different repository".
    #[error(
        "session {session_id} of the broker at {repository_root} is closed and cannot authorize coordinated operations; start a new session with `aethyme broker start --task <text> --short-name <name>` or adopt an active worktree with `aethyme broker start --adopt --task <text> --short-name <name>`. Session ids are numbered per repository: if you meant a session of another repository, run the command from that repository's checkout"
    )]
    ClosedSessionOperation {
        session_id: i64,
        repository_root: String,
    },
    /// `--repo` names a repository none of the session worktree's remotes
    /// identify. The command would otherwise run under, and be journaled
    /// against, a session of an unrelated repository.
    #[error(
        "--repo {requested} does not match session {session_id}'s repository ({session_repositories}, from the remotes of {worktree}). Session ids are numbered per repository and resolved against the broker of the directory you run in: run the command from a checkout of {requested} (`cd <that checkout> && aethyme broker ...`) with a session of that repository's broker"
    )]
    SessionRepositoryMismatch {
        session_id: i64,
        requested: String,
        session_repositories: String,
        worktree: String,
    },
    /// `broker push` declined before anything was sent: the repository has
    /// not authorized session-branch pushes, the branch is not a session
    /// branch, or the remote holds work this session never pushed.
    #[error("broker push refused: {reason}")]
    SessionPushRefused { reason: String },
    #[error(
        "broker push {phase} failed (operation {operation_id}, {status}){}",
        if stderr.is_empty() { String::new() } else { format!(": {stderr}") }
    )]
    SessionPushFailed {
        phase: &'static str,
        operation_id: i64,
        status: &'static str,
        stderr: String,
    },
    #[error("session {session_id}'s branch {branch} does not exist in this repository")]
    SessionBranchMissing { session_id: i64, branch: String },
    /// `broker sync` declined before changing anything: the worktree is
    /// dirty or mid-operation, or there is no default branch to sync with.
    #[error("broker sync refused: {reason}")]
    SessionSyncRefused { reason: String },
    /// A lease release request, acknowledgement or decline the broker would
    /// not record: no other session holds the path, the request is not
    /// pending, or the caller is not its holder (#359).
    #[error("lease request refused: {reason}")]
    LeaseRequestRefused { reason: String },
    #[error("{recovery}")]
    CoordinatedOperationBlocked {
        repository: String,
        operation_id: i64,
        recovery: crate::UnknownOutcomeRecovery,
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
    GraphIntegrityPolicy(#[from] crate::GraphIntegrityPolicyError),
    /// No broker path produces this since graph integrity became advice
    /// (#280); kept so library callers matching on it still compile.
    #[error(transparent)]
    GraphIntegrityRejected(#[from] crate::GraphIntegrityRejection),
    #[error(transparent)]
    RetentionConfig(#[from] crate::RetentionConfigError),
    #[error(transparent)]
    PrePush(#[from] crate::gates::PrePushValidationError),
    #[error("queue entry {entry} is not verified (status: {status}) — submit/simulate first")]
    NotVerified { entry: i64, status: &'static str },
    #[error("refusing submission for session {session_id}: unsafe submission plan: {reason}")]
    UnsafeSubmissionPlan { session_id: i64, reason: String },
    #[error(
        "refusing submission for session {session_id}: owned commit {commit} has {parent_count} parents; normalized replay supports only single-parent commits\naccepted checkpoint: {recorded_baseline}\nsession HEAD: {session_head}\npreserve the pending history before rewriting it:\n  git branch {recovery_branch} {session_head}\nthen review and flatten the owned tree change from the accepted checkpoint:\n  git reset --soft {recorded_baseline}\n  git commit\n  aethyme broker submit --session {session_id}"
    )]
    UnsupportedSubmissionCommit {
        session_id: i64,
        commit: String,
        parent_count: usize,
        recorded_baseline: String,
        session_head: String,
        recovery_branch: String,
    },
    #[error("ship queue entry {entry} was not found")]
    ShipEntryNotFound { entry: i64 },
    #[error("exposure reconciliation plan is unavailable: {reason}")]
    ExposurePlanUnavailable { reason: String },
    #[error("exposure reconciliation confirmation must be a full SHA-256 digest")]
    ExposureConfirmationNotSha256,
    /// See [`BrokerOpError::CleanupConfirmationMismatch`] for why no digest is
    /// offered here (issue #142).
    #[error(
        "the reviewed exposure reconciliation plan no longer matches current state, so nothing \
         was reconciled; review a new plan with `aethyme broker advanced exposures plan` and confirm \
         the digest it prints (the digest passed, {actual}, is stale)"
    )]
    ExposureConfirmationMismatch { actual: String },
    #[error("exposure reconciliation is unsafe: {reasons}")]
    ExposurePlanUnsafe { reasons: String },
    #[error(
        "exposure remote verification operation {operation_id} ended {status}; no lifecycle state changed"
    )]
    ExposureVerificationFailed {
        operation_id: i64,
        status: &'static str,
    },
    #[error(
        "remote default branch moved during exposure reconciliation: expected {expected}, observed {actual}; review a new plan"
    )]
    ExposureRemoteMoved { expected: String, actual: String },
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
    #[error("ship publication policy refused: {reason}. Next: {remediation}")]
    ShipPublicationPolicy { reason: String, remediation: String },
    #[error(
        "ship delivery override {requested} would weaken the trusted repository delivery policy {configured}"
    )]
    ShipDeliveryOverrideUnsafe {
        configured: &'static str,
        requested: &'static str,
    },
    #[error(
        "ship delivery was recommended as {recommendation} because the repository is divergent ({reasons}); select it explicitly with --delivery and re-run the reviewed plan"
    )]
    ShipDeliveryRequiresExplicitSelection {
        recommendation: &'static str,
        reasons: String,
    },
    #[error("ship delivery requires a full SHA-256 plan digest from `ship plan`")]
    ShipDeliveryPlanDigestRequired,
    #[error("ship delivery plan confirmation must be a full 64-character SHA-256 digest")]
    ShipDeliveryPlanDigestNotSha256,
    #[error(
        "the reviewed ship delivery plan no longer matches current state; expected plan digest {expected}, received {actual}; rebuild and review the plan"
    )]
    ShipDeliveryPlanDigestMismatch { expected: String, actual: String },
    #[error("ship delivery is unavailable: {reason}")]
    ShipDeliveryUnavailable { reason: String },
    #[error(
        "delivery branch {branch} already exists at {actual}, but the reviewed source is {expected}; refusing to overwrite it"
    )]
    ShipDeliveryBranchConflict {
        branch: String,
        expected: String,
        actual: String,
    },
    #[error("delivery pull request does not match the reviewed head/base: {reason}")]
    ShipDeliveryPullRequestMismatch { reason: String },
    #[error("ship confirmation must be the full 40-character integration SHA")]
    ShipConfirmationNotFullSha,
    /// Ship keeps both SHAs, unlike its siblings: they are inspectable with
    /// `git log` and are the artifact under review rather than a token standing
    /// in for one, so naming them aids diagnosis instead of short-circuiting it.
    /// The warning is explicit because this is the one lane that publishes
    /// (issue #142).
    #[error(
        "the publication prefix moved since the plan was reviewed: ship now proposes \
         {expected}, not the confirmed {actual}. Do not confirm {expected} without reading it \
         -- it publishes work you have not reviewed. Inspect the difference with \
         `git log --oneline {actual}..{expected}`, then re-run `aethyme broker advanced ship plan`"
    )]
    ShipConfirmationMismatch { expected: String, actual: String },
    #[error(
        "integration reconciliation apply requires --confirm {expected}; review the dry-run plan first"
    )]
    ReconciliationConfirmationRequired { expected: String },
    #[error("integration reconciliation confirmation must be a full 64-character SHA-256 digest")]
    ReconciliationConfirmationNotSha256,
    /// See [`BrokerOpError::CleanupConfirmationMismatch`] for why no digest is
    /// offered here (issue #142).
    #[error(
        "the reviewed integration reconciliation plan no longer matches current state, so \
         nothing was reconciled; review a new plan with \
         `aethyme broker advanced integration reconcile --upstream <ref> --dry-run` and confirm the \
         digest it prints (the digest passed, {actual}, is stale)"
    )]
    ReconciliationConfirmationMismatch { actual: String },
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
         aethyme broker start --reuse --task \"...\" --short-name \"<name>\"   point it at a follow-up task\n  \
         aethyme broker finish --session {id}        close it; removes a broker-created checkout when safe\n  \
         aethyme broker finish close --session {id}  close it but keep the checkout on disk\n  \
         aethyme broker start --replace-stale --task \"...\" --short-name \"<name>\"   close it and register fresh"
    )]
    SessionExistsForWorktree {
        id: i64,
        status: &'static str,
        task: String,
    },
    #[error(
        "refusing to register a session on the main checkout {path}: work there lands on the \
         default branch before any gate runs. Start an isolated worktree instead:\n  \
         aethyme broker start --task \"...\" --short-name \"<name>\"\n\
         Pass --allow-main-checkout only if the operator confirmed it."
    )]
    AdoptMainCheckoutRefused { path: String },
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
    /// Close the existing session and register fresh; policy may reclaim ignored build artifacts.
    ReplaceStale,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdoptOptions {
    pub mode: AdoptMode,
    pub sync_integration: bool,
    pub planned_paths: Vec<String>,
}

impl AdoptOptions {
    pub fn new(mode: AdoptMode) -> Self {
        Self {
            mode,
            sync_integration: false,
            planned_paths: Vec::new(),
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

/// Ownership a re-adoption inherited from the worktree's previous session
/// (issue #294). Closing a session and adopting its worktree again, or
/// replacing a stale one, used to record the current HEAD as the new baseline,
/// so the previous session's unsubmitted commits silently stopped being
/// replayed. The new session now starts from the previous one's ownership
/// boundary whenever that still leaves session-owned commits pending.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AdoptCarriedOwnership {
    /// The closed or replaced session whose baseline was carried forward.
    pub from_session: i64,
    /// The ownership boundary the new session starts from.
    pub baseline: String,
    /// Commits after `baseline` that submit will replay.
    pub pending_owned_commits: usize,
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
    pub planned_explicit_leases: Vec<crate::Lease>,
    pub preparation: crate::PreparationStatus,
    /// Paths this session still targets that a later promotion renamed. Empty
    /// for the ordinary case (issue #145).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub renamed_targets: Vec<crate::RenamedTarget>,
    /// The session's head compared with the freshly fetched default branch:
    /// how far it drifted while the worktree sat unused, and whether catching
    /// up would conflict. Absent when there is no fetched default branch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<crate::DefaultBranchDrift>,
    /// Why `default_branch` is missing or used the last fetched copy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_branch_note: Option<String>,
    /// Present when the session kept its predecessor's ownership boundary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub carried_ownership: Option<AdoptCarriedOwnership>,
}

#[derive(Debug, serde::Serialize)]
pub struct StartReport {
    #[serde(flatten)]
    pub session: Session,
    pub start_base: SessionStartBase,
    pub worktree_placement: WorktreePlacement,
    pub planned_explicit_leases: Vec<crate::Lease>,
    pub preparation: crate::PreparationStatus,
}

/// Result of starting a detached agent process, including where its checkout
/// was placed. Kept separate from [`StartReport`] because detached starts do
/// not accept planned path leases.
#[derive(Debug, serde::Serialize)]
pub struct StartAgentReport {
    #[serde(flatten)]
    pub session: Session,
    pub start_base: SessionStartBase,
    pub worktree_placement: WorktreePlacement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeRootSource {
    HostState,
    /// `[worktrees] root` in the repository's `.aethyme/config.toml`.
    RepositoryConfig,
    EnvironmentOverride,
    LibraryOverride,
    RepositoryFallback,
}

impl WorktreeRootSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HostState => "host state",
            Self::RepositoryConfig => "repository config",
            Self::EnvironmentOverride => "environment override",
            Self::LibraryOverride => "library override",
            Self::RepositoryFallback => "repository fallback",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WorktreeRootPlan {
    pub schema_version: u32,
    pub repository_root: PathBuf,
    pub repository_key: String,
    pub preferred_root: Option<PathBuf>,
    pub preferred_source: Option<WorktreeRootSource>,
    /// Directory holding one subdirectory per repository key, when the layout
    /// is keyed. `None` for layouts that place worktrees directly under the
    /// configured root, where sibling directories are sessions rather than
    /// repositories and must never be swept as orphans.
    pub root_container: Option<PathBuf>,
    pub legacy_fallback_root: PathBuf,
    pub preferred_outside_repository: bool,
    /// Where new sessions go when the configured root cannot take them: the
    /// per-user host-state root. Present only for a configured root.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_state_fallback_root: Option<PathBuf>,
    /// Why the configured root cannot take a new session right now, or why
    /// the `[worktrees]` configuration was ignored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WorktreePlacement {
    pub root: PathBuf,
    pub source: WorktreeRootSource,
    pub outside_repository: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct WorktreeRootMarker {
    pub(crate) schema_version: u32,
    pub(crate) repository_key: String,
    pub(crate) repository_root: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStartBaseEvidence {
    /// The operator named the base with `--base <ref>`, so no inference ran.
    ExplicitBase,
    IntegrationTip,
    /// The fetched upstream default branch (`refs/remotes/<remote>/<branch>`):
    /// used when promotion is off, or when integration has fallen behind it.
    FetchedDefaultBranch,
    RemoteDefaultBranch,
    ConventionalMain,
    ConventionalMaster,
}

impl SessionStartBaseEvidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExplicitBase => "explicit --base",
            Self::IntegrationTip => "integration tip",
            Self::FetchedDefaultBranch => "fetched default branch, as of the last fetch",
            Self::RemoteDefaultBranch => "remote default branch",
            Self::ConventionalMain => "conventional main branch",
            Self::ConventionalMaster => "conventional master branch",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionStartBase {
    pub ref_name: String,
    pub commit: String,
    pub evidence: SessionStartBaseEvidence,
    /// Commits the chosen base is behind the fetched default branch.
    ///
    /// Integration is normally *ahead* of the default branch, carrying work not
    /// yet published. Being behind it means the default branch moved and
    /// integration did not follow, so every session started here inherits that
    /// drift — #137 reports a base 160 commits behind, which a branch cut from
    /// it would have carried into its pull request. `None` when there is no
    /// fetched default branch to compare against.
    pub behind_default_commits: Option<u64>,
    /// Commits the chosen base carries that the fetched default branch does
    /// not.
    ///
    /// Unlike `behind_default_commits` this is the *expected* state -- it is
    /// what promoted-but-unpublished work looks like. It still needs naming,
    /// because a branch cut here inherits every one of these commits and a
    /// pull request opened from it presents them as its own. #283 reports a
    /// 2-commit change whose pull request carried 5 commits from four
    /// sessions, and a 4-file change whose pull request carried 29 files.
    /// The `Start base:` line reads identically whether this is 0 or 20.
    pub ahead_default_commits: Option<u64>,
    /// The ref the comparison used, so the number can be checked.
    pub default_ref: Option<String>,
    /// The integration branch this start deliberately did not use, and why.
    /// Absent when integration was used or does not exist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bypassed_integration: Option<BypassedIntegration>,
    /// Whether `start` refreshed the default branch from its remote before
    /// settling on this base. `None` when no refresh was attempted, e.g. no
    /// fetched default branch is configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetched: Option<bool>,
    /// Why the refresh did not happen, when `fetched` is `false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetch_error: Option<String>,
    /// How long ago the default branch's remote-tracking ref last moved,
    /// when the refresh failed and the base came from that cached copy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_ref_age_seconds: Option<u64>,
}

/// Why a start cut from the fetched default branch instead of integration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationBypassReason {
    /// `[promote] mode = "verify-only"`: integration never moves, so it is
    /// never a current base.
    VerifyOnly,
    /// Integration does not contain the fetched default branch's tip. A branch
    /// cut from it would start behind everything merged upstream since
    /// integration last moved.
    BehindUpstream,
}

/// The integration branch a start bypassed. See
/// [`SessionStartBase::bypassed_integration`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BypassedIntegration {
    pub ref_name: String,
    pub commit: String,
    pub reason: IntegrationBypassReason,
    /// Commits the fetched default branch has that integration lacks.
    pub behind_default_commits: Option<u64>,
    /// Commits integration carries that the fetched default branch lacks.
    pub ahead_default_commits: Option<u64>,
    /// How to bring integration back in line, when it has fallen behind.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery_command: Option<String>,
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
    /// Read-only retention candidates and any already-authorized recovery.
    pub retention: crate::GcHealth,
    /// Present when live sessions can still submit and move integration;
    /// never in a verify-only repository.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub integration_movement: Option<IntegrationMovementNotice>,
    /// Committed work only this machine holds. Reported, never counted
    /// against [`Self::healthy`]: an unpushed commit is not a broken broker.
    #[serde(skip_serializing_if = "crate::UnpushedWorkReport::is_empty")]
    pub unpushed_work: crate::UnpushedWorkReport,
    /// Broker commands that failed in the last day, newest first, with the
    /// error each printed. Reported, never counted against [`Self::healthy`]:
    /// a refused command is the broker working, and the list exists so a
    /// failure can be explained after its terminal is gone.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub recent_command_failures: Vec<RecentCommandFailure>,
    /// A `core.hooksPath` that makes git skip every hook. Reported, never
    /// counted against [`Self::healthy`], and never repaired: hook routing
    /// is the operator's git config.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hooks_path: Option<crate::hooks::HooksPathFinding>,
    /// The `integration.leftover-work` row `status` shows: commits a
    /// verify-only repository's integration branch holds that the published
    /// branch lacks. Reported, never counted against [`Self::healthy`], and
    /// never reconciled: `doctor` only names the dry run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leftover_integration_work: Option<StatusAdvice>,
    /// Milliseconds each doctor phase took (#460).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub phase_timings_ms: std::collections::BTreeMap<String, u64>,
    /// Checks the inspection time budget cut short; what they did not reach
    /// is unknown, not healthy. Omitted when every check completed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub budget_cut: Vec<String>,
}

/// One `broker.command.failed` event as `doctor` reports it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RecentCommandFailure {
    pub event_id: i64,
    pub ts: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<i64>,
    pub command_surface: String,
    pub exit_code: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_class: Option<String>,
    /// Absent for failures recorded before messages were kept, and for
    /// commands that exit without printing an error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// How far back `doctor` looks for failed commands, and how many it lists.
const RECENT_COMMAND_FAILURE_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;
const RECENT_COMMAND_FAILURE_LIMIT: i64 = 10;

fn recent_command_failure(event: &crate::Event) -> Option<RecentCommandFailure> {
    let payload: serde_json::Value = serde_json::from_str(event.payload_json.as_deref()?).ok()?;
    let text = |key: &str| {
        payload
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    Some(RecentCommandFailure {
        event_id: event.id,
        ts: event.ts,
        session_id: event.session_id,
        command_surface: text("command_surface")?,
        exit_code: payload
            .get("exit_code")
            .and_then(serde_json::Value::as_u64)
            .and_then(|code| u8::try_from(code).ok()),
        failure_class: text("failure_class"),
        message: text("message"),
    })
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupDisposition {
    Eligible,
    Dirty,
    PendingCommits,
    UnprovenProvenance,
    UnsafePath,
    InspectionFailed,
}

impl CleanupDisposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Eligible => "eligible",
            Self::Dirty => "dirty",
            Self::PendingCommits => "pending_commits",
            Self::UnprovenProvenance => "unproven_provenance",
            Self::UnsafePath => "unsafe_path",
            Self::InspectionFailed => "inspection_failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupRepresentation {
    Represented,
    Pending,
    Unproven,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CleanupProvenance {
    pub representation: CleanupRepresentation,
    pub session_head: String,
    pub adopted_head: Option<String>,
    pub accepted_session_head: Option<String>,
    pub accepted_integration_commit: Option<String>,
    pub accepted_integration_tree: Option<String>,
    pub accepted_queue_entry_id: Option<i64>,
    pub accepted_queue_status: Option<MergeStatus>,
    pub represented_on: Option<String>,
    /// The commit that carried this head's work, when a recorded
    /// representation -- not ancestry -- is what proved it landed.
    pub represented_by_commit: Option<String>,
    pub pending_commit_count: u64,
}

/// What a recorded representation says about one session head.
enum RecordedRepresentationEvidence {
    /// A named commit on a delivery target carried this head's work.
    Landed {
        representing: String,
        target: String,
    },
    /// The branch already held this head's net content, so no commit carried it.
    AlreadyHeld,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CleanupWorktreePlan {
    pub session_id: i64,
    pub worktree_path: String,
    pub worktree_present: bool,
    pub branch_ref: String,
    pub branch_tip: Option<String>,
    pub delete_branch: bool,
    pub origin: SessionOrigin,
    pub disposition: CleanupDisposition,
    pub provenance: Option<CleanupProvenance>,
    pub estimated_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_inodes: Option<u64>,
    pub reason: String,
    pub inspection_commands: Vec<String>,
    pub force_cleanup_command: String,
}

impl CleanupWorktreePlan {
    pub fn eligible(&self) -> bool {
        self.disposition == CleanupDisposition::Eligible
    }
}

pub const CLEANUP_PLAN_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, serde::Serialize)]
pub struct CleanupPlan {
    pub schema_version: u32,
    pub digest: String,
    pub retained_worktree_count: usize,
    pub eligible_worktree_count: usize,
    pub retained_branch_count: usize,
    pub eligible_branch_count: usize,
    /// Sum over worktrees whose size is known. A floor when
    /// `unmeasured_worktree_count` is non-zero -- read that before treating
    /// this as a total.
    pub estimated_retained_bytes: u64,
    pub estimated_reclaimable_bytes: u64,
    /// Retained worktrees nobody has ever walked. Always `0` after a
    /// [`SizeScan::Measure`](crate::SizeScan) pass, which measures everything
    /// it lists.
    pub unmeasured_worktree_count: usize,
    /// The oldest recorded measurement that went into the totals, so a reader
    /// can tell a fresh figure from one assembled out of stale records.
    pub sizes_measured_at_ms: Option<i64>,
    /// Every ref the plan judged provenance against, as `ref=commit`. Part of
    /// the digest, so a confirmation reviewed before any of them moved no
    /// longer matches after: a target that advanced can turn a retained
    /// worktree into an eligible one, and that is a different plan (#354).
    pub target_snapshot: Vec<String>,
    pub worktrees: Vec<CleanupWorktreePlan>,
    /// Worktrees a recorded-size plan listed without judging eligibility,
    /// because the health-check budget ran out first (#460). Always `0` on a
    /// [`SizeScan::Measure`](crate::SizeScan) pass, which inspects everything,
    /// and left out of the JSON when `0` so a measured plan's digest is
    /// unchanged.
    #[serde(skip_serializing_if = "is_zero_count")]
    pub eligibility_not_inspected_count: usize,
}

fn is_zero_count(count: &usize) -> bool {
    *count == 0
}

/// How long a recorded-size cleanup plan spends judging eligibility before it
/// lists the remaining worktrees uninspected. `doctor`, `certify`, the verify
/// loop and `status --audit` take that path, and each eligibility check runs
/// git over the session's history, so on a repository with hundreds of
/// retained worktrees an unbounded pass ran for many minutes (#460).
pub const HEALTH_CHECK_ELIGIBILITY_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);

impl Default for CleanupPlan {
    fn default() -> Self {
        Self {
            schema_version: CLEANUP_PLAN_SCHEMA_VERSION,
            digest: String::new(),
            retained_worktree_count: 0,
            eligible_worktree_count: 0,
            retained_branch_count: 0,
            eligible_branch_count: 0,
            estimated_retained_bytes: 0,
            estimated_reclaimable_bytes: 0,
            unmeasured_worktree_count: 0,
            sizes_measured_at_ms: None,
            target_snapshot: Vec::new(),
            worktrees: Vec::new(),
            eligibility_not_inspected_count: 0,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CleanupSweepFailure {
    pub session_id: i64,
    pub reason: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CleanupSweepReport {
    pub applied: bool,
    pub plan: CleanupPlan,
    pub removed_session_ids: Vec<i64>,
    pub failures: Vec<CleanupSweepFailure>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct RetentionConfigStatus {
    pub warnings: Vec<crate::RetentionConfigWarning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl RetentionConfigStatus {
    fn is_healthy(&self) -> bool {
        self.warnings.is_empty() && self.error.is_none()
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CleanupRetention {
    pub inventory_complete: bool,
    pub inventory_deferred_sessions: usize,
    /// False on routine status: eligibility requires an explicit audit.
    pub eligibility_checked: bool,
    pub broker_owned_worktree_count: usize,
    pub retained_session_branch_count: usize,
    pub eligible_worktree_count: usize,
    pub estimated_retained_bytes: u64,
    pub estimated_reclaimable_bytes: u64,
    pub estimated_blocked_bytes: u64,
    pub retained_bytes_budget: u64,
    pub over_retained_bytes_budget: bool,
    /// Bytes above the budget. `0` within budget or with no budget set.
    pub retained_bytes_deficit: u64,
    /// Whether removing everything currently eligible would get back under
    /// budget. `false` alongside a deficit is the shape #176 was reported in:
    /// megabytes reclaimable against gigabytes retained. That is not a backlog
    /// somebody can work off, so it must not be advised as one.
    pub clears_retained_bytes_budget: bool,
    /// What the byte totals above are actually able to conclude about the
    /// budget. Status reads recorded sizes rather than walking the disk, so
    /// its total is a floor, and a floor answers "over budget" but not "within
    /// budget" -- the bytes it skipped are exactly the ones that would decide
    /// (#176).
    pub budget_verdict: crate::BudgetVerdict,
    /// Retained worktrees whose size nobody has measured yet.
    pub unmeasured_worktree_count: usize,
    /// The oldest measurement behind these totals.
    pub sizes_measured_at_ms: Option<i64>,
    pub oldest_closed_age_days: u64,
    pub closed_worktrees_policy_days: u32,
    /// Free bytes on the volume this repository's gates run on, read once.
    ///
    /// The retained-bytes budget above is per repository and the volume is
    /// shared, so this is the only reading on `CleanupRetention` that can say
    /// whether work can actually run here. It is measured here rather than at
    /// the advice site so that `severity` and the evidence line reporting
    /// "host free space" are two renderings of one reading and cannot
    /// disagree — which is the failure the escalation below exists to remove.
    ///
    /// It is the volume gates refuse on, not the repository's: see
    /// [`Broker::gate_headroom`].
    pub host_available_bytes: Option<u64>,
    /// The directory [`Self::host_available_bytes`] was read at, so an
    /// operator can tell which volume is short.
    pub host_volume_probe: Option<PathBuf>,
    /// The directory the free-inode count was read at.
    pub host_inode_volume_probe: Option<PathBuf>,
    /// Free inodes on the most constrained broker gate volume.
    pub inodes_free: Option<u64>,
    /// Sum of recorded inode counts for broker-owned worktrees. A floor when
    /// worktree_inode_unmeasured is non-zero; status never walks these trees.
    /// A closed worktree is measured by the cleanup plan's size warmer; a live
    /// one only by `gc storage plan`'s warmer, one directory per run, so live
    /// sessions count as unmeasured until a storage plan has reached them.
    pub worktree_inodes: u64,
    pub worktree_inode_unmeasured: usize,
    pub severity: StatusAdviceSeverity,
    /// Config warnings/errors are carried with the retention picture so
    /// status can explain a bad file without failing before it can report it.
    #[serde(skip_serializing_if = "RetentionConfigStatus::is_healthy")]
    pub retention_config: RetentionConfigStatus,
    /// Directories under a broker worktree root that no session row claims
    /// (#176). Unsized on this path -- status runs often, and the count is
    /// the signal; `gc plan` measures the bytes.
    pub reconciliation: WorktreeReconciliation,
    /// Closed sessions whose checkout is still on disk -- what a state-only
    /// `finish close` leaves behind -- with recorded bytes and the command
    /// that shows which of them GC would reclaim.
    pub closed_worktrees: crate::retention::ClosedWorktreeSummary,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StatusQueueHistory {
    pub schema_version: u32,
    pub terminal_counts: Vec<crate::MergeQueueStatusCount>,
    pub command: String,
}

/// One unresolved coordinated write operation, as status reports it.
///
/// Deliberately not named `queue`: `StatusView::queue` is the merge queue, and
/// a reporter checking status for a stuck operation found that field, read it
/// as authoritative, and concluded nothing was pending (issue #147).
#[derive(Debug, Clone, serde::Serialize)]
pub struct PendingOperationView {
    pub id: i64,
    pub session_id: i64,
    pub provider: String,
    pub repository: String,
    pub scope: String,
    pub status: String,
    pub pid: i64,
    /// Seconds since the record was created.
    pub elapsed_seconds: u64,
    /// True for the operation actually holding its repository's write lock.
    pub holding_lock: bool,
    /// The operation this one is parked behind, when it is not the holder.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_by: Option<i64>,
    /// Freshness and last meaningful progress for a running holder. Keeping
    /// this beside the role lets a status reader distinguish work from a
    /// live process that has stopped making progress.
    pub liveness: crate::operations::OperationLivenessView,
}

/// Everything `broker status` renders, in one serializable shape.
#[derive(Debug, serde::Serialize)]
pub struct StatusView {
    pub deferred_checks: Vec<String>,
    pub leases_refreshed_at_ms: Option<i64>,
    pub leases_refreshed: bool,
    pub phase_timings_ms: std::collections::BTreeMap<String, u64>,
    pub summary: StatusSummary,
    pub advice: Vec<StatusAdvice>,
    /// Durable, non-blocking advisories that remain outstanding.
    pub outstanding_advisories: Vec<crate::Advisory>,
    /// Content-free shown-to-action correlation summary.
    pub advisory_delivery: crate::AdvisoryDeliverySummary,
    /// Promoted entry paths whose publication has not yet been proven.
    pub outstanding_entry_exposures: Vec<crate::EntryPathExposure>,
    pub agents: Vec<AgentView>,
    pub leases: Vec<crate::Lease>,
    /// Liveness of each lease in `leases`, bound to its holder process (#360).
    pub lease_liveness: Vec<crate::lease_liveness::LeaseLivenessView>,
    /// Lease release requests still pending, or resolved in the last day (#359).
    pub lease_release_requests: Vec<crate::lease_requests::LeaseReleaseRequest>,
    pub overlaps: Vec<crate::leases::Overlap>,
    /// `overlaps` grouped by session pair and ranked: pairs whose edits Git
    /// says would conflict first. Classified at the last lease refresh.
    pub overlap_pairs: Vec<crate::OverlapPair>,
    /// Collisions between what live sessions say they will work on, which a
    /// path comparison cannot see until both sides have already edited.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope_overlaps: Vec<crate::ScopeOverlap>,
    /// Named operations a live session declared it is driving (`ownership`).
    pub ownership_claims: Vec<crate::OwnershipClaimView>,
    pub promoted_conflicts: Vec<PromotedConflict>,
    /// Unresolved coordinated write operations, across every repository.
    /// Separate from `queue`, which is the merge queue.
    pub coordinated_operations: Vec<PendingOperationView>,
    /// Live/pending/conflicted rows only. Terminal history is paginated.
    pub queue: Vec<crate::types::MergeQueueEntry>,
    pub queue_history: StatusQueueHistory,
    pub integration_branch: String,
    pub integration_head: String,
    pub main_head: String,
    /// Ref the integration lead is counted against, named so the number
    /// is never read against the wrong baseline.
    pub publication_baseline_ref: String,
    pub publication_baseline_head: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_head: Option<String>,
    /// Commits reachable from local main but not the fetched upstream.
    pub main_ahead_upstream_commits: u64,
    /// Commits reachable from the fetched upstream but not local main.
    pub main_behind_upstream_commits: u64,
    /// Conclusive, read-only classification when integration does not
    /// contain the currently fetched upstream.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub integration_reconciliation: Option<crate::IntegrationDriftAssessment>,
    pub cleanup_retention: CleanupRetention,
    /// Reviews a provider refused and nothing has re-asked for since.
    pub review_refusals: Vec<ReviewRefusalView>,
    /// Every current blocker across the broker's stores, each with the one
    /// command that clears it (`aethyme broker blockers`).
    pub blockers: Vec<crate::Blocker>,
    /// Stores the blocker collection could not read, so an empty `blockers`
    /// is never mistaken for a complete answer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocker_sources_unavailable: Vec<crate::BlockerSourceError>,
    /// Committed work only this machine holds: live sessions' unpushed
    /// commits and integration commits upstream lacks. Omitted when empty.
    #[serde(skip_serializing_if = "crate::UnpushedWorkReport::is_empty")]
    pub unpushed_work: crate::UnpushedWorkReport,
    /// `broker submit` runs in progress, with phase, queue position and last
    /// progress, so a waiting submit can be told from a stuck one. Omitted
    /// when none is running.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub in_flight_submits: Vec<crate::InFlightSubmit>,
}

/// How many refused reviews `broker status` carries.
///
/// `broker status` is mandated as every session's first command, so its cost
/// is a tax on every agent (#182). The newest refusals are the ones an
/// operator can still act on; `aethyme broker review ledger` has the rest.
const REVIEW_REFUSAL_STATUS_LIMIT: usize = 20;

/// One refused review, with the cause stated rather than reconstructed.
///
/// #173: a provider refusal was discarded, so `pending or stale: Security
/// Review` was all an operator ever saw -- the same row a review that was
/// never requested produces, and the same row one still running produces.
/// Ten pull requests in `Aeptus/mockup` sat unmergeable for roughly 48 hours
/// on 2026-09-11 because the difference was only ever found by inference.
///
/// [`Self::class`] says whether waiting helps. [`Self::text`] is what the
/// provider actually said, kept beside the classification rather than
/// replaced by it: the scrape misfires whenever a provider rewords a message,
/// and a misfire must cost precision, never the evidence.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReviewRefusalView {
    pub repository: String,
    pub pull_request: i64,
    pub review_type: String,
    pub head_commit: String,
    pub class: crate::RefusalClass,
    pub text: String,
    /// When the row was moved to `abandoned` -- the moment of the refusal.
    pub refused_at: i64,
}

/// The two fields an agent needs before it starts work, without the
/// per-session diff that dominates the cost of the full view.
///
/// `CLAUDE.md` in a consuming repository mandates `broker status` as the first
/// step of every session, which makes its cost a tax on every agent. The
/// expensive part is recomputing implicit leases: that walks each live
/// session's worktree with two git subprocesses, so it scales with accumulated
/// worktree state rather than with anything the caller asked for -- worst
/// exactly when a fleet is busiest. A reporter measured 2m54s at 19 live
/// sessions, of which only 6.2s was user time (#182).
///
/// This is also small enough to truncate safely. The full document was 377867
/// bytes in that report, so piping it through `head` is a well-founded reflex
/// that silently yields unparseable JSON.
#[derive(Debug, serde::Serialize)]
pub struct StatusBrief {
    pub deferred_checks: Vec<String>,
    pub leases_refreshed_at_ms: Option<i64>,
    pub phase_timings_ms: std::collections::BTreeMap<String, u64>,
    pub summary: StatusSummary,
    pub advice: Vec<StatusAdvice>,
    /// Always false, and serialized rather than implied: `overlap_count` and
    /// `dirty_sessions` here are as of the last refresh by any command, not as
    /// of this call. A caller that needs current lease truth wants the full
    /// view, and should be able to see which one it got.
    pub leases_refreshed: bool,
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
    /// Relation to `baseline_ref`, the published branch -- not to the local
    /// checkout, which `integration status` counts against.
    pub integration_relation: StatusIntegrationRelation,
    pub integration_ahead_main_commits: u64,
    pub integration_head: String,
    pub baseline_ref: String,
    pub baseline_head: String,
    /// The main checkout's HEAD and integration's lead over it: the same
    /// pair `integration status` reports as `main_head` and
    /// `commits_ahead_main`, so the two views can be compared like for like
    /// (#374). After a local fast-forward that has not been pushed, this is 0
    /// while `integration_ahead_main_commits` still counts the unpublished
    /// commits.
    pub main_head: String,
    pub integration_ahead_local_main_commits: u64,
    pub may_move_integration: bool,
    pub commands: Vec<String>,
}

/// Where integration stands against the published baseline and against the
/// local checkout, as one snapshot for the summary.
#[derive(Debug, Clone)]
struct SummaryIntegration {
    branch: String,
    head: String,
    baseline_ref: String,
    baseline_head: String,
    relation: StatusIntegrationRelation,
    ahead_baseline_commits: u64,
    main_head: String,
    main_is_ancestor: bool,
    ahead_main_commits: u64,
    /// False in a verify-only repository, where no session moves integration.
    promotes: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusIntegrationRelation {
    NotChecked,
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

/// Render lease blockers as "session N (status) holds <path> [kind]".
///
/// The blockers were always attached to this error; only the count was printed,
/// so a reader had to query the JSON to learn whether the holder was even
/// working. Naming the holder and its status is what makes the refusal
/// actionable — a lease held by a stale session is a different problem from one
/// held by a live editor.
fn describe_lease_blockers(blockers: &[LeaseBlocker]) -> String {
    if blockers.is_empty() {
        return "no holder recorded".into();
    }
    let mut shown: Vec<String> = blockers
        .iter()
        .take(4)
        .map(|blocker| {
            let holder = match (
                blocker.holder_context.as_deref(),
                blocker.holder_status.as_deref(),
            ) {
                (Some(context), Some(status)) => format!("{context}; {status}"),
                (Some(context), None) => context.to_string(),
                (None, Some(status)) => status.to_string(),
                (None, None) => "status unknown".to_string(),
            };
            format!(
                "session {} ({}) holds {} [{}]",
                blocker.session_id,
                holder,
                blocker.path,
                blocker.kind.as_str()
            )
        })
        .collect();
    if blockers.len() > shown.len() {
        shown.push(format!("and {} more", blockers.len() - shown.len()));
    }
    shown.join("; ")
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LeaseBlocker {
    pub session_id: i64,
    pub path: String,
    pub kind: LeaseKind,
    /// Holder's session status. Only an explicit lease held by a session that
    /// is actively working can refuse (see `LeaseRefusalPolicy`), so the reader
    /// needs the status to tell a live editor from a stale holder.
    pub holder_status: Option<String>,
    /// Repository, Chau7 tab, and AI provider when the holder supplied them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holder_context: Option<String>,
    /// For the submit audit: whether the two sessions' edits conflict
    /// (`high`) or merge cleanly (`low`). Absent where it was not assessed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    /// Why this lease blocks, or why it only warns.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LeaseClaimReport {
    pub session_id: i64,
    pub path: String,
    pub accepted: bool,
    pub blockers: Vec<LeaseBlocker>,
    /// Other live sessions' leases on the claimed path that did not refuse
    /// the claim, each with the reason and how to coordinate.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<LeaseBlocker>,
}

/// Which other sessions' leases may refuse a lease claim, a submit or a
/// guarded exec. One rule for all three so they cannot drift apart again:
/// #475 made submit inform under verify-only while `leases claim` kept
/// refusing on any overlapping lease, including leases held by stale
/// sessions and implicit leases derived from edits.
pub(crate) struct LeaseRefusalPolicy {
    /// Under verify-only every session delivers through its own pull
    /// request, so a lease informs and never refuses.
    verify_only: bool,
    /// Derived status of each live session (active or idle).
    live: std::collections::HashMap<i64, SessionStatus>,
}

impl LeaseRefusalPolicy {
    /// Only an explicit lease held by a session that is actively working may
    /// refuse, and never under verify-only. Implicit leases are telemetry
    /// derived from edits; stale, exited and closed holders are not working.
    pub(crate) fn may_block(&self, blocker: &LeaseBlocker) -> bool {
        Self::refuses(
            self.verify_only,
            blocker.kind,
            self.live.get(&blocker.session_id).copied(),
        )
    }

    /// The rule itself, for callers that already know the holder's derived
    /// status (planned leases at start). The store's transactional recheck
    /// mirrors it in SQL against the stored status.
    pub(crate) fn refuses(
        verify_only: bool,
        kind: LeaseKind,
        holder: Option<SessionStatus>,
    ) -> bool {
        !verify_only && kind == LeaseKind::Explicit && holder == Some(SessionStatus::Active)
    }

    /// Whether this repository delivers through pull requests (verify-only),
    /// read from the configuration committed on the default branch.
    pub(crate) fn verify_only_at(main_root: &Path) -> bool {
        crate::merge::PromoteConfig::load(main_root).mode == crate::merge::PromoteMode::VerifyOnly
    }

    pub(crate) fn is_live(&self, session_id: i64) -> bool {
        self.live.contains_key(&session_id)
    }

    /// Why a lease that does not refuse is still worth knowing about.
    pub(crate) fn non_blocking_reason(&self, session_id: i64, blocker: &LeaseBlocker) -> String {
        let coordinate = format!(
            "coordinate: aethyme broker advanced note send --session {session_id} \
             --to-session {} --message \"…\"",
            blocker.session_id
        );
        if !self.is_live(blocker.session_id) {
            return format!(
                "held by a session that is not live ({}); its leases never block",
                blocker.holder_status.as_deref().unwrap_or("unknown")
            );
        }
        if self.verify_only {
            return format!(
                "this repository delivers through pull requests (verify-only), so a lease \
                 informs rather than blocks; {coordinate}"
            );
        }
        if blocker.kind == LeaseKind::Implicit {
            return format!(
                "the holder is editing this path; implicit leases never block; {coordinate}"
            );
        }
        format!("the holder is not actively working; {coordinate}")
    }
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
    pub owner_status: SessionStatus,
    pub owner_worktree: String,
    pub owner_activity_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_pid_alive: Option<bool>,
    /// Repository, Chau7 tab, and AI provider when the owner supplied them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_context: Option<String>,
    /// Lease liveness bound to the holder process (#360).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub liveness: Option<crate::lease_liveness::LeaseLiveness>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub liveness_evidence: Option<crate::lease_liveness::LeaseLivenessEvidence>,
    pub safe_next_actions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LeasePathPlan {
    pub path: String,
    pub owned: Vec<LeasePlanOverlap>,
    /// Every other session's lease on this path, refusing or not.
    pub conflicts: Vec<LeasePlanOverlap>,
    /// Whether a claim would be refused now under `LeaseRefusalPolicy`.
    pub would_conflict: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LeasePlan {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<i64>,
    pub paths: Vec<LeasePathPlan>,
    pub would_conflict: bool,
}

pub(crate) fn normalize_lease_path(path: &str) -> Result<String, BrokerOpError> {
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

fn normalize_planned_paths(paths: &[String]) -> Result<Vec<String>, BrokerOpError> {
    let mut normalized = paths
        .iter()
        .map(|path| normalize_lease_path(path))
        .collect::<Result<Vec<_>, _>>()?;
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
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
    /// Other sessions' leases on changed paths that do not block: their
    /// edits merge cleanly, or their holder is not actively working. Each
    /// carries the severity and reason it was judged by.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warned_leases: Vec<LeaseBlocker>,
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
    pub newly_dirty_paths: Vec<String>,
    pub new_untracked_paths: Vec<String>,
    pub modified_preexisting_dirty_paths: Vec<String>,
    pub touched_paths: Vec<String>,
    pub outside_lease_paths: Vec<String>,
    pub foreign_paths: Vec<String>,
    pub ok: bool,
}

fn snapshot_path_identities(
    root: &Path,
    paths: &[String],
) -> Result<std::collections::BTreeMap<String, String>, BrokerOpError> {
    paths
        .iter()
        .map(|path| Ok((path.clone(), working_path_identity(root, path)?)))
        .collect()
}

/// Hash one working-tree path without retaining its contents. Missing paths,
/// symlink targets, file modes, and regular file bytes have distinct identities.
fn working_path_identity(root: &Path, relative: &str) -> Result<String, BrokerOpError> {
    let path = root.join(relative);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok("missing".into());
        }
        Err(source) => {
            return Err(BrokerError::Io {
                path: path.clone(),
                source,
            }
            .into());
        }
    };
    let mut digest = Sha256::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        digest.update(metadata.permissions().mode().to_le_bytes());
    }
    if metadata.file_type().is_symlink() {
        digest.update(b"symlink\0");
        let target = std::fs::read_link(&path).map_err(|source| BrokerError::Io {
            path: path.clone(),
            source,
        })?;
        digest.update(target.to_string_lossy().as_bytes());
    } else if metadata.is_file() {
        digest.update(b"file\0");
        let mut file = File::open(&path).map_err(|source| BrokerError::Io {
            path: path.clone(),
            source,
        })?;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer).map_err(|source| BrokerError::Io {
                path: path.clone(),
                source,
            })?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
    } else {
        digest.update(b"other\0");
        digest.update(metadata.len().to_le_bytes());
        if let Ok(modified) = metadata.modified()
            && let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH)
        {
            digest.update(duration.as_nanos().to_le_bytes());
        }
    }
    Ok(format!("{:x}", digest.finalize()))
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
    /// Commits reachable from local main but not the fetched upstream.
    pub main_ahead_upstream_commits: u64,
    /// Commits reachable from the fetched upstream but not local main.
    pub main_behind_upstream_commits: u64,
    pub main_is_ancestor: bool,
    pub commits_ahead_main: u64,
    pub changed_files: Vec<String>,
    pub promoted_entries: Vec<PromotedIntegrationEntry>,
    pub conflicts: Vec<PromotedConflict>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reconciliation: Option<crate::IntegrationDriftAssessment>,
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
    ReconciliationReady,
    Untracked,
}

impl IntegrationDeliveryState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Promoted => "promoted",
            Self::Published => "published",
            Self::LocallySynchronized => "locally_synchronized",
            Self::Blocked => "blocked",
            Self::ReconciliationReady => "reconciliation_ready",
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
    pub pending_commits: Vec<String>,
    pub submission_plan: crate::SubmissionPlan,
    pub leases_refreshed: bool,
    pub affected_gates: Vec<RepairGateSelection>,
    pub next_command: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionCheckpointRecoveryPlan {
    pub session_id: i64,
    pub old_checkpoint: Option<String>,
    pub proposed_checkpoint: Option<String>,
    pub session_head: String,
    pub integration_branch: String,
    pub integration_head: Option<String>,
    pub integration_relation: Option<AdoptIntegrationRelation>,
    pub ahead_commits: u64,
    pub behind_commits: u64,
    pub pending_commits: Vec<String>,
    pub submission_plan: Option<crate::SubmissionPlan>,
    pub preservation_branch: String,
    pub clean_worktree: bool,
    pub safe: bool,
    pub refusals: Vec<String>,
    pub refusal_codes: Vec<CheckpointRefusalCode>,
    pub next_actions: Vec<CheckpointRecoveryAction>,
    pub digest: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointRefusalCode {
    DirtyWorktree,
    MissingAcceptedCheckpoint,
    MissingAcceptedCheckpointObject,
    NoReanchorRequired,
    IntegrationNotAncestor,
    MissingAcceptedIntegrationProof,
    AcceptedIntegrationNotContained,
    SubmissionProvenanceUnsafe,
    SubmissionProvenanceUnavailable,
    MissingIntegrationRef,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CheckpointRecoveryAction {
    pub kind: String,
    pub command: String,
    pub description: String,
    pub mutates_repository: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionCheckpointApplyReport {
    pub plan: SessionCheckpointRecoveryPlan,
    pub applied: bool,
    pub accepted_session_head: String,
    pub preservation_ref: String,
}

fn record_checkpoint_refusal(
    refusals: &mut Vec<String>,
    refusal_codes: &mut Vec<CheckpointRefusalCode>,
    code: CheckpointRefusalCode,
    message: impl Into<String>,
) {
    refusals.push(message.into());
    refusal_codes.push(code);
}

fn finish_checkpoint_recovery_plan(
    mut plan: SessionCheckpointRecoveryPlan,
) -> Result<SessionCheckpointRecoveryPlan, BrokerOpError> {
    if plan.next_actions.is_empty() {
        plan.next_actions = checkpoint_recovery_actions(&plan);
    }
    plan.digest.clear();
    let bytes = serde_json::to_vec(&plan)?;
    plan.digest = format!("{:x}", Sha256::digest(bytes));
    Ok(plan)
}

fn checkpoint_recovery_actions(
    plan: &SessionCheckpointRecoveryPlan,
) -> Vec<CheckpointRecoveryAction> {
    if plan.safe {
        return vec![CheckpointRecoveryAction {
            kind: "apply_reviewed_plan".into(),
            command: format!(
                "aethyme broker advanced checkpoint apply --session {} --confirm <plan-digest>",
                plan.session_id
            ),
            description: "Apply only after reviewing this exact digest-bound plan.".into(),
            mutates_repository: true,
        }];
    }

    let mut actions = vec![CheckpointRecoveryAction {
        kind: "preserve_session_tip".into(),
        command: format!(
            "git branch {} {}",
            plan.preservation_branch, plan.session_head
        ),
        description: "Preserve the complete session tip before any history rewrite.".into(),
        mutates_repository: true,
    }];
    if let Some(integration) = plan.integration_head.as_deref() {
        actions.push(CheckpointRecoveryAction {
            kind: "inspect_divergence".into(),
            command: format!(
                "git log --graph --oneline --decorate --boundary {integration}...{}",
                plan.preservation_branch
            ),
            description:
                "Review both histories; do not blanket-rebase onto integration because it may contain unrelated promoted work."
                    .into(),
            mutates_repository: false,
        });
    }
    actions.push(CheckpointRecoveryAction {
        kind: "start_clean_replay_session".into(),
        command: format!(
                    "aethyme broker start --task \"recover session {} from {}\" --short-name \"Recovery\"",
            plan.session_id, plan.preservation_branch
        ),
        description:
            "Replay only reviewed session-owned commits into the new worktree, then submit normally."
                .into(),
        mutates_repository: true,
    });
    actions
}

/// Outcome class for `broker finish --session`: a human lifecycle helper
/// that only mutates state when the session is safe to close.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishStatus {
    Blocked,
    Closed,
    Cleaned,
    AlreadyClosed,
    AlreadyCleaned,
}

impl FinishStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Closed => "closed",
            Self::Cleaned => "cleaned",
            Self::AlreadyClosed => "already_closed",
            Self::AlreadyCleaned => "already_cleaned",
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FinishOptions {
    pub keep_worktree: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct FinishCleanupReport {
    pub requested: bool,
    pub kept: bool,
    pub attempted: bool,
    pub completed: bool,
    pub reclaimed_bytes: u64,
    pub worktree_removed: bool,
    pub branch_removed: bool,
    pub branch_ref: Option<String>,
    pub branch_tip: Option<String>,
    pub failure: Option<String>,
    pub recovery_action: Option<String>,
}

/// What a representation scan found for one session.
#[derive(Debug, serde::Serialize)]
pub struct RepresentationScan {
    pub session_id: i64,
    pub session_head: String,
    pub base: String,
    pub branch: String,
    pub branch_ref: String,
    pub branch_tip: String,
    /// Paths the session changed, in the order the content comparison uses.
    pub changed_paths: Vec<String>,
    pub search: crate::LandingSearch,
    /// A record already stored for this exact head, if any.
    pub existing: Option<crate::types::SessionRepresentation>,
    pub digest: String,
}

impl RepresentationScan {
    pub fn recorded(&self) -> bool {
        self.existing.is_some()
    }

    pub fn paths(&self) -> usize {
        self.changed_paths.len()
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
    /// Set when this HEAD's work reached the default branch through a
    /// provider-side merge instead of through submit.
    pub representation: Option<SessionRepresentation>,
    pub pending_work: FinishPendingWork,
    pub leases_held: Vec<FinishLease>,
    pub last_gate: Option<FinishGateRun>,
    pub last_graph_integrity: Option<FinishGraphIntegrity>,
    /// Commits on HEAD that no remote holds. Present only when the repository
    /// opted into the push lane (`[delivery] push_session_branches`) and
    /// finish reached that check.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unpushed_commits: Option<u32>,
    pub cleanup_safe: bool,
    pub cleanup: FinishCleanupReport,
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct FinishGraphIntegrity {
    pub status: crate::GraphIntegrityStatus,
    pub tree_hash: String,
    pub policy_digest: String,
    pub engine_version: Option<String>,
    pub changed_paths: Vec<String>,
    /// Unix epoch milliseconds from the event ledger.
    pub recorded_at: i64,
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
    #[serde(default)]
    pub representing_commit: Option<String>,
    pub leases_held: Vec<FinishLease>,
    pub last_gate: Option<FinishGateRun>,
    #[serde(default)]
    pub last_graph_integrity: Option<FinishGraphIntegrity>,
    pub cleanup_safe: bool,
    #[serde(default)]
    pub cleanup: FinishCleanupHandoff,
    pub recommended_next_action: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct FinishCleanupHandoff {
    pub requested: bool,
    pub kept: bool,
    pub attempted: bool,
    pub completed: bool,
    pub reclaimed_bytes: u64,
    pub worktree_removed: bool,
    pub branch_removed: bool,
    pub recovery_action: Option<String>,
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
            representing_commit: report
                .representation
                .as_ref()
                .and_then(|record| record.representing_commit.clone()),
            leases_held: report.leases_held.clone(),
            last_gate: report.last_gate.clone(),
            last_graph_integrity: report.last_graph_integrity.clone(),
            cleanup_safe: report.cleanup_safe,
            cleanup: FinishCleanupHandoff {
                requested: report.cleanup.requested,
                kept: report.cleanup.kept,
                attempted: report.cleanup.attempted,
                completed: report.cleanup.completed,
                reclaimed_bytes: report.cleanup.reclaimed_bytes,
                worktree_removed: report.cleanup.worktree_removed,
                branch_removed: report.cleanup.branch_removed,
                recovery_action: report.cleanup.recovery_action.clone(),
            },
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
    pub mode: GraphImpactMode,
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

/// Work a verify-only repository's integration branch carries that the
/// published branch lacks.
///
/// Nothing moves integration in verify-only mode, so no session owns such
/// commits: they are left over from when the repository promoted. Before this
/// row they surfaced only after a pull-request merge, as a deferred post-merge
/// cleanup demanding reviewed reconciliation, and the `integration.*` rows above
/// are gated to promoting repositories and to `--refresh`. The check is one
/// ancestry test (plus a count when it fails), so it runs on every status;
/// the patch-level classification is added only when a refreshed status has
/// already computed it.
/// The `status` row summarizing the last unattended auto-cleanup pass (#588):
/// what it removed with which proof, and how many closed checkouts it kept.
/// Omitted until a pass has run or when it found nothing to report.
fn push_auto_cleanup_advice(view: &mut StatusView, report: Option<crate::AutoCleanupReport>) {
    let Some(report) = report else {
        return;
    };
    if report.removed.is_empty() && report.kept.is_empty() && report.config_error.is_none() {
        return;
    }
    let mut evidence = report
        .removed
        .iter()
        .map(|removed| {
            format!(
                "removed {} (sessions {:?}) by {} against {} at {}",
                removed.worktree,
                removed.sessions,
                removed.proof,
                removed.contained_in,
                short_commit(&removed.containing_commit)
            )
        })
        .collect::<Vec<_>>();
    evidence.extend(
        report
            .kept
            .iter()
            .map(|kept| format!("kept {}: {}", kept.worktree, kept.reason)),
    );
    if let Some(error) = &report.config_error {
        evidence.push(format!(
            "[cleanup] is invalid, auto-removal is off: {error}"
        ));
    }
    view.advice.push(StatusAdvice {
        id: "cleanup.auto-removal",
        severity: if report.config_error.is_some() {
            StatusAdviceSeverity::Warning
        } else {
            StatusAdviceSeverity::Info
        },
        reason: "the unattended sweep removes closed checkouts whose work is provably on the remote default branch",
        summary: format!(
            "last auto-cleanup removed {} checkout(s), kept {}, deferred {}",
            report.removed.len(),
            report.kept.len(),
            report.deferred
        ),
        session_id: None,
        queue_entry_id: None,
        evidence,
        commands: vec!["aethyme broker gc plan --json".into()],
    });
}

/// The `status` row for an integration branch this very call advanced (#352):
/// the action the old `fast-forward-available` notice asked an operator for,
/// reported as done.
fn push_integration_refresh_advice(
    view: &mut StatusView,
    refresh: Option<crate::IntegrationRefresh>,
) {
    let Some(refresh) = refresh else {
        return;
    };
    view.advice.insert(
        0,
        StatusAdvice {
            id: "integration.refreshed",
            severity: StatusAdviceSeverity::Notice,
            reason: "a verify-only integration branch carried nothing of its own and had fallen behind",
            summary: format!(
                "{} advanced from {} to {} ({}): {}",
                refresh.branch,
                short_commit(&refresh.from),
                short_commit(&refresh.to),
                refresh.upstream_ref,
                refresh.explanation
            ),
            session_id: None,
            queue_entry_id: None,
            evidence: vec![
                format!("from: {}", short_commit(&refresh.from)),
                format!("{}: {}", refresh.upstream_ref, short_commit(&refresh.to)),
            ],
            commands: Vec::new(),
        },
    );
}

fn leftover_integration_advice(
    repo: &GitRepo,
    integration_head: &str,
    (baseline_ref, baseline_head): (&str, &str),
    assessment: Option<&crate::IntegrationDriftAssessment>,
) -> Result<Option<StatusAdvice>, BrokerOpError> {
    // `HEAD` means no `origin/HEAD` names a published branch; measuring
    // against the checkout would call every unpublished commit leftover.
    let tracked;
    let (baseline_ref, baseline_head) = if baseline_ref == "HEAD" {
        let Some(upstream) = repo.tracking_upstream() else {
            return Ok(None);
        };
        tracked = upstream;
        (tracked.0.as_str(), tracked.1.as_str())
    } else {
        (baseline_ref, baseline_head)
    };
    if integration_head == baseline_head || repo.is_ancestor(integration_head, baseline_head) {
        return Ok(None);
    }
    let count = repo.commit_count_between(baseline_head, integration_head)?;
    let upstream = baseline_ref
        .strip_prefix("refs/remotes/")
        .or_else(|| baseline_ref.strip_prefix("refs/heads/"))
        .unwrap_or(baseline_ref);
    let stale_only = assessment.filter(|assessment| assessment.stale_only);
    let mut summary = format!(
        "integration carries {count} {} {upstream} lacks; this repository is verify-only, so no \
         session owns {} and the next pull-request merge will defer integration cleanup",
        plural_word(count as usize, "commit", "commits"),
        if count == 1 { "it" } else { "them" }
    );
    if let Some(assessment) = assessment {
        summary.push_str("; ");
        summary.push_str(&assessment.explanation);
    }
    Ok(Some(StatusAdvice {
        id: "integration.leftover-work",
        severity: if stale_only.is_some() {
            StatusAdviceSeverity::Notice
        } else {
            StatusAdviceSeverity::Warning
        },
        reason: "a verify-only integration branch holds commits the published branch lacks",
        summary,
        session_id: None,
        queue_entry_id: None,
        evidence: vec![
            format!("integration: {}", short_commit(integration_head)),
            format!("{upstream}: {}", short_commit(baseline_head)),
        ],
        commands: vec![format!(
            "aethyme broker advanced integration reconcile --upstream {upstream} --dry-run"
        )],
    }))
}

/// One warning per submit that is gone or has made no progress for
/// [`crate::SUBMIT_STALL_AFTER`]. A submit that is only waiting in line keeps
/// reporting and never appears here.
fn stalled_submit_advice(submits: &[crate::InFlightSubmit]) -> Vec<StatusAdvice> {
    submits
        .iter()
        .filter(|submit| submit.possibly_stalled)
        .map(|submit| {
            let silent = crate::submit_progress::duration_label(submit.last_progress_age_ms);
            StatusAdvice {
                id: "submit.possibly-stalled",
                severity: StatusAdviceSeverity::Warning,
                reason: "a broker submit has stopped reporting progress",
                summary: if submit.alive {
                    format!(
                        "session {}'s submit has made no progress for {silent} (phase: {})",
                        submit.session_id, submit.phase
                    )
                } else {
                    format!(
                        "session {}'s submit process {} is gone; it stopped during: {}",
                        submit.session_id, submit.pid, submit.phase
                    )
                },
                session_id: Some(submit.session_id),
                queue_entry_id: None,
                evidence: vec![format!("last progress: {}", submit.last_progress)],
                commands: vec![format!(
                    "aethyme broker submit --session {}",
                    submit.session_id
                )],
            }
        })
        .collect()
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
    host_operation_db_path: Option<PathBuf>,
    worktree_root_override: Option<PathBuf>,
    /// Set only while a bounded eligibility pass runs: the landing search
    /// stops at it instead of finishing one expensive worktree long past the
    /// pass's budget (#460).
    landing_deadline: std::cell::Cell<Option<std::time::Instant>>,
    /// The main repository's Git common directory, read once: ownership
    /// checks ask for it twice per closed worktree, and each ask was a
    /// `rev-parse` subprocess (#460). Only a successful read is kept.
    main_common_dir: std::cell::OnceCell<PathBuf>,
}

mod cleanup;
mod finish;
mod gates;
mod leases;
mod lifecycle;
mod scope_capture;
mod status;
mod status_helpers;

pub use scope_capture::ScopeCaptureReport;
use scope_capture::derive_scopes_from_task;
pub(super) use status_helpers::*;

#[cfg(test)]
mod pid_liveness_tests;
#[cfg(test)]
mod tests;
