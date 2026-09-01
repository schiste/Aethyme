//! Opt-in, session-owned pull-request review coordination.
//!
//! The state machine is provider-neutral. GitHub is currently the only
//! execution adapter, and it is invoked only by explicit broker commands.

use std::path::Path;
use std::process::Command;

use sha2::{Digest, Sha256};

use crate::broker::{Broker, BrokerOpError};
use crate::operations::CoordinatedCommand;
use crate::types::{MergeQueueEntry, OperationEffect, OperationProvider, OperationStatus};

pub const REVIEW_POLICY_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewProvider {
    Github,
}

impl Default for ReviewProvider {
    fn default() -> Self {
        Self::Github
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewEvidenceAdapter {
    GithubApproval,
    GithubCheckRun,
}

impl Default for ReviewEvidenceAdapter {
    fn default() -> Self {
        Self::GithubApproval
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationUnlockAdapter {
    GithubLabel,
    GithubWorkflow,
    CloudBuildManualTrigger,
}

impl Default for ValidationUnlockAdapter {
    fn default() -> Self {
        Self::GithubLabel
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReviewPolicy {
    pub schema_version: u32,
    pub enabled: bool,
    pub provider: ReviewProvider,
    pub evidence_adapter: ReviewEvidenceAdapter,
    pub required_approvals: u32,
    pub evidence_check_name: Option<String>,
    pub evidence_app_slug: Option<String>,
    pub unlock_adapter: ValidationUnlockAdapter,
    pub unlock_label: String,
    pub workflow: Option<String>,
}

impl Default for ReviewPolicy {
    fn default() -> Self {
        Self {
            schema_version: REVIEW_POLICY_SCHEMA_VERSION,
            enabled: false,
            provider: ReviewProvider::Github,
            evidence_adapter: ReviewEvidenceAdapter::GithubApproval,
            required_approvals: 1,
            evidence_check_name: None,
            evidence_app_slug: None,
            unlock_adapter: ValidationUnlockAdapter::GithubLabel,
            unlock_label: "aethyme-validation-ready".into(),
            workflow: None,
        }
    }
}

impl ReviewPolicy {
    pub fn load(root: &Path) -> Result<Self, BrokerOpError> {
        let path = root.join(".aethyme/config.toml");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(BrokerOpError::ReviewLifecycle {
                    reason: format!("cannot read {}: {error}", path.display()),
                });
            }
        };
        let value =
            text.parse::<toml::Value>()
                .map_err(|error| BrokerOpError::ReviewLifecycle {
                    reason: format!("invalid .aethyme/config.toml: {error}"),
                })?;
        let Some(review) = value.get("review") else {
            return Ok(Self::default());
        };
        let policy: Self =
            review
                .clone()
                .try_into()
                .map_err(|error| BrokerOpError::ReviewLifecycle {
                    reason: format!("invalid [review] policy: {error}"),
                })?;
        policy.validate()?;
        Ok(policy)
    }

    fn validate(&self) -> Result<(), BrokerOpError> {
        if self.schema_version != REVIEW_POLICY_SCHEMA_VERSION {
            return Err(BrokerOpError::ReviewLifecycle {
                reason: format!(
                    "unsupported review policy schema {}; expected {}",
                    self.schema_version, REVIEW_POLICY_SCHEMA_VERSION
                ),
            });
        }
        match self.evidence_adapter {
            ReviewEvidenceAdapter::GithubApproval => {
                if self.required_approvals != 1 {
                    return Err(BrokerOpError::ReviewLifecycle {
                        reason: "the github_approval evidence adapter requires review.required_approvals = 1; do not approximate distinct-reviewer evidence"
                            .into(),
                    });
                }
                if self.evidence_check_name.is_some() || self.evidence_app_slug.is_some() {
                    return Err(BrokerOpError::ReviewLifecycle {
                        reason: "review.evidence_check_name and review.evidence_app_slug are valid only with github_check_run evidence"
                            .into(),
                    });
                }
            }
            ReviewEvidenceAdapter::GithubCheckRun => {
                if self.required_approvals != 0 {
                    return Err(BrokerOpError::ReviewLifecycle {
                        reason: "the github_check_run evidence adapter requires review.required_approvals = 0"
                            .into(),
                    });
                }
                validate_policy_token(
                    "review.evidence_check_name",
                    self.evidence_check_name.as_deref(),
                )?;
                validate_policy_token(
                    "review.evidence_app_slug",
                    self.evidence_app_slug.as_deref(),
                )?;
            }
        }
        if self.unlock_label.is_empty()
            || self.unlock_label.len() > 100
            || self.unlock_label.chars().any(char::is_control)
        {
            return Err(BrokerOpError::ReviewLifecycle {
                reason: "review.unlock_label must contain 1..=100 non-control characters".into(),
            });
        }
        match self.unlock_adapter {
            ValidationUnlockAdapter::GithubLabel if self.workflow.is_some() => {
                Err(BrokerOpError::ReviewLifecycle {
                    reason: "review.workflow is not valid with github_label unlock".into(),
                })
            }
            ValidationUnlockAdapter::GithubWorkflow
                if self.workflow.as_deref().is_none_or(str::is_empty) =>
            {
                Err(BrokerOpError::ReviewLifecycle {
                    reason: "github_workflow unlock requires review.workflow".into(),
                })
            }
            _ => Ok(()),
        }
    }
}

fn validate_policy_token(name: &str, value: Option<&str>) -> Result<(), BrokerOpError> {
    let Some(value) = value else {
        return review_error(&format!("{name} is required"));
    };
    if value.is_empty() || value.len() > 100 || value.chars().any(char::is_control) {
        return review_error(&format!(
            "{name} must contain 1..=100 non-control characters"
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewLifecycleState {
    DraftOpened,
    LocalSubmissionVerified,
    ReviewRequested,
    ChangesRequested,
    ReplacementCommitSubmitted,
    ReviewSatisfied,
    ValidationUnlocked,
}

impl ReviewLifecycleState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DraftOpened => "draft_opened",
            Self::LocalSubmissionVerified => "local_submission_verified",
            Self::ReviewRequested => "review_requested",
            Self::ChangesRequested => "changes_requested",
            Self::ReplacementCommitSubmitted => "replacement_commit_submitted",
            Self::ReviewSatisfied => "review_satisfied",
            Self::ValidationUnlocked => "validation_unlocked",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, crate::BrokerError> {
        match value {
            "draft_opened" => Ok(Self::DraftOpened),
            "local_submission_verified" => Ok(Self::LocalSubmissionVerified),
            "review_requested" => Ok(Self::ReviewRequested),
            "changes_requested" => Ok(Self::ChangesRequested),
            "replacement_commit_submitted" => Ok(Self::ReplacementCommitSubmitted),
            "review_satisfied" => Ok(Self::ReviewSatisfied),
            "validation_unlocked" => Ok(Self::ValidationUnlocked),
            other => Err(crate::BrokerError::InvalidReviewLifecycleState(
                other.into(),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReviewLifecycle {
    pub id: i64,
    pub session_id: i64,
    pub queue_entry_id: Option<i64>,
    pub repository: String,
    pub target_branch: String,
    pub pr_number: i64,
    pub commit_sha: String,
    pub state: ReviewLifecycleState,
    pub generation: i64,
    pub evidence_digest: Option<String>,
    pub unlock_operation_id: Option<i64>,
    pub active: bool,
    pub abandoned_at: Option<i64>,
    pub abandon_reason_digest: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct NewReviewLifecycle {
    pub session_id: i64,
    pub repository: String,
    pub target_branch: String,
    pub pr_number: i64,
    pub commit_sha: String,
    pub evidence_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReviewLifecycleReport {
    pub policy: ReviewPolicy,
    pub lifecycle: ReviewLifecycle,
    pub changed: bool,
    pub operation_id: Option<i64>,
    pub non_blocking_feedback: bool,
    pub next_action: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReviewLifecycleAbandonReport {
    pub lifecycle: ReviewLifecycle,
    pub abandoned: bool,
    pub next_action: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "adapter", rename_all = "snake_case")]
pub enum ReviewSatisfactionEvidence {
    GithubApproval {
        satisfied: bool,
        review_decision: Option<String>,
    },
    GithubCheckRun {
        satisfied: bool,
        check_name: String,
        app_slug: String,
        check_run_id: Option<u64>,
        status: Option<String>,
        conclusion: Option<String>,
        head_sha: String,
    },
}

impl ReviewSatisfactionEvidence {
    pub fn is_satisfied(&self) -> bool {
        match self {
            Self::GithubApproval { satisfied, .. } | Self::GithubCheckRun { satisfied, .. } => {
                *satisfied
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReviewProviderSnapshot {
    pub repository: String,
    pub pr_number: i64,
    pub target_branch: String,
    pub head_sha: String,
    pub state: String,
    pub is_draft: bool,
    pub review_decision: Option<String>,
    pub satisfaction_evidence: ReviewSatisfactionEvidence,
}

pub fn load_review_provider_snapshot(
    cwd: &Path,
    repository: &str,
    pr_number: i64,
    policy: &ReviewPolicy,
) -> Result<ReviewProviderSnapshot, BrokerOpError> {
    if pr_number <= 0 {
        return review_error("pull request number must be positive");
    }
    let target = crate::resolve_github_target(repository, &[])?;
    let output = Command::new("gh")
        .args([
            "pr",
            "view",
            &pr_number.to_string(),
            "--repo",
            &target.display_slug,
            "--json",
            "number,baseRefName,headRefOid,state,isDraft,reviewDecision",
        ])
        .current_dir(cwd)
        .output()
        .map_err(|_| BrokerOpError::ReviewLifecycle {
            reason: "GitHub review evidence is unavailable".into(),
        })?;
    if !output.status.success() {
        return review_error("GitHub review evidence is unavailable; no transition was attempted");
    }
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|_| BrokerOpError::ReviewLifecycle {
            reason: "GitHub returned invalid review evidence".into(),
        })?;
    let number = value.get("number").and_then(serde_json::Value::as_i64);
    let target_branch = value.get("baseRefName").and_then(serde_json::Value::as_str);
    let head_sha = value.get("headRefOid").and_then(serde_json::Value::as_str);
    let state = value.get("state").and_then(serde_json::Value::as_str);
    let is_draft = value.get("isDraft").and_then(serde_json::Value::as_bool);
    let (Some(number), Some(target_branch), Some(head_sha), Some(state), Some(is_draft)) =
        (number, target_branch, head_sha, state, is_draft)
    else {
        return review_error("GitHub review evidence omitted required provenance fields");
    };
    if number != pr_number {
        return review_error("GitHub review evidence returned a different pull request number");
    }
    validate_full_sha(head_sha)?;
    let review_decision = value
        .get("reviewDecision")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let satisfaction_evidence = match policy.evidence_adapter {
        ReviewEvidenceAdapter::GithubApproval => ReviewSatisfactionEvidence::GithubApproval {
            satisfied: review_decision.as_deref() == Some("APPROVED"),
            review_decision: review_decision.clone(),
        },
        ReviewEvidenceAdapter::GithubCheckRun => load_check_run_evidence(
            cwd,
            &target.display_slug,
            head_sha,
            policy
                .evidence_check_name
                .as_deref()
                .expect("validated check name"),
            policy
                .evidence_app_slug
                .as_deref()
                .expect("validated app slug"),
        )?,
    };
    Ok(ReviewProviderSnapshot {
        repository: target.coordination_key,
        pr_number,
        target_branch: target_branch.into(),
        head_sha: head_sha.into(),
        state: state.into(),
        is_draft,
        review_decision,
        satisfaction_evidence,
    })
}

fn load_check_run_evidence(
    cwd: &Path,
    repository: &str,
    head_sha: &str,
    check_name: &str,
    app_slug: &str,
) -> Result<ReviewSatisfactionEvidence, BrokerOpError> {
    let endpoint = format!(
        "repos/{repository}/commits/{head_sha}/check-runs?check_name={}&filter=latest&per_page=100",
        percent_encode_query(check_name)
    );
    let output = Command::new("gh")
        .args([
            "api",
            "--method",
            "GET",
            "-H",
            "Accept: application/vnd.github+json",
            &endpoint,
        ])
        .current_dir(cwd)
        .output()
        .map_err(|_| BrokerOpError::ReviewLifecycle {
            reason: "GitHub check-run review evidence is unavailable".into(),
        })?;
    if !output.status.success() {
        return review_error(
            "GitHub check-run review evidence is unavailable; no transition was attempted",
        );
    }
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).map_err(|_| BrokerOpError::ReviewLifecycle {
            reason: "GitHub returned invalid check-run review evidence".into(),
        })?;
    parse_check_run_evidence(&value, head_sha, check_name, app_slug)
}

fn parse_check_run_evidence(
    value: &serde_json::Value,
    head_sha: &str,
    check_name: &str,
    app_slug: &str,
) -> Result<ReviewSatisfactionEvidence, BrokerOpError> {
    let total_count = value
        .get("total_count")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| BrokerOpError::ReviewLifecycle {
            reason: "GitHub check-run evidence omitted total_count".into(),
        })?;
    let runs = value
        .get("check_runs")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| BrokerOpError::ReviewLifecycle {
            reason: "GitHub check-run evidence omitted check_runs".into(),
        })?;
    if total_count > runs.len() as u64 {
        return review_error("GitHub check-run evidence was truncated; refusing partial evidence");
    }
    let selected = runs
        .iter()
        .filter(|run| {
            run.get("name").and_then(serde_json::Value::as_str) == Some(check_name)
                && run.get("head_sha").and_then(serde_json::Value::as_str) == Some(head_sha)
                && run
                    .get("app")
                    .and_then(|app| app.get("slug"))
                    .and_then(serde_json::Value::as_str)
                    == Some(app_slug)
        })
        .max_by_key(|run| {
            run.get("id")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
        });
    let check_run_id = selected.and_then(|run| run.get("id")?.as_u64());
    let status = selected
        .and_then(|run| run.get("status"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let conclusion = selected
        .and_then(|run| run.get("conclusion"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    Ok(ReviewSatisfactionEvidence::GithubCheckRun {
        satisfied: status.as_deref() == Some("completed")
            && conclusion.as_deref() == Some("success"),
        check_name: check_name.into(),
        app_slug: app_slug.into(),
        check_run_id,
        status,
        conclusion,
        head_sha: head_sha.into(),
    })
}

fn percent_encode_query(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

impl Broker {
    pub fn register_review_lifecycle(
        &mut self,
        session_id: i64,
        repository: &str,
        snapshot: &ReviewProviderSnapshot,
        now: i64,
    ) -> Result<ReviewLifecycleReport, BrokerOpError> {
        let policy = ReviewPolicy::load(&self.main_root_path())?;
        require_enabled(&policy)?;
        let target = crate::resolve_github_target(repository, &[])?;
        if target.coordination_key != snapshot.repository {
            return review_error("provider snapshot repository does not match --repo");
        }
        if snapshot.state != "OPEN" || !snapshot.is_draft {
            return review_error("review registration requires an open draft pull request");
        }
        validate_full_sha(&snapshot.head_sha)?;
        let session = self.store().session(session_id)?;
        if session.status.is_closed() {
            return review_error("review registration requires a live session");
        }
        let session_repo = crate::GitRepo::discover(Path::new(&session.worktree_path))?;
        let session_head = session_repo.head_commit()?;
        if session_head != snapshot.head_sha {
            return review_error(&format!(
                "pull request head {} does not equal session HEAD {}",
                snapshot.head_sha, session_head
            ));
        }
        let evidence_digest = snapshot_digest(snapshot);
        let (lifecycle, changed) = self.store().create_review_lifecycle(
            &NewReviewLifecycle {
                session_id,
                repository: target.coordination_key,
                target_branch: snapshot.target_branch.clone(),
                pr_number: snapshot.pr_number,
                commit_sha: snapshot.head_sha.clone(),
                evidence_digest,
            },
            now,
        )?;
        Ok(report(policy, lifecycle, changed, None))
    }

    pub fn reassign_review_lifecycle(
        &mut self,
        from_session_id: i64,
        to_session_id: i64,
        reason: &str,
        now: i64,
    ) -> Result<ReviewLifecycleReport, BrokerOpError> {
        let policy = ReviewPolicy::load(&self.main_root_path())?;
        require_enabled(&policy)?;
        if from_session_id == to_session_id {
            return review_error("review reassignment requires two different session ids");
        }
        let reason_digest = review_recovery_reason_digest(reason)?;
        let current = required_lifecycle(self, from_session_id)?;
        let from_session = self.store().session(from_session_id)?;
        if !from_session.status.is_closed() {
            return review_error(
                "review reassignment is only allowed from a closed session; finish the live owner or continue using it",
            );
        }
        let to_session = self.store().session(to_session_id)?;
        if to_session.status.is_closed() {
            return review_error("review reassignment requires a live destination session");
        }
        if let Some(existing) = self.store().review_lifecycle_for_session(to_session_id)? {
            return review_error(&format!(
                "destination session {to_session_id} already owns review lifecycle {} for {} PR #{}",
                existing.id, existing.repository, existing.pr_number
            ));
        }
        let checkout = crate::GitRepo::discover(Path::new(&to_session.worktree_path))?;
        let destination_head = checkout.head_commit()?;
        if destination_head != current.commit_sha {
            return review_error(&format!(
                "destination session HEAD {destination_head} does not equal lifecycle commit {}; preserve or submit the intended replacement before reassignment",
                current.commit_sha
            ));
        }
        let lifecycle = self.store().reassign_review_lifecycle(
            current.id,
            from_session_id,
            to_session_id,
            &reason_digest,
            now,
        )?;
        Ok(report(policy, lifecycle, true, None))
    }

    pub fn abandon_review_lifecycle(
        &mut self,
        session_id: i64,
        reason: &str,
        now: i64,
    ) -> Result<ReviewLifecycleAbandonReport, BrokerOpError> {
        let policy = ReviewPolicy::load(&self.main_root_path())?;
        require_enabled(&policy)?;
        let reason_digest = review_recovery_reason_digest(reason)?;
        let current = required_lifecycle(self, session_id)?;
        let lifecycle =
            self.store()
                .abandon_review_lifecycle(current.id, session_id, &reason_digest, now)?;
        Ok(ReviewLifecycleAbandonReport {
            next_action: format!(
                "register {} PR #{} under a live session with `aethyme broker review register --session <id> --repo {} --pr {}`",
                lifecycle.repository,
                lifecycle.pr_number,
                lifecycle
                    .repository
                    .strip_prefix("github.com/")
                    .unwrap_or(&lifecycle.repository),
                lifecycle.pr_number
            ),
            lifecycle,
            abandoned: true,
        })
    }

    pub(crate) fn record_review_submission(
        &mut self,
        entry: &MergeQueueEntry,
    ) -> Result<(), BrokerOpError> {
        let policy = ReviewPolicy::load(&self.main_root_path())?;
        if !policy.enabled {
            return Ok(());
        }
        let Some(current) = self
            .store()
            .review_lifecycle_for_session(entry.session_id)?
        else {
            return Ok(());
        };
        let next = match current.state {
            ReviewLifecycleState::DraftOpened => ReviewLifecycleState::LocalSubmissionVerified,
            ReviewLifecycleState::ChangesRequested if current.commit_sha == entry.head_commit => {
                return Ok(());
            }
            ReviewLifecycleState::ChangesRequested => {
                ReviewLifecycleState::ReplacementCommitSubmitted
            }
            _ if current.commit_sha == entry.head_commit => return Ok(()),
            _ => {
                return review_error(&format!(
                    "session {} has review state {} at {}; a new submission is allowed only after changes_requested",
                    entry.session_id,
                    current.state.as_str(),
                    current.commit_sha
                ));
            }
        };
        self.store().transition_review_lifecycle(
            current.id,
            current.state,
            next,
            Some(entry.id),
            &entry.head_commit,
            None,
            None,
            now_ms(),
        )?;
        Ok(())
    }

    pub fn request_review(
        &mut self,
        session_id: i64,
        snapshot: &ReviewProviderSnapshot,
        now: i64,
    ) -> Result<ReviewLifecycleReport, BrokerOpError> {
        let policy = ReviewPolicy::load(&self.main_root_path())?;
        require_enabled(&policy)?;
        let current = required_lifecycle(self, session_id)?;
        if matches!(
            current.state,
            ReviewLifecycleState::ReviewRequested
                | ReviewLifecycleState::ReviewSatisfied
                | ReviewLifecycleState::ValidationUnlocked
        ) {
            validate_snapshot_identity(&current, snapshot)?;
            return Ok(report(policy, current, false, None));
        }
        if !matches!(
            current.state,
            ReviewLifecycleState::LocalSubmissionVerified
                | ReviewLifecycleState::ReplacementCommitSubmitted
        ) {
            return review_error(
                "review cannot be requested before a successful broker submission",
            );
        }
        validate_snapshot_identity(&current, snapshot)?;
        let operation_id = if current.state == ReviewLifecycleState::LocalSubmissionVerified {
            if !snapshot.is_draft {
                return review_error(
                    "initial ready transition requires the pull request to still be draft",
                );
            }
            let operation = self.run_coordinated_operation(CoordinatedCommand {
                session_id,
                provider: OperationProvider::Github,
                repository: Some(display_slug(&current.repository)),
                resolved_target: None,
                scope: Some(format!("pull_request:{}", current.pr_number)),
                declared_effect: Some(OperationEffect::Write),
                destructive_confirmed: false,
                authorization_reason: Some("explicit review lifecycle request transition".into()),
                args: vec!["pr".into(), "ready".into(), current.pr_number.to_string()],
            })?;
            if operation.operation.status != OperationStatus::Succeeded {
                return review_error(
                    "GitHub ready-for-review transition was not proven successful",
                );
            }
            Some(operation.operation.id)
        } else {
            if snapshot.is_draft {
                return review_error(
                    "replacement review restart requires the pull request to remain non-draft",
                );
            }
            None
        };
        let lifecycle = self.store().transition_review_lifecycle(
            current.id,
            current.state,
            ReviewLifecycleState::ReviewRequested,
            current.queue_entry_id,
            &current.commit_sha,
            Some(&snapshot_digest(snapshot)),
            operation_id,
            now,
        )?;
        Ok(report(policy, lifecycle, true, operation_id))
    }

    pub fn unlock_review_validation(
        &mut self,
        session_id: i64,
        snapshot: &ReviewProviderSnapshot,
        now: i64,
    ) -> Result<ReviewLifecycleReport, BrokerOpError> {
        let policy = ReviewPolicy::load(&self.main_root_path())?;
        require_enabled(&policy)?;
        let mut current = required_lifecycle(self, session_id)?;
        if current.state == ReviewLifecycleState::ValidationUnlocked {
            validate_snapshot(&current, snapshot, false)?;
            require_satisfied_evidence(snapshot)?;
            return Ok(report(policy, current, false, None));
        }
        if current.state == ReviewLifecycleState::ReviewRequested {
            validate_snapshot(&current, snapshot, false)?;
            require_satisfied_evidence(snapshot)?;
            current = self.store().transition_review_lifecycle(
                current.id,
                current.state,
                ReviewLifecycleState::ReviewSatisfied,
                current.queue_entry_id,
                &current.commit_sha,
                Some(&snapshot_digest(snapshot)),
                None,
                now,
            )?;
        } else if current.state != ReviewLifecycleState::ReviewSatisfied {
            return review_error(
                "validation unlock requires review_requested or review_satisfied state",
            );
        }
        validate_snapshot(&current, snapshot, false)?;
        require_satisfied_evidence(snapshot)?;
        let args = match policy.unlock_adapter {
            ValidationUnlockAdapter::GithubLabel => vec![
                "pr".into(),
                "edit".into(),
                current.pr_number.to_string(),
                "--add-label".into(),
                policy.unlock_label.clone(),
            ],
            ValidationUnlockAdapter::GithubWorkflow => vec![
                "workflow".into(),
                "run".into(),
                policy.workflow.clone().expect("validated workflow"),
                "--ref".into(),
                current.commit_sha.clone(),
            ],
            ValidationUnlockAdapter::CloudBuildManualTrigger => {
                return review_error(
                    "cloud_build_manual_trigger is provider-adapter planning only; configure an external adapter without adding GCP credentials to the broker",
                );
            }
        };
        let operation = self.run_coordinated_operation(CoordinatedCommand {
            session_id,
            provider: OperationProvider::Github,
            repository: Some(display_slug(&current.repository)),
            resolved_target: None,
            scope: Some(format!("validation_unlock:pr:{}", current.pr_number)),
            declared_effect: Some(OperationEffect::Write),
            destructive_confirmed: false,
            authorization_reason: Some("explicit review-satisfied validation unlock".into()),
            args,
        })?;
        if operation.operation.status != OperationStatus::Succeeded {
            return review_error("validation unlock outcome was not proven successful");
        }
        let lifecycle = self.store().transition_review_lifecycle(
            current.id,
            current.state,
            ReviewLifecycleState::ValidationUnlocked,
            current.queue_entry_id,
            &current.commit_sha,
            Some(&snapshot_digest(snapshot)),
            Some(operation.operation.id),
            now,
        )?;
        Ok(report(
            policy,
            lifecycle,
            true,
            Some(operation.operation.id),
        ))
    }

    pub(crate) fn apply_review_event(
        &mut self,
        repository: &str,
        pr_number: i64,
        commit_sha: &str,
        event_type: &str,
        evidence_digest: &str,
        now: i64,
    ) -> Result<(), BrokerOpError> {
        let policy = ReviewPolicy::load(&self.main_root_path())?;
        if !policy.enabled {
            return Ok(());
        }
        let Some(current) = self
            .store()
            .review_lifecycle_for_pr(repository, pr_number)?
        else {
            return Ok(());
        };
        if current.commit_sha != commit_sha {
            return Ok(());
        }
        let next = match event_type {
            "review_changes_requested"
                if matches!(
                    current.state,
                    ReviewLifecycleState::ReviewRequested | ReviewLifecycleState::ReviewSatisfied
                ) =>
            {
                ReviewLifecycleState::ChangesRequested
            }
            "review_approved"
                if policy.evidence_adapter == ReviewEvidenceAdapter::GithubApproval
                    && current.state == ReviewLifecycleState::ReviewRequested =>
            {
                ReviewLifecycleState::ReviewSatisfied
            }
            _ => return Ok(()),
        };
        self.store().transition_review_lifecycle(
            current.id,
            current.state,
            next,
            current.queue_entry_id,
            commit_sha,
            Some(evidence_digest),
            None,
            now,
        )?;
        Ok(())
    }
}

fn required_lifecycle(
    broker: &mut Broker,
    session_id: i64,
) -> Result<ReviewLifecycle, BrokerOpError> {
    broker
        .store()
        .review_lifecycle_for_session(session_id)?
        .ok_or_else(|| BrokerOpError::ReviewLifecycle {
            reason: format!("session {session_id} has no review lifecycle"),
        })
}

fn review_recovery_reason_digest(reason: &str) -> Result<String, BrokerOpError> {
    let trimmed = reason.trim();
    if trimmed.is_empty() || trimmed.len() > 500 {
        return review_error("review recovery --reason must contain 1 to 500 characters");
    }
    Ok(format!("{:x}", Sha256::digest(trimmed.as_bytes())))
}

fn require_enabled(policy: &ReviewPolicy) -> Result<(), BrokerOpError> {
    if policy.enabled {
        Ok(())
    } else {
        review_error("review coordination is disabled; add an explicit [review] policy")
    }
}

fn validate_snapshot(
    lifecycle: &ReviewLifecycle,
    snapshot: &ReviewProviderSnapshot,
    require_draft: bool,
) -> Result<(), BrokerOpError> {
    validate_snapshot_identity(lifecycle, snapshot)?;
    if require_draft != snapshot.is_draft {
        return review_error(if require_draft {
            "ready transition requires the pull request to still be draft"
        } else {
            "validation unlock requires a non-draft pull request"
        });
    }
    Ok(())
}

fn validate_snapshot_identity(
    lifecycle: &ReviewLifecycle,
    snapshot: &ReviewProviderSnapshot,
) -> Result<(), BrokerOpError> {
    if snapshot.repository != lifecycle.repository
        || snapshot.pr_number != lifecycle.pr_number
        || snapshot.target_branch != lifecycle.target_branch
    {
        return review_error(
            "provider evidence does not match the registered repository, PR, and base",
        );
    }
    if snapshot.state != "OPEN" {
        return review_error("pull request is no longer open");
    }
    if snapshot.head_sha != lifecycle.commit_sha {
        return review_error(&format!(
            "pull request head drifted from {} to {}; submit and register the replacement before continuing",
            lifecycle.commit_sha, snapshot.head_sha
        ));
    }
    Ok(())
}

fn require_satisfied_evidence(snapshot: &ReviewProviderSnapshot) -> Result<(), BrokerOpError> {
    if snapshot.satisfaction_evidence.is_satisfied() {
        return Ok(());
    }
    match &snapshot.satisfaction_evidence {
        ReviewSatisfactionEvidence::GithubApproval { .. } => {
            review_error("current provider evidence does not report an approved review decision")
        }
        ReviewSatisfactionEvidence::GithubCheckRun {
            check_name,
            app_slug,
            ..
        } => review_error(&format!(
            "current provider evidence does not report a successful {check_name:?} check from GitHub App {app_slug:?} on the exact pull request head"
        )),
    }
}

fn validate_full_sha(value: &str) -> Result<(), BrokerOpError> {
    if value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        review_error("review commit provenance must be a full 40-character Git SHA")
    }
}

fn snapshot_digest(snapshot: &ReviewProviderSnapshot) -> String {
    let evidence = serde_json::to_vec(&snapshot.satisfaction_evidence)
        .expect("review satisfaction evidence is serializable");
    let mut bytes = format!(
        "{}\0{}\0{}\0{}\0{}\0{}\0{}\0",
        snapshot.repository,
        snapshot.pr_number,
        snapshot.target_branch,
        snapshot.head_sha,
        snapshot.state,
        snapshot.is_draft,
        snapshot.review_decision.as_deref().unwrap_or("")
    )
    .into_bytes();
    bytes.extend(evidence);
    crate::sha256_bytes(&bytes)
}

fn display_slug(repository: &str) -> String {
    repository
        .strip_prefix("github.com/")
        .unwrap_or(repository)
        .to_string()
}

fn report(
    policy: ReviewPolicy,
    lifecycle: ReviewLifecycle,
    changed: bool,
    operation_id: Option<i64>,
) -> ReviewLifecycleReport {
    let next_action = match lifecycle.state {
        ReviewLifecycleState::DraftOpened => {
            format!("aethyme broker submit --session {}", lifecycle.session_id)
        }
        ReviewLifecycleState::LocalSubmissionVerified
        | ReviewLifecycleState::ReplacementCommitSubmitted => {
            format!(
                "aethyme broker review request --session {}",
                lifecycle.session_id
            )
        }
        ReviewLifecycleState::ReviewRequested => {
            format!(
                "aethyme broker review show --session {}",
                lifecycle.session_id
            )
        }
        ReviewLifecycleState::ChangesRequested => {
            "commit the replacement through the accepted session, then submit it".into()
        }
        ReviewLifecycleState::ReviewSatisfied => {
            format!(
                "aethyme broker review unlock --session {}",
                lifecycle.session_id
            )
        }
        ReviewLifecycleState::ValidationUnlocked => "validation is explicitly unlocked".into(),
    };
    ReviewLifecycleReport {
        policy,
        lifecycle,
        changed,
        operation_id,
        non_blocking_feedback: true,
        next_action,
    }
}

fn review_error<T>(reason: &str) -> Result<T, BrokerOpError> {
    Err(BrokerOpError::ReviewLifecycle {
        reason: reason.into(),
    })
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().into()
    }

    fn fixture(enabled: bool) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
        std::fs::write(tmp.path().join(".gitignore"), "/.aethyme/broker.db*\n").unwrap();
        std::fs::write(tmp.path().join("README.md"), "initial\n").unwrap();
        if enabled {
            std::fs::write(
                tmp.path().join(".aethyme/config.toml"),
                "[review]\nenabled = true\nunlock_adapter = \"github_label\"\n",
            )
            .unwrap();
        }
        git(tmp.path(), &["add", "-A"]);
        git(tmp.path(), &["commit", "-qm", "initial"]);
        git(
            tmp.path(),
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/acme/product.git",
            ],
        );
        tmp
    }

    fn snapshot(head: &str) -> ReviewProviderSnapshot {
        let review_decision = None;
        ReviewProviderSnapshot {
            repository: "github.com/acme/product".into(),
            pr_number: 42,
            target_branch: "main".into(),
            head_sha: head.into(),
            state: "OPEN".into(),
            is_draft: true,
            review_decision: review_decision.clone(),
            satisfaction_evidence: ReviewSatisfactionEvidence::GithubApproval {
                satisfied: false,
                review_decision,
            },
        }
    }

    #[test]
    fn default_policy_is_byte_path_inert_and_explicit_policy_validates() {
        let disabled = fixture(false);
        let mut broker = Broker::open(disabled.path()).unwrap();
        let session = broker.adopt(disabled.path(), None).unwrap();
        let head = git(disabled.path(), &["rev-parse", "HEAD"]);
        let error = broker
            .register_review_lifecycle(session.id, "acme/product", &snapshot(&head), 1)
            .unwrap_err();
        assert!(error.to_string().contains("disabled"));
        assert!(
            broker
                .store()
                .review_lifecycle_for_session(session.id)
                .unwrap()
                .is_none()
        );

        let enabled = fixture(true);
        let policy = ReviewPolicy::load(enabled.path()).unwrap();
        assert!(policy.enabled);
        assert_eq!(policy.required_approvals, 1);
        assert_eq!(policy.unlock_adapter, ValidationUnlockAdapter::GithubLabel);
    }

    #[test]
    fn review_evidence_policy_is_explicit_and_adapter_specific() {
        let default = ReviewPolicy::default();
        assert_eq!(
            default.evidence_adapter,
            ReviewEvidenceAdapter::GithubApproval
        );
        default.validate().unwrap();

        let check_run = ReviewPolicy {
            evidence_adapter: ReviewEvidenceAdapter::GithubCheckRun,
            required_approvals: 0,
            evidence_check_name: Some("review-gate/codex".into()),
            evidence_app_slug: Some("github-actions".into()),
            ..ReviewPolicy::default()
        };
        check_run.validate().unwrap();

        let missing_actor = ReviewPolicy {
            evidence_app_slug: None,
            ..check_run.clone()
        };
        assert!(
            missing_actor
                .validate()
                .unwrap_err()
                .to_string()
                .contains("evidence_app_slug is required")
        );
        let mixed = ReviewPolicy {
            evidence_adapter: ReviewEvidenceAdapter::GithubApproval,
            required_approvals: 1,
            ..check_run
        };
        assert!(
            mixed
                .validate()
                .unwrap_err()
                .to_string()
                .contains("valid only with github_check_run")
        );
    }

    #[test]
    fn check_run_evidence_is_exact_head_actor_bounded_and_deterministic() {
        let head = "a".repeat(40);
        let other = "b".repeat(40);
        let value = serde_json::json!({
            "total_count": 4,
            "check_runs": [
                {"id": 8, "name": "review-gate/codex", "head_sha": head, "status": "completed", "conclusion": "success", "app": {"slug": "untrusted"}},
                {"id": 9, "name": "review-gate/codex", "head_sha": other, "status": "completed", "conclusion": "success", "app": {"slug": "github-actions"}},
                {"id": 10, "name": "review-gate/codex", "head_sha": head, "status": "completed", "conclusion": "failure", "app": {"slug": "github-actions"}},
                {"id": 11, "name": "review-gate/codex", "head_sha": head, "status": "completed", "conclusion": "success", "app": {"slug": "github-actions"}}
            ]
        });
        let evidence =
            parse_check_run_evidence(&value, &head, "review-gate/codex", "github-actions").unwrap();
        assert!(evidence.is_satisfied());
        assert!(matches!(
            evidence,
            ReviewSatisfactionEvidence::GithubCheckRun {
                check_run_id: Some(11),
                ..
            }
        ));

        let truncated = serde_json::json!({
            "total_count": 101,
            "check_runs": []
        });
        assert!(
            parse_check_run_evidence(&truncated, &head, "review-gate/codex", "github-actions")
                .unwrap_err()
                .to_string()
                .contains("truncated")
        );
    }

    #[test]
    fn submissions_and_review_events_follow_exact_provenance() {
        let tmp = fixture(true);
        let mut broker = Broker::open(tmp.path()).unwrap();
        let session = broker.adopt(tmp.path(), None).unwrap();
        let first_head = git(tmp.path(), &["rev-parse", "HEAD"]);
        let registered = broker
            .register_review_lifecycle(session.id, "Acme/Product", &snapshot(&first_head), 10)
            .unwrap();
        assert_eq!(
            registered.lifecycle.state,
            ReviewLifecycleState::DraftOpened
        );
        assert!(registered.changed);
        let duplicate = broker
            .register_review_lifecycle(session.id, "acme/product", &snapshot(&first_head), 11)
            .unwrap();
        assert!(!duplicate.changed);

        let premature = broker
            .request_review(session.id, &snapshot(&first_head), 12)
            .unwrap_err();
        assert!(
            premature
                .to_string()
                .contains("successful broker submission")
        );
        assert!(broker.store().coordinated_operations().unwrap().is_empty());

        let entry = broker
            .store()
            .submit(session.id, &first_head, &first_head)
            .unwrap();
        broker.record_review_submission(&entry).unwrap();
        let verified = broker
            .store()
            .review_lifecycle_for_session(session.id)
            .unwrap()
            .unwrap();
        assert_eq!(
            verified.state,
            ReviewLifecycleState::LocalSubmissionVerified
        );
        assert_eq!(verified.queue_entry_id, Some(entry.id));

        let requested = broker
            .store()
            .transition_review_lifecycle(
                verified.id,
                verified.state,
                ReviewLifecycleState::ReviewRequested,
                verified.queue_entry_id,
                &verified.commit_sha,
                Some("request"),
                None,
                13,
            )
            .unwrap();
        broker
            .apply_review_event(
                &requested.repository,
                requested.pr_number,
                &"a".repeat(40),
                "review_approved",
                "stale",
                14,
            )
            .unwrap();
        assert_eq!(
            broker
                .store()
                .review_lifecycle_for_session(session.id)
                .unwrap()
                .unwrap()
                .state,
            ReviewLifecycleState::ReviewRequested
        );
        broker
            .apply_review_event(
                &requested.repository,
                requested.pr_number,
                &requested.commit_sha,
                "review_approved",
                "approved",
                15,
            )
            .unwrap();
        let satisfied = broker
            .store()
            .review_lifecycle_for_session(session.id)
            .unwrap()
            .unwrap();
        assert_eq!(satisfied.state, ReviewLifecycleState::ReviewSatisfied);
        let mut dismissed = snapshot(&satisfied.commit_sha);
        dismissed.is_draft = false;
        dismissed.review_decision = Some("CHANGES_REQUESTED".into());
        assert!(
            broker
                .unlock_review_validation(session.id, &dismissed, 16)
                .unwrap_err()
                .to_string()
                .contains("approved review decision")
        );
        let mut changed_base = dismissed.clone();
        changed_base.target_branch = "release".into();
        assert!(
            broker
                .unlock_review_validation(session.id, &changed_base, 16)
                .unwrap_err()
                .to_string()
                .contains("does not match")
        );
        let mut closed = dismissed.clone();
        closed.state = "CLOSED".into();
        assert!(
            broker
                .unlock_review_validation(session.id, &closed, 16)
                .unwrap_err()
                .to_string()
                .contains("no longer open")
        );
        broker
            .apply_review_event(
                &requested.repository,
                requested.pr_number,
                &requested.commit_sha,
                "review_changes_requested",
                "disagreement",
                16,
            )
            .unwrap();
        let changes = broker
            .store()
            .review_lifecycle_for_session(session.id)
            .unwrap()
            .unwrap();
        assert_eq!(changes.state, ReviewLifecycleState::ChangesRequested);
        let unchanged_entry = broker
            .store()
            .submit(session.id, &first_head, &first_head)
            .unwrap();
        broker.record_review_submission(&unchanged_entry).unwrap();
        assert_eq!(
            broker
                .store()
                .review_lifecycle_for_session(session.id)
                .unwrap()
                .unwrap()
                .state,
            ReviewLifecycleState::ChangesRequested
        );

        std::fs::write(tmp.path().join("README.md"), "replacement\n").unwrap();
        git(tmp.path(), &["add", "README.md"]);
        git(tmp.path(), &["commit", "-qm", "replacement"]);
        let replacement = git(tmp.path(), &["rev-parse", "HEAD"]);
        let replacement_entry = broker
            .store()
            .submit(session.id, &replacement, &first_head)
            .unwrap();
        broker.record_review_submission(&replacement_entry).unwrap();
        let replacement_state = broker
            .store()
            .review_lifecycle_for_session(session.id)
            .unwrap()
            .unwrap();
        assert_eq!(
            replacement_state.state,
            ReviewLifecycleState::ReplacementCommitSubmitted
        );
        assert_eq!(replacement_state.commit_sha, replacement);
        let mut replacement_snapshot = snapshot(&replacement_state.commit_sha);
        replacement_snapshot.is_draft = false;
        let restarted = broker
            .request_review(session.id, &replacement_snapshot, 17)
            .unwrap();
        assert_eq!(
            restarted.lifecycle.state,
            ReviewLifecycleState::ReviewRequested
        );
        assert!(restarted.operation_id.is_none());
        assert!(broker.store().coordinated_operations().unwrap().is_empty());
    }
}
