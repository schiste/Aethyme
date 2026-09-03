//! The event catalog: every kind the broker emits, with typed payload
//! constructors. This module is the single source of truth for the event
//! contract — `docs/events-contract.md` documents what this file defines,
//! and emitters elsewhere in the crate must build payloads through these
//! constructors, never with hand-rolled JSON strings.
//!
//! Contract rules (schema_version 1):
//! - Kinds are dotted `<domain>.<what>` strings; existing kinds are never
//!   renamed or repurposed, only added.
//! - Payload fields are additive-only per kind.
//! - Event ids are strictly increasing forever (AUTOINCREMENT — no rowid
//!   reuse, even across deletes/prune), so `--since <id>` cursors are
//!   always safe to resume.

use serde_json::json;

// ── kind constants ───────────────────────────────────────────────────
pub const SESSION_REGISTERED: &str = "session.registered";
pub const SESSION_REUSED: &str = "session.reused";
pub const SESSION_FINISHED: &str = "session.finished";
pub const SESSION_FINISH_CLEANUP_STARTED: &str = "session.finish_cleanup_started";
pub const SESSION_CHECKPOINT_REANCHORED: &str = "session.checkpoint_reanchored";
pub const REVIEW_LIFECYCLE_REASSIGNED: &str = "review.lifecycle_reassigned";
pub const REVIEW_LIFECYCLE_ABANDONED: &str = "review.lifecycle_abandoned";
pub const BROKER_COMMAND_FAILED: &str = "broker.command.failed";
pub const BROKER_GC_APPLIED: &str = "broker.gc.applied";
pub const BROKER_GC_ARTIFACTS_SWEPT: &str = "broker.gc.artifacts-swept";
pub const BROKER_COMMAND_SUCCEEDED: &str = "broker.command.succeeded";
// session.<status> transition kinds are derived from SessionStatus::as_str
// (active/idle/stale/exited/cleaned) by the store.
pub const LEASE_CLAIMED: &str = "lease.claimed";
pub const LEASE_RELEASED: &str = "lease.released";
pub const LEASE_OVERLAP: &str = "lease.overlap";
// gate.<status> kinds derive from GateStatus::as_str (pass/fail/cancelled/error).
pub const GATE_CACHED: &str = "gate.cached";
pub const GRAPH_INTEGRITY_CHECKED: &str = "graph.integrity_checked";
// merge.<status> kinds derive from MergeStatus::as_str.
pub const MERGE_INTEGRATION_BRANCH_CREATED: &str = "merge.integration_branch_created";
pub const MERGE_INTEGRATION_REFRESHED: &str = "merge.integration_refreshed";
pub const MERGE_SUBMISSION_PLANNING_FAILED: &str = "merge.submission_planning_failed";
// operation.<status> transition kinds derive from OperationStatus::as_str.

// ── payload constructors ─────────────────────────────────────────────
// Each returns the canonical JSON payload for its kind. Field names are
// part of the versioned contract; add fields, never rename them.

pub fn session_registered_payload(origin: &str, branch: &str, worktree_path: &str) -> String {
    json!({ "origin": origin, "branch": branch, "worktree_path": worktree_path }).to_string()
}

pub fn session_reused_payload(task: Option<&str>, diff_base: Option<&str>) -> String {
    json!({ "task": task, "diff_base": diff_base }).to_string()
}

pub fn session_finish_cleanup_started_payload(
    worktree_present: bool,
    branch_present: bool,
) -> String {
    json!({
        "worktree_present": worktree_present,
        "branch_present": branch_present,
    })
    .to_string()
}

pub fn session_checkpoint_reanchored_payload(
    old_checkpoint: &str,
    new_checkpoint: &str,
    session_head: &str,
    plan_digest: &str,
    preservation_ref: &str,
) -> String {
    json!({
        "old_checkpoint": old_checkpoint,
        "new_checkpoint": new_checkpoint,
        "session_head": session_head,
        "plan_digest": plan_digest,
        "preservation_ref": preservation_ref,
    })
    .to_string()
}

pub fn review_lifecycle_reassigned_payload(
    lifecycle_id: i64,
    from_session_id: i64,
    to_session_id: i64,
    repository: &str,
    pr_number: i64,
    reason_digest: &str,
) -> String {
    json!({
        "lifecycle_id": lifecycle_id,
        "from_session_id": from_session_id,
        "to_session_id": to_session_id,
        "repository": repository,
        "pr_number": pr_number,
        "reason_digest": reason_digest,
    })
    .to_string()
}

pub fn review_lifecycle_abandoned_payload(
    lifecycle_id: i64,
    repository: &str,
    pr_number: i64,
    reason_digest: &str,
) -> String {
    json!({
        "lifecycle_id": lifecycle_id,
        "repository": repository,
        "pr_number": pr_number,
        "reason_digest": reason_digest,
    })
    .to_string()
}

pub fn session_exit_payload(exit_code: i64) -> String {
    json!({ "exit_code": exit_code }).to_string()
}

pub fn graph_integrity_checked_payload(outcome: &crate::GraphIntegrityOutcome) -> String {
    json!({
        "status": outcome.status.as_str(),
        "enforced": outcome.enforced,
        "tree": outcome.tree_hash,
        "policy": outcome.policy_digest,
        "engine_version": outcome.engine_version,
        "changed_paths": outcome.changed_paths,
    })
    .to_string()
}

pub fn broker_command_outcome_payload(
    command_surface: &str,
    exit_code: u8,
    failure_class: Option<&str>,
    operation_id: Option<i64>,
    queue_entry_id: Option<i64>,
) -> String {
    json!({
        "command_surface": command_surface,
        "exit_code": exit_code,
        "failure_class": failure_class,
        "operation_id": operation_id,
        "queue_entry_id": queue_entry_id,
    })
    .to_string()
}

/// Redacted durable handoff. Deliberately excludes the absolute worktree
/// path, task/command text, warnings, logs, diffs, and file contents.
pub fn session_finished_payload(report: &crate::broker::FinishReport) -> String {
    serde_json::to_string(&crate::broker::FinishHandoff::from(report))
        .expect("FinishHandoff contains only serializable fields")
}

pub fn lease_path_payload(path: &str) -> String {
    json!({ "path": path }).to_string()
}

pub fn gate_result_payload(
    gate: &str,
    tree: &str,
    failure_class: Option<crate::types::GateFailureClass>,
) -> String {
    json!({
        "gate": gate,
        "tree": tree,
        "failure_class": failure_class.map(|class| class.as_str()),
    })
    .to_string()
}

/// `saved_ms` is the cached run's recorded duration — the execution time
/// this cache hit avoided.
pub fn gate_cached_payload(
    gate: &str,
    tree: &str,
    saved_ms: i64,
    cached_status: crate::types::GateStatus,
    failure_class: Option<crate::types::GateFailureClass>,
) -> String {
    json!({
        "gate": gate,
        "tree": tree,
        "saved_ms": saved_ms,
        "cached_status": cached_status.as_str(),
        "failure_class": failure_class.map(|class| class.as_str()),
    })
    .to_string()
}

pub fn merge_submitted_payload(head: &str) -> String {
    json!({ "head": head }).to_string()
}

pub fn merge_submission_planning_failed_payload(head: &str, failure_class: &str) -> String {
    json!({ "head": head, "failure_class": failure_class }).to_string()
}

pub fn integration_branch_created_payload(branch: &str, at_commit: &str) -> String {
    json!({ "branch": branch, "at": at_commit }).to_string()
}

pub fn integration_refreshed_payload(branch: &str, from: &str, to: &str) -> String {
    json!({ "branch": branch, "from": from, "to": to }).to_string()
}

pub fn operation_payload(
    operation_id: i64,
    provider: crate::types::OperationProvider,
    repository: &str,
    scope: &str,
    effect: crate::types::OperationEffect,
    status: crate::types::OperationStatus,
    exit_code: Option<i64>,
) -> String {
    json!({
        "operation_id": operation_id,
        "provider": provider.as_str(),
        "repository": repository,
        "scope": scope,
        "effect": effect.as_str(),
        "status": status.as_str(),
        "exit_code": exit_code,
    })
    .to_string()
}

pub fn merge_promoted_payload(branch: &str, commit: &str) -> String {
    json!({ "branch": branch, "commit": commit }).to_string()
}

pub struct OperatorResolutionPayload<'a> {
    pub operator: &'a str,
    pub reason: &'a str,
    pub resolution_file: &'a str,
    pub upstream_commit: &'a str,
    pub old_integration: &'a str,
}

pub fn merge_externally_landed_payload(
    branch: &str,
    commit: &str,
    classification: &str,
    upstream_ref: &str,
    upstream_landing: Option<&str>,
    operator_resolution: Option<OperatorResolutionPayload<'_>>,
) -> String {
    json!({
        "branch": branch,
        "commit": commit,
        "externally_landed": true,
        "classification": classification,
        "upstream_ref": upstream_ref,
        "upstream_landing": upstream_landing,
        "operator_resolution": operator_resolution.map(|resolution| json!({
            "operator": resolution.operator,
            "reason": resolution.reason,
            "resolution_file": resolution.resolution_file,
            "upstream_commit": resolution.upstream_commit,
            "old_integration": resolution.old_integration,
        })),
    })
    .to_string()
}
