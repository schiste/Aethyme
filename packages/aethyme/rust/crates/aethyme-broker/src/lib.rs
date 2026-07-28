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
mod error;
pub mod events;
mod gates;
mod git;
pub mod hooks;
pub mod init;
mod leases;
mod merge;
mod quick_test;
mod schema;
mod store;
mod types;
mod version;

pub use broker::{
    AdoptMode, AgentView, Broker, BrokerOpError, DoctorReport, PromotedConflict, RepairAction,
    RepairGateSelection, RepairReport, RepairSource, StatusAdvice, StatusAdviceSeverity,
    StatusView,
};
pub use error::BrokerError;
pub use gates::{
    Gate, GateConfigError, GateProgressSink, GateRunOutcome, load_gates, select_gates,
};
pub use git::{GitError, GitRepo, MergeSimulation};
pub use hooks::{HookReport, HookState, HooksError};
pub use leases::{LeaseIgnoreRules, Overlap, detect_overlaps};
pub use merge::{ACTION_REQUIRED_RELPATH, PromoteConfig, SubmitOutcome};
pub use quick_test::{
    Chau7Probe, QuickTestError, QuickTestMode, QuickTestReport, QuickTestStep,
    run_broker_quick_test,
};
pub use schema::{EVENTS_SCHEMA_VERSION, SCHEMA_VERSION};
pub use store::BrokerStore;
pub use types::{
    Event, GateDef, GateFailureClass, GateResult, GateStatus, Lease, LeaseKind, MergeQueueEntry,
    MergeStatus, NewGateResult, NewSession, Session, SessionOrigin, SessionStatus,
};
pub use version::{BinaryBuild, VersionDriftReport, VersionDriftStatus, inspect_version};

/// Repo-relative location of the broker database.
pub const BROKER_DB_RELPATH: &str = ".aethyme/broker.db";
