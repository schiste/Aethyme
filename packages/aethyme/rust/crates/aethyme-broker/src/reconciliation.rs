//! Conservative reconciliation of the local promoted integration layer
//! with an externally advanced upstream ref.
//!
//! This module deliberately does not fetch. Operators decide when remote
//! state is current; reconciliation only inspects the named local ref.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::broker::{Broker, BrokerOpError};
use crate::store::ReconciliationQueueUpdate;
use crate::types::{MergeQueueEntry, MergeStatus};

#[derive(Debug, Clone)]
pub struct IntegrationReconcileOptions {
    pub upstream: String,
    pub apply: bool,
    pub resolution_file: Option<PathBuf>,
    pub confirm: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationReconcileClassification {
    AlreadyLanded,
    SupersededUpstream,
    StillPending,
    GenuinelyConflicting,
    Ambiguous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationReconcileCommitOrigin {
    UpstreamOnlyExternalWork,
    RecordedPromotedWork,
    UnrecordedIntegrationCommit,
    PendingQueueEntry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationReconcileEquivalence {
    None,
    Exact,
    PatchEquivalent,
    Ambiguous,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct IntegrationReconcileCommit {
    pub commit: String,
    pub parents: Vec<String>,
    pub origin: IntegrationReconcileCommitOrigin,
    pub equivalence: IntegrationReconcileEquivalence,
    pub matching_commits: Vec<String>,
    pub patch_id: Option<String>,
    pub files: Vec<String>,
    pub content_empty: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_entry_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_status: Option<MergeStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unrecorded_resolution: Option<IntegrationReconcileUnrecordedResolutionAudit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replayed_commit: Option<String>,
    pub conflicts: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_evidence: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct IntegrationReconcilePlan {
    pub common_base: String,
    pub commits: Vec<IntegrationReconcileCommit>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct IntegrationReconcileResolutionFile {
    schema_version: u32,
    upstream_ref: String,
    upstream_commit: String,
    old_integration: String,
    operator: String,
    #[serde(default)]
    resolutions: Vec<IntegrationReconcileResolution>,
    #[serde(default)]
    unrecorded_resolutions: Vec<IntegrationReconcileUnrecordedResolution>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct IntegrationReconcileResolution {
    queue_entry_id: i64,
    old_merge_commit: String,
    classification: IntegrationReconcileClassification,
    reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationReconcileUnrecordedDisposition {
    PreserveAndReplay,
    ReplacedByExactUpstreamSha,
    DropBecauseContentEmpty,
    /// The commit is preserved on a pushed branch or an open pull request,
    /// so integration may drop it without replay (#464).
    TrackedElsewhere,
}

impl IntegrationReconcileUnrecordedDisposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreserveAndReplay => "preserve_and_replay",
            Self::ReplacedByExactUpstreamSha => "replaced_by_exact_upstream_sha",
            Self::DropBecauseContentEmpty => "drop_because_content_empty",
            Self::TrackedElsewhere => "tracked_elsewhere",
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct IntegrationReconcileUnrecordedResolution {
    integration_commit: String,
    disposition: IntegrationReconcileUnrecordedDisposition,
    #[serde(default)]
    upstream_commit: Option<String>,
    /// `tracked_elsewhere`: the pushed branch that preserves the commit.
    #[serde(default)]
    tracked_branch: Option<String>,
    /// `tracked_elsewhere`: the open pull request whose head preserves it.
    #[serde(default)]
    pull_request: Option<u64>,
    reason: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct IntegrationReconcileResolutionTemplateDocument {
    pub schema_version: u32,
    pub upstream_ref: String,
    pub upstream_commit: String,
    pub old_integration: String,
    pub operator: Option<String>,
    pub resolutions: Vec<IntegrationReconcileRecordedResolutionTemplate>,
    pub unrecorded_resolutions: Vec<IntegrationReconcileUnrecordedResolutionTemplate>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct IntegrationReconcileRecordedResolutionTemplate {
    pub queue_entry_id: i64,
    pub old_merge_commit: String,
    pub classification: Option<IntegrationReconcileClassification>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct IntegrationReconcileUnrecordedResolutionTemplate {
    pub integration_commit: String,
    pub disposition: Option<IntegrationReconcileUnrecordedDisposition>,
    pub upstream_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tracked_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<u64>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct IntegrationReconcileUnrecordedDispositionRule {
    pub value: String,
    pub upstream_commit: String,
    pub condition: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct IntegrationReconcileResolutionContract {
    pub schema_version: u32,
    pub operator: String,
    pub reason: String,
    pub recorded_classification_allowed_values: Vec<String>,
    pub unrecorded_dispositions: Vec<IntegrationReconcileUnrecordedDispositionRule>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct IntegrationReconcileRecordedResolutionEvidence {
    pub queue_entry_id: i64,
    pub old_merge_commit: String,
    pub files: Vec<String>,
    pub conflicts: Vec<String>,
    pub evidence: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct IntegrationReconcileUnrecordedResolutionEvidence {
    pub integration_commit: String,
    pub content_empty: bool,
    pub equivalence: IntegrationReconcileEquivalence,
    pub matching_commits: Vec<String>,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct IntegrationReconcileResolutionTemplate {
    pub document: IntegrationReconcileResolutionTemplateDocument,
    pub field_contract: IntegrationReconcileResolutionContract,
    pub recorded_evidence: Vec<IntegrationReconcileRecordedResolutionEvidence>,
    pub unrecorded_evidence: Vec<IntegrationReconcileUnrecordedResolutionEvidence>,
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct IntegrationReconcileUnrecordedResolutionAudit {
    pub disposition: IntegrationReconcileUnrecordedDisposition,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_commit: Option<String>,
    /// Where a `tracked_elsewhere` commit was proven to survive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tracked_elsewhere: Option<IntegrationReconcileTrackedElsewhere>,
    pub operator: String,
    pub reason: String,
    pub resolution_file: String,
}

/// Evidence that an unrecorded integration commit survives outside
/// integration (#464): the remote branch as the remote reported it, the pull
/// request when one was named, and how the commit is contained there.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct IntegrationReconcileTrackedElsewhere {
    pub remote: String,
    pub branch: String,
    pub remote_commit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<u64>,
    /// `ancestor` when the commit itself is reachable from the branch,
    /// `patch_id` when a commit with the same patch is.
    pub containment: &'static str,
    pub matching_commit: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct IntegrationReconcileResolutionAudit {
    pub operator: String,
    pub reason: String,
    pub resolution_file: String,
    pub upstream_commit: String,
    pub old_integration: String,
}

impl IntegrationReconcileClassification {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AlreadyLanded => "already_landed",
            Self::SupersededUpstream => "superseded_upstream",
            Self::StillPending => "still_pending",
            Self::GenuinelyConflicting => "genuinely_conflicting",
            Self::Ambiguous => "ambiguous",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct IntegrationReconcileEntry {
    pub queue_entry_id: i64,
    pub session_id: i64,
    pub classification: IntegrationReconcileClassification,
    pub old_merge_commit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_landing: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replayed_commit: Option<String>,
    pub files: Vec<String>,
    pub conflicts: Vec<String>,
    pub evidence: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operator_resolution: Option<IntegrationReconcileResolutionAudit>,
}

#[derive(Debug, serde::Serialize)]
pub struct IntegrationReconcileReport {
    pub branch: String,
    pub upstream_ref: String,
    pub local_main: String,
    pub upstream_head: String,
    pub old_integration: String,
    pub new_integration: String,
    pub safe: bool,
    pub applied: bool,
    pub plan: IntegrationReconcilePlan,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_digest: Option<String>,
    pub entries: Vec<IntegrationReconcileEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution_template: Option<IntegrationReconcileResolutionTemplate>,
    pub warnings: Vec<String>,
    pub next_action: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationDriftEntryState {
    AlreadyLanded,
    SupersededUpstream,
    Ambiguous,
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct IntegrationDriftEntry {
    pub queue_entry_id: i64,
    pub session_id: i64,
    pub promotion_commit: String,
    pub state: IntegrationDriftEntryState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_landing: Option<String>,
    pub evidence: String,
}

/// Read-only explanation of a fetched-upstream/integration divergence.
///
/// This intentionally stops before replay simulation. It can prove that a
/// complete recorded layer is stale, but it never guesses that unmatched
/// work is safe to discard or labels it conflict-free.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct IntegrationDriftAssessment {
    pub upstream_ref: String,
    pub upstream_head: String,
    pub integration_head: String,
    pub entries: Vec<IntegrationDriftEntry>,
    pub unrecorded_commits: Vec<String>,
    pub chain_complete: bool,
    pub landed_entry_count: usize,
    pub ambiguous_entry_count: usize,
    pub unresolved_entry_count: usize,
    pub stale_only: bool,
    pub automatic_cleanup_safe: bool,
    pub explanation: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomaticIntegrationCleanupState {
    NotNeeded,
    Cleaned,
    Deferred,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AutomaticIntegrationCleanupReport {
    pub state: AutomaticIntegrationCleanupState,
    pub upstream_ref: String,
    pub upstream_head: String,
    pub old_integration: String,
    pub new_integration: String,
    pub cleaned_queue_entry_ids: Vec<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assessment: Option<IntegrationDriftAssessment>,
    pub explanation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
}

/// The command that refreshed a verify-only integration branch (#352).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationRefreshTrigger {
    Start,
    Status,
    Submit,
    Sync,
}

/// One automatic advance of a verify-only integration branch onto the
/// fetched default branch; also the `broker.integration.refreshed` payload.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct IntegrationRefresh {
    pub trigger: IntegrationRefreshTrigger,
    pub branch: String,
    pub upstream_ref: String,
    pub from: String,
    pub to: String,
    pub cleaned_queue_entry_ids: Vec<i64>,
    pub explanation: String,
}

#[derive(Clone)]
struct Candidate {
    entry: MergeQueueEntry,
    merge_commit: String,
    old_parent: String,
    files: Vec<String>,
}

struct CandidateLayer {
    candidates: Vec<Candidate>,
    integration_layer: Vec<String>,
    described_chain_is_complete: bool,
}

struct LoadedOperatorResolutions {
    path: String,
    operator: String,
    resolutions: BTreeMap<i64, IntegrationReconcileResolution>,
    unrecorded_resolutions: BTreeMap<String, IntegrationReconcileUnrecordedResolution>,
}

impl Broker {
    /// Complete or cancel an interrupted two-phase reconciliation. Called
    /// on every broker open before any command can observe queue/ref drift.
    pub(crate) fn recover_prepared_reconciliation(&mut self) -> Result<(), BrokerOpError> {
        let Some(prepared) = self.store().prepared_integration_reconciliation()? else {
            return Ok(());
        };
        let actual = self
            .repo_handle()
            .resolve_ref(&prepared.branch)
            .unwrap_or_else(|| "<missing>".into());
        if actual == prepared.new_integration {
            self.store().finalize_integration_reconciliation()?;
            crate::warn_unrecorded(
                "refresh the advisory projection",
                self.refresh_advisory_projection(),
            );
            return Ok(());
        }
        if actual == prepared.old_integration {
            self.store().abort_integration_reconciliation()?;
            return Ok(());
        }
        Err(BrokerOpError::ReconciliationRecoveryRequired {
            branch: prepared.branch,
            actual,
            old: prepared.old_integration,
            new: prepared.new_integration,
        })
    }

    /// Classify only conclusive landing evidence without changing refs,
    /// queue rows, worktrees, or remote state.
    pub fn assess_integration_drift(
        &self,
        upstream_ref: &str,
        upstream_head: &str,
        integration_head: &str,
    ) -> Result<IntegrationDriftAssessment, BrokerOpError> {
        // Without a deadline the assessment always completes.
        Ok(self
            .assess_integration_drift_within(upstream_ref, upstream_head, integration_head, None)?
            .unwrap_or_else(|| unreachable!("an assessment without a deadline cannot expire")))
    }

    /// [`Self::assess_integration_drift`] for reporting surfaces: `None` when
    /// `deadline` passed before the classification finished. A caller must
    /// then report the drift as not assessed, never as resolved.
    pub fn assess_integration_drift_within(
        &self,
        upstream_ref: &str,
        upstream_head: &str,
        integration_head: &str,
        deadline: Option<std::time::Instant>,
    ) -> Result<Option<IntegrationDriftAssessment>, BrokerOpError> {
        let expired = || deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline);
        let queue = self.store_ref().merge_queue()?;
        let Some(plan) = build_reconciliation_plan_within(
            self.repo_handle(),
            &queue,
            upstream_head,
            integration_head,
            deadline,
        )?
        else {
            return Ok(None);
        };
        let CandidateLayer {
            candidates,
            described_chain_is_complete,
            ..
        } = build_candidate_layer(self.repo_handle(), &queue, &plan, integration_head)?;
        if expired() {
            return Ok(None);
        }
        let Some(classified) = classify_automatic_entries_within(
            self.repo_handle(),
            &plan,
            &candidates,
            upstream_head,
            deadline,
        )?
        else {
            return Ok(None);
        };

        let entries: Vec<IntegrationDriftEntry> = candidates
            .iter()
            .zip(classified)
            .map(|(candidate, classification)| {
                let (state, upstream_landing, evidence) = match classification {
                    Some(entry) => {
                        let state = match entry.classification {
                            IntegrationReconcileClassification::AlreadyLanded => {
                                IntegrationDriftEntryState::AlreadyLanded
                            }
                            IntegrationReconcileClassification::SupersededUpstream => {
                                IntegrationDriftEntryState::SupersededUpstream
                            }
                            IntegrationReconcileClassification::Ambiguous => {
                                IntegrationDriftEntryState::Ambiguous
                            }
                            IntegrationReconcileClassification::StillPending
                            | IntegrationReconcileClassification::GenuinelyConflicting => {
                                IntegrationDriftEntryState::Unresolved
                            }
                        };
                        (state, entry.upstream_landing, entry.evidence)
                    }
                    None => (
                        IntegrationDriftEntryState::Unresolved,
                        None,
                        "no conclusive upstream landing evidence; reviewed reconciliation is required"
                            .into(),
                    ),
                };
                IntegrationDriftEntry {
                    queue_entry_id: candidate.entry.id,
                    session_id: candidate.entry.session_id,
                    promotion_commit: candidate.merge_commit.clone(),
                    state,
                    upstream_landing,
                    evidence,
                }
            })
            .collect();
        let unrecorded_commits: Vec<String> = plan
            .commits
            .iter()
            .filter(|commit| {
                commit.origin == IntegrationReconcileCommitOrigin::UnrecordedIntegrationCommit
            })
            .map(|commit| commit.commit.clone())
            .collect();
        let landed_entry_count = entries
            .iter()
            .filter(|entry| {
                matches!(
                    entry.state,
                    IntegrationDriftEntryState::AlreadyLanded
                        | IntegrationDriftEntryState::SupersededUpstream
                )
            })
            .count();
        let ambiguous_entry_count = entries
            .iter()
            .filter(|entry| entry.state == IntegrationDriftEntryState::Ambiguous)
            .count();
        let unresolved_entry_count = entries
            .iter()
            .filter(|entry| entry.state == IntegrationDriftEntryState::Unresolved)
            .count();
        let stale_only = described_chain_is_complete
            && !entries.is_empty()
            && unrecorded_commits.is_empty()
            && landed_entry_count == entries.len();
        // Nothing recorded and nothing unrecorded: integration holds no work of
        // its own, so there is no landing evidence that could be missing. The
        // `!entries.is_empty()` in `stale_only` made this case fall through to
        // "some promoted entries lack conclusive upstream landing evidence",
        // which names an empty set and refuses the one state that is provably
        // safe -- integration a strict ancestor of upstream (#290 phase 3.1).
        // A repository that does not promote is in this state after every
        // merge, so the refusal was permanent there.
        let carries_nothing_of_its_own = described_chain_is_complete
            && entries.is_empty()
            && unrecorded_commits.is_empty()
            && self
                .repo_handle()
                .is_ancestor(integration_head, upstream_head);
        let local_main = self.repo_handle().head_commit()?;
        let automatic_cleanup_safe = (stale_only || carries_nothing_of_its_own)
            && self.repo_handle().is_ancestor(&local_main, upstream_head);
        let explanation = if carries_nothing_of_its_own {
            if automatic_cleanup_safe {
                "integration holds nothing upstream lacks; it can be fast-forwarded without review"
                    .into()
            } else {
                "integration holds nothing of its own, but local main is not an ancestor of upstream; automatic cleanup is refused"
                    .into()
            }
        } else if stale_only {
            if automatic_cleanup_safe {
                format!(
                    "all {landed_entry_count} promoted entries have conclusive upstream landing evidence; the recorded integration layer is stale and can be cleaned automatically"
                )
            } else {
                "all promoted entries appear landed, but local main is not an ancestor of upstream; automatic cleanup is refused"
                    .into()
            }
        } else if !described_chain_is_complete || !unrecorded_commits.is_empty() {
            "integration contains work outside the complete recorded promoted layer; explicit reviewed reconciliation is required"
                .into()
        } else {
            "some promoted entries lack conclusive upstream landing evidence; explicit reviewed reconciliation is required"
                .into()
        };

        Ok(Some(IntegrationDriftAssessment {
            upstream_ref: upstream_ref.to_string(),
            upstream_head: upstream_head.to_string(),
            integration_head: integration_head.to_string(),
            entries,
            unrecorded_commits,
            chain_complete: described_chain_is_complete,
            landed_entry_count,
            ambiguous_entry_count,
            unresolved_entry_count,
            stale_only,
            automatic_cleanup_safe,
            explanation,
        }))
    }

    /// Clean only a complete integration layer whose every recorded
    /// promotion is already proven upstream by automatic evidence.
    ///
    /// Mixed, ambiguous, unrecorded, or locally divergent histories remain
    /// on the normal digest-confirmed reconciliation path.
    pub fn auto_cleanup_landed_integration(
        &mut self,
        upstream_ref: &str,
    ) -> Result<AutomaticIntegrationCleanupReport, BrokerOpError> {
        // `status` reaches this through its integration refresh while an
        // inspection deadline is installed. That deadline bounds reads; a
        // reconciliation killed between its ref write and its queue record is
        // exactly the partial state it must never leave (#460 review).
        let _mutation = crate::git::suspend_git_deadline();
        self.recover_prepared_reconciliation()?;
        let upstream_head = self
            .repo_handle()
            .resolve_ref(upstream_ref)
            .ok_or_else(|| BrokerOpError::UpstreamRefNotFound {
                upstream: upstream_ref.to_string(),
            })?;
        let (_, old_integration) = self.integration_head()?;
        if self
            .repo_handle()
            .is_ancestor(&upstream_head, &old_integration)
        {
            return Ok(AutomaticIntegrationCleanupReport {
                state: AutomaticIntegrationCleanupState::NotNeeded,
                upstream_ref: upstream_ref.to_string(),
                upstream_head,
                old_integration: old_integration.clone(),
                new_integration: old_integration,
                cleaned_queue_entry_ids: Vec::new(),
                assessment: None,
                explanation: "integration already contains the fetched upstream".into(),
                next_action: None,
            });
        }

        let manual_command = format!(
            "aethyme broker advanced integration reconcile --upstream {upstream_ref} --dry-run"
        );
        // An integration strictly behind upstream holds nothing upstream
        // lacks, which is the whole question the drift assessment answers, so
        // it is skipped there: on a long history it is the expensive part, and
        // `status` now takes this path (#352). The reviewed plan below still
        // decides; ancestry only spares the explanation.
        let strictly_behind = self
            .repo_handle()
            .is_ancestor(&old_integration, &upstream_head);
        let assessment = if strictly_behind {
            None
        } else {
            let assessment =
                self.assess_integration_drift(upstream_ref, &upstream_head, &old_integration)?;
            if !assessment.automatic_cleanup_safe {
                return Ok(AutomaticIntegrationCleanupReport {
                    state: AutomaticIntegrationCleanupState::Deferred,
                    upstream_ref: upstream_ref.to_string(),
                    upstream_head,
                    old_integration: old_integration.clone(),
                    new_integration: old_integration,
                    cleaned_queue_entry_ids: Vec::new(),
                    explanation: assessment.explanation.clone(),
                    assessment: Some(assessment),
                    next_action: Some(manual_command),
                });
            }
            Some(assessment)
        };

        let dry_run = self.reconcile_integration(IntegrationReconcileOptions {
            upstream: upstream_ref.to_string(),
            apply: false,
            resolution_file: None,
            confirm: None,
        })?;
        let conclusively_landed = dry_run.safe
            && dry_run.new_integration == upstream_head
            && !dry_run.entries.is_empty()
            && dry_run.entries.iter().all(|entry| {
                matches!(
                    entry.classification,
                    IntegrationReconcileClassification::AlreadyLanded
                        | IntegrationReconcileClassification::SupersededUpstream
                )
            });
        // The empty-entry case is not ambiguity, it is the simplest safe
        // outcome: nothing was promoted, upstream moved, and integration is a
        // strict ancestor of it. Requiring a non-empty plan refused exactly
        // that, so after every pull-request merge in a repository that does not
        // promote, integration fell further behind and every new session
        // inherited the gap (#290 phase 3.1).
        //
        // `is_ancestor` is the whole safety argument: integration holds nothing
        // upstream lacks, so advancing it discards no work and rewrites no
        // history. Anything else still defers to the reviewed manual path.
        let fast_forward_only = dry_run.safe
            && dry_run.new_integration == upstream_head
            && dry_run.entries.is_empty()
            && self
                .repo_handle()
                .is_ancestor(&old_integration, &upstream_head);
        if !conclusively_landed && !fast_forward_only {
            return Ok(AutomaticIntegrationCleanupReport {
                state: AutomaticIntegrationCleanupState::Deferred,
                upstream_ref: upstream_ref.to_string(),
                upstream_head,
                old_integration: old_integration.clone(),
                new_integration: old_integration,
                cleaned_queue_entry_ids: Vec::new(),
                assessment,
                explanation: "the full reconciliation plan found work that is not a conclusive upstream landing; automatic cleanup was refused"
                    .into(),
                next_action: Some(manual_command),
            });
        }

        let cleaned_queue_entry_ids: Vec<i64> = dry_run
            .entries
            .iter()
            .map(|entry| entry.queue_entry_id)
            .collect();
        let advanced_by_fast_forward = fast_forward_only && !conclusively_landed;
        let applied = self.reconcile_integration(IntegrationReconcileOptions {
            upstream: upstream_ref.to_string(),
            apply: true,
            resolution_file: None,
            confirm: dry_run.plan_digest,
        })?;
        if !applied.applied {
            return Ok(AutomaticIntegrationCleanupReport {
                state: AutomaticIntegrationCleanupState::Deferred,
                upstream_ref: upstream_ref.to_string(),
                upstream_head,
                old_integration,
                new_integration: applied.new_integration,
                cleaned_queue_entry_ids: Vec::new(),
                assessment,
                explanation:
                    "reconciliation state changed before the confirmed cleanup could apply".into(),
                next_action: Some(manual_command),
            });
        }

        Ok(AutomaticIntegrationCleanupReport {
            state: AutomaticIntegrationCleanupState::Cleaned,
            upstream_ref: upstream_ref.to_string(),
            upstream_head,
            old_integration,
            new_integration: applied.new_integration,
            cleaned_queue_entry_ids,
            assessment,
            explanation: if advanced_by_fast_forward {
                "integration held nothing upstream lacked and was fast-forwarded onto it"
            } else {
                "all recorded integration promotions were proven upstream and cleaned transactionally"
            }
                .into(),
            next_action: None,
        })
    }

    /// Keep a verify-only repository's integration branch on the fetched
    /// default branch (#352).
    ///
    /// Integration there is a disposable verification base: nothing lands
    /// through it, but it falls behind every pull request merged outside the
    /// broker, and a stale copy of main is what `finish` counted as
    /// "unsubmitted" work and what reconcile asked an operator to attest.
    /// When it carries nothing upstream lacks, it is advanced through the same
    /// reviewed-plan machinery as the post-merge cleanup, and the move is
    /// recorded as `broker.integration.refreshed`. An integration that holds
    /// work of its own is left alone for the reviewed path, and promoting
    /// repositories are never touched.
    ///
    /// Never fails the calling command: a refusal or error leaves integration
    /// where it was, and status advice still describes it. `status`, the
    /// orientation step every agent runs first, only takes the strictly-behind
    /// case, which needs no drift assessment.
    pub fn refresh_disposable_integration(
        &mut self,
        trigger: IntegrationRefreshTrigger,
    ) -> Option<IntegrationRefresh> {
        if crate::merge::PromoteConfig::load(self.main_root()).mode
            != crate::merge::PromoteMode::VerifyOnly
        {
            return None;
        }
        let (upstream_ref, upstream_head) = self.repo_handle().upstream_default()?;
        let (branch, integration) = self.integration_head().ok()?;
        if self.repo_handle().is_ancestor(&upstream_head, &integration) {
            return None;
        }
        if trigger == IntegrationRefreshTrigger::Status
            && !self.repo_handle().is_ancestor(&integration, &upstream_head)
        {
            return None;
        }
        let report = match self.auto_cleanup_landed_integration(&upstream_ref) {
            Ok(report) => report,
            Err(error) => {
                crate::warn_unrecorded::<(), _>(
                    "refresh the verify-only integration branch",
                    Err(error),
                );
                return None;
            }
        };
        if report.state != AutomaticIntegrationCleanupState::Cleaned {
            return None;
        }
        let refresh = IntegrationRefresh {
            trigger,
            branch,
            upstream_ref,
            from: report.old_integration,
            to: report.new_integration,
            cleaned_queue_entry_ids: report.cleaned_queue_entry_ids,
            explanation: report.explanation,
        };
        let payload = serde_json::to_string(&refresh).unwrap_or_default();
        crate::warn_unrecorded(
            "record the integration refresh",
            self.store().append_event(
                crate::events::BROKER_INTEGRATION_REFRESHED,
                None,
                Some(&payload),
            ),
        );
        Some(refresh)
    }

    /// Inspect a previously fetched upstream ref and, when `apply` is set,
    /// atomically replace the integration layer plus its durable queue
    /// description. Ambiguity and content conflicts are reportable plans,
    /// never partial mutations.
    pub fn reconcile_integration(
        &mut self,
        options: IntegrationReconcileOptions,
    ) -> Result<IntegrationReconcileReport, BrokerOpError> {
        self.recover_prepared_reconciliation()?;
        let upstream_head = self
            .repo_handle()
            .resolve_ref(&options.upstream)
            .ok_or_else(|| BrokerOpError::UpstreamRefNotFound {
                upstream: options.upstream.clone(),
            })?;
        let local_main = self.repo_handle().head_commit()?;
        let (branch, old_integration) = self.integration_head()?;
        let queue = self.store().merge_queue()?;
        let plan = build_reconciliation_plan(
            self.repo_handle(),
            &queue,
            &upstream_head,
            &old_integration,
        )?;
        let operator_resolutions =
            load_operator_resolutions(&options, &upstream_head, &old_integration)?;
        let mut report = IntegrationReconcileReport {
            branch: branch.clone(),
            upstream_ref: options.upstream.clone(),
            local_main: local_main.clone(),
            upstream_head: upstream_head.clone(),
            old_integration: old_integration.clone(),
            new_integration: upstream_head.clone(),
            safe: true,
            applied: false,
            plan,
            plan_digest: None,
            entries: Vec::new(),
            resolution_file: operator_resolutions
                .as_ref()
                .map(|loaded| loaded.path.clone()),
            resolution_template: None,
            warnings: Vec::new(),
            next_action: String::new(),
        };

        if !self.repo_handle().is_ancestor(&local_main, &upstream_head) {
            let checkout =
                CheckoutHead::describe(self.repo_handle(), &options.upstream, &local_main);
            // Local main *ahead* of upstream is not divergence: it is what an
            // operator gets by pulling upstream into main. When integration
            // contains upstream too, there is nothing to reconcile, and
            // rebuilding onto upstream would only discard that merge (#405).
            let upstream_in_main = self.repo_handle().is_ancestor(&upstream_head, &local_main);
            if upstream_in_main
                && self
                    .repo_handle()
                    .is_ancestor(&upstream_head, &old_integration)
            {
                report.new_integration = old_integration.clone();
                report.next_action = format!(
                    "nothing to reconcile: {} {upstream_head} is already contained in {} and {branch}; no refs or broker rows were changed",
                    options.upstream, checkout.name
                );
                return Ok(report);
            }
            report.safe = false;
            if upstream_in_main {
                report.warnings.push(format!(
                    "{} already contains {} {upstream_head}, but {branch} {old_integration} does not; refusing to rebuild integration behind {}",
                    checkout.label, options.upstream, checkout.name
                ));
                report.next_action = match &checkout.switch_hint {
                    Some(hint) => hint.clone(),
                    None => format!(
                        "inspect how local main absorbed {} outside {branch}; no refs or broker rows were changed",
                        options.upstream
                    ),
                };
            } else {
                report.warnings.push(format!(
                    "{} is not an ancestor of {} {upstream_head}; refusing to choose between divergent histories",
                    checkout.label, options.upstream
                ));
                report.next_action = checkout.switch_hint.clone().unwrap_or_else(|| {
                    "inspect the main/upstream divergence, update the local main checkout, then rerun the dry-run"
                        .into()
                });
            }
            return Ok(report);
        }

        let missing_unrecorded = validate_unrecorded_resolutions(
            self.repo_handle(),
            self.main_root(),
            &upstream_remote(&options.upstream),
            operator_resolutions.as_ref(),
            &mut report.plan,
            &upstream_head,
        )?;
        let unrecorded_count = report
            .plan
            .commits
            .iter()
            .filter(|commit| {
                commit.origin == IntegrationReconcileCommitOrigin::UnrecordedIntegrationCommit
            })
            .count();
        if !missing_unrecorded.is_empty() {
            report.warnings.push(format!(
                "integration contains unrecorded work in {unrecorded_count} commit(s); every SHA requires an explicit reviewed disposition, missing: {}",
                missing_unrecorded.join(", ")
            ));
        } else if unrecorded_count > 0 {
            report.warnings.push(format!(
                "validated reviewed dispositions for {unrecorded_count} unrecorded integration commit(s)"
            ));
        }

        // Every commit in the local pending layer must be explained by the
        // promoted queue. Otherwise rebuilding from only known entries could
        // silently discard an operator-created or partially recorded commit.
        // Once upstream is already an ancestor of integration, it is the
        // trusted boundary. Local main may intentionally remain behind after
        // a fetch, and upstream commits below this boundary are not pending
        // integration work. Sort by first-parent position because queue IDs
        // can be re-promoted out of order after older conflicts are retried.
        let CandidateLayer {
            candidates,
            integration_layer,
            described_chain_is_complete,
        } = build_candidate_layer(self.repo_handle(), &queue, &report.plan, &old_integration)?;
        if !described_chain_is_complete {
            report.safe = false;
            report.warnings.push(
                "integration contains commits that are not a contiguous promoted queue layer; refusing to rebuild because unrecorded work could be lost"
                    .into(),
            );
            report.next_action =
                "inspect integration history and broker queue records; no refs or broker rows were changed"
                    .into();
            return Ok(report);
        }

        if let Some(loaded) = operator_resolutions.as_ref() {
            validate_resolution_candidates(loaded, &candidates)?;
        }

        let mut classified = classify_automatic_entries(
            self.repo_handle(),
            &report.plan,
            &candidates,
            &upstream_head,
        )?;

        // Explicit operator attestations are evaluated only after every
        // conclusive automatic matcher. They cannot replace machine-derived
        // evidence, and they run before replay so one resolved conflict can
        // unblock classification of every later queue entry.
        if let Some(loaded) = operator_resolutions.as_ref() {
            for (index, candidate) in candidates.iter().enumerate() {
                let Some(resolution) = loaded.resolutions.get(&candidate.entry.id) else {
                    continue;
                };
                if let Some(automatic) = classified[index].as_ref()
                    && matches!(
                        automatic.classification,
                        IntegrationReconcileClassification::AlreadyLanded
                            | IntegrationReconcileClassification::SupersededUpstream
                    )
                {
                    return Err(invalid_resolution(
                        &loaded.path,
                        format!(
                            "queue entry {} is already classified automatically as {}; remove its redundant operator resolution",
                            candidate.entry.id,
                            automatic.classification.as_str()
                        ),
                    ));
                }
                let audit = IntegrationReconcileResolutionAudit {
                    operator: loaded.operator.clone(),
                    reason: resolution.reason.trim().to_string(),
                    resolution_file: loaded.path.clone(),
                    upstream_commit: upstream_head.clone(),
                    old_integration: old_integration.clone(),
                };
                let mut entry = entry_report(
                    candidate,
                    IntegrationReconcileClassification::SupersededUpstream,
                    Some(upstream_head.clone()),
                    None,
                    Vec::new(),
                    format!(
                        "operator {} attested that this promotion is superseded upstream: {}",
                        loaded.operator,
                        resolution.reason.trim()
                    ),
                );
                entry.operator_resolution = Some(audit);
                classified[index] = Some(entry);
            }
        }

        let candidates_by_commit: BTreeMap<String, usize> = candidates
            .iter()
            .enumerate()
            .map(|(index, candidate)| (candidate.merge_commit.clone(), index))
            .collect();
        let unrecorded_by_commit: BTreeMap<String, usize> = report
            .plan
            .commits
            .iter()
            .enumerate()
            .filter(|(_, commit)| {
                commit.origin == IntegrationReconcileCommitOrigin::UnrecordedIntegrationCommit
            })
            .map(|(index, commit)| (commit.commit.clone(), index))
            .collect();
        let mut rebuilt = upstream_head.clone();
        let mut replay_blocked = false;
        let mut replay_safe = true;
        for integration_commit in &integration_layer {
            if let Some(index) = candidates_by_commit.get(integration_commit).copied() {
                let candidate = &candidates[index];
                if let Some(existing) = classified[index].as_ref() {
                    if matches!(
                        existing.classification,
                        IntegrationReconcileClassification::Ambiguous
                            | IntegrationReconcileClassification::GenuinelyConflicting
                    ) {
                        replay_blocked = true;
                        replay_safe = false;
                    }
                    continue;
                }
                if candidate.files.is_empty() || replay_blocked {
                    classified[index] = Some(entry_report(
                        candidate,
                        IntegrationReconcileClassification::Ambiguous,
                        None,
                        None,
                        Vec::new(),
                        if replay_blocked {
                            "cannot classify safely after an earlier unresolved replay".into()
                        } else {
                            "promotion has no changed-path evidence".into()
                        },
                    ));
                    replay_blocked = true;
                    replay_safe = false;
                    continue;
                }
                let simulation = self.repo_handle().merge_tree_with_base(
                    &candidate.old_parent,
                    &rebuilt,
                    &candidate.merge_commit,
                )?;
                if !simulation.conflicts.is_empty() {
                    classified[index] = Some(entry_report(
                        candidate,
                        IntegrationReconcileClassification::GenuinelyConflicting,
                        None,
                        None,
                        simulation.conflicts,
                        "promoted delta conflicts when replayed onto current upstream".into(),
                    ));
                    replay_blocked = true;
                    replay_safe = false;
                    continue;
                }
                if simulation.tree == self.repo_handle().commit_tree_id(&rebuilt)? {
                    classified[index] = Some(entry_report(
                        candidate,
                        IntegrationReconcileClassification::SupersededUpstream,
                        Some(rebuilt.clone()),
                        None,
                        Vec::new(),
                        "promoted delta is content-empty when replayed onto current upstream"
                            .into(),
                    ));
                    continue;
                }
                let replayed = self.repo_handle().commit_tree(
                    &simulation.tree,
                    &[&rebuilt],
                    &format!(
                        "chore(broker): reconcile pending queue entry {} (session {})",
                        candidate.entry.id, candidate.entry.session_id
                    ),
                    &crate::attribution::Attribution::broker_only(),
                )?;
                rebuilt = replayed.clone();
                classified[index] = Some(entry_report(
                    candidate,
                    IntegrationReconcileClassification::StillPending,
                    None,
                    Some(replayed),
                    Vec::new(),
                    "promoted delta is absent upstream and replays cleanly".into(),
                ));
                continue;
            }

            let Some(plan_index) = unrecorded_by_commit.get(integration_commit).copied() else {
                continue;
            };
            let planned = &mut report.plan.commits[plan_index];
            let Some(resolution) = planned.unrecorded_resolution.as_ref().cloned() else {
                planned.execution_evidence =
                    Some("operator disposition is required before replay can be simulated".into());
                replay_blocked = true;
                replay_safe = false;
                continue;
            };
            if replay_blocked {
                planned.execution_evidence =
                    Some("cannot execute after an earlier unresolved replay".into());
                replay_safe = false;
                continue;
            }
            match resolution.disposition {
                IntegrationReconcileUnrecordedDisposition::PreserveAndReplay => {
                    let old_parent = planned
                        .parents
                        .first()
                        .expect("non-root integration commit");
                    let simulation = self.repo_handle().merge_tree_with_base(
                        old_parent,
                        &rebuilt,
                        &planned.commit,
                    )?;
                    if !simulation.conflicts.is_empty() {
                        planned.conflicts = simulation.conflicts;
                        planned.execution_evidence = Some(
                            "reviewed unrecorded delta conflicts when replayed onto current upstream"
                                .into(),
                        );
                        replay_blocked = true;
                        replay_safe = false;
                        continue;
                    }
                    let replayed = self.repo_handle().commit_tree(
                        &simulation.tree,
                        &[&rebuilt],
                        &format!(
                            "chore(broker): preserve unrecorded integration commit {}",
                            planned.commit
                        ),
                        &crate::attribution::Attribution::broker_only(),
                    )?;
                    rebuilt = replayed.clone();
                    planned.replayed_commit = Some(replayed);
                    planned.execution_evidence =
                        Some("reviewed unrecorded delta replays cleanly".into());
                }
                IntegrationReconcileUnrecordedDisposition::ReplacedByExactUpstreamSha => {
                    planned.execution_evidence = Some(format!(
                        "operator selected exact upstream replacement {}",
                        resolution.upstream_commit.as_deref().unwrap_or_default()
                    ));
                }
                IntegrationReconcileUnrecordedDisposition::DropBecauseContentEmpty => {
                    planned.execution_evidence =
                        Some("operator reviewed this commit as content-empty".into());
                }
                IntegrationReconcileUnrecordedDisposition::TrackedElsewhere => {
                    planned.execution_evidence =
                        resolution.tracked_elsewhere.as_ref().map(|tracked| {
                            let pull_request = tracked
                                .pull_request
                                .map(|number| format!(" (pull request #{number})"))
                                .unwrap_or_default();
                            format!(
                                "preserved on {}/{} at {}{pull_request} by {} {}; not replayed",
                                tracked.remote,
                                tracked.branch,
                                tracked.remote_commit,
                                tracked.containment,
                                tracked.matching_commit
                            )
                        });
                }
            }
        }

        report.entries = classified.into_iter().flatten().collect();
        report.safe = replay_safe
            && report.entries.iter().all(|entry| {
                !matches!(
                    entry.classification,
                    IntegrationReconcileClassification::Ambiguous
                        | IntegrationReconcileClassification::GenuinelyConflicting
                )
            });
        report.new_integration = rebuilt;
        report.resolution_template = build_resolution_template(
            &options,
            &upstream_head,
            &old_integration,
            operator_resolutions.as_ref(),
            &report.plan,
            &report.entries,
        );

        if !missing_unrecorded.is_empty() {
            report.safe = false;
            report.next_action = format!(
                "review the complete schema_version 2 resolution_template, optionally write it with `aethyme broker advanced integration reconcile --upstream {} --write-resolution-template <path> --dry-run`, fill every null judgment and reason, then rerun with --resolution-file <path>; no refs or broker rows were changed",
                options.upstream
            );
            return Ok(report);
        }
        report.plan_digest = Some(reconciliation_plan_digest(&report)?);

        if !report.safe {
            report.next_action = if report.resolution_file.is_some() {
                "the resolution file does not cover every ambiguous/conflicting entry; update the attestation and rerun the dry-run; no refs or broker rows were changed".into()
            } else {
                "automatic evidence is insufficient; review the blocked entries and use a commit-bound --resolution-file to attest only work that upstream supersedes; no refs or broker rows were changed".into()
            };
            return Ok(report);
        }
        if !options.apply {
            let digest = report.plan_digest.as_deref().unwrap_or_default();
            report.next_action = if report.resolution_file.is_some() {
                format!(
                    "review this dry-run and its operator attestations, then rerun the same command with --apply --confirm {digest}"
                )
            } else {
                format!(
                    "review this dry-run, then run `aethyme broker advanced integration reconcile --upstream {} --apply --confirm {digest}`",
                    options.upstream,
                )
            };
            return Ok(report);
        }

        let expected = report.plan_digest.clone().unwrap_or_default();
        let Some(confirm) = options.confirm.as_deref() else {
            return Err(BrokerOpError::ReconciliationConfirmationRequired { expected });
        };
        if confirm.len() != 64 || !confirm.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(BrokerOpError::ReconciliationConfirmationNotSha256);
        }
        if confirm != expected {
            return Err(BrokerOpError::ReconciliationConfirmationMismatch {
                actual: confirm.to_string(),
            });
        }

        let updates = reconciliation_updates(&branch, &options.upstream, &report.entries);
        self.store().prepare_integration_reconciliation(
            &branch,
            &options.upstream,
            &local_main,
            &old_integration,
            &upstream_head,
            &report.new_integration,
            report.plan_digest.as_deref().unwrap_or_default(),
            &updates,
        )?;
        if let Err(ref_error) = self.repo_handle().update_branch_ref_checked(
            &branch,
            &report.new_integration,
            &old_integration,
        ) {
            self.store().abort_integration_reconciliation()?;
            return Err(ref_error.into());
        }
        if let Err(store_error) = self.store().finalize_integration_reconciliation() {
            if let Err(rollback_error) = self.repo_handle().update_branch_ref_checked(
                &branch,
                &old_integration,
                &report.new_integration,
            ) {
                return Err(BrokerOpError::ReconciliationRollbackFailed {
                    reason: format!(
                        "database error: {store_error}; ref rollback error: {rollback_error}"
                    ),
                });
            }
            self.store().abort_integration_reconciliation()?;
            return Err(store_error.into());
        }
        crate::warn_unrecorded(
            "refresh the advisory projection",
            self.refresh_advisory_projection(),
        );
        report.applied = true;
        report.next_action =
            "integration and broker queue reconciled; submit new session work normally".into();
        Ok(report)
    }
}

fn build_candidate_layer(
    repo: &crate::git::GitRepo,
    queue: &[MergeQueueEntry],
    plan: &IntegrationReconcilePlan,
    old_integration: &str,
) -> Result<CandidateLayer, BrokerOpError> {
    // Every promoted entry ever recorded is a candidate when integration
    // still reaches it, so ancestry and first parents are read in one Git
    // process each rather than per entry (#460).
    let reachable: BTreeSet<String> = repo
        .reachable_commits(old_integration)?
        .into_iter()
        .collect();
    let promoted: Vec<(MergeQueueEntry, String)> = queue
        .iter()
        .filter(|&entry| entry.status == MergeStatus::Promoted)
        .filter_map(|entry| {
            let merge_commit = promoted_commit(entry)?;
            reachable
                .contains(&merge_commit)
                .then(|| (entry.clone(), merge_commit))
        })
        .collect();
    let merge_commits: Vec<String> = promoted.iter().map(|(_, commit)| commit.clone()).collect();
    let shapes = repo.commit_shapes(&merge_commits)?;
    let mut candidates = Vec::new();
    for (entry, merge_commit) in promoted {
        // A parentless commit falls back to the per-commit read, which fails
        // exactly as it did before batching.
        let old_parent = match shapes
            .get(&merge_commit)
            .and_then(|shape| shape.parents.first())
        {
            Some(parent) => parent.clone(),
            None => repo.first_parent(&merge_commit)?,
        };
        let files = repo.changed_between(&old_parent, &merge_commit)?;
        candidates.push(Candidate {
            entry,
            merge_commit,
            old_parent,
            files,
        });
    }

    let integration_layer =
        repo.first_parent_commits_between_oldest(&plan.common_base, old_integration)?;
    let positions: BTreeMap<String, usize> = integration_layer
        .iter()
        .enumerate()
        .map(|(index, commit)| (commit.clone(), index))
        .collect();
    let recorded: BTreeSet<&str> = candidates
        .iter()
        .map(|candidate| candidate.merge_commit.as_str())
        .collect();
    let described_unrecorded: BTreeSet<&str> = plan
        .commits
        .iter()
        .filter(|commit| {
            commit.origin == IntegrationReconcileCommitOrigin::UnrecordedIntegrationCommit
        })
        .map(|commit| commit.commit.as_str())
        .collect();
    let described_chain_is_complete = integration_layer.iter().all(|commit| {
        recorded.contains(commit.as_str()) || described_unrecorded.contains(commit.as_str())
    });
    candidates.sort_by_key(|candidate| {
        positions
            .get(&candidate.merge_commit)
            .copied()
            .unwrap_or(usize::MAX)
    });

    Ok(CandidateLayer {
        candidates,
        integration_layer,
        described_chain_is_complete,
    })
}

fn classify_automatic_entries(
    repo: &crate::git::GitRepo,
    plan: &IntegrationReconcilePlan,
    candidates: &[Candidate],
    upstream_head: &str,
) -> Result<Vec<Option<IntegrationReconcileEntry>>, BrokerOpError> {
    // Without a deadline the classification always completes.
    Ok(
        classify_automatic_entries_within(repo, plan, candidates, upstream_head, None)?
            .unwrap_or_default(),
    )
}

/// [`classify_automatic_entries`], giving up with `None` once `deadline`
/// passes. Reporting surfaces pass one; reconciliation itself never does,
/// because a partial classification must not drive a ref change.
fn classify_automatic_entries_within(
    repo: &crate::git::GitRepo,
    plan: &IntegrationReconcilePlan,
    candidates: &[Candidate],
    upstream_head: &str,
    deadline: Option<std::time::Instant>,
) -> Result<Option<Vec<Option<IntegrationReconcileEntry>>>, BrokerOpError> {
    let expired = || deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline);
    let mut upstream_by_patch: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for commit in plan.commits.iter().filter(|commit| {
        commit.origin == IntegrationReconcileCommitOrigin::UpstreamOnlyExternalWork
    }) {
        if let Some(patch_id) = commit.patch_id.as_ref() {
            upstream_by_patch
                .entry(patch_id.clone())
                .or_default()
                .push(commit.commit.clone());
        }
    }

    let mut classified: Vec<Option<IntegrationReconcileEntry>> = vec![None; candidates.len()];

    // Exact graph ancestry is conclusive and takes precedence over
    // content matching. One `rev-list` answers it for every candidate.
    let upstream_reach: BTreeSet<String> =
        repo.reachable_commits(upstream_head)?.into_iter().collect();
    for (index, candidate) in candidates.iter().enumerate() {
        if upstream_reach.contains(&candidate.merge_commit) {
            classified[index] = Some(entry_report(
                candidate,
                IntegrationReconcileClassification::AlreadyLanded,
                Some(candidate.merge_commit.clone()),
                None,
                Vec::new(),
                "promoted merge commit is reachable from upstream".into(),
            ));
        } else if upstream_reach.contains(&candidate.entry.head_commit) {
            classified[index] = Some(entry_report(
                candidate,
                IntegrationReconcileClassification::AlreadyLanded,
                Some(candidate.entry.head_commit.clone()),
                None,
                Vec::new(),
                "submitted session head is reachable from upstream".into(),
            ));
        }
    }

    // Match largest contiguous groups first. This recognizes one upstream
    // squash commit that contains several promoted entries.
    for group_len in (1..=candidates.len()).rev() {
        for start in 0..=candidates.len().saturating_sub(group_len) {
            if expired() {
                return Ok(None);
            }
            let end = start + group_len;
            if classified[start..end].iter().any(Option::is_some) {
                continue;
            }
            if candidates[start..end]
                .windows(2)
                .any(|pair| pair[1].old_parent != pair[0].merge_commit)
            {
                continue;
            }
            let Some(patch_id) = repo.patch_id_between(
                &candidates[start].old_parent,
                &candidates[end - 1].merge_commit,
            )?
            else {
                continue;
            };
            let Some(matches) = upstream_by_patch.get(&patch_id) else {
                continue;
            };
            if matches.len() > 1 {
                for index in start..end {
                    classified[index] = Some(entry_report(
                        &candidates[index],
                        IntegrationReconcileClassification::Ambiguous,
                        None,
                        None,
                        Vec::new(),
                        format!(
                            "stable patch id matches multiple upstream commits: {}",
                            matches.join(", ")
                        ),
                    ));
                }
                continue;
            }
            let landing = matches[0].clone();
            for index in start..end {
                classified[index] = Some(entry_report(
                    &candidates[index],
                    IntegrationReconcileClassification::AlreadyLanded,
                    Some(landing.clone()),
                    None,
                    Vec::new(),
                    if group_len == 1 {
                        "stable patch id matches upstream commit".into()
                    } else {
                        format!(
                            "stable cumulative patch id matches one upstream squash for {group_len} promoted entries"
                        )
                    },
                ));
            }
        }
    }

    // Identical final content on every path touched by an unmatched
    // promotion is sufficient to call it superseded, but empty deltas are
    // ambiguous because they provide no content evidence.
    for (index, candidate) in candidates.iter().enumerate() {
        if classified[index].is_some() || candidate.files.is_empty() {
            continue;
        }
        if expired() {
            return Ok(None);
        }
        if repo.paths_equal(&candidate.merge_commit, upstream_head, &candidate.files)? {
            classified[index] = Some(entry_report(
                candidate,
                IntegrationReconcileClassification::SupersededUpstream,
                Some(upstream_head.to_string()),
                None,
                Vec::new(),
                "upstream has identical content on every path changed by the promotion".into(),
            ));
        }
    }

    Ok(Some(classified))
}

fn build_reconciliation_plan(
    repo: &crate::git::GitRepo,
    queue: &[MergeQueueEntry],
    upstream_head: &str,
    old_integration: &str,
) -> Result<IntegrationReconcilePlan, BrokerOpError> {
    // Without a deadline the plan always completes.
    Ok(
        build_reconciliation_plan_within(repo, queue, upstream_head, old_integration, None)?
            .unwrap_or_else(|| unreachable!("a plan without a deadline cannot expire")),
    )
}

/// [`build_reconciliation_plan`], giving up with `None` once `deadline`
/// passes between commits. Only reporting passes a deadline.
fn build_reconciliation_plan_within(
    repo: &crate::git::GitRepo,
    queue: &[MergeQueueEntry],
    upstream_head: &str,
    old_integration: &str,
    deadline: Option<std::time::Instant>,
) -> Result<Option<IntegrationReconcilePlan>, BrokerOpError> {
    let expired = || deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline);
    let common_base = repo.merge_base(upstream_head, old_integration)?;
    let upstream_commits = repo.first_parent_commits_between_oldest(&common_base, upstream_head)?;
    let integration_commits =
        repo.first_parent_commits_between_oldest(&common_base, old_integration)?;

    // Parents and patch ids for both layers in two Git processes, not five
    // per commit (#460): on a repository whose integration drifted hundreds
    // of commits behind upstream, the per-commit chain is what kept
    // `status --refresh` running for minutes.
    let layered: Vec<String> = upstream_commits
        .iter()
        .chain(&integration_commits)
        .cloned()
        .collect();
    let shapes = repo.commit_shapes(&layered)?;
    let with_parent: Vec<String> = layered
        .iter()
        .filter(|commit| {
            shapes
                .get(*commit)
                .is_some_and(|shape| !shape.parents.is_empty())
        })
        .cloned()
        .collect();
    let patch_ids = repo.first_parent_patch_ids(&with_parent)?;
    // Every layered commit descends from `common_base`, so it is an ancestor
    // of a head exactly when that head's `common_base..head` range lists it.
    let upstream_reach: BTreeSet<String> = repo
        .commits_between_oldest(&common_base, upstream_head)?
        .into_iter()
        .collect();
    let integration_reach: BTreeSet<String> = repo
        .commits_between_oldest(&common_base, old_integration)?
        .into_iter()
        .collect();
    let known = |commit: &String, reach: &BTreeSet<String>| KnownCommit {
        parents: shapes
            .get(commit)
            .map(|shape| shape.parents.clone())
            .unwrap_or_default(),
        patch_id: patch_ids.get(commit).cloned(),
        reaches_other_side: reach.contains(commit),
    };

    let mut upstream_by_patch: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for commit in &upstream_commits {
        if let Some(patch_id) = patch_ids.get(commit) {
            upstream_by_patch
                .entry(patch_id.clone())
                .or_default()
                .push(commit.clone());
        }
    }
    let mut integration_by_patch: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for commit in &integration_commits {
        if let Some(patch_id) = patch_ids.get(commit) {
            integration_by_patch
                .entry(patch_id.clone())
                .or_default()
                .push(commit.clone());
        }
    }

    let recorded: BTreeMap<String, &MergeQueueEntry> = queue
        .iter()
        .filter(|entry| entry.status == MergeStatus::Promoted)
        .filter_map(|entry| promoted_commit(entry).map(|commit| (commit, entry)))
        .collect();
    let mut commits = Vec::new();
    for commit in &upstream_commits {
        if expired() {
            return Ok(None);
        }
        commits.push(reconcile_commit(
            repo,
            commit,
            IntegrationReconcileCommitOrigin::UpstreamOnlyExternalWork,
            None,
            &integration_by_patch,
            old_integration,
            Some(known(commit, &integration_reach)),
        )?);
    }
    for commit in &integration_commits {
        if expired() {
            return Ok(None);
        }
        let queue_entry = recorded.get(commit).copied();
        commits.push(reconcile_commit(
            repo,
            commit,
            if queue_entry.is_some() {
                IntegrationReconcileCommitOrigin::RecordedPromotedWork
            } else {
                IntegrationReconcileCommitOrigin::UnrecordedIntegrationCommit
            },
            queue_entry,
            &upstream_by_patch,
            upstream_head,
            Some(known(commit, &upstream_reach)),
        )?);
    }
    // Terminal statuses are skipped whether or not their commit still exists.
    // An entry that never landed has nothing to line up against upstream, and
    // nothing keeps its commit alive once git collects it -- so reading its
    // parents aborts the whole pass on `bad object` (#268). A *pending* entry
    // with a missing commit is a different matter and is deliberately left to
    // fail loudly: that state should not occur, and silence would hide it.
    for entry in queue.iter().filter(|entry| {
        !matches!(
            entry.status,
            MergeStatus::Promoted
                | MergeStatus::ExternallyLanded
                | MergeStatus::Superseded
                | MergeStatus::Rejected
        )
    }) {
        commits.push(reconcile_commit(
            repo,
            &entry.head_commit,
            IntegrationReconcileCommitOrigin::PendingQueueEntry,
            Some(entry),
            &upstream_by_patch,
            upstream_head,
            None,
        )?);
    }

    Ok(Some(IntegrationReconcilePlan {
        common_base,
        commits,
    }))
}

/// What [`build_reconciliation_plan`] already read in bulk for a layered
/// commit, so [`reconcile_commit`] does not ask Git again one commit at a
/// time.
struct KnownCommit {
    parents: Vec<String>,
    patch_id: Option<String>,
    reaches_other_side: bool,
}

fn reconcile_commit(
    repo: &crate::git::GitRepo,
    commit: &str,
    origin: IntegrationReconcileCommitOrigin,
    queue_entry: Option<&MergeQueueEntry>,
    other_side_by_patch: &BTreeMap<String, Vec<String>>,
    other_side_head: &str,
    known: Option<KnownCommit>,
) -> Result<IntegrationReconcileCommit, BrokerOpError> {
    let (parents, patch_id, reaches_other_side) = match known {
        Some(known) => (known.parents, known.patch_id, known.reaches_other_side),
        None => {
            let parents = repo.commit_parents(commit)?;
            let patch_id = if let Some(parent) = parents.first() {
                repo.patch_id_between(parent, commit)?
            } else {
                None
            };
            let reaches = repo.is_ancestor(commit, other_side_head);
            (parents, patch_id, reaches)
        }
    };
    let files = if let Some(parent) = parents.first() {
        repo.changed_between(parent, commit)?
    } else {
        Vec::new()
    };
    let (equivalence, matching_commits) = if reaches_other_side {
        (
            IntegrationReconcileEquivalence::Exact,
            vec![commit.to_string()],
        )
    } else if let Some(matches) = patch_id
        .as_ref()
        .and_then(|patch_id| other_side_by_patch.get(patch_id))
    {
        if matches.len() == 1 {
            (
                IntegrationReconcileEquivalence::PatchEquivalent,
                matches.clone(),
            )
        } else {
            (IntegrationReconcileEquivalence::Ambiguous, matches.clone())
        }
    } else {
        (IntegrationReconcileEquivalence::None, Vec::new())
    };
    Ok(IntegrationReconcileCommit {
        commit: commit.to_string(),
        parents,
        origin,
        equivalence,
        matching_commits,
        patch_id,
        content_empty: files.is_empty(),
        files,
        queue_entry_id: queue_entry.map(|entry| entry.id),
        session_id: queue_entry.map(|entry| entry.session_id),
        queue_status: queue_entry.map(|entry| entry.status),
        unrecorded_resolution: None,
        replayed_commit: None,
        conflicts: Vec::new(),
        execution_evidence: None,
    })
}

fn load_operator_resolutions(
    options: &IntegrationReconcileOptions,
    upstream_head: &str,
    old_integration: &str,
) -> Result<Option<LoadedOperatorResolutions>, BrokerOpError> {
    let Some(path) = options.resolution_file.as_ref() else {
        return Ok(None);
    };
    let path_label = path.display().to_string();
    let bytes = std::fs::read(path)
        .map_err(|error| invalid_resolution(&path_label, format!("cannot read file: {error}")))?;
    let document: IntegrationReconcileResolutionFile = serde_json::from_slice(&bytes)
        .map_err(|error| invalid_resolution(&path_label, format!("invalid JSON: {error}")))?;

    if !matches!(document.schema_version, 1 | 2) {
        return Err(invalid_resolution(
            &path_label,
            format!(
                "unsupported schema_version {}; expected 1 or 2",
                document.schema_version
            ),
        ));
    }
    if document.schema_version == 1 && !document.unrecorded_resolutions.is_empty() {
        return Err(invalid_resolution(
            &path_label,
            "unrecorded_resolutions require schema_version 2".into(),
        ));
    }
    if document.upstream_ref != options.upstream {
        return Err(invalid_resolution(
            &path_label,
            format!(
                "upstream_ref {:?} does not match requested upstream {:?}",
                document.upstream_ref, options.upstream
            ),
        ));
    }
    if document.upstream_commit != upstream_head {
        return Err(invalid_resolution(
            &path_label,
            format!(
                "upstream_commit {} does not match current {} head {}; fetch/review upstream and create a new attestation",
                document.upstream_commit, options.upstream, upstream_head
            ),
        ));
    }
    if document.old_integration != old_integration {
        return Err(invalid_resolution(
            &path_label,
            format!(
                "old_integration {} does not match current integration {}; create a new attestation for the current queue layer",
                document.old_integration, old_integration
            ),
        ));
    }
    let operator = document.operator.trim();
    if operator.is_empty() || operator.len() > 200 {
        return Err(invalid_resolution(
            &path_label,
            "operator must contain 1–200 non-whitespace bytes".into(),
        ));
    }
    if document.resolutions.is_empty() && document.unrecorded_resolutions.is_empty() {
        return Err(invalid_resolution(
            &path_label,
            "resolutions or unrecorded_resolutions must contain at least one entry".into(),
        ));
    }

    let mut seen = BTreeSet::new();
    let mut resolutions = BTreeMap::new();
    for resolution in document.resolutions {
        if !seen.insert(resolution.queue_entry_id) {
            return Err(invalid_resolution(
                &path_label,
                format!(
                    "queue entry {} appears more than once",
                    resolution.queue_entry_id
                ),
            ));
        }
        if resolution.classification != IntegrationReconcileClassification::SupersededUpstream {
            return Err(invalid_resolution(
                &path_label,
                format!(
                    "queue entry {} requests {}; operator resolutions may only attest superseded_upstream",
                    resolution.queue_entry_id,
                    resolution.classification.as_str()
                ),
            ));
        }
        let reason = resolution.reason.trim();
        if reason.is_empty() || reason.len() > 4096 {
            return Err(invalid_resolution(
                &path_label,
                format!(
                    "queue entry {} reason must contain 1–4096 non-whitespace bytes",
                    resolution.queue_entry_id
                ),
            ));
        }
        resolutions.insert(resolution.queue_entry_id, resolution);
    }

    let mut seen_unrecorded = BTreeSet::new();
    let mut unrecorded_resolutions = BTreeMap::new();
    for resolution in document.unrecorded_resolutions {
        if resolution.integration_commit.len() != 40
            || !resolution
                .integration_commit
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(invalid_resolution(
                &path_label,
                format!(
                    "unrecorded integration_commit {} must be one full 40-character SHA",
                    resolution.integration_commit
                ),
            ));
        }
        if !seen_unrecorded.insert(resolution.integration_commit.clone()) {
            return Err(invalid_resolution(
                &path_label,
                format!(
                    "unrecorded integration commit {} appears more than once",
                    resolution.integration_commit
                ),
            ));
        }
        let reason = resolution.reason.trim();
        if reason.is_empty() || reason.len() > 4096 {
            return Err(invalid_resolution(
                &path_label,
                format!(
                    "unrecorded integration commit {} reason must contain 1–4096 non-whitespace bytes",
                    resolution.integration_commit
                ),
            ));
        }
        unrecorded_resolutions.insert(resolution.integration_commit.clone(), resolution);
    }

    Ok(Some(LoadedOperatorResolutions {
        path: path_label,
        operator: operator.to_string(),
        resolutions,
        unrecorded_resolutions,
    }))
}

fn validate_unrecorded_resolutions(
    repo: &crate::git::GitRepo,
    main_root: &Path,
    remote: &str,
    loaded: Option<&LoadedOperatorResolutions>,
    plan: &mut IntegrationReconcilePlan,
    upstream_head: &str,
) -> Result<Vec<String>, BrokerOpError> {
    let unrecorded: BTreeSet<String> = plan
        .commits
        .iter()
        .filter(|commit| {
            commit.origin == IntegrationReconcileCommitOrigin::UnrecordedIntegrationCommit
        })
        .map(|commit| commit.commit.clone())
        .collect();
    let Some(loaded) = loaded else {
        return Ok(unrecorded.into_iter().collect());
    };
    for commit in loaded.unrecorded_resolutions.keys() {
        if !unrecorded.contains(commit) {
            return Err(invalid_resolution(
                &loaded.path,
                format!(
                    "unrecorded integration commit {commit} is not present in the current reconciliation plan"
                ),
            ));
        }
    }

    let mut missing = Vec::new();
    for commit in plan.commits.iter_mut().filter(|commit| {
        commit.origin == IntegrationReconcileCommitOrigin::UnrecordedIntegrationCommit
    }) {
        let Some(resolution) = loaded.unrecorded_resolutions.get(&commit.commit) else {
            missing.push(commit.commit.clone());
            continue;
        };
        let names_tracking =
            resolution.tracked_branch.is_some() || resolution.pull_request.is_some();
        if names_tracking
            && resolution.disposition != IntegrationReconcileUnrecordedDisposition::TrackedElsewhere
        {
            return Err(invalid_resolution(
                &loaded.path,
                format!(
                    "unrecorded integration commit {} uses {} and must not name tracked_branch or pull_request",
                    commit.commit,
                    resolution.disposition.as_str()
                ),
            ));
        }
        let mut tracked_elsewhere = None;
        match resolution.disposition {
            IntegrationReconcileUnrecordedDisposition::PreserveAndReplay => {
                if resolution.upstream_commit.is_some() {
                    return Err(invalid_resolution(
                        &loaded.path,
                        format!(
                            "unrecorded integration commit {} uses preserve_and_replay and must not name upstream_commit",
                            commit.commit
                        ),
                    ));
                }
            }
            IntegrationReconcileUnrecordedDisposition::ReplacedByExactUpstreamSha => {
                let Some(replacement) = resolution.upstream_commit.as_deref() else {
                    return Err(invalid_resolution(
                        &loaded.path,
                        format!(
                            "unrecorded integration commit {} uses replaced_by_exact_upstream_sha and must name upstream_commit",
                            commit.commit
                        ),
                    ));
                };
                if replacement.len() != 40
                    || !replacement.bytes().all(|byte| byte.is_ascii_hexdigit())
                    || !repo.is_ancestor(replacement, upstream_head)
                {
                    return Err(invalid_resolution(
                        &loaded.path,
                        format!(
                            "unrecorded integration commit {} replacement {} is not one exact full SHA reachable from current upstream {}",
                            commit.commit, replacement, upstream_head
                        ),
                    ));
                }
            }
            IntegrationReconcileUnrecordedDisposition::DropBecauseContentEmpty => {
                if !commit.content_empty {
                    return Err(invalid_resolution(
                        &loaded.path,
                        format!(
                            "unrecorded integration commit {} is not content-empty and cannot use drop_because_content_empty",
                            commit.commit
                        ),
                    ));
                }
                if resolution.upstream_commit.is_some() {
                    return Err(invalid_resolution(
                        &loaded.path,
                        format!(
                            "unrecorded integration commit {} uses drop_because_content_empty and must not name upstream_commit",
                            commit.commit
                        ),
                    ));
                }
            }
            IntegrationReconcileUnrecordedDisposition::TrackedElsewhere => {
                if resolution.upstream_commit.is_some() {
                    return Err(invalid_resolution(
                        &loaded.path,
                        format!(
                            "unrecorded integration commit {} uses tracked_elsewhere and must not name upstream_commit",
                            commit.commit
                        ),
                    ));
                }
                let evidence = prove_tracked_elsewhere(
                    repo,
                    main_root,
                    remote,
                    upstream_head,
                    commit,
                    resolution,
                )
                .map_err(|reason| {
                    invalid_resolution(
                        &loaded.path,
                        format!(
                            "unrecorded integration commit {} cannot use tracked_elsewhere: {reason}",
                            commit.commit
                        ),
                    )
                })?;
                tracked_elsewhere = Some(evidence);
            }
        }
        commit.unrecorded_resolution = Some(IntegrationReconcileUnrecordedResolutionAudit {
            disposition: resolution.disposition,
            upstream_commit: resolution.upstream_commit.clone(),
            tracked_elsewhere,
            operator: loaded.operator.clone(),
            reason: resolution.reason.trim().to_string(),
            resolution_file: loaded.path.clone(),
        });
    }
    Ok(missing)
}

fn validate_resolution_candidates(
    loaded: &LoadedOperatorResolutions,
    candidates: &[Candidate],
) -> Result<(), BrokerOpError> {
    let candidates_by_id: BTreeMap<i64, &Candidate> = candidates
        .iter()
        .map(|candidate| (candidate.entry.id, candidate))
        .collect();
    for (queue_entry_id, resolution) in &loaded.resolutions {
        let Some(candidate) = candidates_by_id.get(queue_entry_id) else {
            return Err(invalid_resolution(
                &loaded.path,
                format!(
                    "queue entry {queue_entry_id} is not a promoted entry in the current contiguous integration layer"
                ),
            ));
        };
        if resolution.old_merge_commit != candidate.merge_commit {
            return Err(invalid_resolution(
                &loaded.path,
                format!(
                    "queue entry {queue_entry_id} old_merge_commit {} does not match current promoted commit {}",
                    resolution.old_merge_commit, candidate.merge_commit
                ),
            ));
        }
    }
    Ok(())
}

fn build_resolution_template(
    options: &IntegrationReconcileOptions,
    upstream_head: &str,
    old_integration: &str,
    loaded: Option<&LoadedOperatorResolutions>,
    plan: &IntegrationReconcilePlan,
    entries: &[IntegrationReconcileEntry],
) -> Option<IntegrationReconcileResolutionTemplate> {
    let mut recorded = BTreeMap::new();
    if let Some(loaded) = loaded {
        for resolution in loaded.resolutions.values() {
            recorded.insert(
                resolution.queue_entry_id,
                IntegrationReconcileRecordedResolutionTemplate {
                    queue_entry_id: resolution.queue_entry_id,
                    old_merge_commit: resolution.old_merge_commit.clone(),
                    classification: Some(resolution.classification),
                    reason: Some(resolution.reason.trim().to_string()),
                },
            );
        }
    }
    for entry in entries.iter().filter(|entry| {
        matches!(
            entry.classification,
            IntegrationReconcileClassification::Ambiguous
                | IntegrationReconcileClassification::GenuinelyConflicting
        )
    }) {
        recorded.entry(entry.queue_entry_id).or_insert_with(|| {
            IntegrationReconcileRecordedResolutionTemplate {
                queue_entry_id: entry.queue_entry_id,
                old_merge_commit: entry.old_merge_commit.clone(),
                classification: None,
                reason: None,
            }
        });
    }

    let mut unrecorded = BTreeMap::new();
    if let Some(loaded) = loaded {
        for resolution in loaded.unrecorded_resolutions.values() {
            unrecorded.insert(
                resolution.integration_commit.clone(),
                IntegrationReconcileUnrecordedResolutionTemplate {
                    integration_commit: resolution.integration_commit.clone(),
                    disposition: Some(resolution.disposition),
                    upstream_commit: resolution.upstream_commit.clone(),
                    tracked_branch: resolution.tracked_branch.clone(),
                    pull_request: resolution.pull_request,
                    reason: Some(resolution.reason.trim().to_string()),
                },
            );
        }
    }
    for commit in plan.commits.iter().filter(|commit| {
        commit.origin == IntegrationReconcileCommitOrigin::UnrecordedIntegrationCommit
    }) {
        unrecorded.entry(commit.commit.clone()).or_insert_with(|| {
            IntegrationReconcileUnrecordedResolutionTemplate {
                integration_commit: commit.commit.clone(),
                disposition: None,
                upstream_commit: None,
                tracked_branch: None,
                pull_request: None,
                reason: None,
            }
        });
    }
    if recorded.is_empty() && unrecorded.is_empty() {
        return None;
    }

    let document = IntegrationReconcileResolutionTemplateDocument {
        schema_version: 2,
        upstream_ref: options.upstream.clone(),
        upstream_commit: upstream_head.to_string(),
        old_integration: old_integration.to_string(),
        operator: loaded.map(|loaded| loaded.operator.clone()),
        resolutions: recorded.into_values().collect(),
        unrecorded_resolutions: unrecorded.into_values().collect(),
    };
    let complete = document.operator.is_some()
        && document.resolutions.iter().all(|resolution| {
            resolution.classification.is_some()
                && resolution
                    .reason
                    .as_deref()
                    .is_some_and(|reason| !reason.trim().is_empty())
        })
        && document.unrecorded_resolutions.iter().all(|resolution| {
            resolution.disposition.is_some()
                && resolution
                    .reason
                    .as_deref()
                    .is_some_and(|reason| !reason.trim().is_empty())
                && match resolution.disposition {
                    Some(IntegrationReconcileUnrecordedDisposition::ReplacedByExactUpstreamSha) => {
                        resolution.upstream_commit.is_some()
                    }
                    Some(
                        IntegrationReconcileUnrecordedDisposition::PreserveAndReplay
                        | IntegrationReconcileUnrecordedDisposition::DropBecauseContentEmpty,
                    ) => resolution.upstream_commit.is_none(),
                    Some(IntegrationReconcileUnrecordedDisposition::TrackedElsewhere) => {
                        resolution.upstream_commit.is_none()
                            && (resolution.tracked_branch.is_some()
                                != resolution.pull_request.is_some())
                    }
                    None => false,
                }
        });
    let recorded_evidence = document
        .resolutions
        .iter()
        .filter_map(|resolution| {
            entries
                .iter()
                .find(|entry| entry.queue_entry_id == resolution.queue_entry_id)
                .map(|entry| IntegrationReconcileRecordedResolutionEvidence {
                    queue_entry_id: entry.queue_entry_id,
                    old_merge_commit: entry.old_merge_commit.clone(),
                    files: entry.files.clone(),
                    conflicts: entry.conflicts.clone(),
                    evidence: entry.evidence.clone(),
                })
        })
        .collect();
    let unrecorded_evidence = plan
        .commits
        .iter()
        .filter(|commit| {
            commit.origin == IntegrationReconcileCommitOrigin::UnrecordedIntegrationCommit
        })
        .map(|commit| IntegrationReconcileUnrecordedResolutionEvidence {
            integration_commit: commit.commit.clone(),
            content_empty: commit.content_empty,
            equivalence: commit.equivalence,
            matching_commits: commit.matching_commits.clone(),
            files: commit.files.clone(),
        })
        .collect();

    Some(IntegrationReconcileResolutionTemplate {
        document,
        field_contract: IntegrationReconcileResolutionContract {
            schema_version: 2,
            operator: "required; 1-200 non-whitespace bytes".into(),
            reason: "required for every entry; 1-4096 non-whitespace bytes".into(),
            recorded_classification_allowed_values: vec!["superseded_upstream".into()],
            unrecorded_dispositions: vec![
                IntegrationReconcileUnrecordedDispositionRule {
                    value: "preserve_and_replay".into(),
                    upstream_commit: "forbidden".into(),
                    condition: "replay the exact integration delta onto current upstream".into(),
                },
                IntegrationReconcileUnrecordedDispositionRule {
                    value: "replaced_by_exact_upstream_sha".into(),
                    upstream_commit: "required; full SHA reachable from current upstream".into(),
                    condition: "the named upstream commit is the reviewed exact replacement".into(),
                },
                IntegrationReconcileUnrecordedDispositionRule {
                    value: "drop_because_content_empty".into(),
                    upstream_commit: "forbidden".into(),
                    condition: "allowed only when content_empty evidence is true".into(),
                },
                IntegrationReconcileUnrecordedDispositionRule {
                    value: "tracked_elsewhere".into(),
                    upstream_commit: "forbidden".into(),
                    condition: "name exactly one of tracked_branch (a branch pushed to the upstream remote) or pull_request (an open pull request); the commit, or one with the same patch, must be reachable from that branch's remote head, which must be fetched; integration then drops it without replay".into(),
                },
            ],
        },
        recorded_evidence,
        unrecorded_evidence,
        complete,
    })
}

/// The remote an upstream ref such as `origin/main` or
/// `refs/remotes/origin/main` belongs to; `origin` when it names none.
fn upstream_remote(upstream: &str) -> String {
    let short = upstream.strip_prefix("refs/remotes/").unwrap_or(upstream);
    match short.split_once('/') {
        Some((remote, _)) if !remote.is_empty() => remote.to_string(),
        _ => "origin".to_string(),
    }
}

/// How long one remote read for `tracked_elsewhere` evidence may take.
const TRACKED_ELSEWHERE_REMOTE_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

/// Prove that an unrecorded integration commit survives outside integration
/// (#464): on a branch the remote reports right now, or on the head of an
/// open pull request. The remote head must already be fetched; containment
/// is the commit's own ancestry or an identical patch on that branch, never a
/// raw diff. Any doubt is a refusal.
fn prove_tracked_elsewhere(
    repo: &crate::git::GitRepo,
    main_root: &Path,
    remote: &str,
    upstream_head: &str,
    commit: &IntegrationReconcileCommit,
    resolution: &IntegrationReconcileUnrecordedResolution,
) -> Result<IntegrationReconcileTrackedElsewhere, String> {
    let (branch, pull_request, pull_request_head) =
        match (&resolution.tracked_branch, resolution.pull_request) {
            (Some(branch), None) => (branch.trim().to_string(), None, None),
            (None, Some(number)) => {
                let (branch, head) = open_pull_request_head(main_root, number)?;
                (branch, Some(number), Some(head))
            }
            _ => return Err("name exactly one of tracked_branch or pull_request".into()),
        };
    if branch.is_empty()
        || branch.len() > 255
        || branch.starts_with('-')
        || branch.contains("..")
        || branch
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control() || "~^:?*[\\".contains(ch))
    {
        return Err(format!("{branch:?} is not a branch name"));
    }
    let remote_commit = repo
        .remote_branch_head(remote, &branch, TRACKED_ELSEWHERE_REMOTE_BUDGET)
        .ok_or_else(|| {
            format!("could not read refs/heads/{branch} on {remote}; it is not proven pushed")
        })?;
    if let Some(head) = pull_request_head
        && head != remote_commit
    {
        return Err(format!(
            "pull request head {head} differs from {remote}/{branch} at {remote_commit}; fetch and retry"
        ));
    }
    if repo.resolve_ref(&remote_commit).is_none() {
        return Err(format!(
            "{remote}/{branch} head {remote_commit} is not in the local object store; run `git fetch {remote}` first"
        ));
    }
    if repo.is_ancestor(&commit.commit, &remote_commit) {
        return Ok(IntegrationReconcileTrackedElsewhere {
            remote: remote.to_string(),
            branch,
            remote_commit,
            pull_request,
            containment: "ancestor",
            matching_commit: commit.commit.clone(),
        });
    }
    let Some(patch_id) = commit.patch_id.as_deref() else {
        return Err(format!(
            "it is not reachable from {remote}/{branch} and has no patch to compare"
        ));
    };
    let base = repo
        .merge_base(upstream_head, &remote_commit)
        .map_err(|error| format!("cannot compare with {remote}/{branch}: {error}"))?;
    let branch_commits = repo
        .first_parent_commits_between_oldest(&base, &remote_commit)
        .map_err(|error| format!("cannot list {remote}/{branch}: {error}"))?;
    let patch_ids = repo
        .first_parent_patch_ids(&branch_commits)
        .map_err(|error| format!("cannot read patches on {remote}/{branch}: {error}"))?;
    let matching_commit = branch_commits
        .iter()
        .find(|candidate| patch_ids.get(*candidate).map(String::as_str) == Some(patch_id))
        .ok_or_else(|| {
            format!("neither it nor an identical patch is on {remote}/{branch} at {remote_commit}")
        })?;
    Ok(IntegrationReconcileTrackedElsewhere {
        remote: remote.to_string(),
        branch,
        remote_commit,
        pull_request,
        containment: "patch_id",
        matching_commit: matching_commit.clone(),
    })
}

/// Head branch and commit of an open pull request, read with the trusted gh.
fn open_pull_request_head(main_root: &Path, number: u64) -> Result<(String, String), String> {
    let mut command = crate::operations::github_command();
    command
        .args([
            "pr",
            "view",
            &number.to_string(),
            "--json",
            "state,headRefName,headRefOid",
        ])
        .current_dir(main_root)
        .stdin(std::process::Stdio::null());
    let output =
        crate::bounded_output::output_within(&mut command, TRACKED_ELSEWHERE_REMOTE_BUDGET)
            .map_err(|error| format!("could not run gh pr view {number}: {error}"))?
            .ok_or_else(|| format!("gh pr view {number} did not answer in time"))?;
    if !output.status.success() {
        return Err(format!(
            "gh pr view {number} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("gh pr view {number} returned unreadable JSON: {error}"))?;
    if value.get("state").and_then(serde_json::Value::as_str) != Some("OPEN") {
        return Err(format!("pull request #{number} is not open"));
    }
    let branch = value
        .get("headRefName")
        .and_then(serde_json::Value::as_str)
        .filter(|branch| !branch.is_empty())
        .ok_or_else(|| format!("pull request #{number} reports no head branch"))?;
    let head = value
        .get("headRefOid")
        .and_then(serde_json::Value::as_str)
        .filter(|head| head.len() == 40 && head.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| format!("pull request #{number} reports no head commit"))?;
    Ok((branch.to_string(), head.to_string()))
}

fn invalid_resolution(path: &str, reason: String) -> BrokerOpError {
    BrokerOpError::InvalidReconciliationResolution {
        path: path.to_string(),
        reason,
    }
}

/// Hash only reviewed, reproducible inputs. Replayed commit IDs are omitted:
/// `git commit-tree` embeds time, so those objects may differ between a
/// dry-run and a later confirmed execution even when every source fact and
/// selected disposition is identical.
fn reconciliation_plan_digest(
    report: &IntegrationReconcileReport,
) -> Result<String, BrokerOpError> {
    let commits = report
        .plan
        .commits
        .iter()
        .map(|commit| {
            serde_json::json!({
                "commit": commit.commit,
                "parents": commit.parents,
                "origin": commit.origin,
                "equivalence": commit.equivalence,
                "matching_commits": commit.matching_commits,
                "patch_id": commit.patch_id,
                "files": commit.files,
                "content_empty": commit.content_empty,
                "queue_entry_id": commit.queue_entry_id,
                "session_id": commit.session_id,
                "queue_status": commit.queue_status,
                "unrecorded_resolution": commit.unrecorded_resolution,
            })
        })
        .collect::<Vec<_>>();
    let entries = report
        .entries
        .iter()
        .map(|entry| {
            serde_json::json!({
                "queue_entry_id": entry.queue_entry_id,
                "session_id": entry.session_id,
                "classification": entry.classification,
                "old_merge_commit": entry.old_merge_commit,
                "upstream_landing": entry.upstream_landing,
                "files": entry.files,
                "conflicts": entry.conflicts,
                "evidence": entry.evidence,
                "operator_resolution": entry.operator_resolution,
            })
        })
        .collect::<Vec<_>>();
    let canonical = serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "branch": report.branch,
        "upstream_ref": report.upstream_ref,
        "local_main": report.local_main,
        "upstream_head": report.upstream_head,
        "old_integration": report.old_integration,
        "common_base": report.plan.common_base,
        "commits": commits,
        "entries": entries,
    }))?;
    Ok(format!("{:x}", Sha256::digest(canonical)))
}

fn promoted_commit(entry: &MergeQueueEntry) -> Option<String> {
    let details: serde_json::Value = serde_json::from_str(entry.details_json.as_deref()?).ok()?;
    details.get("commit")?.as_str().map(str::to_string)
}

fn entry_report(
    candidate: &Candidate,
    classification: IntegrationReconcileClassification,
    upstream_landing: Option<String>,
    replayed_commit: Option<String>,
    conflicts: Vec<String>,
    evidence: String,
) -> IntegrationReconcileEntry {
    IntegrationReconcileEntry {
        queue_entry_id: candidate.entry.id,
        session_id: candidate.entry.session_id,
        classification,
        old_merge_commit: candidate.merge_commit.clone(),
        upstream_landing,
        replayed_commit,
        files: candidate.files.clone(),
        conflicts,
        evidence,
        operator_resolution: None,
    }
}

fn reconciliation_updates(
    branch: &str,
    upstream_ref: &str,
    entries: &[IntegrationReconcileEntry],
) -> Vec<ReconciliationQueueUpdate> {
    entries
        .iter()
        .filter_map(|entry| {
            let (status, details, replayed_commit) = match entry.classification {
                IntegrationReconcileClassification::AlreadyLanded
                | IntegrationReconcileClassification::SupersededUpstream => {
                    let details = crate::events::merge_externally_landed_payload(
                        branch,
                        &entry.old_merge_commit,
                        entry.classification.as_str(),
                        upstream_ref,
                        entry.upstream_landing.as_deref(),
                        entry.operator_resolution.as_ref().map(|resolution| {
                            crate::events::OperatorResolutionPayload {
                                operator: &resolution.operator,
                                reason: &resolution.reason,
                                resolution_file: &resolution.resolution_file,
                                upstream_commit: &resolution.upstream_commit,
                                old_integration: &resolution.old_integration,
                            }
                        }),
                    );
                    (MergeStatus::ExternallyLanded, details, None)
                }
                IntegrationReconcileClassification::StillPending => {
                    let replayed = entry.replayed_commit.clone()?;
                    let details = serde_json::json!({
                        "branch": branch,
                        "commit": replayed,
                        "reconciled_from": entry.old_merge_commit,
                        "upstream_ref": upstream_ref,
                    })
                    .to_string();
                    (MergeStatus::Promoted, details, Some(replayed))
                }
                IntegrationReconcileClassification::GenuinelyConflicting
                | IntegrationReconcileClassification::Ambiguous => return None,
            };
            Some(ReconciliationQueueUpdate {
                queue_entry_id: entry.queue_entry_id,
                status,
                merged_tree: None,
                details_json: details,
                classification: entry.classification.as_str().to_string(),
                old_merge_commit: entry.old_merge_commit.clone(),
                upstream_landing: entry.upstream_landing.clone(),
                replayed_commit,
            })
        })
        .collect()
}

/// How a reconcile refusal names the commit it compared against upstream:
/// the primary checkout's HEAD. That is "local main" only when the checkout
/// is on the branch the upstream ref names. On any other branch, calling it
/// local main sent operators to inspect a main/upstream divergence that did
/// not exist (#463).
struct CheckoutHead {
    /// Short name for running text: "local main", or the checked-out branch.
    name: String,
    /// The name with its commit, for the warning.
    label: String,
    /// The fix when the checkout is not on the upstream's branch.
    switch_hint: Option<String>,
}

impl CheckoutHead {
    fn describe(repo: &crate::git::GitRepo, upstream: &str, head: &str) -> Self {
        let expected = upstream
            .rsplit_once('/')
            .map_or(upstream, |(_, branch)| branch)
            .to_string();
        let current = repo
            .current_branch()
            .ok()
            .filter(|branch| !branch.is_empty());
        match current.as_deref() {
            Some(branch) if branch == expected => Self {
                name: format!("local {expected}"),
                label: format!("local {expected} {head}"),
                switch_hint: None,
            },
            current => {
                let (name, on) = match current {
                    Some("HEAD") | None => (
                        "the primary checkout's detached HEAD".to_string(),
                        "a detached HEAD".to_string(),
                    ),
                    Some(branch) => (
                        format!("the primary checkout's branch {branch}"),
                        format!("branch {branch}"),
                    ),
                };
                Self {
                    label: format!("{name} (HEAD {head}; {on}, not {expected})"),
                    name,
                    switch_hint: Some(format!(
                        "the primary checkout is on {on}, not {expected}: run `git switch {expected}` there (or compare {expected} itself), then rerun the dry-run; no refs or broker rows were changed"
                    )),
                }
            }
        }
    }
}
