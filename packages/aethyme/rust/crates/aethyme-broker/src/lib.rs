//! Local agent broker — operational state model (Phase 1).
//!
//! This crate owns the broker's SQLite store at `<repo>/.aethyme/broker.db`:
//! sessions, leases, gates, gate results, the merge queue, and the
//! append-only event log. It is deliberately a **library first**: every
//! operation and query is a typed method on [`BrokerStore`]; the CLI (and
//! any later TUI) are thin clients and must never touch the database
//! directly.
//!
//! Design contract (see `docs/aethyme-local-agent-broker.md` at the repo
//! root):
//! - Broker state never goes into the graph schema; the graph engine's
//!   redb/fragment storage is a separate, read-only advisory concern.
//! - Sessions are identified by their worktree (attach-first): `pid` and
//!   `command` are optional metadata for spawned sessions only.
//! - The events table is append-only and carries a `schema_version` per
//!   row from day one — it is the versioned integration contract for any
//!   future surface.
//! - Many concurrent CLI *processes* coordinate through this database:
//!   WAL mode + busy_timeout, no daemon. Designed for 15 concurrent
//!   sessions, stress-tested at 20 (see `tests/stress.rs`).

mod advisories;
mod atomic_file;
pub mod agent_hook;
pub mod attribution;
mod broker;
mod chau7_tabs;
mod disk_headroom;
pub use disk_headroom::{
    DEFAULT_GATE_HEADROOM_BYTES, available_bytes, refusal as disk_headroom_refusal,
};
pub mod cli;
mod cli_output;
mod console;
pub mod contract_check;
mod delivery;
mod error;
pub mod events;
mod exposures;
mod external_events;
mod gate_doctor;
mod gates;
mod gc;
mod git;
mod github_target;
mod graph_impact;
mod graph_integrity;
mod homebrew;
pub mod hooks;
mod host_operations;
mod host_state;
pub mod init;
pub mod install_health;
mod issue_form;
mod lease_export;
mod leases;
pub mod main_reconcile;
mod measurement;
mod merge;
mod operation_stats;
mod operations;
pub mod plugin_cli;
mod pr;
mod pr_link;
mod pr_monitoring;
pub use pr_monitoring::{
    activate as activate_pr_monitoring, active_sessions as pr_monitoring_sessions,
    deactivate as deactivate_pr_monitoring, is_active as pr_monitoring_is_active,
};
mod pr_projection;
mod pr_watch;
mod preparation;
mod promotion_record;
mod quick_test;
mod readiness;
mod recommendations;
mod reconciliation;
mod release_compatibility;
pub mod release_manifest;
mod remote_target;
mod removal;
pub mod renamed_targets;
mod report;
mod report_filing;
pub use chau7_tabs::{
    Chau7DispatchAction, Chau7Resolution, Chau7ResolutionRefusal, Chau7Tab, Chau7TabReadiness,
    dispatch_action, resolve_session_tab, workspace_tab_ids,
};
pub use pr_link::{creates_pull_request, pull_request_number_from_output};
mod reclaim;
mod reclaim_order;
pub use reclaim::{
    ReclaimCandidate, ReclaimOutcome, ReclaimPlan, apply as apply_reclaim,
    classify as classify_reclaim, directory_bytes, is_artefact_directory,
    is_artefact_directory_with_extras, is_within as reclaim_is_within, reclaimable_bytes,
    scan as scan_reclaim, scan_with_extra_directories as scan_reclaim_with_extra_directories,
};
pub mod representation;
pub use representation::{
    ContentVerdict, Landing, LandingOutcome, LandingSearch, SessionContent,
    content_at as representation_content_at, find_landing,
    plan_digest as representation_plan_digest, session_content,
};
mod repository_contract;
mod resources;
mod retention;
mod review;
mod review_backend;
mod review_execution;
mod review_facts;
mod review_ledger;
mod review_report;
mod review_trigger;
mod schema;
mod session_abandonment;
mod ship;
mod store;
mod storage;
mod types;
mod update;
mod update_cache;
mod verification;
mod verify_loop;
mod version;
mod worktree_reconcile;

pub use aethyme_graph_storage::{
    GRAPH_CONFIG_RELPATH, GraphAuthority, GraphIntegrityPolicy, GraphIntegrityPolicyError,
};
pub use attribution::{Attribution, Identity};
pub use broker::{
    AdoptIntegrationDrift, AdoptIntegrationRelation, AdoptIntegrationSync,
    AdoptIntegrationSyncOutcome, AdoptMode, AdoptOptions, AdoptOutcome, AdoptReport, AgentView,
    Broker, BrokerOpError, CLEANUP_PLAN_SCHEMA_VERSION, CheckpointRecoveryAction,
    CheckpointRefusalCode, CleanupDisposition, CleanupPlan, CleanupProvenance,
    CleanupRepresentation, CleanupRetention, CleanupSweepFailure, CleanupSweepReport,
    CleanupWorktreePlan, DoctorRepairStatus, DoctorReport, FinishCleanupHandoff,
    FinishCleanupReport, FinishDelivery, FinishGateCacheSource, FinishGateRun,
    FinishGraphIntegrity, FinishHandoff, FinishLease, FinishLeaseState, FinishOptions,
    FinishPendingWork, FinishReport, FinishStatus, GuardedExecReport, IntegrationDeliveryState,
    IntegrationLiveSession, IntegrationMovementNotice, IntegrationNextAction,
    IntegrationStabilityReport, IntegrationStatusView, LeaseBlocker, LeaseClaimReport,
    LeaseOverlapRelation, LeasePathPlan, LeasePlan, LeasePlanOverlap, OwnershipAuditReport,
    PromotedConflict, PromotedIntegrationEntry, RepairAction, RepairGateSelection, RepairReport,
    RepairSource, RepresentationScan, RetentionConfigStatus, SESSION_NOTE_MAX_BYTES, SemanticGateAdvice,
    SemanticGateSelection, SemanticGateSource, SemanticGateSourceStatus,
    SemanticGateSuggestionChain, SessionCheckpointApplyReport, SessionCheckpointRecoveryPlan,
    SessionHandoffReport, SessionStartBase, SessionStartBaseEvidence, StartAgentReport,
    StartReport, StatusAdvice, StatusAdviceSeverity, StatusBrief, StatusIntegrationRelation,
    StatusSummary, StatusView, VersionRepairReport, VersionRepairStep, WORKTREE_ROOT_SCHEMA_VERSION,
    WorktreePlacement, WorktreeRootPlan, WorktreeRootSource,
};
pub use console::{
    CONSOLE_EXCLUSIVE_KEY, CONSOLE_INTEGRATION_REF, CONSOLE_MARKER_DIGEST_ENV, CONSOLE_MARKER_ENV,
    CONSOLE_MARKER_SCHEMA_VERSION, CONSOLE_NAMESPACE_KEY, CONSOLE_PORT_KEY, CONSOLE_SLOT_KEY,
    ConsoleConfig, ConsoleIdentity, ConsoleIntegrationRelation, ConsoleMarkerRecord, ConsoleMode,
    ConsoleRevision, ConsoleRuntimeMarker, DEFAULT_CONSOLE_POOL_LIMIT, DEFAULT_CONSOLE_PORT,
    DEFAULT_CONSOLE_PORT_END, DEFAULT_CONSOLE_TTL_SECONDS, console_identity, console_leases,
    console_marker_directory, console_marker_for_lease, console_port, console_request,
    console_request_with_options, console_revision, read_console_markers, remove_console_marker,
    worktree_fingerprint, write_console_marker,
};
pub use delivery::{
    DEFAULT_DELIVERY_CLAIM_SECONDS, DELIVERY_ADAPTER_PROTOCOL_VERSION,
    DELIVERY_OUTBOX_SCHEMA_VERSION, DeliveryClaimReport, DeliveryCompletion, DeliveryEnvelope,
    DeliveryError, DeliveryOutboxItem, DeliveryPolicy, DeliveryStatus, DeliverySubscription,
};
pub use error::BrokerError;
pub use exposures::{
    AdvisoryReconciliationItem, EXPOSURE_RECONCILIATION_SCHEMA_VERSION,
    ExposureReconciliationApplyReport, ExposureReconciliationPlan, ExposureRemainingItem,
};
pub use external_events::{
    EXTERNAL_EVENT_MAX_AGE_MS, EXTERNAL_EVENT_SCHEMA_VERSION, ExternalEventEnvelope,
    ExternalEventError, ExternalEventIngestReport, ExternalEventKind,
    ExternalEventOwnershipCandidate, ExternalEventProvider, ExternalEventReconcileReport,
    ExternalEventReconciliation, ExternalEventRecord, ExternalEventStatus,
    ExternalVerificationMethod, VerifiedExternalSource, external_event_digest,
};
pub use gate_doctor::{
    GateDiagnostic, GateDiagnosticConfidence, GateDiagnosticId, GateDiagnosticSeverity,
    GateDoctorError, GateDoctorGate, GateDoctorReport, GateProbeMutations, GateProbeOutcome,
    GateProbeReport, GateProbeWorktree, inspect_gate_quality, probe_gate_quality,
    static_gate_diagnostics,
};
pub use gates::{
    CachePolicy, GATE_SCOPE_MANIFEST_SCHEMA_VERSION, GATES_CONFIG_RELPATH, Gate, GateConfigError,
    GateProgressSink, GateResourceProvenance, GateRunOutcome, GateScopeDefinition, GateScopeError,
    GateScopeEvaluation, GateScopeManifest, GateScopeSelection, GraphIntegrityScopeContract,
    PrePushPlan, PrePushReport, PrePushUpdate, PrePushValidationError, SemanticGateScopeContract,
    evaluate_gate_scope, evaluate_gate_scope_with_graph, gate_scope_manifest,
    gate_scope_manifest_with_graph, load_gates, load_gates_at_commit, parse_gates, plan_pre_push,
    select_gates, verify_gate_scope_manifest,
};
pub use gc::{GC_PLAN_SCHEMA_VERSION, UNCLASSIFIED_ARTIFACT_REPORT_THRESHOLD_BYTES};
pub use git::{GitError, GitRepo, MergeSimulation, RemoteDefaultBranch};
pub use github_target::{
    GithubApiTargetEvidence, GithubTargetError, ResolvedGithubTarget, resolve_github_target,
};
pub use graph_impact::{
    GRAPH_IMPACT_MAX_DEPTH, GRAPH_IMPACT_MAX_NODES, GRAPH_IMPACT_RESULT_LIMIT, GraphImpactChain,
    GraphImpactLookup, GraphImpactMode, GraphImpactProvider, GraphImpactQuery, GraphImpactStatus,
    GraphStoreImpactProvider,
};
pub use graph_integrity::{GraphIntegrityOutcome, GraphIntegrityRejection, GraphIntegrityStatus};
pub use homebrew::render_homebrew_formula;
pub use hooks::{HookReport, HookSnippet, HookState, HooksError};
pub use host_operations::{
    HostOperation, HostOperationError, HostOperationGuard, default_host_operation_db_path,
    host_operation, reconcile_host_operation,
};
pub use issue_form::{
    ISSUE_FORM_RENDER_SCHEMA_VERSION, ISSUE_REVIEW_ARTIFACT_SCHEMA_VERSION, IssueFormFieldKind,
    IssueFormFieldStatus, IssueFormRenderResult, IssueFormRenderedField, IssueFormWriteResult,
    render_issue_form, write_issue_form_render_atomic,
};
pub use lease_export::{
    DEFAULT_LEASE_ROUTING_EXPORT_LIMIT, LEASE_ROUTING_EXPORT_SCHEMA_VERSION,
    LeaseExportConflictState, LeaseExportPathKind, LeaseExportRepository, LeaseExportSelector,
    LeaseExportState, LeaseRoutingConfiguration, LeaseRoutingExport, LeaseRoutingExportError,
    LeaseRoutingExportOptions, LeaseRoutingItem, MAX_LEASE_ROUTING_EXPORT_LIMIT,
};
pub use leases::{LeaseIgnoreRules, Overlap, detect_overlaps};
pub use main_reconcile::{
    MAIN_RECONCILE_SCHEMA_VERSION, MainReconcileApplyReport, MainReconcileCommit,
    MainReconcileDisposition, MainReconcilePlan, MainReconcileResolution,
    MainReconcileResolutionDocument, MainReconcileResolutionTemplate,
};
pub use merge::{
    ACTION_REQUIRED_RELPATH, PromoteConfig, SubmissionCommitOwnership, SubmissionCommitProvenance,
    SubmissionConflict, SubmissionGateVerification, SubmissionGateVerificationStatus,
    SubmissionIntegrationState, SubmissionPlan, SubmitOutcome,
};
pub use operation_stats::{
    DEFAULT_OPERATION_STATS_LIMIT, HooksOutsideLockStats, MAX_OPERATION_STATS_LIMIT,
    OPERATION_STATS_SCHEMA_VERSION, OperationKindStats, OperationQueueDepthStats, OperationStats,
    OperationTimingDistribution, RefDeterminationStats, UnrelatedContentionStats,
};
pub use operations::{
    CoordinatedCommand, CoordinatedOperationReport, OperationReconcileReport,
    OperationReconciliation, OperationReconciliationRecovery, OperationReconciliationState,
    OperationShowReport, PostMergeCleanupReport, PostMergeCleanupState, QueueWait,
    UnknownOutcomeRecovery, classify_gh, classify_git,
};
pub use pr::{
    PrActivityItem, PrCheckOptions, PrCheckReport, PrCheckRun, PrDecision, PrDecisionStatus,
    PrDispatchReport, PrDispatchStatus, PrError, PrMarker, PrSummary,
};
pub use pr_projection::{
    COMMENT_MARKER, OwnedComment, PR_PROJECTION_SCHEMA_VERSION, PrProjectionAction,
    PrProjectionError, PrProjectionFacts, PrProjectionPolicy, ProjectedReview,
    ProjectedReviewState, ReviewProjection, find_owned_comment, project, render_comment,
    rest_comment_id,
};
pub use pr_watch::{
    DEFAULT_PR_SCHEDULER_LIMIT, DEFAULT_PR_WATCH_INTERVAL_SECONDS,
    GithubCliPullRequestWatchProvider, MAX_PR_SCHEDULER_LIMIT, NewPullRequestWatch,
    PULL_REQUEST_SCHEDULER_SCHEMA_VERSION, PULL_REQUEST_WATCH_SCHEMA_VERSION, PullRequestActivity,
    PullRequestActivityBatch, PullRequestActivityKind, PullRequestActivityMetadata,
    PullRequestBatchAckOutcome, PullRequestBatchStatus, PullRequestSchedulerDisposition,
    PullRequestSchedulerTickReport, PullRequestSchedulerWatchResult, PullRequestSnapshot,
    PullRequestWatch, PullRequestWatchError, PullRequestWatchPollReport, PullRequestWatchProvider,
    PullRequestWatchRequest, PullRequestWatchStatus,
};
pub use preparation::{
    PREPARATION_CONFIG_RELPATH, PREPARATION_SCHEMA_VERSION, PreparationCachePolicy,
    PreparationConfig, PreparationError, PreparationReport, PreparationState, PreparationStatus,
    PreparationStep, PreparationStepResult, RuntimeProbe,
};
pub use promotion_record::{
    PROMOTION_RECORD_PLAN_SCHEMA_VERSION, PromotionRecordApplyReport, PromotionRecordPlan,
    UnrecordedPromotion,
};
pub use quick_test::{
    Chau7Probe, QuickTestError, QuickTestGateOutcome, QuickTestGateReport, QuickTestMode,
    QuickTestOptions, QuickTestReport, QuickTestStep, run_broker_quick_test,
    run_broker_quick_test_with_options,
};
pub use readiness::{
    READINESS_SCHEMA_VERSION, ReadinessAction, ReadinessDimension, ReadinessDimensionId,
    ReadinessEvidence, ReadinessFinding, ReadinessReport, ReadinessState, RepositoryOperatingMode,
    RepositoryReadinessMode, inspect_repository_readiness, render_readiness_json,
    render_readiness_text,
};
pub use recommendations::{
    MaintainerRecommendation, RECOMMENDATION_DURATION_MIN_SAMPLES,
    RECOMMENDATION_GATE_HISTORY_LIMIT, RECOMMENDATION_HISTORY_LIMIT, RECOMMENDATION_MIN_SAMPLES,
    RECOMMENDATION_SCHEMA_VERSION, RECOMMENDATION_SLOW_MEDIAN_MS, RECOMMENDATION_SLOW_P95_MS,
    RecommendationConfidence, RecommendationKind, derive_conflict_recommendations,
    derive_gate_recommendations, derive_isolation_resource_recommendations,
};
pub use reconciliation::{
    AutomaticIntegrationCleanupReport, AutomaticIntegrationCleanupState,
    IntegrationDriftAssessment, IntegrationDriftEntry, IntegrationDriftEntryState,
    IntegrationReconcileClassification, IntegrationReconcileCommit,
    IntegrationReconcileCommitOrigin, IntegrationReconcileEntry, IntegrationReconcileEquivalence,
    IntegrationReconcileOptions, IntegrationReconcilePlan,
    IntegrationReconcileRecordedResolutionEvidence, IntegrationReconcileRecordedResolutionTemplate,
    IntegrationReconcileReport, IntegrationReconcileResolutionAudit,
    IntegrationReconcileResolutionContract, IntegrationReconcileResolutionTemplate,
    IntegrationReconcileResolutionTemplateDocument, IntegrationReconcileUnrecordedDisposition,
    IntegrationReconcileUnrecordedDispositionRule, IntegrationReconcileUnrecordedResolutionAudit,
    IntegrationReconcileUnrecordedResolutionEvidence,
    IntegrationReconcileUnrecordedResolutionTemplate,
};
pub use release_compatibility::{
    BROKER_STORAGE_CURRENT_SCHEMA, BROKER_STORAGE_MINIMUM_SCHEMA, ENGINE_PROTOCOL_VERSION,
    MINIMUM_GIT_VERSION, REPOSITORY_SCHEMA_VERSION,
};
pub use release_manifest::{
    RELEASE_MANIFEST_SCHEMA_VERSION, RELEASE_TARGETS, REQUIRED_RELEASE_BINARIES, ReleaseArtifact,
    ReleaseBrokerStorageCompatibility, ReleaseCompatibility, ReleaseInstaller, ReleaseManifest,
};
pub use remote_target::{
    RemoteAssertionEvidence, RemoteCommandSelectionEvidence, RemoteIdentityEvidence,
    RemoteResolutionEvidence, RemoteTargetError, RemoteUrlEvidence, RemoteUrlSource,
    RemoteUrlSyntax, ResolvedRemoteTarget, resolve_remote_command_target, resolve_remote_target,
};
pub use renamed_targets::RenamedTarget;
pub use report::{
    InvalidReportEntry, PreparedReport, REPORT_DOCUMENT_SCHEMA_VERSION, REPORT_FILINGS_FILENAME,
    REPORT_FILINGS_SCHEMA_VERSION, REPORT_INVENTORY_SCHEMA_VERSION, REPORT_MAX_BYTES,
    REPORT_RECENT_EVENT_LIMIT, REPORT_RECENT_GATE_LIMIT, REPORT_RECENT_OPERATION_LIMIT,
    REPORT_SNAPSHOT_SCHEMA_VERSION, ReportBuild, ReportCaptureError, ReportCaptureResult,
    ReportDocument, ReportEventType, ReportFilingState, ReportGateCacheSource,
    ReportGateObservation, ReportGateProvenance, ReportGraphIntegrity, ReportInspection,
    ReportKind, ReportLastFailure, ReportList, ReportOperation, ReportPlatform, ReportSession,
    ReportSnapshot, ReportSnapshotBuilder, ReportSummary, list_reports, prepare_report,
    show_report, write_report_atomic,
};
pub use report_filing::{
    REPORT_FILE_SCHEMA_VERSION, ReportFileError, ReportFileResult, ReportFileState,
    file_reviewed_report,
};
pub use repository_contract::{
    CANONICAL_REPOSITORY_MARKER_PATH, LOCAL_REPOSITORY_MARKER_PATH, RepositoryContract,
    RepositoryDeploymentMode, detect_repository_mode, repository_managed_paths,
    repository_state_digest,
};
pub use measurement::{
    BudgetVerdict, MeasuredTotal, SizeRecord, SizeRecords, SizeScan, budget_verdict,
};
pub use reclaim_order::{
    ReclaimOrder, ReclaimRanking, clears_budget, deficit_bytes,
    order_for as reclaim_order_for, over_budget,
};
pub use resources::{
    HOST_RESOURCE_REQUEST_SCHEMA_VERSION, HOST_RESOURCE_SCHEMA_VERSION, HostLeaseState,
    HostResourceAllocation, HostResourceConflict, HostResourceCoordinator, HostResourceError,
    HostResourceBlocker, HostResourceExplanation, HostResourceGrant, HostResourceHolder,
    HostResourceKind, HostResourceLease, HostResourcePlan, HostResourceReapLease,
    HostResourceReapReport, HostResourceRequest, HostResourceRequirement, HostResourceRunError,
    HostResourceRunReport, HostResourceWaitAdvice,
    default_host_resource_db_path, resource_environment_key, validate_host_resource_requirements,
};
pub use retention::{
    BROKER_CONFIG_RELPATH, GcApplyReport, GcArtifactCandidate, GcBlocker, GcBlockerSummary,
    GcCheckpointPinRelease, GcDeclinedArtifact, GcFileAction, GcFileCandidate, GcHealth,
    GcOrphanCandidate, GcPlan,
    GcPublicationExposureExpiry, GcRowCandidate, GcRowKind, GcWorktreeCandidate,
    GcWorktreeBlockerSummary,
    RETENTION_POLICY_SCHEMA_VERSION, RetentionConfigError, RetentionConfigWarning, RetentionPolicy,
    RetentionPolicyLoadReport, load_retention_policy, load_retention_policy_report,
};
pub use review::{
    REVIEW_POLICY_SCHEMA_VERSION, ReviewEvidenceAdapter, ReviewLifecycle,
    ReviewLifecycleAbandonReport, ReviewLifecycleReport, ReviewLifecycleState, ReviewPolicy,
    ReviewProvider, ReviewProviderSnapshot, ReviewSatisfactionEvidence, ValidationUnlockAdapter,
    load_review_provider_snapshot,
};
pub use review_backend::{
    InFlightReview, REVIEW_ROUTING_SCHEMA_VERSION, ReviewBackend, ReviewDispatchAction,
    ReviewFallback, ReviewRoute, ReviewRoutingError, ReviewRoutingPolicy, dispatch_review,
    finished_workspaces, review_prompt,
};
pub use review_execution::{
    Chau7Handoff, Chau7Teardown, DeferredReview, GhCall, LedgerWrite, ReviewExecutionPlan,
    plan_execution,
};
pub use review_facts::{
    ProviderPullRequest, PullRequestObservation, derive_trigger, first_time_contributor,
};
pub use review_ledger::{
    ExpiredReview, RefusalClass, ReviewRefusal, ReviewRequest, ReviewRequestState, ReviewVerdict,
    ReviewWaiver, ReviewerIdentity, expired, in_flight, last_refusal, spend_by_type, waiver_for,
};
pub use review_report::{
    REVIEW_REPORTING_SCHEMA_VERSION, ReviewReportingError, ReviewReportingPolicy, ReviewSeverity,
};
pub use review_trigger::{
    ChangeFacts, ClassificationConflict, CommitClassification, EligibleReview,
    REVIEW_TRIGGER_SCHEMA_VERSION, ReviewFreshness, ReviewSchedule, ReviewSpend, ReviewTrigger,
    ReviewTriggerDecision, ReviewTriggerError, ReviewTriggerPolicy, ReviewTriggerRule,
    classification_conflicts, decide, eligible_types, parse_classification, schedule,
};
pub use schema::{EVENTS_SCHEMA_VERSION, SCHEMA_VERSION};
pub use session_abandonment::{
    AbandonmentDecision, AbandonmentVerdict, SessionActivity, abandoned as abandoned_sessions,
    decide as decide_abandonment, survey as survey_abandonment,
};
pub use ship::{
    DeliveryCheck, DeliveryChecksSummary, DeliveryExecutionReport, DeliveryExecutionState,
    DeliveryPullRequest, PUBLICATION_POLICY_SCHEMA_VERSION, PullRequestDeliveryReport,
    REPOSITORY_DELIVERY_POLICY_SCHEMA_VERSION, RepositoryDeliveryConfig, RepositoryDeliveryMode,
    RepositoryDeliveryModeSource, RepositoryDeliverySelection, ShipExecutionReport, ShipFreshness,
    ShipFreshnessResult, ShipLocalMainSync, ShipPlan, ShipPublicationAssessment,
    ShipPublicationAuthorization, ShipPublicationAuthorizationKind, ShipPublicationMode,
    ShipPublicationPolicy, ShipPush, ShipReviewEvidence,
};
pub use store::BrokerStore;
pub use storage::{
    STORAGE_PLAN_SCHEMA_VERSION, STORAGE_RECONCILIATION_SCHEMA_VERSION, StorageApplyFailure,
    StorageApplyReport, StorageAppliedItem, StorageCandidate, StorageCandidateKind,
    StorageDirectoryKind, StorageEntry, StorageError, StorageFilesystemKind, StorageMarkerStatus,
    StoragePlan, StoragePrimaryArtifact, StoragePrimaryCandidate, StoragePrimaryCheckout,
    StorageReconciliation, StorageRoot, StorageSource, StorageSummary,
    storage_apply, storage_plan,
};
pub use types::{
    Advisory, AdvisoryAction, AdvisoryAudience, AdvisoryDeliveryMetric, AdvisoryDeliverySummary,
    AdvisoryDeliverySurface, AdvisoryEvidence, AdvisoryList, AdvisoryProducer,
    AdvisoryResolutionState, AdvisorySeverity, CoordinatedOperation,
    DEFAULT_OPERATION_HISTORY_LIMIT, EntryExposureResolutionKind, EntryExposureState,
    EntryPathExposure, Event, GateDef, GateFailureClass, GateResult, GateStatus, Lease, LeaseKind,
    MAX_OPERATION_HISTORY_LIMIT, MERGE_QUEUE_HISTORY_SCHEMA_VERSION, MergeQueueEntry,
    MergeQueueHistoryPage, MergeQueueStatusCount, MergeStatus, NewAdvisory,
    NewCoordinatedOperation, NewGateResult, NewPrWatchState, NewSession, OperationEffect,
    OperationHistoryPage, OperationHistoryQuery, OperationIdentityProvenance, OperationProvider,
    OperationStatus, PrWatchState, Session, SessionCleanupState, SessionNote, SessionNoteList,
    SessionContext, SessionOrigin, SessionStatus,
};
pub use update::{
    INSTALL_RECEIPT_FILENAME, INSTALL_RECEIPT_SCHEMA_VERSION, InstallReceipt, InstallationMethod,
    InstallationProvenance, UPDATE_PLAN_SCHEMA_VERSION, UpdateAction, UpdateArchive, UpdateChannel,
    UpdateError, UpdateExecutionReport, UpdatePlan, bootstrap_install, build_update_plan,
    current_release_target, detect_installation, execute_confirmed_update, release_target_for,
    run_update_cli, sha256_bytes,
};
pub use verify_loop::{
    VerifyLoopCommandReport, VerifyLoopReport, VerifyLoopStep, VerifyLoopStepStatus,
};
pub use version::{
    BinaryBuild, VersionDriftReport, VersionDriftStatus, current_binary_build, inspect_version,
};
pub use worktree_reconcile::{
    DirectoryClaim, ObservedDirectory, ReconciledDirectory, UnclaimedDirectory,
    WORKTREE_RECONCILIATION_SCHEMA_VERSION, WorktreeReconciliation,
    reconcile as reconcile_worktree_directories, summarise as summarise_worktree_reconciliation,
};

/// Repo-relative location of the broker database.
pub const BROKER_DB_RELPATH: &str = ".aethyme/broker.db";

/// Absolute path that overrides where a repository's broker database lives.
///
/// Broker state is per-repository, and `main_root()` resolves it from the git
/// common directory -- which for a worktree is the *main* checkout. That is
/// deliberate for the product and hostile to a test: a test binary's working
/// directory is its crate directory inside a real checkout, so a CLI it spawns
/// resolved the developer's live database and migrated it (#163). A branch that
/// added a migration therefore bricked every installed binary on the machine the
/// moment its tests ran.
///
/// A harness sets this to a file it owns. Unset -- the only state a shipped
/// binary ever runs in -- resolution is exactly what it was.
pub const BROKER_DB_ENV: &str = "AETHYME_BROKER_DB";

/// Where a repository's broker database lives, honouring [`BROKER_DB_ENV`].
///
/// The override is used verbatim, so a relative value resolves against the
/// process working directory. Deliberate: an env var that silently rewrote the
/// path it was given would be one more place a caller cannot predict what it
/// opened, which is the whole complaint behind #163.
pub fn broker_db_path(repo_root: &std::path::Path) -> std::path::PathBuf {
    broker_db_path_with(repo_root, std::env::var_os(BROKER_DB_ENV))
}

/// The resolution itself, with the environment passed in.
///
/// Split out so it is testable: mutating a process-wide environment variable
/// from a test races every other test in the binary, and a resolution rule
/// this load-bearing should not go untested for that reason.
fn broker_db_path_with(
    repo_root: &std::path::Path,
    override_value: Option<std::ffi::OsString>,
) -> std::path::PathBuf {
    match override_value {
        Some(path) if !path.is_empty() => std::path::PathBuf::from(path),
        // An empty value is the shell's way of saying "unset" (`VAR= cmd`), and
        // the empty path is not a database anyone meant to name.
        _ => repo_root.join(BROKER_DB_RELPATH),
    }
}

#[cfg(test)]
mod broker_db_path_tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    #[test]
    fn without_an_override_the_database_stays_under_the_repository() {
        assert_eq!(
            broker_db_path_with(Path::new("/repo"), None),
            PathBuf::from("/repo/.aethyme/broker.db")
        );
    }

    #[test]
    fn an_override_replaces_the_path_rather_than_relocating_the_repository() {
        // Verbatim: the override names the file, not a root to join
        // `.aethyme/broker.db` onto. A harness that pointed at a temp *file*
        // and got a temp *directory* back would silently write next door.
        assert_eq!(
            broker_db_path_with(Path::new("/repo"), Some(OsString::from("/tmp/t/pinned.db"))),
            PathBuf::from("/tmp/t/pinned.db")
        );
    }

    #[test]
    fn an_empty_override_is_the_same_as_no_override() {
        assert_eq!(
            broker_db_path_with(Path::new("/repo"), Some(OsString::new())),
            PathBuf::from("/repo/.aethyme/broker.db")
        );
    }
}

/// Repo-relative generated projection of outstanding advisory rows.
pub const BROKER_ADVISORY_RELPATH: &str = ".aethyme/broker-advisory.md";
