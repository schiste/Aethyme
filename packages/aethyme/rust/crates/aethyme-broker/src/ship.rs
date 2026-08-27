//! Explicit publication of a verified integration tip.
//!
//! Planning is observational: it resolves the selected promoted entry,
//! integration tip, local default branch, and the remote's advertised HEAD
//! without fetching, updating refs, or publishing anything.

use crate::broker::{Broker, BrokerOpError};
use crate::merge::PromoteConfig;
use crate::operations::{CoordinatedCommand, CoordinatedOperationReport, UnknownOutcomeRecovery};
use crate::remote_target::ResolvedRemoteTarget;
use crate::types::{
    CoordinatedOperation, EntryExposureResolutionKind, EntryPathExposure, MergeQueueEntry,
    MergeStatus, OperationEffect, OperationProvider, Session,
};

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
    pub target: ResolvedRemoteTarget,
    pub proposed_push: ShipPush,
    pub local_main_sync_safe: bool,
}

fn ship_operation_failure(
    phase: &'static str,
    report: &CoordinatedOperationReport,
) -> BrokerOpError {
    if report.operation.status == crate::OperationStatus::OutcomeUnknown {
        BrokerOpError::CoordinatedOperationBlocked {
            repository: report.operation.repository.clone(),
            operation_id: report.operation.id,
            recovery: UnknownOutcomeRecovery::from_operation(&report.operation),
        }
    } else {
        BrokerOpError::ShipOperationFailed {
            phase,
            operation_id: report.operation.id,
            status: report.operation.status.as_str(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ShipExecutionReport {
    pub plan: ShipPlan,
    pub fetch_operation: CoordinatedOperation,
    pub push_operation: CoordinatedOperation,
    pub verify_operation: CoordinatedOperation,
    pub published_sha: String,
    pub verified_remote_sha: String,
    pub resolved_exposures: Vec<EntryPathExposure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync_operation: Option<CoordinatedOperation>,
    pub local_main_sync: ShipLocalMainSync,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ShipLocalMainSync {
    pub requested: bool,
    pub synchronized: bool,
    pub before_sha: String,
    pub after_sha: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub follow_up_command: Option<String>,
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
        let target = self
            .repo_handle()
            .resolve_remote_target(&remote, None)
            .map_err(|error| BrokerOpError::ShipPlanUnavailable {
                what: "remote target",
                reason: error.to_string(),
            })?;
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
            target,
            proposed_push,
            local_main_sync_safe,
        })
    }

    /// Publish the exact confirmed integration SHA with an ordinary push.
    /// Remote freshness is refreshed through the durable operation
    /// coordinator before the push, and the advertised remote ref is checked
    /// again afterward.
    pub fn ship_execute(
        &mut self,
        entry_id: i64,
        confirm: &str,
    ) -> Result<ShipExecutionReport, BrokerOpError> {
        self.ship_execute_with_sync(entry_id, confirm, false)
    }

    pub fn ship_execute_with_sync(
        &mut self,
        entry_id: i64,
        confirm: &str,
        sync_main: bool,
    ) -> Result<ShipExecutionReport, BrokerOpError> {
        if confirm.len() != 40
            || !confirm
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        {
            return Err(BrokerOpError::ShipConfirmationNotFullSha);
        }
        let plan = self.ship_plan(entry_id)?;
        if confirm != plan.integration_sha {
            return Err(BrokerOpError::ShipConfirmationMismatch {
                expected: plan.integration_sha,
                actual: confirm.into(),
            });
        }
        if sync_main {
            validate_local_main_sync(self, &plan, confirm)
                .map_err(|reason| BrokerOpError::ShipLocalMainUnsafe { reason })?;
        }
        let planned_base = plan.planned_remote_base_sha.clone().ok_or_else(|| {
            BrokerOpError::ShipRemoteBaseUnavailable {
                tracking_ref: tracking_ref(&plan),
            }
        })?;
        let main_root = self.main_root().to_path_buf();
        let tracking_ref = tracking_ref(&plan);

        let fetch = self.run_coordinated_operation_at(
            CoordinatedCommand {
                session_id: plan.originating_session.id,
                provider: OperationProvider::Git,
                repository: None,
                resolved_target: Some(plan.target.clone()),
                scope: Some(format!("ship:fetch:{}", plan.remote_default_branch_ref)),
                declared_effect: None,
                destructive_confirmed: false,
                authorization_reason: Some(format!(
                    "confirmed broker ship for queue entry {}",
                    plan.queue_entry.id
                )),
                args: vec![
                    "fetch".into(),
                    "--no-tags".into(),
                    "--force".into(),
                    plan.target.remote_name.clone(),
                    format!("{}:{tracking_ref}", plan.remote_default_branch_ref),
                ],
            },
            &main_root,
        )?;
        if !fetch.ok() {
            return Err(ship_operation_failure("fetch", &fetch));
        }
        let fetched_remote = self
            .repo_handle()
            .resolve_ref(&tracking_ref)
            .ok_or_else(|| BrokerOpError::ShipPlanUnavailable {
                what: "fetched remote default branch",
                reason: format!("{tracking_ref} does not resolve after fetch"),
            })?;
        if fetched_remote != planned_base {
            return Err(BrokerOpError::ShipRemoteMoved {
                remote_ref: plan.remote_default_branch_ref.clone(),
                expected: planned_base,
                actual: fetched_remote,
            });
        }
        if !self.repo_handle().is_ancestor(&fetched_remote, confirm) {
            return Err(BrokerOpError::ShipNonFastForward {
                remote_ref: plan.remote_default_branch_ref.clone(),
                remote_sha: fetched_remote,
                integration_sha: confirm.into(),
            });
        }
        let current_integration = self
            .repo_handle()
            .resolve_ref(&format!("refs/heads/{}", plan.integration_ref))
            .ok_or_else(|| BrokerOpError::ShipPlanUnavailable {
                what: "integration ref",
                reason: format!("{} disappeared during execution", plan.integration_ref),
            })?;
        if current_integration != confirm {
            return Err(BrokerOpError::ShipConfirmationMismatch {
                expected: current_integration,
                actual: confirm.into(),
            });
        }

        let push = self.run_coordinated_operation_at(
            CoordinatedCommand {
                session_id: plan.originating_session.id,
                provider: OperationProvider::Git,
                repository: None,
                resolved_target: Some(plan.target.clone()),
                scope: Some(format!("ship:push:{}", plan.remote_default_branch_ref)),
                declared_effect: None,
                destructive_confirmed: false,
                authorization_reason: Some(format!(
                    "confirmed broker ship for queue entry {} at {confirm}",
                    plan.queue_entry.id
                )),
                args: vec![
                    "push".into(),
                    plan.target.remote_name.clone(),
                    format!("{confirm}:{}", plan.remote_default_branch_ref),
                ],
            },
            &main_root,
        )?;
        if !push.ok() {
            return Err(ship_operation_failure("push", &push));
        }

        let verify = self.run_coordinated_operation_at(
            CoordinatedCommand {
                session_id: plan.originating_session.id,
                provider: OperationProvider::Git,
                repository: None,
                resolved_target: Some(plan.target.clone()),
                scope: Some(format!("ship:verify:{}", plan.remote_default_branch_ref)),
                declared_effect: Some(OperationEffect::Read),
                destructive_confirmed: false,
                authorization_reason: None,
                args: vec![
                    "ls-remote".into(),
                    plan.target.remote_name.clone(),
                    plan.remote_default_branch_ref.clone(),
                ],
            },
            &main_root,
        )?;
        if !verify.ok() {
            return Err(ship_operation_failure("verification", &verify));
        }
        let verified_remote_sha = ls_remote_sha(&verify.stdout, &plan.remote_default_branch_ref)
            .unwrap_or_else(|| "<missing>".into());
        if verified_remote_sha != confirm {
            self.store().transition_coordinated_operation(
                verify.operation.id,
                crate::OperationStatus::Failed,
                Some(1),
                Some(
                    &serde_json::json!({
                        "reason": "remote_verification_mismatch",
                        "expected": confirm,
                        "actual": verified_remote_sha,
                    })
                    .to_string(),
                ),
            )?;
            return Err(BrokerOpError::ShipVerificationMismatch {
                remote_ref: plan.remote_default_branch_ref.clone(),
                expected: confirm.into(),
                actual: verified_remote_sha,
            });
        }

        // Remote observation is the publication authority. Resolve every
        // outstanding promoted entry contained in the verified tip, not only
        // the entry selected to authorize this publication. Plans, pushes,
        // and stale/unknown observations never reach this point.
        let contained_entry_ids = self
            .store()
            .outstanding_entry_path_exposures()?
            .into_iter()
            .filter(|exposure| {
                self.repo_handle()
                    .is_ancestor(&exposure.promotion_sha, &verified_remote_sha)
            })
            .map(|exposure| exposure.queue_entry_id)
            .collect::<Vec<_>>();
        let resolution_evidence = format!(
            "broker ship verified remote {} at {}",
            plan.remote_default_branch_ref, verified_remote_sha
        );
        let resolved_exposures = self.store().resolve_entry_path_exposures(
            &contained_entry_ids,
            EntryExposureResolutionKind::ShipVerified,
            &verified_remote_sha,
            &resolution_evidence,
        )?;
        let _ = self.refresh_advisory_projection();

        let before_sha = plan.local_default_branch_sha.clone();
        let (sync_operation, local_main_sync) = if sync_main {
            validate_local_main_sync(self, &plan, confirm).map_err(|reason| {
                BrokerOpError::ShipLocalMainMovedAfterPublish {
                    published_sha: confirm.into(),
                    reason,
                }
            })?;
            let sync = self.run_coordinated_operation_at(
                CoordinatedCommand {
                    session_id: plan.originating_session.id,
                    provider: OperationProvider::Git,
                    repository: None,
                    resolved_target: None,
                    scope: Some(format!("ship:sync:{}", plan.local_default_branch_ref)),
                    declared_effect: None,
                    destructive_confirmed: false,
                    authorization_reason: Some(format!(
                        "explicit --sync-main after publishing queue entry {} at {confirm}",
                        plan.queue_entry.id
                    )),
                    args: vec!["merge".into(), "--ff-only".into(), confirm.into()],
                },
                &main_root,
            )?;
            if !sync.ok() {
                return Err(ship_operation_failure("local-main synchronization", &sync));
            }
            let after_sha = self.repo_handle().head_commit()?;
            if after_sha != confirm {
                self.store().transition_coordinated_operation(
                    sync.operation.id,
                    crate::OperationStatus::Failed,
                    Some(1),
                    Some(
                        &serde_json::json!({
                            "reason": "local_main_verification_mismatch",
                            "expected": confirm,
                            "actual": after_sha,
                        })
                        .to_string(),
                    ),
                )?;
                return Err(BrokerOpError::ShipLocalMainMovedAfterPublish {
                    published_sha: confirm.into(),
                    reason: format!("expected local main {confirm}, observed {after_sha}"),
                });
            }
            (
                Some(sync.operation),
                ShipLocalMainSync {
                    requested: true,
                    synchronized: true,
                    before_sha,
                    after_sha,
                    follow_up_command: None,
                },
            )
        } else {
            let after_sha = self.repo_handle().head_commit()?;
            (
                None,
                ShipLocalMainSync {
                    requested: false,
                    synchronized: false,
                    before_sha,
                    after_sha,
                    follow_up_command: Some(format!(
                        "aethyme broker ship execute --entry {} --confirm {} --sync-main",
                        plan.queue_entry.id, confirm
                    )),
                },
            )
        };

        Ok(ShipExecutionReport {
            plan,
            fetch_operation: fetch.operation,
            push_operation: push.operation,
            verify_operation: verify.operation,
            published_sha: confirm.into(),
            verified_remote_sha,
            resolved_exposures,
            sync_operation,
            local_main_sync,
        })
    }
}

fn validate_local_main_sync(broker: &Broker, plan: &ShipPlan, confirm: &str) -> Result<(), String> {
    let default_branch = plan
        .local_default_branch_ref
        .strip_prefix("refs/heads/")
        .unwrap_or(&plan.local_default_branch_ref);
    let current_branch = broker
        .repo_handle()
        .current_branch()
        .map_err(|error| error.to_string())?;
    if current_branch != default_branch {
        return Err(format!(
            "primary checkout is on {current_branch}, expected {default_branch}"
        ));
    }
    if broker
        .repo_handle()
        .is_dirty()
        .map_err(|error| error.to_string())?
    {
        return Err("primary default-branch checkout is dirty".into());
    }
    let current_head = broker
        .repo_handle()
        .head_commit()
        .map_err(|error| error.to_string())?;
    let current_ref = broker
        .repo_handle()
        .resolve_ref(&plan.local_default_branch_ref)
        .ok_or_else(|| format!("{} no longer resolves", plan.local_default_branch_ref))?;
    if current_head != plan.local_default_branch_sha || current_ref != plan.local_default_branch_sha
    {
        return Err(format!(
            "local main moved since planning: expected {}, observed HEAD {} and ref {}",
            plan.local_default_branch_sha, current_head, current_ref
        ));
    }
    if !broker.repo_handle().is_ancestor(&current_head, confirm) {
        return Err(format!(
            "local main {current_head} has diverged from confirmed integration {confirm}"
        ));
    }
    Ok(())
}

fn tracking_ref(plan: &ShipPlan) -> String {
    let branch = plan
        .remote_default_branch_ref
        .strip_prefix("refs/heads/")
        .unwrap_or(&plan.remote_default_branch_ref);
    format!("refs/remotes/{}/{branch}", plan.target.remote_name)
}

fn ls_remote_sha(output: &str, ref_name: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let (sha, name) = line.split_once('\t')?;
        (name == ref_name
            && sha.len() == 40
            && sha.chars().all(|character| character.is_ascii_hexdigit()))
        .then(|| sha.to_string())
    })
}
