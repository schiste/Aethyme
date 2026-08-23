//! Explicit publication of a verified integration tip.
//!
//! Planning is observational: it resolves the selected promoted entry,
//! integration tip, local default branch, and the remote's advertised HEAD
//! without fetching, updating refs, or publishing anything.

use crate::broker::{Broker, BrokerOpError};
use crate::merge::PromoteConfig;
use crate::types::{MergeQueueEntry, MergeStatus, Session};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShipFreshnessResult {
    Ready,
    AlreadyPublished,
    RemoteTrackingMissing,
    RemoteTrackingStale,
    NonFastForward,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ShipFreshness {
    pub result: ShipFreshnessResult,
    pub remote_matches_planned_base: bool,
    pub remote_is_ancestor_of_integration: bool,
    pub integration_is_ancestor_of_remote: bool,
    pub fast_forward: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ShipPush {
    pub remote: String,
    pub source_sha: String,
    pub destination_ref: String,
    pub refspec: String,
    pub command: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ShipPlan {
    pub queue_entry: MergeQueueEntry,
    pub originating_session: Session,
    pub integration_ref: String,
    pub integration_sha: String,
    pub local_default_branch_ref: String,
    pub local_default_branch_sha: String,
    pub remote_default_branch_ref: String,
    pub remote_default_branch_sha: String,
    pub planned_remote_base_sha: Option<String>,
    pub freshness: ShipFreshness,
    pub target_repository: String,
    pub remote: String,
    pub proposed_push: ShipPush,
    pub local_main_sync_safe: bool,
}

impl Broker {
    /// Build a serializable, mutation-free publication plan for one promoted
    /// queue entry. The selected entry identifies provenance; publication is
    /// always the exact current integration tip containing that promotion.
    pub fn ship_plan(&mut self, entry_id: i64) -> Result<ShipPlan, BrokerOpError> {
        let entry = self
            .store()
            .merge_queue()?
            .into_iter()
            .find(|entry| entry.id == entry_id)
            .ok_or(BrokerOpError::ShipEntryNotFound { entry: entry_id })?;
        if entry.status != MergeStatus::Promoted {
            return Err(BrokerOpError::ShipEntryNotPromoted {
                entry: entry.id,
                status: entry.status.as_str(),
            });
        }
        let session = self.store().session(entry.session_id)?;

        let integration_ref = PromoteConfig::load(self.main_root()).branch;
        let integration_sha = self
            .repo_handle()
            .resolve_ref(&format!("refs/heads/{integration_ref}"))
            .ok_or_else(|| BrokerOpError::ShipPlanUnavailable {
                what: "integration ref",
                reason: format!("refs/heads/{integration_ref} does not exist"),
            })?;
        let promotion = entry
            .details_json
            .as_deref()
            .and_then(|details| serde_json::from_str::<serde_json::Value>(details).ok())
            .and_then(|details| details.get("commit")?.as_str().map(str::to_string))
            .ok_or_else(|| BrokerOpError::ShipPlanUnavailable {
                what: "promotion commit",
                reason: format!("entry {} has no promoted commit detail", entry.id),
            })?;
        if !self.repo_handle().is_ancestor(&promotion, &integration_sha) {
            return Err(BrokerOpError::ShipEntryNotOnIntegration {
                entry: entry.id,
                promotion,
                integration: integration_ref,
                head: integration_sha,
            });
        }

        let current_branch = self.repo_handle().current_branch()?;
        let remote = self
            .repo_handle()
            .config_get(&format!("branch.{current_branch}.remote"))
            .filter(|remote| remote != ".")
            .unwrap_or_else(|| "origin".into());
        let target_repository = self.repo_handle().remote_url(&remote)?;
        let remote_default = self.repo_handle().remote_default_branch(&remote)?;
        let default_branch = remote_default
            .ref_name
            .strip_prefix("refs/heads/")
            .ok_or_else(|| BrokerOpError::ShipPlanUnavailable {
                what: "remote default branch",
                reason: format!(
                    "advertised ref {} is not under refs/heads/",
                    remote_default.ref_name
                ),
            })?;
        let local_default_branch_ref = format!("refs/heads/{default_branch}");
        let local_default_branch_sha = self
            .repo_handle()
            .resolve_ref(&local_default_branch_ref)
            .ok_or_else(|| BrokerOpError::ShipPlanUnavailable {
                what: "local default branch",
                reason: format!("{local_default_branch_ref} does not exist"),
            })?;
        let tracking_ref = format!("refs/remotes/{remote}/{default_branch}");
        let planned_remote_base_sha = self.repo_handle().resolve_ref(&tracking_ref);
        let remote_matches_planned_base = planned_remote_base_sha
            .as_deref()
            .is_some_and(|planned| planned == remote_default.sha);
        let remote_is_ancestor_of_integration = self
            .repo_handle()
            .is_ancestor(&remote_default.sha, &integration_sha);
        let integration_is_ancestor_of_remote = self
            .repo_handle()
            .is_ancestor(&integration_sha, &remote_default.sha);
        let result = if remote_default.sha == integration_sha {
            ShipFreshnessResult::AlreadyPublished
        } else if planned_remote_base_sha.is_none() {
            ShipFreshnessResult::RemoteTrackingMissing
        } else if !remote_matches_planned_base {
            ShipFreshnessResult::RemoteTrackingStale
        } else if remote_is_ancestor_of_integration {
            ShipFreshnessResult::Ready
        } else {
            ShipFreshnessResult::NonFastForward
        };
        let fast_forward = remote_is_ancestor_of_integration;

        let destination_ref = remote_default.ref_name.clone();
        let refspec = format!("{integration_sha}:{destination_ref}");
        let proposed_push = ShipPush {
            remote: remote.clone(),
            source_sha: integration_sha.clone(),
            destination_ref,
            refspec: refspec.clone(),
            command: vec!["git".into(), "push".into(), remote.clone(), refspec],
        };
        let local_main_sync_safe = current_branch == default_branch
            && self.repo_handle().head_commit()? == local_default_branch_sha
            && !self.repo_handle().is_dirty()?
            && local_default_branch_sha == remote_default.sha
            && self
                .repo_handle()
                .is_ancestor(&local_default_branch_sha, &integration_sha);

        Ok(ShipPlan {
            queue_entry: entry,
            originating_session: session,
            integration_ref,
            integration_sha,
            local_default_branch_ref,
            local_default_branch_sha,
            remote_default_branch_ref: remote_default.ref_name,
            remote_default_branch_sha: remote_default.sha,
            planned_remote_base_sha,
            freshness: ShipFreshness {
                result,
                remote_matches_planned_base,
                remote_is_ancestor_of_integration,
                integration_is_ancestor_of_remote,
                fast_forward,
            },
            target_repository,
            remote,
            proposed_push,
            local_main_sync_safe,
        })
    }
}
