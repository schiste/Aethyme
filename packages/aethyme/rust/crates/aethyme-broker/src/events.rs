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

use serde::{Deserialize, Serialize};
use serde_json::json;

// ── kind constants ───────────────────────────────────────────────────
pub const OPERATION_FETCH_RECONCILED: &str = "operation.fetch_reconciled";
pub const OPERATION_INSPECTED: &str = "operation.inspected";
pub const SESSION_REGISTERED: &str = "session.registered";
pub const SESSION_REUSED: &str = "session.reused";
/// A session retired by `start --adopt --replace-stale` while its successor
/// kept the same checkout; payload `{"successor_session_id": N}`.
pub const SESSION_REPLACED: &str = "session.replaced";
pub const SESSION_CONTEXT_UPDATED: &str = "session.context_updated";
pub const SESSION_FINISHED: &str = "session.finished";
pub const SESSION_HOLDER_BOUND: &str = "session.holder_bound";
/// The broker first saw a session's holder process gone (#360). Starts the
/// lease stale-grace clock for that holder.
pub const SESSION_HOLDER_GONE: &str = "session.holder_gone";
pub const SESSION_INSTALL_RECORDED: &str = "session.install_recorded";
pub const SESSION_FINISH_CLEANUP_STARTED: &str = "session.finish_cleanup_started";
pub const SESSION_CHECKPOINT_REANCHORED: &str = "session.checkpoint_reanchored";
pub const REVIEW_LIFECYCLE_REASSIGNED: &str = "review.lifecycle_reassigned";
pub const REVIEW_LIFECYCLE_ABANDONED: &str = "review.lifecycle_abandoned";
pub const QUALITY_REPORT_PUBLISHED: &str = "quality_report.published";
pub const QUALITY_REPORT_PUBLICATION_FAILED: &str = "quality_report.publication_failed";
pub const BROKER_COMMAND_FAILED: &str = "broker.command.failed";
pub const BROKER_GC_APPLIED: &str = "broker.gc.applied";
pub const BROKER_GC_ARTIFACTS_SWEPT: &str = "broker.gc.artifacts-swept";
/// A detached non-pruning repack was started because the repository's loose
/// objects passed the threshold; the payload carries the estimate.
pub const BROKER_OBJECT_STORE_MAINTENANCE_STARTED: &str = "broker.object_store.maintenance_started";

/// Dead shared preparation-cache entries reclaimed without an operator.
pub const BROKER_GC_PREPARATION_SWEPT: &str = "broker.gc.preparation-swept";
/// A disposable checkout removed by the unattended sweep, with its proof (#588).
pub const BROKER_CLEANUP_AUTO_REMOVED: &str = "broker.cleanup.auto_removed";
pub const BROKER_COMMAND_SUCCEEDED: &str = "broker.command.succeeded";
/// A closed session's uncommitted or unlanded work was written to a verified
/// recovery archive before its worktree was removed.
pub const BROKER_CLEANUP_ARCHIVED: &str = "broker.cleanup.archived";
/// A reviewed `gc apply` removed one recovery archive.
pub const BROKER_GC_RECOVERY_ARCHIVE_REMOVED: &str = "broker.gc.recovery-archive-removed";
/// `broker push` published a session's own branch (and maybe opened a draft PR).
pub const BROKER_SESSION_PUSHED: &str = "broker.session.pushed";
/// `broker sync` rebased or merged a session onto the default branch.
pub const BROKER_SESSION_SYNCED: &str = "broker.session.synced";
/// A verify-only integration branch advanced onto the fetched default branch
/// because it carried nothing of its own (#352).
pub const BROKER_INTEGRATION_REFRESHED: &str = "broker.integration.refreshed";
/// The primary checkout was clean and strictly behind its configured upstream.
pub const BROKER_CHECKOUT_FAST_FORWARDED: &str = "broker.checkout.fast_forwarded";
/// A session was closed with committed work no remote holds, on an explicit
/// `--abandon --reason` under the push lane.
pub const BROKER_SESSION_ABANDONED_UNPUSHED: &str = "broker.session.abandoned_unpushed";
// session.<status> transition kinds are derived from SessionStatus::as_str
// (active/idle/stale/exited/cleaned) by the store.
pub const LEASE_CLAIMED: &str = "lease.claimed";
pub const LEASE_RELEASED: &str = "lease.released";
/// A session asked another to release a lease (#359). The event id is the
/// request id; the outcome events below name it.
pub const LEASE_RELEASE_REQUESTED: &str = "lease.release_requested";
/// The holder acknowledged a release request and released the lease.
pub const LEASE_RELEASE_ACKED: &str = "lease.release_acked";
/// The holder declined a release request, with a reason.
pub const LEASE_RELEASE_DECLINED: &str = "lease.release_declined";
/// The holder was gone past the stale grace; the broker released for it.
pub const LEASE_RELEASE_GRANTED: &str = "lease.release_granted";
pub const LEASE_OVERLAP: &str = "lease.overlap";
pub const GUARD_OUT_OF_LEASE_WRITE: &str = "guard.out_of_lease_write";
pub const GUARD_UNTRACKED_ARTIFACT: &str = "guard.untracked_artifact";
// gate.<status> kinds derive from GateStatus::as_str (pass/fail/cancelled/error).
pub const GATE_CACHED: &str = "gate.cached";
pub const GATE_CACHE_BYPASSED: &str = "gate.cache_bypassed";
/// A human approved a repository's gate/prepare policy with `broker trust`.
pub const GATE_POLICY_TRUSTED: &str = "gate.policy_trusted";
/// A repository with gate history was trusted with its current policy the
/// first time the trust check saw it.
pub const GATE_POLICY_TRUST_GRANDFATHERED: &str = "gate.policy_trust_grandfathered";
pub const GRAPH_INTEGRITY_CHECKED: &str = "graph.integrity_checked";
pub const GRAPH_IMPACT_EVALUATED: &str = "graph.impact_evaluated";
// merge.<status> kinds derive from MergeStatus::as_str.
pub const MERGE_INTEGRATION_BRANCH_CREATED: &str = "merge.integration_branch_created";
pub const MERGE_INTEGRATION_REFRESHED: &str = "merge.integration_refreshed";
pub const MERGE_SUBMISSION_PLANNING_FAILED: &str = "merge.submission_planning_failed";
pub const MERGE_POLICY_DEFERRED: &str = "merge.policy_deferred";
// operation.<status> transition kinds derive from OperationStatus::as_str.

// ── payload constructors ─────────────────────────────────────────────
// Each returns the canonical JSON payload for its kind. Field names are
// part of the versioned contract; add fields, never rename them.

pub fn session_registered_payload(origin: &str, branch: &str, worktree_path: &str) -> String {
    json!({ "origin": origin, "branch": branch, "worktree_path": worktree_path }).to_string()
}

/// `session.abandoned`: an unattended session crossed its configured age
/// window while its recorded owner and lease evidence allowed closure.
pub fn session_abandoned_payload(
    idle_ms: i64,
    abandon_after_ms: i64,
    pid_alive: Option<bool>,
) -> String {
    json!({
        "idle_ms": idle_ms,
        "abandon_after_ms": abandon_after_ms,
        "pid_alive": pid_alive,
    })
    .to_string()
}

pub fn session_reused_payload(task: Option<&str>, diff_base: Option<&str>) -> String {
    json!({ "task": task, "diff_base": diff_base }).to_string()
}

/// `session.holder_bound`: the agent process that now holds the session, why
/// it does, and the holder it replaced (#393).
pub fn session_holder_bound_payload(
    holder: &crate::session_holder::AgentProcess,
    reason: &str,
    previous: Option<&crate::session_holder::AgentProcess>,
) -> String {
    session_holder_bound_payload_with_task_lease(holder, reason, previous, false)
}

/// `session.holder_bound` for a holder that explicitly entered the guarded
/// task-lease protocol. Ordinary start/adopt and legacy rebinding events do
/// not gain an expiry lease until guarded admission upgrades them.
#[cfg(test)]
pub(crate) fn session_holder_task_lease_bound_payload(
    holder: &crate::session_holder::AgentProcess,
    reason: &str,
    previous: Option<&crate::session_holder::AgentProcess>,
) -> String {
    session_holder_bound_payload_with_task_lease(holder, reason, previous, true)
}

/// Add or renew the task-lease extension without replacing holder-binding
/// provenance or other additive fields from the original payload.
pub(crate) fn session_holder_task_lease_heartbeat_payload(
    existing: &serde_json::Value,
) -> Option<String> {
    let mut payload = existing.clone();
    payload.as_object()?;
    payload["task_lease"] = json!(crate::session_holder::TaskLease::current());
    Some(payload.to_string())
}

/// Remove only the task-lease extension while preserving the holder binding
/// and any other additive payload fields.
pub(crate) fn session_holder_task_lease_downgraded_payload(
    existing: &serde_json::Value,
) -> Option<String> {
    let mut payload = existing.clone();
    payload.as_object_mut()?.remove("task_lease")?;
    Some(payload.to_string())
}

fn session_holder_bound_payload_with_task_lease(
    holder: &crate::session_holder::AgentProcess,
    reason: &str,
    previous: Option<&crate::session_holder::AgentProcess>,
    task_lease: bool,
) -> String {
    let mut payload = json!({
        "pid": holder.pid,
        "started": holder.started,
        "command": holder.command,
        "reason": reason,
        "previous_pid": previous.map(|process| process.pid),
        "previous_started": previous.map(|process| process.started.as_str()),
    });
    if task_lease {
        payload["task_lease"] = json!(crate::session_holder::TaskLease::current());
    }
    payload.to_string()
}

/// `session.holder_gone`: the holder the broker found no longer running.
pub fn session_holder_gone_payload(holder: &crate::session_holder::AgentProcess) -> String {
    json!({
        "pid": holder.pid,
        "started": holder.started,
        "command": holder.command,
    })
    .to_string()
}

/// `session.install_recorded`: the router build that started or adopted the
/// session, and the engine banner on PATH beside it (#293).
pub fn session_install_recorded_payload(
    router: &crate::version::BinaryBuild,
    engine_banner: Option<&str>,
) -> String {
    json!({
        "version": router.version,
        "revision": router.revision,
        "describe": router.describe,
        "commit": router.commit,
        "path": router.path,
        "engine_banner": engine_banner,
    })
    .to_string()
}

pub fn session_context_updated_payload(context: &crate::SessionContext) -> String {
    json!({
        "repository_name": context.repository_name,
        "tab_name": context.tab_name,
        "ai_provider": context.ai_provider,
        "short_name": context.short_name,
    })
    .to_string()
}

pub fn session_abandoned_unpushed_payload(
    session_id: i64,
    branch: &str,
    head: &str,
    unpushed_commits: u32,
    reason: &str,
) -> String {
    json!({
        "session_id": session_id,
        "branch": branch,
        "head": head,
        "unpushed_commits": unpushed_commits,
        "reason": reason,
    })
    .to_string()
}

/// Filesystem identity tuple and root-generation digest captured by spawn.
/// Missing or changed identity cannot authorize unattended checkout removal.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SpawnedWorktreeInstanceWitness {
    pub(crate) schema_version: u8,
    pub(crate) worktree_path: String,
    pub(crate) worktree_device: u64,
    pub(crate) worktree_inode: u64,
    pub(crate) git_pointer_device: u64,
    pub(crate) git_pointer_inode: u64,
    pub(crate) git_admin_path: String,
    pub(crate) git_admin_device: u64,
    pub(crate) git_admin_inode: u64,
    pub(crate) git_common_path: String,
    pub(crate) git_common_device: u64,
    pub(crate) git_common_inode: u64,
    /// Digest of the random root xattr written exclusively during broker
    /// spawn. `None` is retained for legacy rows, which cannot authorize
    /// automatic removal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) root_generation_sha256: Option<String>,
    /// Captured only at spawn. Schema 3 can recognize the same filesystem
    /// object after macOS assigns a new mount-scoped device number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) volume_uuids: Option<SpawnedWorktreeVolumeUuids>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SpawnedWorktreeVolumeUuids {
    pub(crate) worktree: [u8; 16],
    pub(crate) git_pointer: [u8; 16],
    pub(crate) git_admin: [u8; 16],
    pub(crate) git_common: [u8; 16],
}

impl SpawnedWorktreeInstanceWitness {
    pub(crate) fn is_supported(&self) -> bool {
        let shape_valid = match (self.schema_version, &self.volume_uuids) {
            (2, None) => true,
            (3, Some(volumes)) => [
                volumes.worktree,
                volumes.git_pointer,
                volumes.git_admin,
                volumes.git_common,
            ]
            .iter()
            .all(|uuid| uuid.iter().any(|byte| *byte != 0)),
            _ => false,
        };
        shape_valid
            && self
                .root_generation_sha256
                .as_deref()
                .is_some_and(crate::root_generation::valid_digest)
    }

    fn same_object(
        saved_device: u64,
        saved_inode: u64,
        saved_uuid: Option<[u8; 16]>,
        current_device: u64,
        current_inode: u64,
        current_uuid: Option<[u8; 16]>,
    ) -> bool {
        saved_inode == current_inode
            && match (saved_uuid, current_uuid) {
                (Some(saved), Some(current)) => saved == current,
                (None, _) => saved_device == current_device,
                _ => false,
            }
    }

    pub(crate) fn same_git_common(&self, current: &Self) -> bool {
        self.is_supported()
            && current.is_supported()
            && self.git_common_path == current.git_common_path
            && Self::same_object(
                self.git_common_device,
                self.git_common_inode,
                self.volume_uuids.as_ref().map(|v| v.git_common),
                current.git_common_device,
                current.git_common_inode,
                current.volume_uuids.as_ref().map(|v| v.git_common),
            )
    }

    /// Compare persisted spawn authority with a fresh observation. Legacy
    /// witnesses retain their exact device checks; never infer a missing UUID.
    /// Snapshot equality elsewhere intentionally remains byte-for-byte strict.
    pub(crate) fn matches_current_instance(&self, current: &Self) -> bool {
        self.worktree_path == current.worktree_path
            && self.git_admin_path == current.git_admin_path
            && self.root_generation_sha256 == current.root_generation_sha256
            && self.same_checkout_objects(current)
    }

    /// Filesystem object identity alone, without generation equality, for
    /// recognizing an already removed historical alias. Mount-number changes
    /// on a saved UUID are not evidence of a different checkout object.
    pub(crate) fn same_checkout_objects(&self, current: &Self) -> bool {
        self.same_git_common(current)
            && Self::same_object(
                self.worktree_device,
                self.worktree_inode,
                self.volume_uuids.as_ref().map(|v| v.worktree),
                current.worktree_device,
                current.worktree_inode,
                current.volume_uuids.as_ref().map(|v| v.worktree),
            )
            && Self::same_object(
                self.git_pointer_device,
                self.git_pointer_inode,
                self.volume_uuids.as_ref().map(|v| v.git_pointer),
                current.git_pointer_device,
                current.git_pointer_inode,
                current.volume_uuids.as_ref().map(|v| v.git_pointer),
            )
            && Self::same_object(
                self.git_admin_device,
                self.git_admin_inode,
                self.volume_uuids.as_ref().map(|v| v.git_admin),
                current.git_admin_device,
                current.git_admin_inode,
                current.volume_uuids.as_ref().map(|v| v.git_admin),
            )
    }

    /// Identity for reviewing artifact-use markers, which removes only a
    /// marker and never a checkout. macOS renumbers mounts, so a witness saved
    /// without volume UUIDs can differ from today's device numbers while
    /// naming the same objects. The random root generation written at spawn
    /// already binds this checkout, so such a witness compares paths, inodes
    /// and that generation here. A saved UUID must still match, and a witness
    /// without a generation stays unsupported. Reclaiming a checkout keeps the
    /// exact checks in [`Self::matches_current_instance`].
    pub(crate) fn matches_instance_for_marker_review(&self, current: &Self) -> bool {
        let same = |saved_inode: u64,
                    saved_uuid: Option<[u8; 16]>,
                    current_inode: u64,
                    current_uuid: Option<[u8; 16]>| {
            saved_inode == current_inode
                && saved_uuid.is_none_or(|saved| current_uuid == Some(saved))
        };
        let saved = self.volume_uuids.as_ref();
        let observed = current.volume_uuids.as_ref();
        self.is_supported()
            && current.is_supported()
            && self.worktree_path == current.worktree_path
            && self.git_admin_path == current.git_admin_path
            && self.git_common_path == current.git_common_path
            && self.root_generation_sha256 == current.root_generation_sha256
            && same(
                self.worktree_inode,
                saved.map(|v| v.worktree),
                current.worktree_inode,
                observed.map(|v| v.worktree),
            )
            && same(
                self.git_pointer_inode,
                saved.map(|v| v.git_pointer),
                current.git_pointer_inode,
                observed.map(|v| v.git_pointer),
            )
            && same(
                self.git_admin_inode,
                saved.map(|v| v.git_admin),
                current.git_admin_inode,
                observed.map(|v| v.git_admin),
            )
            && same(
                self.git_common_inode,
                saved.map(|v| v.git_common),
                current.git_common_inode,
                observed.map(|v| v.git_common),
            )
    }

    /// The filesystem half of re-proving a schema-2 witness after macOS
    /// renumbered its mounts. Schema 2 saved device numbers without volume
    /// UUIDs, and a restart or remount assigns new ones, so the exact
    /// comparison fails for the same objects forever. This accepts only when
    /// every path, all four inodes and the random root generation written at
    /// spawn are unchanged, and the renumbering kept which of the four objects
    /// shared a device. It is never authority on its own: reclaim also needs
    /// the Git registration proof in `auto_cleanup`.
    pub(crate) fn matches_after_device_renumber(&self, current: &Self) -> bool {
        let saved_devices = [
            self.worktree_device,
            self.git_pointer_device,
            self.git_admin_device,
            self.git_common_device,
        ];
        let current_devices = [
            current.worktree_device,
            current.git_pointer_device,
            current.git_admin_device,
            current.git_common_device,
        ];
        let same_sharing = (0..4).all(|left| {
            (0..4).all(|right| {
                (saved_devices[left] == saved_devices[right])
                    == (current_devices[left] == current_devices[right])
            })
        });
        self.schema_version == 2
            && self.volume_uuids.is_none()
            && self.is_supported()
            && current.is_supported()
            && self.worktree_path == current.worktree_path
            && self.git_admin_path == current.git_admin_path
            && self.git_common_path == current.git_common_path
            && self.root_generation_sha256 == current.root_generation_sha256
            && self.worktree_inode == current.worktree_inode
            && self.git_pointer_inode == current.git_pointer_inode
            && self.git_admin_inode == current.git_admin_inode
            && self.git_common_inode == current.git_common_inode
            && same_sharing
    }
}

/// Immutable identity captured before `finish` begins physical cleanup. A
/// later retry may use it only when the same registered checkout is still
/// present and its remaining dirt is exclusively unstaged tracked deletions.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct FinishCleanupRecoveryProof {
    pub(crate) schema_version: u8,
    pub(crate) worktree_path: String,
    pub(crate) worktree_device: u64,
    pub(crate) worktree_inode: u64,
    pub(crate) git_pointer_device: u64,
    pub(crate) git_pointer_inode: u64,
    pub(crate) git_admin_path: String,
    pub(crate) git_admin_device: u64,
    pub(crate) git_admin_inode: u64,
    pub(crate) git_common_path: String,
    pub(crate) git_common_device: u64,
    pub(crate) git_common_inode: u64,
    pub(crate) branch_ref: String,
    pub(crate) head: String,
    pub(crate) branch_tip: String,
    /// Spawn-time root marker digest. Legacy proofs deserialize as `None` and
    /// cannot authorize resuming physical cleanup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) root_generation_sha256: Option<String>,
    /// Ignored directory roots that cleanup classified as disposable before
    /// Git began removing this checkout. A later orphan retry may remove
    /// entries beneath these exact prefixes; it must preserve newly appeared
    /// ignored files and directories elsewhere.
    #[serde(default)]
    pub(crate) disposable_ignored_root_prefixes: Vec<String>,
}

impl FinishCleanupRecoveryProof {
    /// A finish intent may use only the generation captured at spawn. The
    /// current checkout's metadata and root marker alone cannot reconstruct
    /// ownership after a path has been replaced.
    pub(crate) fn matches_spawned_instance(
        &self,
        witness: &SpawnedWorktreeInstanceWitness,
    ) -> bool {
        self.schema_version == 3
            && witness.is_supported()
            && self.worktree_path == witness.worktree_path
            && self.worktree_device == witness.worktree_device
            && self.worktree_inode == witness.worktree_inode
            && self.git_pointer_device == witness.git_pointer_device
            && self.git_pointer_inode == witness.git_pointer_inode
            && self.git_admin_path == witness.git_admin_path
            && self.git_admin_device == witness.git_admin_device
            && self.git_admin_inode == witness.git_admin_inode
            && self.git_common_path == witness.git_common_path
            && self.git_common_device == witness.git_common_device
            && self.git_common_inode == witness.git_common_inode
            && self
                .root_generation_sha256
                .as_deref()
                .is_some_and(crate::root_generation::valid_digest)
            && self.root_generation_sha256.as_deref() == witness.root_generation_sha256.as_deref()
    }
}

/// Build the stable v1 payload for the start of finish cleanup.
///
/// The recovery proof is an additive field produced only by the broker's
/// internal cleanup path; this public constructor keeps its original
/// signature for downstream callers.
pub fn session_finish_cleanup_started_payload(
    worktree_present: bool,
    branch_present: bool,
) -> String {
    session_finish_cleanup_started_payload_with_proof(worktree_present, branch_present, None)
}

pub(crate) fn session_finish_cleanup_started_payload_with_proof(
    worktree_present: bool,
    branch_present: bool,
    recovery_proof: Option<&FinishCleanupRecoveryProof>,
) -> String {
    json!({
        "worktree_present": worktree_present,
        "branch_present": branch_present,
        "recovery_proof": recovery_proof,
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

pub fn graph_impact_evaluated_payload(report: &crate::GraphImpactReport) -> String {
    json!({
        "schema_version": report.schema_version,
        "revision": report.repository.revision,
        "changed_files": report.request.changed_files,
        "diff_digest": report.request.diff_digest,
        "mode": report.request.mode.as_str(),
        "status": report.status.as_str(),
        "confidence": report.confidence.as_str(),
        "graph_revision": report.provenance.graph_revision,
        "engine_version": report.provenance.engine_version,
        "request_digest": report.provenance.request_digest,
        "result_digest": report.provenance.result_digest,
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

pub fn quality_report_publication_payload(
    report: &crate::QualityReport,
    outcome: &str,
    operation_id: Option<i64>,
    external_id: Option<&str>,
) -> String {
    json!({
        "schema_version": report.schema_version,
        "repository": report.repository,
        "pull_request": report.pull_request,
        "revision": report.revision,
        "base_revision": report.base_revision,
        "report_digest": report.provenance.report_digest,
        "status": report.status.as_str(),
        "outcome": outcome,
        "operation_id": operation_id,
        "external_id": external_id,
    })
    .to_string()
}

pub fn session_exit_payload(exit_code: i64) -> String {
    json!({ "exit_code": exit_code }).to_string()
}

/// A submission changed gate or graph policy; it was judged by the base
/// policy, and its own change applies once it lands.
pub fn merge_policy_deferred_payload(
    base: &str,
    gates_changed: bool,
    graph_policy_changed: bool,
) -> String {
    json!({
        "base": base,
        "gates_changed": gates_changed,
        "graph_policy_changed": graph_policy_changed,
    })
    .to_string()
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

/// `message` is the failure's error text, already redacted and capped by
/// the caller; it is omitted when absent so a success payload is unchanged.
pub fn broker_command_outcome_payload(
    command_surface: &str,
    exit_code: u8,
    failure_class: Option<&str>,
    operation_id: Option<i64>,
    queue_entry_id: Option<i64>,
    message: Option<&str>,
) -> String {
    let mut payload = json!({
        "command_surface": command_surface,
        "exit_code": exit_code,
        "failure_class": failure_class,
        "operation_id": operation_id,
        "queue_entry_id": queue_entry_id,
    });
    if let Some(message) = message {
        payload["message"] = json!(message);
    }
    payload.to_string()
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

/// `lease.released` for a lease a terminal transition released (#358):
/// which lease generation (`lease_id`, `created_at`) and why.
pub fn lease_released_payload(path: &str, reason: &str, lease_id: i64, created_at: i64) -> String {
    json!({
        "path": path,
        "reason": reason,
        "lease_id": lease_id,
        "created_at": created_at,
    })
    .to_string()
}

pub fn guard_paths_payload(paths: &[String]) -> String {
    json!({ "paths": paths }).to_string()
}

pub fn gate_result_payload(
    gate: &str,
    tree: &str,
    failure_class: Option<crate::types::GateFailureClass>,
    run_id: i64,
    cache_provenance: Option<&crate::GateCacheProvenance>,
) -> String {
    let mut payload = json!({
        "gate": gate,
        "tree": tree,
        "failure_class": failure_class.map(|class| class.as_str()),
        "run_id": run_id,
    });
    if let Some(provenance) = cache_provenance {
        payload["cache_provenance"] = json!(provenance);
    }
    payload.to_string()
}

/// `saved_ms` is the cached run's recorded duration — the execution time
/// this cache hit avoided.
pub fn gate_cached_payload(
    gate: &str,
    tree: &str,
    saved_ms: i64,
    cached_status: crate::types::GateStatus,
    failure_class: Option<crate::types::GateFailureClass>,
    run_id: i64,
    cache_provenance: &crate::GateCacheProvenance,
) -> String {
    json!({
        "gate": gate,
        "tree": tree,
        "saved_ms": saved_ms,
        "cached_status": cached_status.as_str(),
        "failure_class": failure_class.map(|class| class.as_str()),
        "run_id": run_id,
        "cache_provenance": cache_provenance,
    })
    .to_string()
}

pub fn gate_cache_bypassed_payload(gate: &str, tree: &str) -> String {
    json!({ "gate": gate, "tree": tree }).to_string()
}

pub fn gate_policy_trust_payload(policy_sha256: &str, source: &str) -> String {
    json!({ "policy_sha256": policy_sha256, "source": source }).to_string()
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

pub fn broker_checkout_fast_forwarded_payload(
    trigger: &str,
    branch: &str,
    upstream_ref: &str,
    from: &str,
    to: &str,
) -> String {
    json!({
        "trigger": trigger,
        "branch": branch,
        "upstream_ref": upstream_ref,
        "from": from,
        "to": to,
    })
    .to_string()
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

/// Automatic evidence-based fetch reconciliation is distinct from an
/// operator's attestation, whose event kinds retain their frozen meaning.
pub fn fetch_reconciled_payload(
    operation_id: i64,
    repository: &str,
    scope: &str,
    effect: crate::types::OperationEffect,
    status: crate::types::OperationStatus,
    exit_code: Option<i64>,
) -> String {
    fetch_reconciled_payload_from(
        FETCH_RECONCILED_OBSERVED_COMPLETION,
        operation_id,
        repository,
        scope,
        effect,
        status,
        exit_code,
    )
}

/// `reconciliation_source` of a fetch settled by observed child completion.
pub const FETCH_RECONCILED_OBSERVED_COMPLETION: &str = "observed_fetch_completion";
/// `reconciliation_source` of a repeatable fetch closed without an operator
/// because fetching again is its whole recovery (added 2026-11-12).
pub const FETCH_RECONCILED_REPEATABLE_AUTO_RESOLVED: &str = "repeatable_fetch_auto_resolved";

/// [`fetch_reconciled_payload`] naming which automatic rule settled it.
pub fn fetch_reconciled_payload_from(
    source: &str,
    operation_id: i64,
    repository: &str,
    scope: &str,
    effect: crate::types::OperationEffect,
    status: crate::types::OperationStatus,
    exit_code: Option<i64>,
) -> String {
    let mut payload: serde_json::Value = serde_json::from_str(&operation_payload(
        operation_id,
        crate::types::OperationProvider::Git,
        repository,
        scope,
        effect,
        status,
        exit_code,
    ))
    .expect("operation payload is JSON");
    payload["reconciliation_source"] = json!(source);
    payload.to_string()
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

#[cfg(test)]
mod spawned_instance_volume_tests {
    use super::{SpawnedWorktreeInstanceWitness, SpawnedWorktreeVolumeUuids};

    fn witness() -> SpawnedWorktreeInstanceWitness {
        SpawnedWorktreeInstanceWitness {
            schema_version: 3,
            worktree_path: "/work/task".into(),
            worktree_device: 11,
            worktree_inode: 101,
            git_pointer_device: 11,
            git_pointer_inode: 102,
            git_admin_path: "/repo/.git/worktrees/task".into(),
            git_admin_device: 22,
            git_admin_inode: 201,
            git_common_path: "/repo/.git".into(),
            git_common_device: 22,
            git_common_inode: 202,
            root_generation_sha256: Some("a".repeat(64)),
            volume_uuids: Some(SpawnedWorktreeVolumeUuids {
                worktree: [1; 16],
                git_pointer: [1; 16],
                git_admin: [2; 16],
                git_common: [2; 16],
            }),
        }
    }

    #[test]
    fn remount_accepts_saved_volume_and_inode_without_relaxing_snapshot_equality() {
        let saved = witness();
        let mut current = saved.clone();
        current.worktree_device += 10;
        current.git_pointer_device += 10;
        current.git_admin_device += 10;
        current.git_common_device += 10;
        assert!(saved.matches_current_instance(&current));
        assert_ne!(
            saved, current,
            "apply-time snapshots must still notice changes"
        );
        let encoded = serde_json::to_string(&saved).unwrap();
        let decoded = serde_json::from_str::<SpawnedWorktreeInstanceWitness>(&encoded).unwrap();
        assert_eq!(saved, decoded);
    }

    fn uuid_less_witness() -> SpawnedWorktreeInstanceWitness {
        let mut saved = witness();
        saved.schema_version = 2;
        saved.volume_uuids = None;
        saved
    }

    fn renumbered(saved: &SpawnedWorktreeInstanceWitness) -> SpawnedWorktreeInstanceWitness {
        let mut current = witness();
        current.worktree_device = saved.worktree_device + 10;
        current.git_pointer_device = saved.git_pointer_device + 10;
        current.git_admin_device = saved.git_admin_device + 10;
        current.git_common_device = saved.git_common_device + 10;
        current
    }

    #[test]
    fn marker_review_accepts_a_renumbered_mount_that_reclaim_still_refuses() {
        let saved = uuid_less_witness();
        let current = renumbered(&saved);
        assert!(saved.matches_instance_for_marker_review(&current));
        assert!(!saved.matches_current_instance(&current));
    }

    #[test]
    fn marker_review_refuses_replaced_objects_paths_generations_and_volumes() {
        let saved = uuid_less_witness();
        let mutations: &[fn(&mut SpawnedWorktreeInstanceWitness)] = &[
            |v| v.worktree_inode += 1,
            |v| v.git_pointer_inode += 1,
            |v| v.git_admin_inode += 1,
            |v| v.git_common_inode += 1,
            |v| v.worktree_path.push('x'),
            |v| v.git_admin_path.push('x'),
            |v| v.git_common_path.push('x'),
            |v| v.root_generation_sha256 = Some("b".repeat(64)),
            |v| v.root_generation_sha256 = None,
        ];
        for (index, mutate) in mutations.iter().enumerate() {
            let mut current = renumbered(&saved);
            mutate(&mut current);
            assert!(
                !saved.matches_instance_for_marker_review(&current),
                "mutation {index}"
            );
        }

        let with_uuids = witness();
        let mut other_volume = renumbered(&with_uuids);
        other_volume.volume_uuids.as_mut().unwrap().git_common = [3; 16];
        assert!(!with_uuids.matches_instance_for_marker_review(&other_volume));
        assert!(with_uuids.matches_instance_for_marker_review(&renumbered(&with_uuids)));
    }

    #[test]
    fn marker_review_refuses_a_witness_without_a_spawn_generation() {
        let mut saved = uuid_less_witness();
        saved.schema_version = 1;
        saved.root_generation_sha256 = None;
        assert!(!saved.matches_instance_for_marker_review(&renumbered(&saved)));
    }

    #[test]
    fn device_renumber_accepts_a_schema_two_witness_whose_objects_are_unchanged() {
        let saved = uuid_less_witness();
        let current = renumbered(&saved);
        assert!(!saved.matches_current_instance(&current));
        assert!(saved.matches_after_device_renumber(&current));
    }

    #[test]
    fn device_renumber_refuses_each_changed_object_path_and_generation() {
        let saved = uuid_less_witness();
        let mutations: &[(&str, fn(&mut SpawnedWorktreeInstanceWitness))] = &[
            ("worktree inode", |v| v.worktree_inode += 1),
            ("pointer inode", |v| v.git_pointer_inode += 1),
            ("admin inode", |v| v.git_admin_inode += 1),
            ("common inode", |v| v.git_common_inode += 1),
            ("worktree path", |v| v.worktree_path.push('x')),
            ("admin path", |v| v.git_admin_path.push('x')),
            ("common path", |v| v.git_common_path.push('x')),
            ("generation", |v| {
                v.root_generation_sha256 = Some("b".repeat(64))
            }),
            ("generation missing", |v| v.root_generation_sha256 = None),
        ];
        for (name, mutate) in mutations {
            let mut current = renumbered(&saved);
            mutate(&mut current);
            assert!(!saved.matches_after_device_renumber(&current), "{name}");
        }
    }

    #[test]
    fn device_renumber_refuses_a_changed_device_sharing_pattern() {
        let saved = uuid_less_witness();
        // At spawn the checkout and the repository were on different devices;
        // a checkout now on the repository's own device is a different layout.
        let mut current = renumbered(&saved);
        current.worktree_device = current.git_common_device;
        current.git_pointer_device = current.git_common_device;
        assert!(!saved.matches_after_device_renumber(&current));
        // The admin directory leaving the common directory's device too.
        let mut current = renumbered(&saved);
        current.git_admin_device += 1;
        assert!(!saved.matches_after_device_renumber(&current));
    }

    #[test]
    fn device_renumber_applies_only_to_uuid_less_schema_two_witnesses() {
        // Schema 3 already compares stable volume UUIDs; a mismatch there is a
        // different volume, never a renumbered one.
        let with_uuids = witness();
        let mut other_volume = renumbered(&with_uuids);
        other_volume.volume_uuids.as_mut().unwrap().git_common = [3; 16];
        assert!(!with_uuids.matches_after_device_renumber(&other_volume));
        // Schema 1 has no spawn generation and cannot be re-proven.
        let mut legacy = uuid_less_witness();
        legacy.schema_version = 1;
        legacy.root_generation_sha256 = None;
        assert!(!legacy.matches_after_device_renumber(&renumbered(&legacy)));
    }

    #[test]
    fn remount_refuses_replaced_objects_volumes_paths_and_generations() {
        let saved = witness();
        let mutations: &[fn(&mut SpawnedWorktreeInstanceWitness)] = &[
            |v| v.worktree_inode += 1,
            |v| v.git_pointer_inode += 1,
            |v| v.git_admin_inode += 1,
            |v| v.git_common_inode += 1,
            |v| v.worktree_path.push('x'),
            |v| v.git_admin_path.push('x'),
            |v| v.git_common_path.push('x'),
            |v| v.root_generation_sha256 = Some("b".repeat(64)),
            |v| v.root_generation_sha256 = None,
            |v| v.volume_uuids.as_mut().unwrap().worktree = [3; 16],
            |v| v.volume_uuids.as_mut().unwrap().git_pointer = [3; 16],
            |v| v.volume_uuids.as_mut().unwrap().git_admin = [3; 16],
            |v| v.volume_uuids.as_mut().unwrap().git_common = [3; 16],
            |v| v.volume_uuids.as_mut().unwrap().git_common = [0; 16],
            |v| v.volume_uuids = None,
            |v| v.schema_version = 99,
        ];
        for (index, mutate) in mutations.iter().enumerate() {
            let mut current = saved.clone();
            mutate(&mut current);
            assert!(
                !saved.matches_current_instance(&current),
                "mutation {index}"
            );
        }
    }

    #[test]
    fn legacy_witness_requires_original_device_and_never_infers_a_volume() {
        let current = witness();
        let mut legacy = current.clone();
        legacy.schema_version = 2;
        legacy.volume_uuids = None;
        assert!(legacy.matches_current_instance(&current));
        assert!(legacy.matches_current_instance(&legacy));
        let mut remounted = current.clone();
        remounted.git_admin_device += 1;
        remounted.git_common_device += 1;
        assert!(!legacy.matches_current_instance(&remounted));
        assert!(!current.matches_current_instance(&legacy));
        let mut malformed = legacy.clone();
        malformed.volume_uuids = current.volume_uuids.clone();
        assert!(!malformed.matches_current_instance(&current));
    }
}
