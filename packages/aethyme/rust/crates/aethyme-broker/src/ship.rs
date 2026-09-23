//! Explicit publication of a verified integration tip.
//!
//! Planning is observational: it resolves the selected promoted entry,
//! integration tip, local default branch, and the remote's advertised HEAD
//! without fetching, updating refs, or publishing anything.

use crate::broker::BrokerOpError::ShipDeliveryPullRequestMismatch;
use crate::broker::{Broker, BrokerOpError};
use crate::git::GitRepo;
use crate::merge::PromoteConfig;
use crate::operations::{CoordinatedCommand, CoordinatedOperationReport, UnknownOutcomeRecovery};
use crate::remote_target::ResolvedRemoteTarget;
use crate::types::{
    CoordinatedOperation, EntryExposureResolutionKind, EntryPathExposure, MergeQueueEntry,
    MergeStatus, OperationEffect, OperationProvider, Session,
};
use std::path::Path;
use std::time::Duration;

pub const PUBLICATION_POLICY_SCHEMA_VERSION: u32 = 1;
pub const REPOSITORY_DELIVERY_POLICY_SCHEMA_VERSION: u32 = 1;

/// Each remote delivery operation gets one shared admission-and-child budget.
/// The coordinator kills a child that outlives this budget and records remote
/// writes as `outcome_unknown`, so a wedged network cannot hold the repository
/// lane indefinitely or invite a blind retry.
const DELIVERY_OPERATION_BUDGET: Duration = Duration::from_secs(30);

fn delivery_operation_wait() -> crate::QueueWait {
    crate::QueueWait::Seconds(DELIVERY_OPERATION_BUDGET.as_secs())
}

/// The repository-level route used for delivering a promoted prefix.
///
/// This is intentionally distinct from [`ShipPublicationMode`]. Publication
/// policy answers whether a direct push is authorized; delivery mode answers
/// which safe transport (local main or a pull request) should be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryDeliveryMode {
    LocalMainMerge,
    PullRequest,
}

impl RepositoryDeliveryMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalMainMerge => "local_main_merge",
            Self::PullRequest => "pull_request",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "local_main_merge" => Ok(Self::LocalMainMerge),
            "pull_request" => Ok(Self::PullRequest),
            _ => Err(format!(
                "delivery mode {value:?} is invalid; expected local_main_merge or pull_request"
            )),
        }
    }
}

/// Where the effective delivery mode came from. A recommendation is kept
/// separate from an explicit repository policy so an operator can see why a
/// route was selected without losing the policy's authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryDeliveryModeSource {
    TrustedConfig,
    CliOverride,
    LegacyDefault,
    DivergenceRecommendation,
}

impl RepositoryDeliveryModeSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TrustedConfig => "trusted_config",
            Self::CliOverride => "cli_override",
            Self::LegacyDefault => "legacy_default",
            Self::DivergenceRecommendation => "divergence_recommendation",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct RepositoryDeliveryConfigTable {
    default: String,
}

/// Typed `[delivery]` configuration. The parser is deliberately strict even
/// though certification remains warning-oriented for forward-compatible
/// unknown configuration: selecting an invalid delivery route must fail
/// closed rather than silently choose direct publication.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RepositoryDeliveryConfig {
    pub schema_version: u32,
    pub default: RepositoryDeliveryMode,
}

impl RepositoryDeliveryConfig {
    pub fn from_config_text(text: &str) -> Result<Option<Self>, String> {
        let value = text
            .parse::<toml::Value>()
            .map_err(|error| format!(".aethyme/config.toml is invalid: {error}"))?;
        let Some(delivery) = value.get("delivery") else {
            return Ok(None);
        };
        let table: RepositoryDeliveryConfigTable = delivery
            .clone()
            .try_into()
            .map_err(|error| format!("[delivery] policy is invalid: {error}"))?;
        Ok(Some(Self {
            schema_version: REPOSITORY_DELIVERY_POLICY_SCHEMA_VERSION,
            default: RepositoryDeliveryMode::parse(&table.default)?,
        }))
    }

    pub fn load(main_root: &Path) -> Result<Option<Self>, String> {
        let path = main_root.join(".aethyme/config.toml");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
        };
        Self::from_config_text(&text)
    }
}

/// All policy and ref evidence used to choose a delivery route. This is part
/// of the read-only ship plan and is carried into execution through its plan
/// digest.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RepositoryDeliverySelection {
    pub mode: RepositoryDeliveryMode,
    pub source: RepositoryDeliveryModeSource,
    pub configured_mode: Option<RepositoryDeliveryMode>,
    pub recommended_mode: Option<RepositoryDeliveryMode>,
    pub divergence_reasons: Vec<String>,
    pub trusted_config_ref: String,
    pub trusted_config_commit: String,
    pub trusted_config_digest: Option<String>,
    pub reason: String,
    /// A recommendation is not an authorization to mutate. The operator must
    /// select a route explicitly before a recommended PR can be opened.
    pub requires_explicit_selection: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ShipPublicationMode {
    #[default]
    Direct,
    ReviewGated,
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
    /// Whether this push is what puts the entry on the remote default branch.
    /// Most of an included prefix is history that is already published; the
    /// suffix is what a publication review is actually about (issue #141).
    pub newly_published: bool,
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
    pub delivery: RepositoryDeliverySelection,
    /// SHA-256 digest of the exact delivery inputs. It binds the selected
    /// mode, trusted policy, refs, and proposed SHA between plan and execute.
    pub plan_digest: String,
    pub local_main_sync_safe: bool,
    pub local_main_sync_assessment: ShipLocalMainSyncAssessment,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ShipLocalMainSyncAssessment {
    pub safe: bool,
    pub current_branch_matches: bool,
    pub local_head_unchanged: bool,
    pub fast_forward: bool,
    /// Commits reachable from local main but not from the integration tip.
    /// They are reported explicitly so a local-main delivery can be reviewed
    /// or handed to the existing main-reconciliation workflow before any ref
    /// movement is attempted.
    pub local_commits_not_in_integration: Vec<String>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryExecutionState {
    Published,
    /// The provider confirmed the PR merge, but the target branch has not
    /// yet been independently verified to contain the reviewed delivery tip.
    PullRequestMerged,
    PullRequestOpen,
    PullRequestChecksPending,
    PullRequestChecksFailed,
    PullRequestChecksPassed,
    PullRequestChecksUnknown,
}

impl DeliveryExecutionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Published => "published",
            Self::PullRequestMerged => "pull_request_merged",
            Self::PullRequestOpen => "pull_request_open",
            Self::PullRequestChecksPending => "pull_request_checks_pending",
            Self::PullRequestChecksFailed => "pull_request_checks_failed",
            Self::PullRequestChecksPassed => "pull_request_checks_passed",
            Self::PullRequestChecksUnknown => "pull_request_checks_unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DeliveryCheck {
    pub name: String,
    pub status: Option<String>,
    pub conclusion: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DeliveryChecksSummary {
    pub total: usize,
    pub pending: usize,
    pub failed: usize,
    pub passed: usize,
    pub unknown: usize,
    pub checks: Vec<DeliveryCheck>,
}

impl DeliveryChecksSummary {
    fn state(&self) -> DeliveryExecutionState {
        if self.failed > 0 {
            DeliveryExecutionState::PullRequestChecksFailed
        } else if self.unknown > 0 {
            DeliveryExecutionState::PullRequestChecksUnknown
        } else if self.pending > 0 {
            DeliveryExecutionState::PullRequestChecksPending
        } else if self.total > 0 && self.passed == self.total {
            DeliveryExecutionState::PullRequestChecksPassed
        } else {
            DeliveryExecutionState::PullRequestOpen
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DeliveryPullRequest {
    pub number: i64,
    pub url: String,
    pub state: String,
    pub is_draft: bool,
    pub merged_at: Option<String>,
    pub head_repository: String,
    pub head_branch: String,
    pub head_sha: String,
    pub base_branch: String,
    pub base_sha: String,
    pub checks: DeliveryChecksSummary,
}

#[derive(Debug, serde::Serialize)]
pub struct PullRequestDeliveryReport {
    pub plan: ShipPlan,
    pub delivery_state: DeliveryExecutionState,
    pub branch: String,
    pub proposed_sha: String,
    pub remote_base_sha: String,
    pub branch_operation: Option<CoordinatedOperation>,
    pub fetch_operation: CoordinatedOperation,
    pub push_operation: CoordinatedOperation,
    pub create_operation: Option<CoordinatedOperation>,
    pub verify_operation: CoordinatedOperation,
    /// Present when a merged PR caused an additional target-branch fetch.
    /// Opening or inspecting an open PR does not verify publication.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_verification_operation: Option<CoordinatedOperation>,
    /// The target branch tip observed by the merged-PR verification, when a
    /// merged PR has been observed. It is intentionally separate from the PR
    /// base SHA: the latter is provider metadata, while this is the remote
    /// ref evidence used to decide whether publication is complete.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_remote_sha: Option<String>,
    pub pull_request: DeliveryPullRequest,
    /// Deliberately empty: opening a PR is not publication and must not resolve
    /// entry exposures or publication advisories.
    pub resolved_exposures: Vec<EntryPathExposure>,
    pub resolved_advisories: Vec<crate::Advisory>,
}

#[derive(Debug, serde::Serialize)]
pub enum DeliveryExecutionReport {
    LocalMainMerge {
        ship: ShipExecutionReport,
        preservation_operation: Option<CoordinatedOperation>,
        merge_operation: Option<CoordinatedOperation>,
        plan_digest: String,
    },
    PullRequest(PullRequestDeliveryReport),
}

fn resolve_verified_publication(
    broker: &mut Broker,
    plan: &ShipPlan,
    verified_remote_sha: &str,
) -> Result<(Vec<EntryPathExposure>, Vec<crate::Advisory>), BrokerOpError> {
    let contained_entry_ids = broker
        .store()
        .outstanding_entry_path_exposures()?
        .into_iter()
        .filter(|exposure| {
            broker
                .repo_handle()
                .is_ancestor(&exposure.promotion_sha, verified_remote_sha)
        })
        .map(|exposure| exposure.queue_entry_id)
        .collect::<Vec<_>>();
    let resolution_evidence = format!(
        "broker ship verified remote {} at {}",
        plan.remote_default_branch_ref, verified_remote_sha
    );
    let resolved_exposures = broker.store().resolve_entry_path_exposures(
        &contained_entry_ids,
        EntryExposureResolutionKind::ShipVerified,
        verified_remote_sha,
        &resolution_evidence,
    )?;
    let resolved_advisories = broker
        .store()
        .resolve_entry_advisories_without_active_leases(
            &contained_entry_ids,
            &resolution_evidence,
        )?;
    let _ = broker.refresh_advisory_projection();
    Ok((resolved_exposures, resolved_advisories))
}

impl Broker {
    /// Build a serializable, mutation-free publication plan through one exact
    /// promoted queue entry. Later integration promotions are listed but never
    /// silently added to the proposed push.
    pub fn ship_plan(&mut self, entry_id: i64) -> Result<ShipPlan, BrokerOpError> {
        self.ship_plan_with_delivery(entry_id, None)
    }

    /// As [`Self::ship_plan`], optionally selecting the repository delivery
    /// route explicitly. The route is selected only after the remote default
    /// branch has been observed, so a plan can bind policy to the exact target
    /// commit it describes.
    pub fn ship_plan_with_delivery(
        &mut self,
        entry_id: i64,
        delivery_override: Option<RepositoryDeliveryMode>,
    ) -> Result<ShipPlan, BrokerOpError> {
        self.ship_plan_with_delivery_mode(entry_id, delivery_override, true)
    }

    /// Build the original direct-publication plan for the compatibility API.
    /// The legacy execute methods intentionally do not opt into repository
    /// delivery routing; keeping their planning path independent also
    /// preserves their established remote-moved error after a remote commit
    /// that is not present in the local object database.
    fn ship_plan_for_legacy_execution(&mut self, entry_id: i64) -> Result<ShipPlan, BrokerOpError> {
        self.ship_plan_with_delivery_mode(entry_id, None, false)
    }

    fn ship_plan_with_delivery_mode(
        &mut self,
        entry_id: i64,
        delivery_override: Option<RepositoryDeliveryMode>,
        select_delivery_policy: bool,
    ) -> Result<ShipPlan, BrokerOpError> {
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
                // Resolved below, once the remote default branch is known.
                newly_published: true,
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
            &remote_default.sha,
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
            default_branch,
            &local_default_branch_ref,
            &local_default_branch_sha,
            &publication_sha,
        )
        .map_err(|reason| BrokerOpError::ShipPlanUnavailable {
            what: "local-main synchronization",
            reason,
        })?;
        let mut local_main_sync_assessment = local_main_sync_assessment;
        local_main_sync_assessment.local_commits_not_in_integration = self
            .repo_handle()
            .commits_excluding_oldest(&local_default_branch_sha, &integration_sha)
            .map_err(|error| BrokerOpError::ShipPlanUnavailable {
                what: "local-main reconciliation",
                reason: format!("cannot list local commits absent from integration: {error}"),
            })?;
        let local_main_sync_safe = local_main_sync_assessment.safe
            && local_main_sync_assessment
                .local_commits_not_in_integration
                .is_empty();

        // An entry already contained in the remote default branch is history, not
        // part of what this push publishes. When the remote tip is unknown the
        // flag stays set, so an uncertain plan shows more rather than less.
        if !remote_default.sha.is_empty() {
            for item in &mut included_entries {
                item.newly_published = !self
                    .repo_handle()
                    .is_ancestor(&item.promotion_sha, &remote_default.sha);
            }
        }

        let delivery = if select_delivery_policy {
            repository_delivery_selection(
                self,
                &target,
                &remote_default,
                &local_default_branch_sha,
                &publication_sha,
                &integration_sha,
                delivery_override,
            )?
        } else {
            legacy_delivery_selection(&remote_default)
        };
        let mut proposed_push = proposed_push;
        if delivery.mode == RepositoryDeliveryMode::PullRequest {
            let branch = delivery_branch_name_for(entry.id, &publication_sha);
            let destination_ref = format!("refs/heads/{branch}");
            let refspec = format!("{publication_sha}:{destination_ref}");
            proposed_push = ShipPush {
                remote: remote.clone(),
                source_sha: publication_sha.clone(),
                destination_ref,
                refspec: refspec.clone(),
                command: vec!["git".into(), "push".into(), remote.clone(), refspec],
            };
        }
        let mut plan = ShipPlan {
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
            delivery,
            plan_digest: String::new(),
            local_main_sync_safe,
            local_main_sync_assessment,
        };
        plan.plan_digest = delivery_plan_digest(&plan)?;
        Ok(plan)
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
        let plan = match self.ship_plan(entry_id) {
            Ok(plan) => {
                if plan.delivery.source == RepositoryDeliveryModeSource::TrustedConfig {
                    return Err(BrokerOpError::ShipDeliveryUnavailable {
                        reason: "the repository has an explicit delivery policy; use ship_execute_delivery so the policy and plan digest are enforced".into(),
                    });
                }
                plan
            }
            Err(BrokerOpError::ShipPlanUnavailable { what, reason })
                if what == "trusted delivery policy"
                    && reason.contains("not available locally") =>
            {
                // Preserve the legacy remote-moved behavior when the remote
                // advanced to an object this checkout has not fetched yet.
                // The mode-aware CLI still fails closed because it cannot
                // inspect the trusted policy without that object.
                self.ship_plan_for_legacy_execution(entry_id)?
            }
            Err(error) => return Err(error),
        };
        if confirm != plan.publication_sha {
            return Err(BrokerOpError::ShipConfirmationMismatch {
                expected: plan.publication_sha,
                actual: confirm.into(),
            });
        }
        self.ship_execute_from_plan(
            plan,
            confirm,
            sync_main,
            break_glass,
            break_glass_reason,
            crate::QueueWait::Forever,
        )
    }

    fn ship_execute_from_plan(
        &mut self,
        plan: ShipPlan,
        confirm: &str,
        sync_main: bool,
        break_glass: bool,
        break_glass_reason: Option<&str>,
        operation_wait: crate::QueueWait,
    ) -> Result<ShipExecutionReport, BrokerOpError> {
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

        let fetch = self.run_coordinated_operation_at_with_wait(
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
            operation_wait,
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
        if fetched_remote != planned_base && fetched_remote != confirm {
            // The only acceptable deviation from the planned base is the
            // exact reviewed publication itself. That is the idempotent
            // retry case; any other remote movement still requires a re-plan.
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

        let push = self.run_coordinated_operation_at_with_wait(
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
            operation_wait,
        )?;
        if !push.ok() {
            return Err(ship_operation_failure("push", &push));
        }

        let verify = self.run_coordinated_operation_at_with_wait(
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
            operation_wait,
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
        let (resolved_exposures, resolved_advisories) =
            resolve_verified_publication(self, &plan, &verified_remote_sha)?;

        let before_sha = plan.local_default_branch_sha.clone();
        let (sync_operation, local_main_sync) = if sync_main {
            validate_local_main_sync(self, &plan, confirm).map_err(|reason| {
                BrokerOpError::ShipLocalMainMovedAfterPublish {
                    published_sha: confirm.into(),
                    reason,
                }
            })?;
            let sync = self.run_coordinated_operation_at_with_wait(
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
                operation_wait,
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

impl Broker {
    /// Execute the delivery route selected by [`Self::ship_plan`]. The legacy
    /// no-policy/no-divergence route delegates to the original direct ship so
    /// existing callers retain their publication semantics. Configured routes
    /// require the plan digest, and a divergence recommendation requires an
    /// explicit `pull_request` selection before any mutation.
    #[allow(clippy::too_many_arguments)]
    pub fn ship_execute_delivery(
        &mut self,
        entry_id: i64,
        confirm: &str,
        delivery_override: Option<RepositoryDeliveryMode>,
        plan_digest: Option<&str>,
        sync_main: bool,
        break_glass: bool,
        break_glass_reason: Option<&str>,
    ) -> Result<DeliveryExecutionReport, BrokerOpError> {
        let plan = self.ship_plan_with_delivery(entry_id, delivery_override)?;
        if plan.delivery.requires_explicit_selection && delivery_override.is_none() {
            return Err(BrokerOpError::ShipDeliveryRequiresExplicitSelection {
                recommendation: plan
                    .delivery
                    .recommended_mode
                    .map(RepositoryDeliveryMode::as_str)
                    .unwrap_or("pull_request"),
                reasons: plan.delivery.divergence_reasons.join(", "),
            });
        }
        validate_delivery_execution_confirmation(&plan, confirm, delivery_override, plan_digest)?;
        validate_delivery_source(self, &plan)?;

        match plan.delivery.mode {
            RepositoryDeliveryMode::PullRequest => {
                if sync_main {
                    return Err(BrokerOpError::ShipDeliveryUnavailable {
                        reason: "--sync-main is not part of pull-request delivery; merge the PR and synchronize main in a separate reviewed action".into(),
                    });
                }
                if break_glass || break_glass_reason.is_some() {
                    return Err(BrokerOpError::ShipDeliveryUnavailable {
                        reason: "--break-glass applies only to direct publication, not to opening a pull request".into(),
                    });
                }
                Ok(DeliveryExecutionReport::PullRequest(
                    execute_pull_request_delivery(self, plan)?,
                ))
            }
            RepositoryDeliveryMode::LocalMainMerge => {
                if sync_main {
                    return Err(BrokerOpError::ShipDeliveryUnavailable {
                        reason: "--sync-main is redundant for local_main_merge; that delivery mode already merges the reviewed source into local main before publication".into(),
                    });
                }
                if plan.delivery.source == RepositoryDeliveryModeSource::LegacyDefault {
                    return Ok(DeliveryExecutionReport::LocalMainMerge {
                        ship: self.ship_execute_with_policy(
                            entry_id,
                            confirm,
                            sync_main,
                            break_glass,
                            break_glass_reason,
                        )?,
                        preservation_operation: None,
                        merge_operation: None,
                        plan_digest: plan.plan_digest,
                    });
                }

                // A configured or explicitly selected local-main route must
                // honor the complete read-only assessment from the plan. In
                // particular, a local tip with commits absent from
                // integration is not an authorized fast-forward source even
                // if a later re-check happens to look clean.
                if !plan.local_main_sync_safe {
                    let default_branch = plan
                        .local_default_branch_ref
                        .strip_prefix("refs/heads/")
                        .unwrap_or(&plan.local_default_branch_ref);
                    return Err(BrokerOpError::ShipLocalMainUnsafe {
                        reason: local_main_sync_refusal(
                            &plan.local_main_sync_assessment,
                            default_branch,
                            &plan,
                        ),
                    });
                }

                let preservation_operation =
                    preserve_local_main_before_delivery(self, &plan, confirm)?;
                let merge_operation = merge_local_main_before_delivery(self, &plan, confirm)?;
                // The local main ref is now exactly at the reviewed source tip.
                // The existing direct path still performs the remote fetch,
                // non-force push, verification, and exposure resolution.
                let mut ship = self.ship_execute_from_plan(
                    plan.clone(),
                    confirm,
                    false,
                    break_glass,
                    break_glass_reason,
                    delivery_operation_wait(),
                )?;
                // The merge above is the delivery mode's local-main
                // synchronization. Keep the legacy nested report truthful;
                // its optional sync operation remains None because the outer
                // delivery report carries the distinct merge operation.
                ship.local_main_sync = ShipLocalMainSync {
                    requested: true,
                    synchronized: true,
                    before_sha: plan.local_default_branch_sha.clone(),
                    after_sha: confirm.into(),
                    follow_up_command: None,
                };
                Ok(DeliveryExecutionReport::LocalMainMerge {
                    ship,
                    preservation_operation,
                    merge_operation,
                    plan_digest: plan.plan_digest,
                })
            }
        }
    }
}

fn repository_delivery_selection(
    broker: &Broker,
    _target: &ResolvedRemoteTarget,
    remote_default: &crate::git::RemoteDefaultBranch,
    local_default_sha: &str,
    publication_sha: &str,
    integration_sha: &str,
    delivery_override: Option<RepositoryDeliveryMode>,
) -> Result<RepositoryDeliverySelection, BrokerOpError> {
    let trusted_config_ref = remote_default.ref_name.clone();
    let trusted_config_commit = remote_default.sha.clone();
    if broker
        .repo_handle()
        .resolve_ref(&trusted_config_commit)
        .is_none()
    {
        return Err(BrokerOpError::ShipPlanUnavailable {
            what: "trusted delivery policy",
            reason: format!(
                "the remote default SHA {trusted_config_commit} is not available locally; fetch it before planning delivery"
            ),
        });
    }
    let (configured, trusted_config_digest) = match broker
        .repo_handle()
        .file_at_commit(&trusted_config_commit, ".aethyme/config.toml")?
    {
        Some(text) => {
            let digest = crate::sha256_bytes(text.as_bytes());
            let config = RepositoryDeliveryConfig::from_config_text(&text).map_err(|reason| {
                BrokerOpError::ShipPlanUnavailable {
                    what: "trusted delivery policy",
                    reason,
                }
            })?;
            (config, Some(digest))
        }
        None => (None, None),
    };

    let divergence_reasons = delivery_divergence_reasons(
        broker,
        &remote_default.sha,
        local_default_sha,
        publication_sha,
        integration_sha,
    )?;
    let recommended_mode =
        (!divergence_reasons.is_empty()).then_some(RepositoryDeliveryMode::PullRequest);

    if configured
        .as_ref()
        .is_some_and(|config| config.default == RepositoryDeliveryMode::PullRequest)
        && delivery_override == Some(RepositoryDeliveryMode::LocalMainMerge)
    {
        return Err(BrokerOpError::ShipDeliveryOverrideUnsafe {
            configured: RepositoryDeliveryMode::PullRequest.as_str(),
            requested: RepositoryDeliveryMode::LocalMainMerge.as_str(),
        });
    }

    let (mode, source, reason, requires_explicit_selection) = match delivery_override {
        Some(mode) => (
            mode,
            RepositoryDeliveryModeSource::CliOverride,
            format!("explicit --delivery {} override", mode.as_str()),
            false,
        ),
        None => match configured.as_ref().map(|config| config.default) {
            Some(mode) => (
                mode,
                RepositoryDeliveryModeSource::TrustedConfig,
                format!(
                    "selected by trusted [delivery] policy at {trusted_config_ref} @ {trusted_config_commit}"
                ),
                false,
            ),
            None if recommended_mode.is_some() => (
                RepositoryDeliveryMode::PullRequest,
                RepositoryDeliveryModeSource::DivergenceRecommendation,
                "pull_request is recommended because repository refs or the working tree diverge"
                    .into(),
                true,
            ),
            None => (
                RepositoryDeliveryMode::LocalMainMerge,
                RepositoryDeliveryModeSource::LegacyDefault,
                "no [delivery] policy is present; preserving legacy direct-ship behavior".into(),
                false,
            ),
        },
    };

    Ok(RepositoryDeliverySelection {
        mode,
        source,
        configured_mode: configured.map(|config| config.default),
        recommended_mode,
        divergence_reasons,
        trusted_config_ref,
        trusted_config_commit,
        trusted_config_digest,
        reason,
        requires_explicit_selection,
    })
}

fn legacy_delivery_selection(
    remote_default: &crate::git::RemoteDefaultBranch,
) -> RepositoryDeliverySelection {
    RepositoryDeliverySelection {
        mode: RepositoryDeliveryMode::LocalMainMerge,
        source: RepositoryDeliveryModeSource::LegacyDefault,
        configured_mode: None,
        recommended_mode: None,
        divergence_reasons: Vec::new(),
        trusted_config_ref: remote_default.ref_name.clone(),
        trusted_config_commit: remote_default.sha.clone(),
        trusted_config_digest: None,
        reason: "legacy direct-publication API; repository delivery routing is not selected".into(),
        requires_explicit_selection: false,
    }
}

fn delivery_divergence_reasons(
    broker: &Broker,
    remote_default_sha: &str,
    local_default_sha: &str,
    publication_sha: &str,
    integration_sha: &str,
) -> Result<Vec<String>, BrokerOpError> {
    let repo = broker.repo_handle();
    let mut reasons = Vec::new();
    let local_commits_not_in_integration = repo
        .commits_excluding_oldest(local_default_sha, integration_sha)
        .map_err(|error| BrokerOpError::ShipPlanUnavailable {
            what: "local-main reconciliation",
            reason: format!("cannot list local commits absent from integration: {error}"),
        })?;
    if !local_commits_not_in_integration.is_empty() {
        reasons.push("local_commits_not_in_integration".into());
    }
    if local_default_sha != remote_default_sha {
        let relation = if repo.is_ancestor(local_default_sha, remote_default_sha) {
            "local_main_behind_remote"
        } else if repo.is_ancestor(remote_default_sha, local_default_sha) {
            "local_main_ahead_of_remote"
        } else {
            "local_main_and_remote_diverged"
        };
        reasons.push(relation.into());
    }

    if remote_default_sha != publication_sha {
        if repo.is_ancestor(remote_default_sha, publication_sha) {
            // This is the normal promoted-but-not-published shape, not a
            // divergence: the proposed prefix cleanly extends the target.
        } else if repo.is_ancestor(publication_sha, remote_default_sha) {
            reasons.push("remote_default_ahead_of_publication".into());
        } else {
            reasons.push("remote_default_and_publication_diverged".into());
        }
    }

    let tracked_dirty = repo.tracked_dirty_paths()?;
    let untracked = delivery_operator_untracked_paths(repo)?;
    if !tracked_dirty.is_empty() || !untracked.is_empty() {
        let mut paths = tracked_dirty;
        paths.extend(untracked);
        paths.sort();
        paths.dedup();
        reasons.push(format!("working_tree_not_clean: {}", paths.join(", ")));
    }
    reasons.sort();
    reasons.dedup();
    Ok(reasons)
}

fn delivery_operator_untracked_paths(repo: &GitRepo) -> Result<Vec<String>, BrokerOpError> {
    Ok(repo
        .untracked_paths()?
        .into_iter()
        .filter(|path| !is_broker_runtime_path(path))
        .collect())
}

fn is_broker_runtime_path(path: &str) -> bool {
    path == crate::BROKER_DB_RELPATH
        || path.starts_with(".aethyme/broker.db.")
        || path == crate::BROKER_ADVISORY_RELPATH
        || path.starts_with(".aethyme/logs/")
        || path.starts_with(".aethyme/reports/")
        || path.starts_with(".aethyme/run/")
        || path.starts_with(".aethyme/worktrees/")
        || path == ".aethyme/worktree-sizes.json"
        || path == ".aethyme/gc-journal.json"
        || path == ".aethyme/gc.lock"
        || path == ".aethyme/graph_store.redb"
        || path == ".aethyme/graph_store.redb.indexing"
        || path.starts_with(".aethyme/graph/")
}

fn delivery_plan_digest(plan: &ShipPlan) -> Result<String, BrokerOpError> {
    #[derive(serde::Serialize)]
    struct DeliveryPlanAuthorization<'a> {
        schema_version: u32,
        entry_id: i64,
        integration_ref: &'a str,
        integration_sha: &'a str,
        publication_sha: &'a str,
        local_default_branch_ref: &'a str,
        local_default_branch_sha: &'a str,
        remote_default_branch_ref: &'a str,
        remote_default_branch_sha: &'a str,
        planned_remote_base_sha: &'a Option<String>,
        local_commits_not_in_integration: &'a [String],
        remote_name: &'a str,
        proposed_destination_ref: &'a str,
        proposed_refspec: &'a str,
        preservation_ref: String,
        target: &'a str,
        delivery: &'a RepositoryDeliverySelection,
    }
    let bytes = serde_json::to_vec(&DeliveryPlanAuthorization {
        schema_version: REPOSITORY_DELIVERY_POLICY_SCHEMA_VERSION,
        entry_id: plan.queue_entry.id,
        integration_ref: &plan.integration_ref,
        integration_sha: &plan.integration_sha,
        publication_sha: &plan.publication_sha,
        local_default_branch_ref: &plan.local_default_branch_ref,
        local_default_branch_sha: &plan.local_default_branch_sha,
        remote_default_branch_ref: &plan.remote_default_branch_ref,
        remote_default_branch_sha: &plan.remote_default_branch_sha,
        planned_remote_base_sha: &plan.planned_remote_base_sha,
        local_commits_not_in_integration: &plan
            .local_main_sync_assessment
            .local_commits_not_in_integration,
        remote_name: &plan.proposed_push.remote,
        proposed_destination_ref: &plan.proposed_push.destination_ref,
        proposed_refspec: &plan.proposed_push.refspec,
        preservation_ref: delivery_preservation_ref(plan),
        target: &plan.target.coordination_key,
        delivery: &plan.delivery,
    })?;
    Ok(crate::sha256_bytes(&bytes))
}

fn validate_delivery_execution_confirmation(
    plan: &ShipPlan,
    confirm: &str,
    delivery_override: Option<RepositoryDeliveryMode>,
    plan_digest: Option<&str>,
) -> Result<(), BrokerOpError> {
    if confirm.len() != 40
        || !confirm
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return Err(BrokerOpError::ShipConfirmationNotFullSha);
    }
    if confirm != plan.publication_sha {
        return Err(BrokerOpError::ShipConfirmationMismatch {
            expected: plan.publication_sha.clone(),
            actual: confirm.into(),
        });
    }
    if delivery_override.is_some()
        || plan.delivery.source != RepositoryDeliveryModeSource::LegacyDefault
    {
        let Some(plan_digest) = plan_digest else {
            return Err(BrokerOpError::ShipDeliveryPlanDigestRequired);
        };
        if plan_digest.len() != 64
            || !plan_digest
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        {
            return Err(BrokerOpError::ShipDeliveryPlanDigestNotSha256);
        }
        if !plan_digest.eq_ignore_ascii_case(&plan.plan_digest) {
            return Err(BrokerOpError::ShipDeliveryPlanDigestMismatch {
                expected: plan.plan_digest.clone(),
                actual: plan_digest.into(),
            });
        }
    }
    Ok(())
}

fn delivery_preservation_ref(plan: &ShipPlan) -> String {
    format!(
        "refs/heads/aethyme/preserve/delivery-q{}-{}",
        plan.queue_entry.id,
        &plan.local_default_branch_sha[..12.min(plan.local_default_branch_sha.len())]
    )
}

fn validate_delivery_source(broker: &Broker, plan: &ShipPlan) -> Result<(), BrokerOpError> {
    let current_integration = broker
        .repo_handle()
        .resolve_ref(&format!("refs/heads/{}", plan.integration_ref))
        .ok_or_else(|| BrokerOpError::ShipPlanUnavailable {
            what: "integration ref",
            reason: format!(
                "{} disappeared during delivery execution",
                plan.integration_ref
            ),
        })?;
    if !broker
        .repo_handle()
        .is_ancestor(&plan.publication_sha, &current_integration)
    {
        return Err(BrokerOpError::ShipEntryNotOnIntegration {
            entry: plan.queue_entry.id,
            promotion: plan.publication_sha.clone(),
            integration: plan.integration_ref.clone(),
            head: current_integration,
        });
    }
    Ok(())
}

fn preserve_local_main_before_delivery(
    broker: &mut Broker,
    plan: &ShipPlan,
    confirm: &str,
) -> Result<Option<CoordinatedOperation>, BrokerOpError> {
    let observed_remote = broker
        .repo_handle()
        .remote_default_branch(&plan.target.remote_name)?;
    if observed_remote.ref_name != plan.remote_default_branch_ref {
        return Err(BrokerOpError::ShipDeliveryUnavailable {
            reason: format!(
                "the remote default ref changed from {} to {}; rebuild the delivery plan",
                plan.remote_default_branch_ref, observed_remote.ref_name
            ),
        });
    }
    if observed_remote.sha != plan.remote_default_branch_sha && observed_remote.sha != confirm {
        return Err(BrokerOpError::ShipRemoteMoved {
            remote_ref: plan.remote_default_branch_ref.clone(),
            expected: plan.remote_default_branch_sha.clone(),
            actual: observed_remote.sha,
        });
    }
    if plan.planned_remote_base_sha.as_deref() != Some(plan.remote_default_branch_sha.as_str()) {
        return Err(BrokerOpError::ShipDeliveryUnavailable {
            reason: "the remote default branch changed or has no fetched tracking SHA since planning; rebuild the delivery plan before moving local main".into(),
        });
    }
    if !plan.freshness.remote_is_ancestor_of_integration {
        return Err(BrokerOpError::ShipDeliveryUnavailable {
            reason: format!(
                "local_main_merge cannot publish {confirm}: the reviewed remote base {} is not an ancestor of the promoted source; choose --delivery pull_request",
                plan.remote_default_branch_sha
            ),
        });
    }

    let main_root = broker.main_root_path();
    let repo = GitRepo::discover(&main_root).map_err(BrokerOpError::Git)?;
    let default_branch = plan
        .local_default_branch_ref
        .strip_prefix("refs/heads/")
        .unwrap_or(&plan.local_default_branch_ref)
        .to_string();
    let assessment = assess_local_main_sync_repo(
        &repo,
        &default_branch,
        &plan.local_default_branch_ref,
        &plan.local_default_branch_sha,
        confirm,
    )
    .map_err(|reason| BrokerOpError::ShipLocalMainUnsafe { reason })?;
    if repo.current_branch().map_err(BrokerOpError::Git)? == default_branch
        && repo.head_commit().map_err(BrokerOpError::Git)? == confirm
        && repo
            .resolve_ref(&plan.local_default_branch_ref)
            .is_some_and(|sha| sha == confirm)
        && assessment.tracked_dirty_paths.is_empty()
    {
        // A retry may be planned after a previous attempt completed the local
        // merge but before publishing. The reviewed source is already the
        // local tip, so there is no older main state left to preserve.
        return Ok(None);
    }
    if !assessment.safe {
        return Err(BrokerOpError::ShipLocalMainUnsafe {
            reason: local_main_sync_refusal(&assessment, &default_branch, plan),
        });
    }

    let preservation_ref = delivery_preservation_ref(plan);
    if let Some(actual) = broker.repo_handle().resolve_ref(&preservation_ref) {
        if actual == plan.local_default_branch_sha {
            return Ok(None);
        }
        return Err(BrokerOpError::ShipDeliveryUnavailable {
            reason: format!(
                "preservation ref {preservation_ref} already points to {actual}, not the reviewed local main tip {}; refusing to overwrite it",
                plan.local_default_branch_sha
            ),
        });
    }

    let expected_ref = plan.local_default_branch_ref.clone();
    let expected_head = plan.local_default_branch_sha.clone();
    let expected_source = confirm.to_string();
    let preflight_plan = plan.clone();
    let preflight_root = main_root.clone();
    let preservation_for_preflight = preservation_ref.clone();
    let preservation_branch = preservation_ref
        .strip_prefix("refs/heads/")
        .unwrap_or(&preservation_ref)
        .to_string();
    let operation = broker.run_coordinated_operation_at_with_hooks(
        CoordinatedCommand {
            session_id: plan.originating_session.id,
            provider: OperationProvider::Git,
            repository: None,
            resolved_target: None,
            scope: Some(format!("delivery:preserve:{preservation_ref}")),
            declared_effect: Some(OperationEffect::Write),
            destructive_confirmed: false,
            authorization_reason: Some(format!(
                "preserve reviewed local main tip {} before delivery plan {} for queue entry {}",
                plan.local_default_branch_sha, plan.plan_digest, plan.queue_entry.id
            )),
            args: vec!["branch".into(), preservation_branch, expected_head.clone()],
        },
        &main_root,
        crate::QueueWait::Forever,
        move || {
            let repo = GitRepo::discover(&preflight_root).map_err(|error| error.to_string())?;
            if repo.resolve_ref(&preservation_for_preflight).is_some() {
                return Err(format!(
                    "preservation ref {} appeared while waiting; inspect it and rebuild the plan",
                    preservation_for_preflight
                ));
            }
            let branch = expected_ref
                .strip_prefix("refs/heads/")
                .unwrap_or(&expected_ref);
            let assessment = assess_local_main_sync_repo(
                &repo,
                branch,
                &expected_ref,
                &expected_head,
                &expected_source,
            )
            .map_err(|error| error.to_string())?;
            if assessment.safe {
                Ok(())
            } else {
                Err(local_main_sync_refusal(
                    &assessment,
                    branch,
                    &preflight_plan,
                ))
            }
        },
        |_, _| Ok(None),
    )?;
    if !operation.ok() {
        return Err(ship_operation_failure(
            "local-main preservation",
            &operation,
        ));
    }
    let actual = broker
        .repo_handle()
        .resolve_ref(&preservation_ref)
        .ok_or_else(|| BrokerOpError::ShipDeliveryUnavailable {
            reason: format!(
                "preservation ref {preservation_ref} was not visible after its successful operation"
            ),
        })?;
    if actual != plan.local_default_branch_sha {
        return Err(BrokerOpError::ShipDeliveryUnavailable {
            reason: format!(
                "preservation ref {preservation_ref} resolved to {actual}, expected {}",
                plan.local_default_branch_sha
            ),
        });
    }
    Ok(Some(operation.operation))
}

fn merge_local_main_before_delivery(
    broker: &mut Broker,
    plan: &ShipPlan,
    confirm: &str,
) -> Result<Option<CoordinatedOperation>, BrokerOpError> {
    if plan.planned_remote_base_sha.as_deref() != Some(plan.remote_default_branch_sha.as_str()) {
        return Err(BrokerOpError::ShipDeliveryUnavailable {
            reason: "the remote default branch changed or has no fetched tracking SHA since planning; rebuild the delivery plan before moving local main".into(),
        });
    }
    if !plan.freshness.remote_is_ancestor_of_integration {
        return Err(BrokerOpError::ShipDeliveryUnavailable {
            reason: format!(
                "local_main_merge cannot publish {confirm}: the reviewed remote base {} is not an ancestor of the promoted source; choose --delivery pull_request",
                plan.remote_default_branch_sha
            ),
        });
    }

    let main_root = broker.main_root_path();
    let repo = GitRepo::discover(&main_root).map_err(BrokerOpError::Git)?;
    let default_branch = plan
        .local_default_branch_ref
        .strip_prefix("refs/heads/")
        .unwrap_or(&plan.local_default_branch_ref)
        .to_string();
    let assessment = assess_local_main_sync_repo(
        &repo,
        &default_branch,
        &plan.local_default_branch_ref,
        &plan.local_default_branch_sha,
        confirm,
    )
    .map_err(|reason| BrokerOpError::ShipLocalMainUnsafe { reason })?;
    if repo.current_branch().map_err(BrokerOpError::Git)? == default_branch
        && repo.head_commit().map_err(BrokerOpError::Git)? == confirm
        && repo
            .resolve_ref(&plan.local_default_branch_ref)
            .is_some_and(|sha| sha == confirm)
        && assessment.tracked_dirty_paths.is_empty()
    {
        // A prior attempt may have completed the local merge before losing
        // its process or before the publication push. The exact reviewed tip
        // is already in place, so repeating the merge would be neither useful
        // nor evidence of a new mutation.
        return Ok(None);
    }
    if !assessment.safe {
        return Err(BrokerOpError::ShipLocalMainUnsafe {
            reason: local_main_sync_refusal(&assessment, &default_branch, plan),
        });
    }

    let expected_branch = plan.local_default_branch_ref.clone();
    let expected_head = plan.local_default_branch_sha.clone();
    let expected_source = confirm.to_string();
    let preflight_plan = plan.clone();
    let main_root_for_preflight = main_root.clone();
    let merge = broker.run_coordinated_operation_at_with_hooks(
        CoordinatedCommand {
            session_id: plan.originating_session.id,
            provider: OperationProvider::Git,
            repository: None,
            resolved_target: None,
            scope: Some(format!("delivery:local-main-merge:{expected_branch}")),
            declared_effect: Some(OperationEffect::Write),
            destructive_confirmed: false,
            authorization_reason: Some(format!(
                "reviewed local_main_merge delivery plan {} for queue entry {}",
                plan.plan_digest, plan.queue_entry.id
            )),
            args: vec!["merge".into(), "--ff-only".into(), expected_source.clone()],
        },
        &main_root,
        crate::QueueWait::Forever,
        move || {
            let repo =
                GitRepo::discover(&main_root_for_preflight).map_err(|error| error.to_string())?;
            let assessment = assess_local_main_sync_repo(
                &repo,
                &default_branch,
                &expected_branch,
                &expected_head,
                &expected_source,
            )
            .map_err(|error| error.to_string())?;
            if assessment.safe {
                Ok(())
            } else {
                Err(local_main_sync_refusal(
                    &assessment,
                    &default_branch,
                    &preflight_plan,
                ))
            }
        },
        |_, _| Ok(None),
    )?;
    if !merge.ok() {
        return Err(ship_operation_failure("local-main merge", &merge));
    }

    let after = repo
        .resolve_ref(&plan.local_default_branch_ref)
        .ok_or_else(|| BrokerOpError::ShipDeliveryUnavailable {
            reason: format!(
                "local main ref {} disappeared after the merge operation",
                plan.local_default_branch_ref
            ),
        })?;
    if after != confirm {
        return Err(BrokerOpError::ShipDeliveryUnavailable {
            reason: format!(
                "local main verification expected {confirm}, observed {after}; inspect the coordinated operation before retrying"
            ),
        });
    }
    Ok(Some(merge.operation))
}

const DELIVERY_PR_FIELDS: &str = "number,url,state,isDraft,mergedAt,headRefName,headRefOid,headRepository,headRepositoryOwner,baseRefName,baseRefOid,statusCheckRollup";

fn execute_pull_request_delivery(
    broker: &mut Broker,
    plan: ShipPlan,
) -> Result<PullRequestDeliveryReport, BrokerOpError> {
    if plan.target.normalized_host != "github.com" {
        return Err(BrokerOpError::ShipDeliveryUnavailable {
            reason: format!(
                "pull-request delivery currently supports github.com remotes; resolved {}",
                plan.target.normalized_host
            ),
        });
    }
    let repository = plan.target.display_slug.clone();
    let main_root = broker.main_root_path();
    let tracking = tracking_ref(&plan);
    let fetch = broker.run_coordinated_operation_at_with_wait(
        CoordinatedCommand {
            session_id: plan.originating_session.id,
            provider: OperationProvider::Git,
            repository: None,
            resolved_target: Some(plan.target.clone()),
            scope: Some(format!("delivery:fetch:{}", plan.remote_default_branch_ref)),
            declared_effect: None,
            destructive_confirmed: false,
            authorization_reason: Some(format!(
                "revalidate pull-request delivery base for queue entry {}",
                plan.queue_entry.id
            )),
            args: vec![
                "fetch".into(),
                "--no-tags".into(),
                "--force".into(),
                plan.target.remote_name.clone(),
                format!("{}:{tracking}", plan.remote_default_branch_ref),
            ],
        },
        &main_root,
        delivery_operation_wait(),
    )?;
    if !fetch.ok() {
        return Err(ship_operation_failure("delivery base fetch", &fetch));
    }
    let fetched_base = broker.repo_handle().resolve_ref(&tracking).ok_or_else(|| {
        BrokerOpError::ShipPlanUnavailable {
            what: "fetched delivery base",
            reason: format!("{tracking} does not resolve after fetch"),
        }
    })?;
    if fetched_base != plan.remote_default_branch_sha {
        return Err(BrokerOpError::ShipRemoteMoved {
            remote_ref: plan.remote_default_branch_ref.clone(),
            expected: plan.remote_default_branch_sha.clone(),
            actual: fetched_base,
        });
    }

    let branch = delivery_branch_name(&plan);
    let branch_ref = format!("refs/heads/{branch}");
    let branch_operation = match broker.repo_handle().resolve_ref(&branch_ref) {
        Some(actual) if actual != plan.publication_sha => {
            return Err(BrokerOpError::ShipDeliveryBranchConflict {
                branch,
                expected: plan.publication_sha,
                actual,
            });
        }
        Some(_) => None,
        None => {
            let create = broker.run_coordinated_operation_at_with_wait(
                CoordinatedCommand {
                    session_id: plan.originating_session.id,
                    provider: OperationProvider::Git,
                    repository: None,
                    resolved_target: None,
                    scope: Some(format!("delivery:branch:{branch_ref}")),
                    declared_effect: Some(OperationEffect::Write),
                    destructive_confirmed: false,
                    authorization_reason: Some(format!(
                        "create deterministic delivery branch for queue entry {} at {}",
                        plan.queue_entry.id, plan.publication_sha
                    )),
                    args: vec![
                        "branch".into(),
                        branch.clone(),
                        plan.publication_sha.clone(),
                    ],
                },
                &main_root,
                delivery_operation_wait(),
            )?;
            if !create.ok() {
                return Err(ship_operation_failure("delivery branch creation", &create));
            }
            Some(create.operation)
        }
    };

    let remote_branch_ref = format!("refs/heads/{branch}");
    let remote_branch = broker
        .repo_handle()
        .remote_ref_oids(
            &plan.target.remote_name,
            std::slice::from_ref(&remote_branch_ref),
        )?
        .remove(&remote_branch_ref)
        .flatten();
    if let Some(actual) = remote_branch.as_deref()
        && actual != plan.publication_sha
    {
        return Err(BrokerOpError::ShipDeliveryBranchConflict {
            branch,
            expected: plan.publication_sha,
            actual: actual.into(),
        });
    }

    // Pushing the exact same SHA on a retry is safe and lets Git provide the
    // idempotent "Everything up-to-date" result. No force or lease-expanding
    // refspec is ever constructed here.
    let push = broker.run_coordinated_operation_at_with_wait(
        CoordinatedCommand {
            session_id: plan.originating_session.id,
            provider: OperationProvider::Git,
            repository: None,
            resolved_target: Some(plan.target.clone()),
            scope: Some(format!("delivery:push:{remote_branch_ref}")),
            declared_effect: Some(OperationEffect::Write),
            destructive_confirmed: false,
            authorization_reason: Some(format!(
                "push exact pull-request delivery head {} for queue entry {}",
                plan.publication_sha, plan.queue_entry.id
            )),
            args: vec![
                "push".into(),
                plan.target.remote_name.clone(),
                format!("{}:{remote_branch_ref}", plan.publication_sha),
            ],
        },
        &main_root,
        delivery_operation_wait(),
    )?;
    if !push.ok() {
        return Err(ship_operation_failure("delivery branch push", &push));
    }

    let existing = delivery_github_read(
        broker,
        plan.originating_session.id,
        &repository,
        format!("delivery:pr-list:{branch}"),
        vec![
            "pr".into(),
            "list".into(),
            "--head".into(),
            branch.clone(),
            "--state".into(),
            "all".into(),
            "--json".into(),
            DELIVERY_PR_FIELDS.into(),
        ],
        &main_root,
    )?;
    let mut create_operation = None;
    let mut pull_request = find_delivery_pull_request(
        &existing.stdout,
        &branch,
        &plan.remote_default_branch_ref,
        &plan.publication_sha,
        &plan.remote_default_branch_sha,
        &repository,
    )?;

    if pull_request.is_none() {
        let base_branch = branch_name_from_ref(&plan.remote_default_branch_ref);
        let create = broker.run_coordinated_operation_at_with_wait(
            CoordinatedCommand {
                session_id: plan.originating_session.id,
                provider: OperationProvider::Github,
                repository: Some(repository.clone()),
                resolved_target: None,
                scope: Some(format!("delivery:pr-create:{branch}")),
                declared_effect: Some(OperationEffect::Write),
                destructive_confirmed: false,
                authorization_reason: Some(format!(
                    "open pull-request delivery for queue entry {} at {}",
                    plan.queue_entry.id, plan.publication_sha
                )),
                args: vec![
                    "pr".into(),
                    "create".into(),
                    "--head".into(),
                    branch.clone(),
                    "--base".into(),
                    base_branch,
                    "--title".into(),
                    format!("Deliver broker queue entry {}", plan.queue_entry.id),
                    "--body".into(),
                    format!(
                        "Aethyme delivery for queue entry {}. Exact promoted head: {}.\n\nThis PR is a proposal; broker publication exposures remain unresolved until the merge is verified on the target default branch.",
                        plan.queue_entry.id, plan.publication_sha
                    ),
                ],
            },
            &main_root,
            delivery_operation_wait(),
        )?;
        if !create.ok() {
            return Err(ship_operation_failure("pull-request creation", &create));
        }
        create_operation = Some(create.operation);

        // Read the provider after create even when gh printed a URL. This is
        // the authoritative identity check and makes retries converge on the
        // exact PR rather than trusting output text alone.
        let after_create = delivery_github_read(
            broker,
            plan.originating_session.id,
            &repository,
            format!("delivery:pr-list-after-create:{branch}"),
            vec![
                "pr".into(),
                "list".into(),
                "--head".into(),
                branch.clone(),
                "--state".into(),
                "all".into(),
                "--json".into(),
                DELIVERY_PR_FIELDS.into(),
            ],
            &main_root,
        )?;
        pull_request = find_delivery_pull_request(
            &after_create.stdout,
            &branch,
            &plan.remote_default_branch_ref,
            &plan.publication_sha,
            &plan.remote_default_branch_sha,
            &repository,
        )?;
    }

    let pull_request = pull_request.ok_or_else(|| BrokerOpError::ShipDeliveryUnavailable {
        reason: format!(
            "gh did not expose an open pull request for delivery branch {branch}; inspect the coordinated create operation before retrying"
        ),
    })?;
    let verify = delivery_github_read(
        broker,
        plan.originating_session.id,
        &repository,
        format!("delivery:pr-verify:{}", pull_request.number),
        vec![
            "pr".into(),
            "view".into(),
            pull_request.number.to_string(),
            "--json".into(),
            DELIVERY_PR_FIELDS.into(),
        ],
        &main_root,
    )?;
    let pull_request = parse_delivery_pull_request(
        &verify.stdout,
        &branch,
        &plan.remote_default_branch_ref,
        &plan.publication_sha,
        &plan.remote_default_branch_sha,
        &repository,
    )?;
    if pull_request.state != "OPEN" {
        if pull_request.merged_at.is_some() {
            let target_verification =
                verify_delivery_target(broker, &plan, &pull_request.head_sha, &main_root)?;
            let (delivery_state, resolved_exposures, resolved_advisories) = if target_verification
                .contains_head
            {
                let (resolved_exposures, resolved_advisories) =
                    resolve_verified_publication(broker, &plan, &target_verification.target_sha)?;
                (
                    DeliveryExecutionState::Published,
                    resolved_exposures,
                    resolved_advisories,
                )
            } else {
                (
                    DeliveryExecutionState::PullRequestMerged,
                    Vec::new(),
                    Vec::new(),
                )
            };
            return Ok(PullRequestDeliveryReport {
                plan,
                delivery_state,
                branch,
                proposed_sha: pull_request.head_sha.clone(),
                remote_base_sha: pull_request.base_sha.clone(),
                branch_operation,
                fetch_operation: fetch.operation,
                push_operation: push.operation,
                create_operation,
                verify_operation: verify.operation,
                target_verification_operation: Some(target_verification.operation),
                target_remote_sha: Some(target_verification.target_sha),
                pull_request,
                resolved_exposures,
                resolved_advisories,
            });
        }
        return Err(BrokerOpError::ShipDeliveryPullRequestMismatch {
            reason: format!(
                "PR #{} is {}, not OPEN",
                pull_request.number, pull_request.state
            ),
        });
    }
    let delivery_state = pull_request.checks.state();

    Ok(PullRequestDeliveryReport {
        plan,
        delivery_state,
        branch,
        proposed_sha: pull_request.head_sha.clone(),
        remote_base_sha: pull_request.base_sha.clone(),
        branch_operation,
        fetch_operation: fetch.operation,
        push_operation: push.operation,
        create_operation,
        verify_operation: verify.operation,
        target_verification_operation: None,
        target_remote_sha: None,
        pull_request,
        resolved_exposures: Vec::new(),
        resolved_advisories: Vec::new(),
    })
}

fn delivery_branch_name(plan: &ShipPlan) -> String {
    delivery_branch_name_for(plan.queue_entry.id, &plan.publication_sha)
}

fn delivery_branch_name_for(queue_entry_id: i64, publication_sha: &str) -> String {
    format!(
        "aethyme/delivery/q{}-{}",
        queue_entry_id,
        &publication_sha[..12.min(publication_sha.len())]
    )
}

fn branch_name_from_ref(reference: &str) -> String {
    reference
        .strip_prefix("refs/heads/")
        .unwrap_or(reference)
        .to_string()
}

fn delivery_github_read(
    broker: &mut Broker,
    session_id: i64,
    repository: &str,
    scope: String,
    args: Vec<String>,
    cwd: &Path,
) -> Result<CoordinatedOperationReport, BrokerOpError> {
    let report = broker.run_coordinated_operation_at_with_wait(
        CoordinatedCommand {
            session_id,
            provider: OperationProvider::Github,
            repository: Some(repository.into()),
            resolved_target: None,
            scope: Some(scope),
            declared_effect: Some(OperationEffect::Read),
            destructive_confirmed: false,
            authorization_reason: None,
            args,
        },
        cwd,
        delivery_operation_wait(),
    )?;
    if !report.ok() {
        return Err(ship_operation_failure(
            "GitHub delivery observation",
            &report,
        ));
    }
    Ok(report)
}

#[derive(Debug)]
struct DeliveryTargetVerification {
    operation: CoordinatedOperation,
    target_sha: String,
    contains_head: bool,
}

/// Re-fetch the target after the provider reports a merge. A merged PR is
/// provider evidence only; publication is reached only when the exact
/// reviewed delivery head is also reachable from the target ref observed by
/// Git. A successful fetch with a different target tip is still useful
/// evidence, so callers can report the distinct merged-but-not-published
/// state without resolving queue exposures.
fn verify_delivery_target(
    broker: &mut Broker,
    plan: &ShipPlan,
    head_sha: &str,
    cwd: &Path,
) -> Result<DeliveryTargetVerification, BrokerOpError> {
    let tracking = tracking_ref(plan);
    let fetch = broker.run_coordinated_operation_at_with_wait(
        CoordinatedCommand {
            session_id: plan.originating_session.id,
            provider: OperationProvider::Git,
            repository: None,
            resolved_target: Some(plan.target.clone()),
            scope: Some(format!(
                "delivery:target-verify:{}",
                plan.remote_default_branch_ref
            )),
            // Fetch updates the local tracking ref, so leave the effect
            // inferred rather than downgrading the coordinator's write
            // classification. No remote ref is mutated, but an interrupted
            // fetch leaves local evidence uncertain, so recovery stays
            // conservative.
            declared_effect: None,
            destructive_confirmed: false,
            authorization_reason: Some(format!(
                "refresh target branch evidence after provider merge for queue entry {}",
                plan.queue_entry.id
            )),
            args: vec![
                "fetch".into(),
                "--no-tags".into(),
                "--force".into(),
                plan.target.remote_name.clone(),
                format!("{}:{tracking}", plan.remote_default_branch_ref),
            ],
        },
        cwd,
        delivery_operation_wait(),
    )?;
    if !fetch.ok() {
        return Err(ship_operation_failure(
            "delivery target verification",
            &fetch,
        ));
    }
    let target_sha = broker.repo_handle().resolve_ref(&tracking).ok_or_else(|| {
        BrokerOpError::ShipPlanUnavailable {
            what: "verified delivery target",
            reason: format!("{tracking} does not resolve after target verification fetch"),
        }
    })?;
    let contains_head = broker.repo_handle().is_ancestor(head_sha, &target_sha);
    Ok(DeliveryTargetVerification {
        operation: fetch.operation,
        target_sha,
        contains_head,
    })
}

fn find_delivery_pull_request(
    stdout: &str,
    branch: &str,
    base_ref: &str,
    head_sha: &str,
    base_sha: &str,
    repository: &str,
) -> Result<Option<DeliveryPullRequest>, BrokerOpError> {
    let value: serde_json::Value = serde_json::from_str(stdout).map_err(|error| {
        BrokerOpError::ShipDeliveryPullRequestMismatch {
            reason: format!("gh pr list returned invalid JSON: {error}"),
        }
    })?;
    let entries =
        value
            .as_array()
            .ok_or_else(|| BrokerOpError::ShipDeliveryPullRequestMismatch {
                reason: "gh pr list returned a non-array JSON value".into(),
            })?;
    if entries.is_empty() {
        return Ok(None);
    }
    if entries.len() > 1 {
        return Err(BrokerOpError::ShipDeliveryPullRequestMismatch {
            reason: format!(
                "delivery branch {branch} has {} pull requests; refusing to choose one",
                entries.len()
            ),
        });
    }
    let parsed = parse_delivery_pull_request_value(
        &entries[0],
        branch,
        base_ref,
        head_sha,
        base_sha,
        repository,
    );
    parsed.map(Some)
}

fn parse_delivery_pull_request(
    stdout: &str,
    branch: &str,
    base_ref: &str,
    head_sha: &str,
    base_sha: &str,
    repository: &str,
) -> Result<DeliveryPullRequest, BrokerOpError> {
    let value: serde_json::Value = serde_json::from_str(stdout).map_err(|error| {
        BrokerOpError::ShipDeliveryPullRequestMismatch {
            reason: format!("gh pr view returned invalid JSON: {error}"),
        }
    })?;
    parse_delivery_pull_request_value(&value, branch, base_ref, head_sha, base_sha, repository)
}

fn parse_delivery_pull_request_value(
    value: &serde_json::Value,
    branch: &str,
    base_ref: &str,
    head_sha: &str,
    base_sha: &str,
    repository: &str,
) -> Result<DeliveryPullRequest, BrokerOpError> {
    let number = value
        .get("number")
        .and_then(serde_json::Value::as_i64)
        .filter(|number| *number > 0)
        .ok_or_else(|| ShipDeliveryPullRequestMismatch {
            reason: "pull request has no positive number".into(),
        })?;
    let url = value
        .get("url")
        .and_then(serde_json::Value::as_str)
        .filter(|url| !url.is_empty())
        .ok_or_else(|| ShipDeliveryPullRequestMismatch {
            reason: format!("PR #{number} has no URL"),
        })?
        .to_string();
    let state = value
        .get("state")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let is_draft = value
        .get("isDraft")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let merged_at = string_field(value, "mergedAt");
    let actual_head_branch = string_field(value, "headRefName");
    let actual_head_sha = string_field(value, "headRefOid");
    let actual_base_branch = string_field(value, "baseRefName");
    let actual_base_sha = string_field(value, "baseRefOid");
    let head_repository = repository_field(value, "headRepository").or_else(|| {
        repository_field(value, "headRepositoryOwner").and_then(|owner| {
            repository
                .split_once('/')
                .map(|(_, name)| format!("{owner}/{name}"))
        })
    });
    let head_repository = head_repository.ok_or_else(|| ShipDeliveryPullRequestMismatch {
        reason: format!("PR #{number} has no verifiable head repository"),
    })?;
    let expected_base_branch = branch_name_from_ref(base_ref);
    let actual_base_branch = actual_base_branch.ok_or_else(|| ShipDeliveryPullRequestMismatch {
        reason: format!("PR #{number} has no base branch"),
    })?;
    let actual_head_branch = actual_head_branch.ok_or_else(|| ShipDeliveryPullRequestMismatch {
        reason: format!("PR #{number} has no head branch"),
    })?;
    let actual_head_sha = actual_head_sha.ok_or_else(|| ShipDeliveryPullRequestMismatch {
        reason: format!("PR #{number} has no head SHA"),
    })?;
    let actual_base_sha = actual_base_sha.ok_or_else(|| ShipDeliveryPullRequestMismatch {
        reason: format!("PR #{number} has no base SHA"),
    })?;
    if actual_head_branch != branch {
        return Err(ShipDeliveryPullRequestMismatch {
            reason: format!("expected head {branch}, observed {actual_head_branch}"),
        });
    }
    if actual_head_sha != head_sha {
        return Err(ShipDeliveryPullRequestMismatch {
            reason: format!("expected head SHA {head_sha}, observed {actual_head_sha}"),
        });
    }
    if actual_base_branch != expected_base_branch {
        return Err(ShipDeliveryPullRequestMismatch {
            reason: format!("expected base {expected_base_branch}, observed {actual_base_branch}"),
        });
    }
    if actual_base_sha != base_sha {
        return Err(ShipDeliveryPullRequestMismatch {
            reason: format!("expected base SHA {base_sha}, observed {actual_base_sha}"),
        });
    }
    if !head_repository.eq_ignore_ascii_case(repository) {
        return Err(ShipDeliveryPullRequestMismatch {
            reason: format!("expected head repository {repository}, observed {head_repository}"),
        });
    }
    if is_draft {
        return Err(ShipDeliveryPullRequestMismatch {
            reason: format!("PR #{number} is still a draft"),
        });
    }

    Ok(DeliveryPullRequest {
        number,
        url,
        state,
        is_draft,
        merged_at,
        head_repository,
        head_branch: actual_head_branch,
        head_sha: actual_head_sha,
        base_branch: actual_base_branch,
        base_sha: actual_base_sha,
        checks: parse_delivery_checks(value.get("statusCheckRollup")),
    })
}

fn string_field(value: &serde_json::Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn repository_field(value: &serde_json::Value, field: &str) -> Option<String> {
    let field = value.get(field)?;
    field
        .as_str()
        .map(str::to_string)
        .or_else(|| {
            field
                .get("nameWithOwner")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .or_else(|| {
            let owner = field
                .get("owner")
                .and_then(|owner| owner.get("login"))
                .and_then(serde_json::Value::as_str)?;
            let name = field.get("name").and_then(serde_json::Value::as_str)?;
            Some(format!("{owner}/{name}"))
        })
        .or_else(|| {
            field
                .get("login")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
}

fn parse_delivery_checks(value: Option<&serde_json::Value>) -> DeliveryChecksSummary {
    let Some(entries) = value.and_then(serde_json::Value::as_array) else {
        return DeliveryChecksSummary {
            total: 0,
            pending: 0,
            failed: 0,
            passed: 0,
            unknown: 0,
            checks: Vec::new(),
        };
    };
    let checks = entries
        .iter()
        .map(|entry| DeliveryCheck {
            name: entry
                .get("name")
                .or_else(|| entry.get("context"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown-check")
                .to_string(),
            status: entry
                .get("status")
                .or_else(|| entry.get("state"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            conclusion: entry
                .get("conclusion")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            url: entry
                .get("detailsUrl")
                .or_else(|| entry.get("targetUrl"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        })
        .collect::<Vec<_>>();
    let mut pending = 0;
    let mut failed = 0;
    let mut passed = 0;
    let mut unknown = 0;
    for check in &checks {
        match delivery_check_state(check) {
            DeliveryCheckState::Pending => pending += 1,
            DeliveryCheckState::Failed => failed += 1,
            DeliveryCheckState::Passed => passed += 1,
            DeliveryCheckState::Unknown => unknown += 1,
        }
    }
    DeliveryChecksSummary {
        total: checks.len(),
        pending,
        failed,
        passed,
        unknown,
        checks,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeliveryCheckState {
    Pending,
    Failed,
    Passed,
    Unknown,
}

fn delivery_check_state(check: &DeliveryCheck) -> DeliveryCheckState {
    let conclusion = check.conclusion.as_deref().map(str::to_ascii_uppercase);
    match conclusion.as_deref() {
        Some("SUCCESS" | "NEUTRAL" | "SKIPPED") => DeliveryCheckState::Passed,
        Some(
            "FAILURE" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED" | "STARTUP_FAILURE" | "STALE",
        ) => DeliveryCheckState::Failed,
        Some(_) => DeliveryCheckState::Unknown,
        None => match check
            .status
            .as_deref()
            .map(str::to_ascii_uppercase)
            .as_deref()
        {
            // Legacy commit-status contexts expose their result as `state`
            // rather than a check-run `conclusion`; classify those explicitly
            // while keeping unfamiliar provider values unknown.
            Some("SUCCESS" | "EXPECTED") => DeliveryCheckState::Passed,
            Some("ERROR" | "FAILURE") => DeliveryCheckState::Failed,
            Some("PENDING") => DeliveryCheckState::Pending,
            Some("COMPLETED") => DeliveryCheckState::Unknown,
            Some("QUEUED" | "IN_PROGRESS" | "REQUESTED") => DeliveryCheckState::Pending,
            Some(_) => DeliveryCheckState::Unknown,
            None => DeliveryCheckState::Unknown,
        },
    }
}

fn publication_assessment(
    broker: &mut Broker,
    policy_source_commit: &str,
    included_entries: &[ShipPromotedEntry],
    queue: &[MergeQueueEntry],
    target_repository: &str,
    target_branch: &str,
) -> Result<ShipPublicationAssessment, BrokerOpError> {
    let policy = publication_policy_at(broker, policy_source_commit)?;
    if policy.mode == ShipPublicationMode::Direct {
        return Ok(ShipPublicationAssessment {
            policy,
            source_commit: policy_source_commit.into(),
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
        source_commit: policy_source_commit.into(),
        satisfied,
        evidence,
        remediation,
    })
}

fn publication_policy_at(
    broker: &Broker,
    policy_source_commit: &str,
) -> Result<ShipPublicationPolicy, BrokerOpError> {
    if broker
        .repo_handle()
        .resolve_ref(policy_source_commit)
        .is_none()
    {
        return Err(BrokerOpError::ShipPlanUnavailable {
            what: "trusted publication policy",
            reason: format!(
                "the remote default SHA {policy_source_commit} is not available locally; fetch it before planning publication"
            ),
        });
    }
    let Some(text) = broker
        .repo_handle()
        .file_at_commit(policy_source_commit, ".aethyme/config.toml")?
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
        return Err(local_main_sync_refusal(&assessment, default_branch, plan));
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
    assess_local_main_sync_repo(
        broker.repo_handle(),
        default_branch,
        default_branch_ref,
        expected_head,
        confirm,
    )
}

fn assess_local_main_sync_repo(
    repo: &GitRepo,
    default_branch: &str,
    default_branch_ref: &str,
    expected_head: &str,
    confirm: &str,
) -> Result<ShipLocalMainSyncAssessment, String> {
    let current_branch = repo.current_branch().map_err(|error| error.to_string())?;
    let current_head = repo.head_commit().map_err(|error| error.to_string())?;
    let current_ref = repo
        .resolve_ref(default_branch_ref)
        .ok_or_else(|| format!("{default_branch_ref} no longer resolves"))?;
    let mut tracked_dirty_paths = repo
        .tracked_dirty_paths()
        .map_err(|error| error.to_string())?;
    let mut untracked_paths = repo.untracked_paths().map_err(|error| error.to_string())?;
    tracked_dirty_paths.sort();
    tracked_dirty_paths.dedup();
    untracked_paths.sort();
    untracked_paths.dedup();
    let fast_forward = repo.is_ancestor(&current_head, confirm);
    let incoming_paths = if fast_forward {
        repo.changed_between(&current_head, confirm)
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
        local_commits_not_in_integration: Vec::new(),
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

/// Every refusal names a bounded next step. Refusing without one leaves ref
/// surgery as the only visible way forward, which is what this lane exists to
/// make unnecessary (issue #141). Nothing here suggests a destructive command:
/// the remote publication has already succeeded by this point, and only local
/// synchronization is being declined.
fn local_main_sync_refusal(
    assessment: &ShipLocalMainSyncAssessment,
    default_branch: &str,
    plan: &ShipPlan,
) -> String {
    if !assessment.current_branch_matches {
        return format!(
            "primary checkout is not on expected default branch {default_branch}; \
             check it out there and re-run `aethyme broker ship execute --entry {} \
             --confirm {} --sync-main`",
            plan.queue_entry.id, plan.publication_sha,
        );
    }
    if !assessment.local_head_unchanged {
        return format!(
            "local {} moved since planning, so the reviewed synchronization no longer \
             applies; review a new plan with `aethyme broker ship plan --entry {}`",
            plan.local_default_branch_ref, plan.queue_entry.id,
        );
    }
    if !assessment.local_commits_not_in_integration.is_empty() {
        return format!(
            "local {} contains commits not represented by integration: {}. Review them with \
             `aethyme broker main reconcile plan` before retrying delivery",
            plan.local_default_branch_ref,
            assessment.local_commits_not_in_integration.join(", "),
        );
    }
    if !assessment.fast_forward {
        return format!(
            "local {} carries commits this publication does not contain, so fast-forwarding \
             it would discard them. The remote publication already succeeded; only local \
             synchronization is refused. List what would be lost with \
             `git log --oneline {}..{}`, preserve it with \
             `git branch aethyme/preserve/local-main {}`, then replay anything still needed \
             through a broker session and submit it",
            plan.local_default_branch_ref,
            plan.publication_sha,
            plan.local_default_branch_sha,
            plan.local_default_branch_sha,
        );
    }
    if !assessment.tracked_dirty_paths.is_empty() {
        return format!(
            "primary default-branch checkout has tracked changes that a fast-forward would \
             overwrite: {}. Commit them through a broker session, or stash them with \
             `git stash push -- {}`, then retry --sync-main",
            assessment.tracked_dirty_paths.join(", "),
            assessment.tracked_dirty_paths.join(" "),
        );
    }
    if !assessment.conflicting_untracked_paths.is_empty() {
        return format!(
            "untracked paths would collide with the incoming fast-forward: {}. Move or remove \
             them, then retry --sync-main",
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
    use super::{
        DeliveryExecutionState, REPOSITORY_DELIVERY_POLICY_SCHEMA_VERSION,
        RepositoryDeliveryConfig, RepositoryDeliveryMode, checkout_paths_collide,
        is_broker_runtime_path, parse_delivery_pull_request_value,
    };

    #[test]
    fn checkout_collision_covers_exact_and_file_directory_replacements() {
        assert!(checkout_paths_collide("feature.txt", "feature.txt"));
        assert!(checkout_paths_collide("feature/nested.txt", "feature"));
        assert!(checkout_paths_collide("feature", "feature/nested.txt"));
        assert!(!checkout_paths_collide(".codex/local.md", "src/main.rs"));
        assert!(!checkout_paths_collide("feature-old", "feature"));
    }

    #[test]
    fn delivery_divergence_ignores_broker_runtime_artifacts_but_not_operator_files() {
        assert!(is_broker_runtime_path(".aethyme/broker-advisory.md"));
        assert!(is_broker_runtime_path(".aethyme/run/gates/1.log"));
        assert!(is_broker_runtime_path(
            ".aethyme/logs/command-metrics.jsonl"
        ));
        assert!(!is_broker_runtime_path(".aethyme/config.toml"));
        assert!(!is_broker_runtime_path("operator-note.txt"));
    }

    #[test]
    fn delivery_config_is_strict_and_has_only_the_two_supported_modes() {
        assert_eq!(
            RepositoryDeliveryConfig::from_config_text("schema = 1\n").unwrap(),
            None
        );
        let config =
            RepositoryDeliveryConfig::from_config_text("[delivery]\ndefault = \"pull_request\"\n")
                .unwrap()
                .expect("delivery policy");
        assert_eq!(config.default, RepositoryDeliveryMode::PullRequest);
        assert_eq!(
            config.schema_version,
            REPOSITORY_DELIVERY_POLICY_SCHEMA_VERSION
        );
        assert!(
            RepositoryDeliveryConfig::from_config_text("[delivery]\ndefault = \"reset_hard\"\n")
                .is_err()
        );
        assert!(
            RepositoryDeliveryConfig::from_config_text(
                "[delivery]\ndefault = \"pull_request\"\nallow_force = true\n"
            )
            .is_err()
        );
    }

    #[test]
    fn pull_request_parser_verifies_identity_and_classifies_checks() {
        let value = serde_json::json!({
            "number": 17,
            "url": "https://github.com/acme/project/pull/17",
            "state": "OPEN",
            "isDraft": false,
            "headRefName": "aethyme/delivery/q17-deadbeefcafe",
            "headRefOid": "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            "headRepository": {"nameWithOwner": "acme/project"},
            "baseRefName": "main",
            "baseRefOid": "0123456789012345678901234567890123456789",
            "statusCheckRollup": [
                {"name": "build", "status": "COMPLETED", "conclusion": "SUCCESS"},
                {"name": "lint", "status": "IN_PROGRESS", "conclusion": null},
                {"name": "deploy", "status": "COMPLETED", "conclusion": "FAILURE"},
                {"context": "legacy-status", "state": "SUCCESS"}
            ]
        });
        let pull_request = parse_delivery_pull_request_value(
            &value,
            "aethyme/delivery/q17-deadbeefcafe",
            "refs/heads/main",
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            "0123456789012345678901234567890123456789",
            "acme/project",
        )
        .unwrap();
        assert_eq!(pull_request.number, 17);
        assert_eq!(pull_request.checks.total, 4);
        assert_eq!(pull_request.checks.passed, 2);
        assert_eq!(pull_request.checks.pending, 1);
        assert_eq!(pull_request.checks.failed, 1);
        assert_eq!(
            pull_request.checks.state(),
            DeliveryExecutionState::PullRequestChecksFailed
        );
    }

    #[test]
    fn pull_request_parser_refuses_a_rebased_head_or_wrong_base() {
        let value = serde_json::json!({
            "number": 17,
            "url": "https://github.com/acme/project/pull/17",
            "state": "OPEN",
            "isDraft": false,
            "headRefName": "aethyme/delivery/q17-deadbeefcafe",
            "headRefOid": "changedchangedchangedchangedchangedchanged",
            "headRepository": "acme/project",
            "baseRefName": "main",
            "baseRefOid": "0123456789012345678901234567890123456789",
            "statusCheckRollup": []
        });
        let error = parse_delivery_pull_request_value(
            &value,
            "aethyme/delivery/q17-deadbeefcafe",
            "refs/heads/main",
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
            "0123456789012345678901234567890123456789",
            "acme/project",
        )
        .unwrap_err();
        assert!(error.to_string().contains("head SHA"));
    }
}
