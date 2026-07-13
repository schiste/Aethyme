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
// session.<status> transition kinds are derived from SessionStatus::as_str
// (active/idle/stale/exited/cleaned) by the store.
pub const LEASE_CLAIMED: &str = "lease.claimed";
pub const LEASE_RELEASED: &str = "lease.released";
pub const LEASE_OVERLAP: &str = "lease.overlap";
// gate.<status> kinds derive from GateStatus::as_str (pass/fail/cancelled/error).
// merge.<status> kinds derive from MergeStatus::as_str.
pub const MERGE_INTEGRATION_BRANCH_CREATED: &str = "merge.integration_branch_created";

// ── payload constructors ─────────────────────────────────────────────
// Each returns the canonical JSON payload for its kind. Field names are
// part of the versioned contract; add fields, never rename them.

pub fn session_registered_payload(origin: &str, branch: &str, worktree_path: &str) -> String {
    json!({ "origin": origin, "branch": branch, "worktree_path": worktree_path }).to_string()
}

pub fn session_exit_payload(exit_code: i64) -> String {
    json!({ "exit_code": exit_code }).to_string()
}

pub fn lease_path_payload(path: &str) -> String {
    json!({ "path": path }).to_string()
}

pub fn gate_result_payload(gate: &str, tree: &str) -> String {
    json!({ "gate": gate, "tree": tree }).to_string()
}

pub fn merge_submitted_payload(head: &str) -> String {
    json!({ "head": head }).to_string()
}

pub fn integration_branch_created_payload(branch: &str, at_commit: &str) -> String {
    json!({ "branch": branch, "at": at_commit }).to_string()
}

pub fn merge_promoted_payload(branch: &str, commit: &str) -> String {
    json!({ "branch": branch, "commit": commit }).to_string()
}
