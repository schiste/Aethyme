//! Reconciling a local default branch that carries work integration does not.
//!
//! `ship` publishes an exact promoted prefix and refuses to discard anything
//! local that the prefix omits. That refusal is correct, but on its own it
//! leaves ref surgery as the only way forward (issue #143).
//!
//! Ancestry cannot answer whether the local work still matters. The broker
//! promotes by replaying content into a squashed commit, so a commit whose work
//! already landed is usually not an ancestor of anything on integration, and
//! `git cherry` misses the same cases because patch ids do not survive
//! squashing. Representation is therefore decided by content: a commit is
//! already represented when, for every path it touched, integration's blob
//! matches what the commit produced.
//!
//! That test is deliberately narrow. It answers "is this commit's effect
//! present" and nothing else, which is exactly what makes moving the branch
//! safe. Anything it cannot prove is reported as unrepresented and refuses the
//! apply rather than being guessed at.

use sha2::{Digest, Sha256};

use crate::{Broker, BrokerOpError};

pub const MAIN_RECONCILE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MainReconcileDisposition {
    /// Every path the commit touched already has the commit's content on
    /// integration, so moving the branch loses no work.
    AlreadyRepresented,
    /// At least one path differs. The commit's effect is not on integration and
    /// must be replayed through a session before the branch can move.
    Unrepresented,
}

impl MainReconcileDisposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AlreadyRepresented => "already_represented",
            Self::Unrepresented => "unrepresented",
        }
    }
}

/// What an operator decided about a commit the plan could not prove represented.
///
/// Only unrepresented commits need a decision. `AlreadyRepresented` is computed
/// from content and is never chosen, because letting an operator assert it would
/// defeat the check that makes moving the branch safe (issue #143).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MainReconcileResolution {
    /// Replay it through a broker session and submit before reconciling. The
    /// default, and the only disposition that keeps the work in integration.
    ReplayThroughBroker,
    /// Accept that it leaves the default branch. It remains reachable from the
    /// preservation ref, which is created before anything moves.
    ArchiveLocal,
    /// Keep it on the branch and refuse to move at all, so publication stays
    /// blocked until it is dealt with.
    KeepLocalAndBlockPublication,
}

impl MainReconcileResolution {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReplayThroughBroker => "replay_through_broker",
            Self::ArchiveLocal => "archive_local",
            Self::KeepLocalAndBlockPublication => "keep_local_and_block_publication",
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MainReconcileResolutionEntry {
    pub commit: String,
    pub resolution: MainReconcileResolution,
    /// Required: a disposition without a stated reason is not a review.
    pub reason: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MainReconcileResolutionDocument {
    pub schema_version: u32,
    #[serde(default)]
    pub operator: Option<String>,
    pub resolutions: Vec<MainReconcileResolutionEntry>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MainReconcileResolutionTemplate {
    pub schema_version: u32,
    pub operator: Option<String>,
    pub resolutions: Vec<MainReconcileResolutionTemplateEntry>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MainReconcileResolutionTemplateEntry {
    pub commit: String,
    pub subject: String,
    pub resolution: &'static str,
    pub reason: String,
}

/// How an apply moves the local default branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MainReconcileStrategy {
    /// The local branch carries commits integration does not, all proven
    /// represented: it is reset onto integration after a preservation ref
    /// records the pre-move tip.
    ResetOntoIntegration,
    /// The local branch is a strict ancestor of integration and carries
    /// nothing of its own (#374): an exact `merge --ff-only` loses nothing,
    /// so no preservation ref is needed.
    FastForward,
}

impl MainReconcileStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ResetOntoIntegration => "reset_onto_integration",
            Self::FastForward => "fast_forward",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MainReconcileCommit {
    pub commit: String,
    pub subject: String,
    pub disposition: MainReconcileDisposition,
    /// Why the disposition holds, in paths a reader can check by hand.
    pub evidence: String,
    /// Operator decision, present only for unrepresented commits once a
    /// resolution file has been supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<MainReconcileResolution>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MainReconcilePlan {
    pub schema_version: u32,
    pub digest: String,
    pub default_branch: String,
    pub local_ref: String,
    pub local_sha: String,
    pub integration_ref: String,
    pub integration_sha: String,
    /// Uncommitted tracked changes. Any at all refuse the apply: they are work
    /// no plan can classify, and moving the branch under them would lose it.
    pub dirty_tracked_paths: Vec<String>,
    pub commits: Vec<MainReconcileCommit>,
    pub strategy: MainReconcileStrategy,
    /// Ref created before a reset moves the branch, so the pre-move tip is
    /// recoverable even when every commit is represented. A fast-forward
    /// leaves nothing behind and does not create it.
    pub preservation_ref: String,
    pub safe: bool,
    pub refusal: Option<String>,
}

impl MainReconcilePlan {
    pub fn unrepresented(&self) -> impl Iterator<Item = &MainReconcileCommit> {
        self.commits
            .iter()
            .filter(|commit| commit.disposition == MainReconcileDisposition::Unrepresented)
    }

    fn seal(&mut self) -> Result<(), BrokerOpError> {
        // The digest binds the decision, not the observation: which commits were
        // classified how, against which two tips. Incidental repository state
        // that the apply re-proves anyway stays out of it.
        #[derive(serde::Serialize)]
        struct Authorization<'a> {
            schema_version: u32,
            local_sha: &'a str,
            integration_sha: &'a str,
            dirty_tracked_paths: &'a [String],
            commits: Vec<(&'a str, &'static str, Option<&'static str>)>,
            strategy: &'static str,
        }
        let bytes = serde_json::to_vec(&Authorization {
            schema_version: self.schema_version,
            local_sha: &self.local_sha,
            integration_sha: &self.integration_sha,
            dirty_tracked_paths: &self.dirty_tracked_paths,
            commits: self
                .commits
                .iter()
                .map(|commit| {
                    (
                        commit.commit.as_str(),
                        commit.disposition.as_str(),
                        commit.resolution.map(MainReconcileResolution::as_str),
                    )
                })
                .collect(),
            strategy: self.strategy.as_str(),
        })?;
        self.digest = format!("{:x}", Sha256::digest(bytes));
        Ok(())
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct MainReconcileApplyReport {
    pub digest: String,
    pub default_branch: String,
    pub strategy: MainReconcileStrategy,
    /// `None` for a fast-forward, which leaves no commit behind to preserve.
    pub preservation_ref: Option<String>,
    pub moved_from: String,
    pub moved_to: String,
    pub represented_commits: usize,
}

impl Broker {
    /// Read-only classification of everything the local default branch carries
    /// that integration does not.
    pub fn main_reconcile_plan(&mut self) -> Result<MainReconcilePlan, BrokerOpError> {
        self.main_reconcile_plan_with(None)
    }

    /// As [`Self::main_reconcile_plan`], applying an operator resolution file.
    pub fn main_reconcile_plan_with(
        &mut self,
        resolutions: Option<&MainReconcileResolutionDocument>,
    ) -> Result<MainReconcilePlan, BrokerOpError> {
        let (default_branch, local_ref, local_sha) = self.default_branch_tip()?;
        let integration_ref = crate::merge::PromoteConfig::load(&self.main_root_path()).branch;
        let integration_sha =
            self.integration_tip()
                .ok_or_else(|| BrokerOpError::MainReconcileUnavailable {
                    reason: format!("{integration_ref} does not resolve"),
                })?;

        let repo = self.repo_handle();
        let local_only = repo
            .commits_between_oldest(&integration_sha, &local_sha)
            .map_err(|source| BrokerOpError::MainReconcileUnavailable {
                reason: format!("cannot list {default_branch} commits: {source}"),
            })?;

        let mut commits: Vec<MainReconcileCommit> = Vec::new();
        for commit in &local_only {
            commits.push(classify_commit(repo, commit, &integration_sha)?);
        }

        // Tracked only: untracked files survive the move and are unrelated to it,
        // which is the same distinction `ship` draws when it preserves them.
        let dirty_tracked_paths = repo.tracked_dirty_paths().map_err(|source| {
            BrokerOpError::MainReconcileUnavailable {
                reason: format!("cannot inspect the primary checkout: {source}"),
            }
        })?;

        if let Some(document) = resolutions {
            for item in commits
                .iter_mut()
                .filter(|item| item.disposition == MainReconcileDisposition::Unrepresented)
            {
                item.resolution = document
                    .resolutions
                    .iter()
                    .find(|entry| {
                        entry.commit == item.commit || item.commit.starts_with(&entry.commit)
                    })
                    .map(|entry| entry.resolution);
            }
        }

        // Only a commit the operator archived stops blocking. An unresolved one,
        // or one kept deliberately, still refuses -- the point of the file is to
        // record a decision, not to wave the check through.
        let blocking = commits
            .iter()
            .filter(|item| item.disposition == MainReconcileDisposition::Unrepresented)
            .filter(|item| item.resolution != Some(MainReconcileResolution::ArchiveLocal))
            .count();
        let unresolved = commits
            .iter()
            .filter(|item| item.disposition == MainReconcileDisposition::Unrepresented)
            .filter(|item| item.resolution.is_none())
            .count();
        let unrepresented = blocking;
        // Both strategies move whatever HEAD names, so the checkout has to be
        // on the branch the plan classified or the apply would move another.
        let checked_out = repo.symbolic_ref("HEAD");
        let strategy = if local_only.is_empty() {
            MainReconcileStrategy::FastForward
        } else {
            MainReconcileStrategy::ResetOntoIntegration
        };
        let refusal = if checked_out.as_deref() != Some(local_ref.as_str()) {
            Some(format!(
                "the primary checkout is on {}, not {local_ref}; switch it to {default_branch} before reconciling",
                checked_out.as_deref().unwrap_or("a detached HEAD")
            ))
        } else if !dirty_tracked_paths.is_empty() {
            Some(format!(
                "the primary checkout has {} uncommitted tracked path(s); commit them through a broker session or stash them before reconciling",
                dirty_tracked_paths.len()
            ))
        } else if unrepresented > 0 {
            Some(format!(
                "{unrepresented} commit(s) on {default_branch} are not represented on {integration_ref} ({unresolved} with no recorded decision); replay them through a broker session and submit, or record a reviewed disposition with `main reconcile plan --write-resolution-template <path>` and pass it back with --resolution-file"
            ))
        } else if local_sha == integration_sha {
            Some(format!(
                "{default_branch} is already at {integration_ref}; nothing to reconcile"
            ))
        } else if strategy == MainReconcileStrategy::FastForward
            && !repo.is_ancestor(&local_sha, &integration_sha)
        {
            // An empty local-only list should imply ancestry; a shallow or
            // grafted history can break that, and a fast-forward must be proven.
            Some(format!(
                "{default_branch} carries no commits of its own but is not an ancestor of {integration_ref}, so it cannot be fast-forwarded"
            ))
        } else {
            None
        };

        let mut plan = MainReconcilePlan {
            schema_version: MAIN_RECONCILE_SCHEMA_VERSION,
            digest: String::new(),
            default_branch: default_branch.clone(),
            local_ref,
            local_sha: local_sha.clone(),
            integration_ref,
            integration_sha,
            dirty_tracked_paths,
            commits,
            strategy,
            preservation_ref: format!(
                "aethyme/preserve/{default_branch}-{}",
                &local_sha[..12.min(local_sha.len())]
            ),
            safe: refusal.is_none(),
            refusal,
        };
        plan.seal()?;
        Ok(plan)
    }

    /// A template naming every commit that needs a decision, pre-filled with the
    /// safe default so an operator edits rather than composes.
    pub fn main_reconcile_resolution_template(
        &mut self,
    ) -> Result<MainReconcileResolutionTemplate, BrokerOpError> {
        let plan = self.main_reconcile_plan()?;
        Ok(MainReconcileResolutionTemplate {
            schema_version: MAIN_RECONCILE_SCHEMA_VERSION,
            operator: None,
            resolutions: plan
                .unrepresented()
                .map(|commit| MainReconcileResolutionTemplateEntry {
                    commit: commit.commit.clone(),
                    subject: commit.subject.clone(),
                    resolution: MainReconcileResolution::ReplayThroughBroker.as_str(),
                    reason: String::new(),
                })
                .collect(),
        })
    }

    /// Move the local default branch onto integration, after re-proving that
    /// every commit it would leave behind is already represented there.
    pub fn main_reconcile_apply(
        &mut self,
        session_id: i64,
        confirm: &str,
    ) -> Result<MainReconcileApplyReport, BrokerOpError> {
        self.main_reconcile_apply_with(session_id, confirm, None)
    }

    /// As [`Self::main_reconcile_apply`], re-proving the same resolution file the
    /// plan was reviewed with. The digest binds the decisions, so a different
    /// file simply fails to match.
    pub fn main_reconcile_apply_with(
        &mut self,
        session_id: i64,
        confirm: &str,
        resolutions: Option<&MainReconcileResolutionDocument>,
    ) -> Result<MainReconcileApplyReport, BrokerOpError> {
        let plan = self.main_reconcile_plan_with(resolutions)?;
        if !plan.digest.eq_ignore_ascii_case(confirm) {
            return Err(BrokerOpError::MainReconcileConfirmationMismatch {
                actual: confirm.to_owned(),
            });
        }
        if let Some(reason) = plan.refusal.clone() {
            return Err(BrokerOpError::MainReconcileUnsafe { reason });
        }

        let fast_forward = plan.strategy == MainReconcileStrategy::FastForward;
        if !fast_forward {
            // Preserve before moving. The commits being left behind are
            // represented by content, but they are still the only copy of that
            // history.
            let repo = self.repo_handle();
            repo.create_branch_at(&plan.preservation_ref, &plan.local_sha)
                .map_err(|source| BrokerOpError::MainReconcileUnavailable {
                    reason: format!("cannot create {}: {source}", plan.preservation_ref),
                })?;
        }

        let main_root = self.main_root_path();
        let (declared_effect, destructive_confirmed, args) = if fast_forward {
            // `--ff-only` to the exact reviewed SHA: git itself refuses if the
            // branch moved off the ancestor the plan proved, so this can only
            // add commits, never drop one.
            (
                crate::OperationEffect::Write,
                false,
                vec![
                    "merge".into(),
                    "--ff-only".into(),
                    plan.integration_sha.clone(),
                ],
            )
        } else {
            // Declared destructive on purpose, and confirmed only here: the
            // digest proved every commit represented, the preservation ref
            // already exists, and no tracked path is dirty. Those three are
            // exactly the "resolved exact targets" the guard asks for.
            (
                crate::OperationEffect::Destructive,
                true,
                vec![
                    "reset".into(),
                    "--hard".into(),
                    plan.integration_sha.clone(),
                ],
            )
        };
        let moved = self.run_coordinated_operation_at(
            crate::CoordinatedCommand {
                session_id,
                provider: crate::OperationProvider::Git,
                repository: None,
                resolved_target: None,
                scope: Some(format!("main-reconcile:{}", plan.local_ref)),
                declared_effect: Some(declared_effect),
                destructive_confirmed,
                authorization_reason: Some(format!(
                    "reviewed main reconcile {} onto {}",
                    plan.digest, plan.integration_sha
                )),
                args,
            },
            &main_root,
        )?;
        if !moved.ok() {
            return Err(BrokerOpError::MainReconcileUnavailable {
                reason: if fast_forward {
                    format!(
                        "fast-forwarding {} to {} failed; {} is unchanged at {}",
                        plan.default_branch,
                        plan.integration_sha,
                        plan.default_branch,
                        plan.local_sha
                    )
                } else {
                    format!(
                        "moving {} onto {} failed; the pre-move tip is preserved at {}",
                        plan.default_branch, plan.integration_sha, plan.preservation_ref
                    )
                },
            });
        }

        Ok(MainReconcileApplyReport {
            digest: plan.digest,
            default_branch: plan.default_branch,
            strategy: plan.strategy,
            preservation_ref: (!fast_forward).then_some(plan.preservation_ref),
            moved_from: plan.local_sha,
            moved_to: plan.integration_sha,
            represented_commits: plan.commits.len(),
        })
    }
}

/// A commit is represented when integration already holds the content it
/// produced for every path it touched. A deletion is represented when the path
/// is absent there.
fn classify_commit(
    repo: &crate::GitRepo,
    commit: &str,
    integration_sha: &str,
) -> Result<MainReconcileCommit, BrokerOpError> {
    let subject = repo
        .commit_message(commit)
        .map(|message| message.lines().next().unwrap_or_default().to_string())
        .unwrap_or_default();
    let changed = repo.commit_changed_paths(commit).map_err(|source| {
        BrokerOpError::MainReconcileUnavailable {
            reason: format!(
                "cannot inspect {}: {source}",
                &commit[..12.min(commit.len())]
            ),
        }
    })?;

    if changed.is_empty() {
        return Ok(MainReconcileCommit {
            commit: commit.to_string(),
            subject,
            disposition: MainReconcileDisposition::AlreadyRepresented,
            evidence: "commit changes no paths".into(),
            resolution: None,
        });
    }

    for (status, path) in &changed {
        let wanted = if status.starts_with('D') {
            None
        } else {
            repo.blob_at(commit, path)
        };
        let present = repo.blob_at(integration_sha, path);
        if wanted != present {
            return Ok(MainReconcileCommit {
                commit: commit.to_string(),
                subject,
                disposition: MainReconcileDisposition::Unrepresented,
                evidence: format!("{path} differs on the integration tip"),
                resolution: None,
            });
        }
    }

    Ok(MainReconcileCommit {
        commit: commit.to_string(),
        subject,
        disposition: MainReconcileDisposition::AlreadyRepresented,
        evidence: format!("{} path(s) match the integration tip", changed.len()),
        resolution: None,
    })
}
