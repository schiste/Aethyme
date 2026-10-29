//! Reviewed recovery of an exact set of closed checkouts.
//!
//! The batch confirmation binds the same per-checkout content confirmations
//! used by single-session recovery. It never discovers additional targets at
//! apply time, and one protected checkout cannot prevent independent targets
//! from progressing. Every removal still uses the ordinary verified archive
//! and lifecycle-guarded revalidation path.

use sha2::{Digest, Sha256};

use crate::{Broker, BrokerOpError, CleanupResolveOutcome, SessionOrigin};

pub const CLEANUP_RESOLVE_BATCH_SCHEMA_VERSION: u32 = 1;
pub const MAX_CLEANUP_RESOLVE_BATCH_TARGETS: usize = 4096;

#[derive(Debug, Clone, serde::Serialize)]
pub struct CleanupResolveBatchTarget {
    pub session_id: i64,
    pub worktree_path: Option<String>,
    pub target_origin: Option<SessionOrigin>,
    /// No confirmation means this item is blocked and has no removal authority.
    pub confirmation: Option<String>,
    pub preserved_file_bytes: Option<u64>,
    pub unlanded_commits: Option<usize>,
    pub blocker: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CleanupResolveBatchPlan {
    pub schema_version: u32,
    pub repository_path: String,
    pub allow_adopted: bool,
    pub digest: String,
    pub targets: Vec<CleanupResolveBatchTarget>,
    pub apply_command: String,
}

#[derive(Debug, serde::Serialize)]
pub struct CleanupResolveBatchResult {
    pub session_id: i64,
    pub outcome: Option<CleanupResolveOutcome>,
    pub error: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct CleanupResolveBatchOutcome {
    pub schema_version: u32,
    pub plan_digest: String,
    pub results: Vec<CleanupResolveBatchResult>,
    pub removed_count: usize,
    /// Replan only these exact IDs to resume a partially completed batch.
    pub remaining_session_ids: Vec<i64>,
}

fn refused(session_id: i64, reason: impl Into<String>) -> BrokerOpError {
    BrokerOpError::CleanupResolveRefused {
        id: session_id,
        reason: reason.into(),
    }
}

pub(crate) fn validated_targets(ids: &[i64]) -> Result<Vec<i64>, BrokerOpError> {
    if ids.is_empty() || ids.len() > MAX_CLEANUP_RESOLVE_BATCH_TARGETS {
        return Err(refused(
            0,
            format!(
                "recovery requires between 1 and {MAX_CLEANUP_RESOLVE_BATCH_TARGETS} exact session IDs"
            ),
        ));
    }
    let mut ids = ids.to_vec();
    ids.sort_unstable();
    if let Some(id) = ids.iter().find(|id| **id <= 0) {
        return Err(refused(*id, "session IDs must be positive"));
    }
    if let Some(pair) = ids.windows(2).find(|pair| pair[0] == pair[1]) {
        return Err(refused(pair[0], "duplicate session ID in recovery batch"));
    }
    Ok(ids)
}

fn batch_digest(
    repository: &str,
    allow_adopted: bool,
    targets: &[CleanupResolveBatchTarget],
) -> String {
    let mut hash = Sha256::new();
    hash.update(b"aethyme.cleanup.resolve-batch/v1\0");
    hash.update((repository.len() as u64).to_be_bytes());
    hash.update(repository.as_bytes());
    hash.update([u8::from(allow_adopted)]);
    hash.update((targets.len() as u64).to_be_bytes());
    for target in targets {
        hash.update(target.session_id.to_be_bytes());
        for field in [
            target.worktree_path.as_deref().unwrap_or_default(),
            target
                .target_origin
                .map(SessionOrigin::as_str)
                .unwrap_or_default(),
        ] {
            hash.update((field.len() as u64).to_be_bytes());
            hash.update(field.as_bytes());
        }
        // Per-item confirmations already bind content, path and cleanup policy.
        // Blocker prose is diagnostic and must not become unstable authority.
        match target.confirmation.as_deref() {
            Some(confirmation) => {
                hash.update([1]);
                hash.update((confirmation.len() as u64).to_be_bytes());
                hash.update(confirmation.as_bytes());
            }
            None => hash.update([0]),
        }
    }
    format!("{:x}", hash.finalize())
}

impl Broker {
    /// Plan compact recovery for an explicit set, including already delivered
    /// spawned checkouts. Planning is read-only and ignores no safety refusal.
    pub fn cleanup_resolve_batch_plan(
        &self,
        session_ids: &[i64],
        allow_adopted: bool,
    ) -> Result<CleanupResolveBatchPlan, BrokerOpError> {
        let ids = validated_targets(session_ids)?;
        let mut targets = Vec::with_capacity(ids.len());
        for session_id in ids {
            let target = match self.cleanup_resolve_retirement_plan(session_id, allow_adopted) {
                Ok(plan) => CleanupResolveBatchTarget {
                    session_id,
                    worktree_path: Some(plan.worktree_path),
                    target_origin: Some(plan.target_origin),
                    confirmation: Some(plan.digest),
                    preserved_file_bytes: Some(
                        plan.staged_patch_bytes
                            .saturating_add(plan.unstaged_patch_bytes)
                            .saturating_add(plan.untracked_bytes)
                            .saturating_add(plan.ignored_bytes),
                    ),
                    unlanded_commits: Some(plan.unlanded_commits.len()),
                    blocker: None,
                },
                Err(error) => CleanupResolveBatchTarget {
                    session_id,
                    worktree_path: None,
                    target_origin: None,
                    confirmation: None,
                    preserved_file_bytes: None,
                    unlanded_commits: None,
                    blocker: Some(error.to_string()),
                },
            };
            targets.push(target);
        }
        let repository_path = self.main_root().to_string_lossy().into_owned();
        let digest = batch_digest(&repository_path, allow_adopted, &targets);
        let ids = targets
            .iter()
            .map(|target| target.session_id.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        let adopted_flag = if allow_adopted {
            " --allow-adopted"
        } else {
            ""
        };
        let apply_command = format!(
            "aethyme broker finish cleanup resolve {ids} --batch --archive{adopted_flag} --confirm {digest}"
        );
        Ok(CleanupResolveBatchPlan {
            schema_version: CLEANUP_RESOLVE_BATCH_SCHEMA_VERSION,
            digest,
            allow_adopted,
            repository_path,
            targets,
            apply_command,
        })
    }

    /// Revalidate the whole confirmation before the first side effect, then
    /// revalidate each selected item again under its ordinary archive guards.
    pub fn cleanup_resolve_batch_apply(
        &mut self,
        session_ids: &[i64],
        confirm: &str,
        allow_adopted: bool,
    ) -> Result<CleanupResolveBatchOutcome, BrokerOpError> {
        if confirm.len() != 64 || !confirm.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(BrokerOpError::CleanupResolveConfirmationNotSha256);
        }
        let plan = self.cleanup_resolve_batch_plan(session_ids, allow_adopted)?;
        if plan.digest != confirm {
            return Err(refused(
                0,
                "recovery batch confirmation changed; review the current exact-target plan",
            ));
        }
        let mut results = Vec::with_capacity(plan.targets.len());
        let mut remaining_session_ids = Vec::new();
        let mut removed_count = 0;
        for target in plan.targets {
            let session_id = target.session_id;
            let result = match target.confirmation {
                Some(confirmation) => match self.cleanup_resolve_retirement_apply(
                    session_id,
                    &confirmation,
                    allow_adopted,
                ) {
                    Ok(outcome) => {
                        if outcome.worktree_removed {
                            removed_count += 1;
                        } else {
                            remaining_session_ids.push(session_id);
                        }
                        CleanupResolveBatchResult {
                            session_id,
                            outcome: Some(outcome),
                            error: None,
                        }
                    }
                    Err(error) => {
                        remaining_session_ids.push(session_id);
                        CleanupResolveBatchResult {
                            session_id,
                            outcome: None,
                            error: Some(error.to_string()),
                        }
                    }
                },
                None => {
                    remaining_session_ids.push(session_id);
                    CleanupResolveBatchResult {
                        session_id,
                        outcome: None,
                        error: target.blocker,
                    }
                }
            };
            results.push(result);
        }
        Ok(CleanupResolveBatchOutcome {
            schema_version: CLEANUP_RESOLVE_BATCH_SCHEMA_VERSION,
            plan_digest: plan.digest,
            results,
            removed_count,
            remaining_session_ids,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(session_id: i64, confirmation: Option<&str>) -> CleanupResolveBatchTarget {
        CleanupResolveBatchTarget {
            session_id,
            worktree_path: Some(format!("/checkouts/{session_id}")),
            target_origin: Some(SessionOrigin::Spawned),
            confirmation: confirmation.map(str::to_owned),
            preserved_file_bytes: None,
            unlanded_commits: None,
            blocker: None,
        }
    }

    #[test]
    fn exact_targets_are_canonical_and_reject_ambiguous_or_unbounded_inputs() {
        assert_eq!(validated_targets(&[3, 1, 2]).unwrap(), [1, 2, 3]);
        assert!(validated_targets(&[]).is_err());
        assert!(validated_targets(&[0]).is_err());
        assert!(validated_targets(&[-1]).is_err());
        assert!(validated_targets(&[1, 1]).is_err());
        assert!(validated_targets(&vec![1; MAX_CLEANUP_RESOLVE_BATCH_TARGETS + 1]).is_err());
        let ids: Vec<_> = (1..=900).collect();
        assert_eq!(validated_targets(&ids).unwrap(), ids);
    }

    #[test]
    fn confirmation_binds_repository_exact_set_content_and_newly_eligible_items() {
        let original = vec![target(1, Some("first")), target(2, None)];
        let digest = batch_digest("/repo", false, &original);
        assert_ne!(digest, batch_digest("/other-repo", false, &original));
        assert_ne!(digest, batch_digest("/repo", false, &original[..1]));
        assert_ne!(
            digest,
            batch_digest(
                "/repo",
                false,
                &[target(1, Some("changed")), target(2, None)]
            )
        );
        assert_ne!(
            digest,
            batch_digest(
                "/repo",
                false,
                &[target(1, Some("first")), target(2, Some("new"))]
            )
        );
        assert_ne!(digest, batch_digest("/repo", true, &original));
        let mut changed_path = original.clone();
        changed_path[0].worktree_path = Some("/replacement".into());
        assert_ne!(digest, batch_digest("/repo", false, &changed_path));
        let mut changed_origin = original.clone();
        changed_origin[0].target_origin = Some(SessionOrigin::Adopted);
        assert_ne!(digest, batch_digest("/repo", false, &changed_origin));
        let mut diagnostic_change = original.clone();
        diagnostic_change[1].blocker = Some("a newer diagnostic".into());
        assert_eq!(digest, batch_digest("/repo", false, &diagnostic_change));
    }
}
