//! Typed rows and enums for the broker store.
//!
//! Enums are stored as stable lowercase TEXT. The `as_str`/`parse` pairs
//! are the single source of truth for those strings — they are part of the
//! on-disk contract, so variants may be added but never renamed.

use crate::error::BrokerError;

macro_rules! text_enum {
    ($name:ident, $field:literal, { $($variant:ident => $text:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name {
            $($variant,)+
        }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text,)+
                }
            }

            pub fn parse(value: &str) -> Result<Self, BrokerError> {
                match value {
                    $($text => Ok(Self::$variant),)+
                    other => Err(BrokerError::InvalidEnumValue {
                        field: $field,
                        value: other.to_string(),
                    }),
                }
            }
        }

        // JSON output uses exactly the on-disk strings, so the --json
        // contract and the storage contract can never drift apart.
        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let value = <String as serde::Deserialize>::deserialize(d)?;
                Self::parse(&value).map_err(serde::de::Error::custom)
            }
        }
    };
}

text_enum!(SessionOrigin, "sessions.origin", {
    Adopted => "adopted",
    Spawned => "spawned",
});

text_enum!(SessionStatus, "sessions.status", {
    Active => "active",
    Idle => "idle",
    Stale => "stale",
    Exited => "exited",
    Closed => "closed",
    Cleaned => "cleaned",
});

impl SessionStatus {
    pub fn is_closed(self) -> bool {
        matches!(self, Self::Closed | Self::Cleaned)
    }
}

text_enum!(SessionCleanupState, "sessions.cleanup_state", {
    Open => "open",
    Closed => "closed",
    Cleaned => "cleaned",
});

text_enum!(LeaseKind, "leases.kind", {
    Implicit => "implicit",
    Explicit => "explicit",
});

text_enum!(GateStatus, "gate_results.status", {
    Pass => "pass",
    Fail => "fail",
    Cancelled => "cancelled",
    Error => "error",
});

text_enum!(GateFailureClass, "gate_results.failure_class", {
    TestFailure => "test_failure",
    Environment => "environment",
    ResourceContention => "resource_contention",
    Timeout => "timeout",
    CachedPriorFail => "cached_prior_fail",
    Unknown => "unknown",
});

text_enum!(MergeStatus, "merge_queue.status", {
    Submitted => "submitted",
    Simulating => "simulating",
    Conflict => "conflict",
    Verified => "verified",
    Promoted => "promoted",
    ExternallyLanded => "externally_landed",
    Rejected => "rejected",
    Superseded => "superseded",
});

text_enum!(OperationProvider, "coordinated_operations.provider", {
    Git => "git",
    Github => "github",
});

text_enum!(OperationEffect, "coordinated_operations.effect", {
    Read => "read",
    Write => "write",
    Destructive => "destructive",
});

text_enum!(OperationStatus, "coordinated_operations.status", {
    Prepared => "prepared",
    Running => "running",
    Succeeded => "succeeded",
    Failed => "failed",
    OutcomeUnknown => "outcome_unknown",
    ReconciledSucceeded => "reconciled_succeeded",
    ReconciledFailed => "reconciled_failed",
});

text_enum!(OperationIdentityProvenance, "coordinated_operations.identity_provenance", {
    LegacyUnverifiedIdentity => "legacy_unverified_identity",
    VerifiedCanonical => "verified_canonical",
    LocalRepository => "local_repository",
});

text_enum!(AdvisorySeverity, "advisories.severity", {
    Info => "info",
    Warning => "warning",
    Critical => "critical",
});

text_enum!(AdvisoryResolutionState, "advisories.resolution_state", {
    Outstanding => "outstanding",
    Acknowledged => "acknowledged",
    Resolved => "resolved",
});

text_enum!(EntryExposureState, "entry_path_exposures.state", {
    Outstanding => "outstanding",
    Resolved => "resolved",
});

text_enum!(EntryExposureResolutionKind, "entry_path_exposures.resolution_kind", {
    ShipVerified => "ship_verified",
    ExternalReconciliation => "external_reconciliation",
});

/// One bounded, structured fact supporting a non-blocking advisory.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AdvisoryEvidence {
    pub kind: String,
    pub summary: String,
}

/// Immutable input used by broker subsystems to persist an advisory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAdvisory {
    /// Stable idempotency key chosen by the advisory producer.
    pub identity: String,
    pub session_id: Option<i64>,
    pub severity: AdvisorySeverity,
    pub queue_entry_id: Option<i64>,
    pub integration_sha: Option<String>,
    pub paths: Vec<String>,
    pub evidence: Vec<AdvisoryEvidence>,
}

/// Authoritative durable advisory row. Acknowledgement never deletes history.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Advisory {
    pub id: i64,
    pub identity: String,
    pub session_id: Option<i64>,
    pub severity: AdvisorySeverity,
    pub queue_entry_id: Option<i64>,
    pub integration_sha: Option<String>,
    pub paths: Vec<String>,
    pub evidence: Vec<AdvisoryEvidence>,
    pub created_at: i64,
    pub resolution_state: AdvisoryResolutionState,
    pub acknowledged_at: Option<i64>,
    pub resolved_at: Option<i64>,
    pub resolution_evidence: Option<String>,
}

/// Exact promoted paths that remain exposed until publication is proven.
///
/// Ownership follows the queue entry rather than a live session, so closing
/// or rebasing a worktree cannot erase unpublished integration state.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct EntryPathExposure {
    pub id: i64,
    pub queue_entry_id: i64,
    pub promotion_sha: String,
    pub paths: Vec<String>,
    pub created_at: i64,
    pub state: EntryExposureState,
    pub resolved_at: Option<i64>,
    pub resolution_kind: Option<EntryExposureResolutionKind>,
    pub resolution_sha: Option<String>,
    pub resolution_evidence: Option<String>,
}

/// Stable JSON envelope for `advisories list`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AdvisoryList {
    pub advisories: Vec<Advisory>,
    pub outstanding_count: usize,
    pub includes_acknowledged: bool,
}

/// One bounded local message between two broker sessions in this repository.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionNote {
    pub id: i64,
    pub sender_session_id: i64,
    pub recipient_session_id: i64,
    pub message: String,
    pub created_at: i64,
    pub acknowledged_at: Option<i64>,
}

/// Stable JSON envelope for `note list`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionNoteList {
    pub notes: Vec<SessionNote>,
    pub unread_count: usize,
}

/// Durable intent recorded before a coordinated command starts.
#[derive(Debug, Clone)]
pub struct NewCoordinatedOperation {
    pub session_id: i64,
    pub provider: OperationProvider,
    pub repository: String,
    pub scope: String,
    pub effect: OperationEffect,
    pub authorization_reason: Option<String>,
    /// Redacted JSON array. Secret-bearing argument values never enter the DB.
    pub command_json: String,
    pub pid: i64,
    pub host_operation_id: Option<String>,
    pub identity_provenance: OperationIdentityProvenance,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CoordinatedOperation {
    pub id: i64,
    pub session_id: i64,
    pub provider: OperationProvider,
    pub repository: String,
    pub scope: String,
    pub effect: OperationEffect,
    pub status: OperationStatus,
    pub authorization_reason: Option<String>,
    pub command_json: String,
    pub pid: i64,
    pub exit_code: Option<i64>,
    pub details_json: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub finished_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_operation_id: Option<String>,
    pub identity_provenance: OperationIdentityProvenance,
}

pub const DEFAULT_OPERATION_HISTORY_LIMIT: u32 = 50;
pub const MAX_OPERATION_HISTORY_LIMIT: u32 = 500;

/// Stable newest-first query contract for the coordinated-operation journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationHistoryQuery {
    pub limit: u32,
    /// Exclusive operation-id cursor. Only rows with smaller ids are returned.
    pub before_id: Option<i64>,
    pub session_id: Option<i64>,
    pub status: Option<OperationStatus>,
    pub repository: Option<String>,
    pub provider: Option<OperationProvider>,
}

impl Default for OperationHistoryQuery {
    fn default() -> Self {
        Self {
            limit: DEFAULT_OPERATION_HISTORY_LIMIT,
            before_id: None,
            session_id: None,
            status: None,
            repository: None,
            provider: None,
        }
    }
}

/// One stable cursor page from the coordinated-operation journal.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OperationHistoryPage {
    pub operations: Vec<CoordinatedOperation>,
    pub next_before_id: Option<i64>,
}

/// Input for registering a session. Attach-first: only the worktree and
/// branch are identity; everything else is optional metadata.
#[derive(Debug, Clone)]
pub struct NewSession {
    pub worktree_path: String,
    pub branch: String,
    pub origin: SessionOrigin,
    pub task: Option<String>,
    /// Merge base the session diffs against (commit hash).
    pub diff_base: Option<String>,
    /// Immutable original adoption boundary. When omitted, storage seeds it
    /// from `diff_base`; later reuse may refresh only `diff_base`.
    pub adoption_base: Option<String>,
    /// Immutable checkout HEAD observed when this session identity was created.
    /// When omitted, storage conservatively seeds it from `adoption_base`, then
    /// `diff_base`, for callers using the older registration contract.
    pub adopted_head: Option<String>,
    /// Repository deployment contract accepted for this session.
    pub repository_contract: Option<crate::RepositoryContract>,
    /// Spawned sessions only.
    pub pid: Option<i64>,
    pub command: Option<String>,
    pub log_path: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Session {
    pub id: i64,
    pub worktree_path: String,
    pub branch: String,
    pub origin: SessionOrigin,
    pub status: SessionStatus,
    pub cleanup_state: SessionCleanupState,
    pub closed_at: Option<i64>,
    pub cleanup_completed_at: Option<i64>,
    pub task: Option<String>,
    pub diff_base: Option<String>,
    pub adoption_base: Option<String>,
    /// Immutable checkout HEAD observed when this session was first adopted.
    pub adopted_head: Option<String>,
    /// Session HEAD most recently accepted into integration.
    pub accepted_session_head: Option<String>,
    /// Integration commit produced by that acceptance.
    pub accepted_integration_commit: Option<String>,
    /// Integration tree verified for that acceptance.
    pub accepted_integration_tree: Option<String>,
    /// Queue entry proving the acceptance.
    pub accepted_queue_entry_id: Option<i64>,
    /// Unix epoch milliseconds when the acceptance was recorded.
    pub accepted_at: Option<i64>,
    pub repository_contract: Option<crate::RepositoryContract>,
    pub pid: Option<i64>,
    pub command: Option<String>,
    pub log_path: Option<String>,
    pub exit_code: Option<i64>,
    /// Unix epoch milliseconds.
    pub created_at: i64,
    pub updated_at: i64,
    pub last_activity_at: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Lease {
    pub id: i64,
    pub session_id: i64,
    /// Repo-relative path (file or directory prefix).
    pub path: String,
    pub kind: LeaseKind,
    pub created_at: i64,
    /// Unix epoch milliseconds; `None` = no expiry.
    pub expires_at: Option<i64>,
    pub released_at: Option<i64>,
}

/// Snapshot of a gate definition (source of truth is `.aethyme/gates.toml`;
/// the row exists so gate_results stay interpretable after config changes).
#[derive(Debug, Clone, serde::Serialize)]
pub struct GateDef {
    pub name: String,
    pub command: String,
    /// Ascending = cheaper first.
    pub cost_tier: i64,
    /// JSON array of glob strings.
    pub triggers_json: String,
    /// JSON array of generic host resource requirements.
    pub resources_json: String,
    pub resource_ttl_seconds: i64,
    pub resource_wait_seconds: i64,
    pub managed_cache_json: Option<String>,
    /// Stable digest of every execution-relevant field.
    pub definition_hash: String,
    pub updated_at: i64,
}

/// Input for recording one gate run.
#[derive(Debug, Clone)]
pub struct NewGateResult {
    pub gate_name: String,
    pub tree_hash: String,
    pub definition_hash: String,
    pub status: GateStatus,
    pub failure_class: Option<GateFailureClass>,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<i64>,
    pub wait_duration_ms: Option<i64>,
    pub first_output_ms: Option<i64>,
    pub output_bytes: Option<i64>,
    pub log_path: Option<String>,
    pub session_id: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GateResult {
    pub id: i64,
    pub gate_name: String,
    /// Git tree hash the gate ran against — the cache key.
    pub tree_hash: String,
    pub definition_hash: String,
    pub status: GateStatus,
    pub failure_class: Option<GateFailureClass>,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<i64>,
    pub wait_duration_ms: Option<i64>,
    pub first_output_ms: Option<i64>,
    pub output_bytes: Option<i64>,
    pub log_path: Option<String>,
    pub session_id: Option<i64>,
    pub created_at: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MergeQueueEntry {
    pub id: i64,
    pub session_id: i64,
    pub head_commit: String,
    pub base_commit: String,
    pub status: MergeStatus,
    /// Tree written by `git merge-tree` when simulation succeeded.
    pub merged_tree: Option<String>,
    /// JSON details (conflict file list, gate summary, ...).
    pub details_json: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// One append-only event. `schema_version` is per-row so readers can
/// interpret old rows after the event contract evolves.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Event {
    pub id: i64,
    pub schema_version: i64,
    /// Unix epoch milliseconds.
    pub ts: i64,
    /// Dotted kind, e.g. `session.started`, `lease.overlap`, `gate.failed`.
    pub kind: String,
    pub session_id: Option<i64>,
    /// JSON payload.
    pub payload_json: Option<String>,
}

/// Durable cursor for PR follow-up routing. The broker stores a compact
/// fingerprint rather than every comment/review id, so the watch state
/// stays small and remains independent from GitHub's full API shape.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PrWatchState {
    pub id: i64,
    pub target_branch: String,
    pub pr_number: i64,
    pub activity_fingerprint: String,
    pub marker: String,
    pub last_dispatch_at: Option<i64>,
    pub last_agent_session_id: Option<i64>,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub struct NewPrWatchState {
    pub target_branch: String,
    pub pr_number: i64,
    pub activity_fingerprint: String,
    pub marker: String,
    pub last_dispatch_at: Option<i64>,
    pub last_agent_session_id: Option<i64>,
}
