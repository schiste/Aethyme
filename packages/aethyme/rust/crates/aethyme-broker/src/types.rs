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
    Rejected => "rejected",
    Superseded => "superseded",
});

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
    pub task: Option<String>,
    pub diff_base: Option<String>,
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
    pub updated_at: i64,
}

/// Input for recording one gate run.
#[derive(Debug, Clone)]
pub struct NewGateResult {
    pub gate_name: String,
    pub tree_hash: String,
    pub status: GateStatus,
    pub failure_class: Option<GateFailureClass>,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<i64>,
    pub log_path: Option<String>,
    pub session_id: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GateResult {
    pub id: i64,
    pub gate_name: String,
    /// Git tree hash the gate ran against — the cache key.
    pub tree_hash: String,
    pub status: GateStatus,
    pub failure_class: Option<GateFailureClass>,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<i64>,
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
