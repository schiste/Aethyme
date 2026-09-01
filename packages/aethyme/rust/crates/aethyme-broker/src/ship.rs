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

pub const PUBLICATION_POLICY_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShipPublicationMode {
    Direct,
    ReviewGated,
}

impl Default for ShipPublicationMode {
    fn default() -> Self {
        Self::Direct
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ShipPublicationPolicy {
    pub schema_version: u32,
    pub mode: ShipPublicationMode,
    pub allow_break_glass: bool,
}

impl Default for ShipPublicationPolicy {
    fn default() -> Self {
        Self {
            schema_version: PUBLICATION_POLICY_SCHEMA_VERSION,
            mode: ShipPublicationMode::Direct,
            allow_break_glass: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ShipReviewEvidence {
    pub queue_entry_id: i64,
    pub session_id: i64,
    pub covered: bool,
    pub lifecycle_id: Option<i64>,
    pub reviewed_queue_entry_id: Option<i64>,
    pub repository: Option<String>,
    pub pr_number: Option<i64>,
    pub target_branch: Option<String>,
    pub reviewed_commit_sha: Option<String>,
    pub lifecycle_state: Option<String>,
    pub lifecycle_generation: Option<i64>,
    pub evidence_digest: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ShipPublicationAssessment {
    pub policy: ShipPublicationPolicy,
    pub source_commit: String,
    pub satisfied: bool,
    pub evidence: Vec<ShipReviewEvidence>,
    pub remediation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ShipPublicationAuthorizationKind {
    Direct,
    Reviewed,
    BreakGlass,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ShipPublicationAuthorization {
    pub kind: ShipPublicationAuthorizationKind,
    pub policy_source_commit: String,
    pub live_evidence_revalidated: bool,
    pub reason_digest: Option<String>,
}

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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ShipPromotedEntry {
    pub queue_entry_id: i64,
    pub session_id: i64,
    pub promotion_sha: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ShipPlan {
    pub queue_entry: MergeQueueEntry,
    pub originating_session: Session,
    pub integration_ref: String,
    /// Current integration tip, retained as planning context.
    pub integration_sha: String,
    /// Exact selected promoted prefix authorized for publication.
    pub publication_sha: String,
    pub included_entries: Vec<ShipPromotedEntry>,
    pub excluded_entries: Vec<ShipPromotedEntry>,
    pub local_default_branch_ref: String,
    pub local_default_branch_sha: String,
    pub remote_default_branch_ref: String,
    pub remote_default_branch_sha: String,
    pub planned_remote_base_sha: Option<String>,
    pub freshness: ShipFreshness,
    pub target: ResolvedRemoteTarget,
    pub proposed_push: ShipPush,
    pub publication_policy: ShipPublicationAssessment,
    pub local_main_sync_safe: bool,
    pub local_main_sync_assessment: ShipLocalMainSyncAssessment,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ShipLocalMainSyncAssessment {
    pub safe: bool,
    pub current_branch_matches: bool,
    pub local_head_unchanged: bool,
    pub fast_forward: bool,
    pub tracked_dirty_paths: Vec<String>,
    pub untracked_paths: Vec<String>,
    pub conflicting_untracked_paths: Vec<String>,
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
    pub publication_authorization: ShipPublicationAuthorization,
    pub verified_remote_sha: String,
    pub resolved_exposures: Vec<EntryPathExposure>,
    pub resolved_advisories: Vec<crate::Advisory>,
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
    /// Build a serializable, mutation-free publication plan through one exact
    /// promoted queue entry. Later integration promotions are listed but never
    /// silently added to the proposed push.
    pub fn ship_plan(&mut self, entry_id: i64) -> Result<ShipPlan, BrokerOpError> {
        let queue = self.store().merge_queue()?;
        let entry = queue
            .iter()
            .into_iter()
            .find(|entry| entry.id == entry_id)
            .cloned()
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
        let publication_sha = promotion.clone();
        let mut included_entries = Vec::new();
        let mut excluded_entries = Vec::new();
        for promoted in queue
            .iter()
            .filter(|candidate| candidate.status == MergeStatus::Promoted)
        {
            let promoted_sha =
                promotion_sha(promoted).ok_or_else(|| BrokerOpError::ShipPlanUnavailable {
                    what: "promoted queue provenance",
                    reason: format!("entry {} has no promoted commit detail", promoted.id),
                })?;
            if !self
                .repo_handle()
                .is_ancestor(&promoted_sha, &integration_sha)
            {
                continue;
            }
            let item = ShipPromotedEntry {
                queue_entry_id: promoted.id,
                session_id: promoted.session_id,
                promotion_sha: promoted_sha.clone(),
            };
            if self
                .repo_handle()
                .is_ancestor(&promoted_sha, &publication_sha)
            {
                included_entries.push(item);
            } else {
                excluded_entries.push(item);
            }
        }
        included_entries.sort_by_key(|item| item.queue_entry_id);
        excluded_entries.sort_by_key(|item| item.queue_entry_id);
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
        let publication_policy = publication_assessment(
            self,
            &publication_sha,
            &included_entries,
            &queue,
            &target.coordination_key,
            default_branch,
        )?;
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
            .is_ancestor(&remote_default.sha, &publication_sha);
        let integration_is_ancestor_of_remote = self
            .repo_handle()
            .is_ancestor(&publication_sha, &remote_default.sha);
        let result = if remote_default.sha == publication_sha {
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

        if fast_forward {
            let recorded = included_entries
                .iter()
                .map(|item| item.promotion_sha.as_str())
                .collect::<std::collections::BTreeSet<_>>();
            let unrecorded = self
                .repo_handle()
                .first_parent_commits_between_oldest(&remote_default.sha, &publication_sha)?
                .into_iter()
                .filter(|commit| !recorded.contains(commit.as_str()))
                .collect::<Vec<_>>();
            if !unrecorded.is_empty() {
                return Err(BrokerOpError::ShipPlanUnavailable {
                    what: "publication prefix",
                    reason: format!(
                        "selected prefix contains unrecorded integration commits: {}",
                        unrecorded.join(", ")
                    ),
                });
            }
        }

        let destination_ref = remote_default.ref_name.clone();
        let refspec = format!("{publication_sha}:{destination_ref}");
        let proposed_push = ShipPush {
            remote: remote.clone(),
            source_sha: publication_sha.clone(),
            destination_ref,
            refspec: refspec.clone(),
            command: vec!["git".into(), "push".into(), remote.clone(), refspec],
        };
        let local_main_sync_assessment = assess_local_main_sync(
            self,
            &default_branch,
            &local_default_branch_ref,
            &local_default_branch_sha,
            &publication_sha,
        )
        .map_err(|reason| BrokerOpError::ShipPlanUnavailable {
            what: "local-main synchronization",
            reason,
        })?;
        let local_main_sync_safe = local_main_sync_assessment.safe;

        Ok(ShipPlan {
            queue_entry: entry,
            originating_session: session,
            integration_ref,
            integration_sha,
            publication_sha,
            included_entries,
            excluded_entries,
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
            publication_policy,
            local_main_sync_safe,
            local_main_sync_assessment,
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
        self.ship_execute_with_policy(entry_id, confirm, false, false, None)
    }

    pub fn ship_execute_with_sync(
        &mut self,
        entry_id: i64,
        confirm: &str,
        sync_main: bool,
    ) -> Result<ShipExecutionReport, BrokerOpError> {
        self.ship_execute_with_policy(entry_id, confirm, sync_main, false, None)
    }

    pub fn ship_execute_with_policy(
        &mut self,
        entry_id: i64,
        confirm: &str,
        sync_main: bool,
        break_glass: bool,
        break_glass_reason: Option<&str>,
    ) -> Result<ShipExecutionReport, BrokerOpError> {
        if confirm.len() != 40
            || !confirm
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        {
            return Err(BrokerOpError::ShipConfirmationNotFullSha);
        }
        let plan = self.ship_plan(entry_id)?;
        if confirm != plan.publication_sha {
            return Err(BrokerOpError::ShipConfirmationMismatch {
                expected: plan.publication_sha,
                actual: confirm.into(),
            });
        }
        let publication_authorization =
            authorize_publication(self, &plan, break_glass, break_glass_reason)?;
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
        if !self
            .repo_handle()
            .is_ancestor(confirm, &current_integration)
        {
            return Err(BrokerOpError::ShipEntryNotOnIntegration {
                entry: plan.queue_entry.id,
                promotion: confirm.into(),
                integration: plan.integration_ref.clone(),
                head: current_integration,
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
                    "{}; confirmed broker ship for queue entry {} at {confirm}",
                    publication_authorization_label(&publication_authorization),
                    plan.queue_entry.id,
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
        let resolved_advisories = self
            .store()
            .resolve_entry_advisories_without_active_leases(
                &contained_entry_ids,
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
            publication_authorization,
            verified_remote_sha,
            resolved_exposures,
            resolved_advisories,
            sync_operation,
            local_main_sync,
        })
    }
}

fn publication_assessment(
    broker: &mut Broker,
    publication_sha: &str,
    included_entries: &[ShipPromotedEntry],
    queue: &[MergeQueueEntry],
    target_repository: &str,
    target_branch: &str,
) -> Result<ShipPublicationAssessment, BrokerOpError> {
    let policy = publication_policy_at(broker, publication_sha)?;
    if policy.mode == ShipPublicationMode::Direct {
        return Ok(ShipPublicationAssessment {
            policy,
            source_commit: publication_sha.into(),
            satisfied: true,
            evidence: Vec::new(),
            remediation: None,
        });
    }

    let included_ids = included_entries
        .iter()
        .map(|entry| entry.queue_entry_id)
        .collect::<std::collections::BTreeSet<_>>();
    let mut evidence = Vec::with_capacity(included_entries.len());
    for included in included_entries {
        let queue_entry = queue
            .iter()
            .find(|entry| entry.id == included.queue_entry_id)
            .expect("included entry came from queue");
        let lifecycle = broker
            .store()
            .review_lifecycle_for_session(included.session_id)?;
        let Some(lifecycle) = lifecycle else {
            evidence.push(ShipReviewEvidence {
                queue_entry_id: included.queue_entry_id,
                session_id: included.session_id,
                covered: false,
                lifecycle_id: None,
                reviewed_queue_entry_id: None,
                repository: None,
                pr_number: None,
                target_branch: None,
                reviewed_commit_sha: None,
                lifecycle_state: None,
                lifecycle_generation: None,
                evidence_digest: None,
                reason: "session has no registered review lifecycle".into(),
            });
            continue;
        };
        let reviewed_entry = lifecycle
            .queue_entry_id
            .and_then(|id| queue.iter().find(|entry| entry.id == id));
        let reviewed_entry_in_prefix = lifecycle
            .queue_entry_id
            .is_some_and(|id| included_ids.contains(&id));
        let reviewed_entry_matches = reviewed_entry.is_some_and(|entry| {
            entry.session_id == lifecycle.session_id
                && entry.head_commit == lifecycle.commit_sha
                && entry.status == MergeStatus::Promoted
        });
        let target_matches =
            lifecycle.repository == target_repository && lifecycle.target_branch == target_branch;
        let includes_entry_commit = broker
            .repo_handle()
            .is_ancestor(&queue_entry.head_commit, &lifecycle.commit_sha);
        let covered = lifecycle.state == crate::ReviewLifecycleState::ValidationUnlocked
            && target_matches
            && reviewed_entry_in_prefix
            && reviewed_entry_matches
            && includes_entry_commit;
        let reason = if lifecycle.state != crate::ReviewLifecycleState::ValidationUnlocked {
            format!("review lifecycle is {}", lifecycle.state.as_str())
        } else if !target_matches {
            "review lifecycle repository or base does not match the publication target".into()
        } else if !reviewed_entry_in_prefix {
            "reviewed queue entry is outside the selected publication prefix".into()
        } else if !reviewed_entry_matches {
            "review lifecycle queue and commit provenance do not match a promoted entry".into()
        } else if !includes_entry_commit {
            "reviewed session commit does not contain this included entry commit".into()
        } else {
            "covered by exact validation-unlocked review provenance".into()
        };
        evidence.push(ShipReviewEvidence {
            queue_entry_id: included.queue_entry_id,
            session_id: included.session_id,
            covered,
            lifecycle_id: Some(lifecycle.id),
            reviewed_queue_entry_id: lifecycle.queue_entry_id,
            repository: Some(lifecycle.repository),
            pr_number: Some(lifecycle.pr_number),
            target_branch: Some(lifecycle.target_branch),
            reviewed_commit_sha: Some(lifecycle.commit_sha),
            lifecycle_state: Some(lifecycle.state.as_str().into()),
            lifecycle_generation: Some(lifecycle.generation),
            evidence_digest: lifecycle.evidence_digest,
            reason,
        });
    }
    let satisfied = !evidence.is_empty() && evidence.iter().all(|item| item.covered);
    let remediation = (!satisfied).then(|| {
        "complete and unlock review for every included session, then rebuild broker ship plan"
            .into()
    });
    Ok(ShipPublicationAssessment {
        policy,
        source_commit: publication_sha.into(),
        satisfied,
        evidence,
        remediation,
    })
}

fn publication_policy_at(
    broker: &Broker,
    source_commit: &str,
) -> Result<ShipPublicationPolicy, BrokerOpError> {
    let Some(text) = broker
        .repo_handle()
        .file_at_commit(source_commit, ".aethyme/config.toml")?
    else {
        return Ok(ShipPublicationPolicy::default());
    };
    let value =
        text.parse::<toml::Value>()
            .map_err(|error| BrokerOpError::ShipPublicationPolicy {
                reason: format!("committed .aethyme/config.toml is invalid: {error}"),
                remediation: "fix and submit the committed publication policy".into(),
            })?;
    let Some(publication) = value.get("publication") else {
        return Ok(ShipPublicationPolicy::default());
    };
    let policy: ShipPublicationPolicy =
        publication
            .clone()
            .try_into()
            .map_err(|error| BrokerOpError::ShipPublicationPolicy {
                reason: format!("committed [publication] policy is invalid: {error}"),
                remediation: "fix and submit the committed publication policy".into(),
            })?;
    if policy.schema_version != PUBLICATION_POLICY_SCHEMA_VERSION {
        return Err(BrokerOpError::ShipPublicationPolicy {
            reason: format!(
                "publication policy schema {} is unsupported; expected {}",
                policy.schema_version, PUBLICATION_POLICY_SCHEMA_VERSION
            ),
            remediation: "upgrade Aethyme or use a supported committed policy schema".into(),
        });
    }
    Ok(policy)
}

fn authorize_publication(
    broker: &mut Broker,
    plan: &ShipPlan,
    break_glass: bool,
    break_glass_reason: Option<&str>,
) -> Result<ShipPublicationAuthorization, BrokerOpError> {
    if break_glass {
        if plan.publication_policy.policy.mode != ShipPublicationMode::ReviewGated {
            return Err(BrokerOpError::ShipPublicationPolicy {
                reason: "--break-glass is not valid for the direct publication profile".into(),
                remediation: "execute the confirmed direct ship without --break-glass".into(),
            });
        }
        if !plan.publication_policy.policy.allow_break_glass {
            return Err(BrokerOpError::ShipPublicationPolicy {
                reason: "the committed review-gated policy does not allow break-glass publication"
                    .into(),
                remediation: "complete review evidence or submit a reviewed policy change".into(),
            });
        }
        let reason = break_glass_reason.unwrap_or_default();
        if reason.is_empty() || reason.len() > 500 || reason.chars().any(char::is_control) {
            return Err(BrokerOpError::ShipPublicationPolicy {
                reason: "--break-glass requires --reason with 1..=500 non-control characters"
                    .into(),
                remediation: "provide the separately authorized emergency reason".into(),
            });
        }
        return Ok(ShipPublicationAuthorization {
            kind: ShipPublicationAuthorizationKind::BreakGlass,
            policy_source_commit: plan.publication_policy.source_commit.clone(),
            live_evidence_revalidated: false,
            reason_digest: Some(crate::sha256_bytes(reason.as_bytes())),
        });
    }
    if break_glass_reason.is_some() {
        return Err(BrokerOpError::ShipPublicationPolicy {
            reason: "--reason for ship publication is accepted only with --break-glass".into(),
            remediation: "remove --reason or add the separately authorized --break-glass flag"
                .into(),
        });
    }
    if plan.publication_policy.policy.mode == ShipPublicationMode::Direct {
        return Ok(ShipPublicationAuthorization {
            kind: ShipPublicationAuthorizationKind::Direct,
            policy_source_commit: plan.publication_policy.source_commit.clone(),
            live_evidence_revalidated: false,
            reason_digest: None,
        });
    }
    if !plan.publication_policy.satisfied {
        return Err(BrokerOpError::ShipPublicationPolicy {
            reason: "the selected promoted prefix is not covered by review evidence".into(),
            remediation: plan
                .publication_policy
                .remediation
                .clone()
                .unwrap_or_else(|| "rebuild broker ship plan after review".into()),
        });
    }

    let mut validated = std::collections::BTreeSet::new();
    for evidence in &plan.publication_policy.evidence {
        let lifecycle_id = evidence
            .lifecycle_id
            .expect("satisfied evidence has lifecycle");
        if !validated.insert(lifecycle_id) {
            continue;
        }
        let session_id = evidence.session_id;
        let lifecycle = broker
            .store()
            .review_lifecycle_for_session(session_id)?
            .ok_or_else(|| BrokerOpError::ShipPublicationPolicy {
                reason: format!("review lifecycle {lifecycle_id} disappeared"),
                remediation: "rebuild broker ship plan".into(),
            })?;
        let unchanged = lifecycle.id == lifecycle_id
            && Some(lifecycle.generation) == evidence.lifecycle_generation
            && lifecycle.state == crate::ReviewLifecycleState::ValidationUnlocked
            && lifecycle.queue_entry_id == evidence.reviewed_queue_entry_id
            && Some(lifecycle.commit_sha.as_str()) == evidence.reviewed_commit_sha.as_deref()
            && lifecycle.evidence_digest == evidence.evidence_digest;
        if !unchanged {
            return Err(BrokerOpError::ShipPublicationPolicy {
                reason: format!("review lifecycle {lifecycle_id} changed since planning"),
                remediation: "rebuild broker ship plan and review its exact evidence".into(),
            });
        }
        let repository = lifecycle
            .repository
            .strip_prefix("github.com/")
            .unwrap_or(&lifecycle.repository);
        let review_policy = crate::ReviewPolicy::load(broker.main_root())?;
        let snapshot = crate::load_review_provider_snapshot(
            broker.main_root(),
            repository,
            lifecycle.pr_number,
            &review_policy,
        )?;
        let live_valid = snapshot.repository == lifecycle.repository
            && snapshot.pr_number == lifecycle.pr_number
            && snapshot.target_branch == lifecycle.target_branch
            && snapshot.head_sha == lifecycle.commit_sha
            && snapshot.state == "OPEN"
            && !snapshot.is_draft
            && snapshot.satisfaction_evidence.is_satisfied();
        if !live_valid {
            return Err(BrokerOpError::ShipPublicationPolicy {
                reason: format!(
                    "live review evidence for PR #{} no longer matches its satisfied exact commit and base",
                    lifecycle.pr_number
                ),
                remediation: format!(
                    "inspect PR #{} and rebuild broker ship plan after evidence is current",
                    lifecycle.pr_number
                ),
            });
        }
    }
    Ok(ShipPublicationAuthorization {
        kind: ShipPublicationAuthorizationKind::Reviewed,
        policy_source_commit: plan.publication_policy.source_commit.clone(),
        live_evidence_revalidated: true,
        reason_digest: None,
    })
}

fn publication_authorization_label(authorization: &ShipPublicationAuthorization) -> String {
    match authorization.kind {
        ShipPublicationAuthorizationKind::Direct => "direct publication policy".into(),
        ShipPublicationAuthorizationKind::Reviewed => "live review evidence revalidated".into(),
        ShipPublicationAuthorizationKind::BreakGlass => format!(
            "explicit break-glass authorization reason SHA-256 {}",
            authorization.reason_digest.as_deref().unwrap_or("missing")
        ),
    }
}

fn promotion_sha(entry: &MergeQueueEntry) -> Option<String> {
    let details = serde_json::from_str::<serde_json::Value>(entry.details_json.as_deref()?).ok()?;
    details.get("commit")?.as_str().map(str::to_string)
}

fn validate_local_main_sync(broker: &Broker, plan: &ShipPlan, confirm: &str) -> Result<(), String> {
    let default_branch = plan
        .local_default_branch_ref
        .strip_prefix("refs/heads/")
        .unwrap_or(&plan.local_default_branch_ref);
    let assessment = assess_local_main_sync(
        broker,
        default_branch,
        &plan.local_default_branch_ref,
        &plan.local_default_branch_sha,
        confirm,
    )?;
    if !assessment.safe {
        return Err(local_main_sync_refusal(&assessment, default_branch));
    }
    Ok(())
}

fn assess_local_main_sync(
    broker: &Broker,
    default_branch: &str,
    default_branch_ref: &str,
    expected_head: &str,
    confirm: &str,
) -> Result<ShipLocalMainSyncAssessment, String> {
    let current_branch = broker
        .repo_handle()
        .current_branch()
        .map_err(|error| error.to_string())?;
    let current_head = broker
        .repo_handle()
        .head_commit()
        .map_err(|error| error.to_string())?;
    let current_ref = broker
        .repo_handle()
        .resolve_ref(default_branch_ref)
        .ok_or_else(|| format!("{default_branch_ref} no longer resolves"))?;
    let mut tracked_dirty_paths = broker
        .repo_handle()
        .tracked_dirty_paths()
        .map_err(|error| error.to_string())?;
    let mut untracked_paths = broker
        .repo_handle()
        .untracked_paths()
        .map_err(|error| error.to_string())?;
    tracked_dirty_paths.sort();
    tracked_dirty_paths.dedup();
    untracked_paths.sort();
    untracked_paths.dedup();
    let fast_forward = broker.repo_handle().is_ancestor(&current_head, confirm);
    let incoming_paths = if fast_forward {
        broker
            .repo_handle()
            .changed_between(&current_head, confirm)
            .map_err(|error| error.to_string())?
    } else {
        Vec::new()
    };
    let mut conflicting_untracked_paths = untracked_paths
        .iter()
        .filter(|untracked| {
            incoming_paths
                .iter()
                .any(|incoming| checkout_paths_collide(untracked, incoming))
        })
        .cloned()
        .collect::<Vec<_>>();
    conflicting_untracked_paths.sort();
    conflicting_untracked_paths.dedup();
    let current_branch_matches = current_branch == default_branch;
    let local_head_unchanged = current_head == expected_head && current_ref == expected_head;
    let safe = current_branch_matches
        && local_head_unchanged
        && fast_forward
        && tracked_dirty_paths.is_empty()
        && conflicting_untracked_paths.is_empty();
    Ok(ShipLocalMainSyncAssessment {
        safe,
        current_branch_matches,
        local_head_unchanged,
        fast_forward,
        tracked_dirty_paths,
        untracked_paths,
        conflicting_untracked_paths,
    })
}

fn checkout_paths_collide(a: &str, b: &str) -> bool {
    a == b
        || a.strip_prefix(b)
            .is_some_and(|suffix| suffix.starts_with('/'))
        || b.strip_prefix(a)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn local_main_sync_refusal(
    assessment: &ShipLocalMainSyncAssessment,
    default_branch: &str,
) -> String {
    if !assessment.current_branch_matches {
        return format!("primary checkout is not on expected default branch {default_branch}");
    }
    if !assessment.local_head_unchanged {
        return "local main moved since planning".into();
    }
    if !assessment.fast_forward {
        return "local main has diverged from the confirmed publication".into();
    }
    if !assessment.tracked_dirty_paths.is_empty() {
        return format!(
            "primary default-branch checkout has tracked changes: {}",
            assessment.tracked_dirty_paths.join(", ")
        );
    }
    if !assessment.conflicting_untracked_paths.is_empty() {
        return format!(
            "untracked paths would collide with the incoming fast-forward: {}",
            assessment.conflicting_untracked_paths.join(", ")
        );
    }
    "local-main synchronization is unsafe".into()
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

#[cfg(test)]
mod tests {
    use super::checkout_paths_collide;

    #[test]
    fn checkout_collision_covers_exact_and_file_directory_replacements() {
        assert!(checkout_paths_collide("feature.txt", "feature.txt"));
        assert!(checkout_paths_collide("feature/nested.txt", "feature"));
        assert!(checkout_paths_collide("feature", "feature/nested.txt"));
        assert!(!checkout_paths_collide(".codex/local.md", "src/main.rs"));
        assert!(!checkout_paths_collide("feature-old", "feature"));
    }
}
