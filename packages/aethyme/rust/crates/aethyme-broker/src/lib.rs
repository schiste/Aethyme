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

mod broker;
pub mod cli;
pub mod contract_check;
mod error;
pub mod events;
mod gates;
mod git;
mod graph_impact;
pub mod hooks;
pub mod init;
mod leases;
mod merge;
mod operations;
mod pr;
mod quick_test;
mod reconciliation;
mod schema;
mod ship;
mod store;
mod types;
mod verify_loop;
mod version;

pub use broker::{
    AdoptIntegrationDrift, AdoptIntegrationRelation, AdoptIntegrationSync,
    AdoptIntegrationSyncOutcome, AdoptMode, AdoptOptions, AdoptOutcome, AdoptReport, AgentView,
    Broker, BrokerOpError, DoctorRepairStatus, DoctorReport, FinishDelivery, FinishGateCacheSource,
    FinishGateRun, FinishHandoff, FinishLease, FinishLeaseState, FinishPendingWork, FinishReport,
    FinishStatus, GuardedExecReport, IntegrationDeliveryState, IntegrationLiveSession,
    IntegrationMovementNotice, IntegrationNextAction, IntegrationStabilityReport,
    IntegrationStatusView, LeaseBlocker, LeaseClaimReport, LeaseOverlapRelation, LeasePathPlan,
    LeasePlan, LeasePlanOverlap, OwnershipAuditReport, PromotedConflict, PromotedIntegrationEntry,
    RepairAction, RepairGateSelection, RepairReport, RepairSource, SemanticGateAdvice,
    SemanticGateSelection, SemanticGateSource, SemanticGateSourceStatus,
    SemanticGateSuggestionChain, SessionHandoffReport, StatusAdvice, StatusAdviceSeverity,
    StatusIntegrationRelation, StatusSummary, StatusView, VersionRepairReport,
};
pub use error::BrokerError;
pub use gates::{
    CachePolicy, Gate, GateConfigError, GateProgressSink, GateRunOutcome, load_gates, select_gates,
};
pub use git::{GitError, GitRepo, MergeSimulation, RemoteDefaultBranch};
pub use graph_impact::{
    GRAPH_IMPACT_MAX_DEPTH, GRAPH_IMPACT_MAX_NODES, GRAPH_IMPACT_RESULT_LIMIT, GraphImpactChain,
    GraphImpactLookup, GraphImpactProvider, GraphImpactQuery, GraphImpactStatus,
    GraphStoreImpactProvider,
};
pub use hooks::{HookReport, HookState, HooksError};
pub use leases::{LeaseIgnoreRules, Overlap, detect_overlaps};
pub use merge::{ACTION_REQUIRED_RELPATH, PromoteConfig, SubmitOutcome};
pub use operations::{
    CoordinatedCommand, CoordinatedOperationReport, OperationReconcileReport, classify_gh,
    classify_git,
};
pub use pr::{
    PrActivityItem, PrCheckOptions, PrCheckReport, PrCheckRun, PrDecision, PrDecisionStatus,
    PrDispatchReport, PrDispatchStatus, PrError, PrMarker, PrSummary,
};
pub use quick_test::{
    Chau7Probe, QuickTestError, QuickTestGateOutcome, QuickTestGateReport, QuickTestMode,
    QuickTestOptions, QuickTestReport, QuickTestStep, run_broker_quick_test,
    run_broker_quick_test_with_options,
};
pub use reconciliation::{
    IntegrationReconcileClassification, IntegrationReconcileEntry, IntegrationReconcileOptions,
    IntegrationReconcileReport, IntegrationReconcileResolutionAudit,
};
pub use schema::{EVENTS_SCHEMA_VERSION, SCHEMA_VERSION};
pub use ship::{
    ShipExecutionReport, ShipFreshness, ShipFreshnessResult, ShipLocalMainSync, ShipPlan, ShipPush,
};
pub use store::BrokerStore;
pub use types::{
    CoordinatedOperation, Event, GateDef, GateFailureClass, GateResult, GateStatus, Lease,
    LeaseKind, MergeQueueEntry, MergeStatus, NewCoordinatedOperation, NewGateResult,
    NewPrWatchState, NewSession, OperationEffect, OperationProvider, OperationStatus, PrWatchState,
    Session, SessionOrigin, SessionStatus,
};
pub use verify_loop::{
    VerifyLoopCommandReport, VerifyLoopReport, VerifyLoopStep, VerifyLoopStepStatus,
};
pub use version::{BinaryBuild, VersionDriftReport, VersionDriftStatus, inspect_version};

/// Repo-relative location of the broker database.
pub const BROKER_DB_RELPATH: &str = ".aethyme/broker.db";
