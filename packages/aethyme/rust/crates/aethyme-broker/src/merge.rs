//! Merge simulation & promotion queue (Phase 5).
//!
//! Flow: `submit` snapshots a session's head into the queue, simulates
//! the merge onto the local integration branch (`git merge-tree`, no
//! worktree mutation), and — when clean — runs the affected gates on the
//! materialized merged tree in a throwaway worktree. Conflicts reject
//! the submission *before any gate runs* and write actionable
//! instructions into the session's worktree (the broker messages the
//! agent — decision 2026-07-10). `promote` advances the integration
//! branch to the verified merge commit; other queued entries whose base
//! moved are re-simulated.
//!
//! Boundary contract: promotion only advances the **local** integration
//! branch; publication is an explicit, confirmed `broker ship` operation.
//! The broker never opens PRs. The promotion trigger is a config setting
//! (`[promote] mode = "auto" | "manual"`) — **auto by
//! default** (decision 2026-07-13, after the first dogfood run: verified
//! means verified; holding it for a human command makes the human the
//! bottleneck). `mode = "manual"` restores explicit `broker promote`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::broker::{Broker, BrokerOpError};
use crate::gates::GateRunOutcome;
use crate::git::GitRepo;
use crate::types::{
    AdvisoryEvidence, AdvisorySeverity, MergeQueueEntry, MergeStatus, NewAdvisory,
    VerificationCandidate,
};

pub const DEFAULT_INTEGRATION_BRANCH: &str = "aethyme/integration";
pub const ACTION_REQUIRED_RELPATH: &str = ".aethyme/broker-action-required.md";
const PROMOTION_SUBJECT_MAX_CHARS: usize = 72;
const VERIFY_ONLY_FALLBACK_REASON: &str = "no fetched default branch to verify against; verified against the \
     integration branch instead (set the main checkout's upstream or fetch it)";

/// Materialize a typed candidate commit without invoking any gate or
/// verification policy. The caller supplies the replay result and provenance.
fn mint_verification_candidate(
    repo: &GitRepo,
    base: &str,
    inputs: Vec<String>,
    tree: &str,
    message: &str,
    attribution: &crate::attribution::Attribution,
    history_parent: Option<&str>,
) -> Result<VerificationCandidate, BrokerOpError> {
    // The optional second parent is the exact, fully owned source DAG. Keep
    // the verified integration base first, and preserve broker attribution
    // and contract decisions on the newly gated commit as on linear replay.
    let parents = match history_parent {
        Some(head) => vec![base, head],
        None => vec![base],
    };
    let commit = repo.commit_tree(tree, &parents, message, attribution)?;
    Ok(VerificationCandidate {
        base: base.to_string(),
        inputs,
        tree: tree.to_string(),
        commit,
    })
}

/// Read the candidate projection written before typed candidate rows existed.
fn legacy_verification_candidate(entry: &MergeQueueEntry) -> Option<VerificationCandidate> {
    let details = entry
        .details_json
        .as_deref()
        .and_then(|details| serde_json::from_str::<serde_json::Value>(details).ok())?;
    let commit = details.get("merge_commit")?.as_str()?.to_string();
    let base = details
        .get("base")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    Some(VerificationCandidate {
        base,
        inputs: Vec::new(),
        tree: entry.merged_tree.clone().unwrap_or_default(),
        commit,
    })
}

pub(crate) fn require_session_checkout_identity(
    session: &crate::Session,
    checkout: &GitRepo,
    expected_head: Option<&str>,
) -> Result<String, BrokerOpError> {
    let actual_branch = checkout.current_branch()?;
    let actual_head = checkout.head_commit()?;
    // On the recorded branch the checkout's HEAD is that branch's tip, so
    // resolve the recorded ref only when they differ. This is the rule status
    // uses, and it keeps a session adopted on a detached checkout (recorded
    // branch `HEAD`, as routed reviews are) from failing `refs/heads/HEAD`.
    let recorded_branch_head = if actual_branch == session.branch {
        Some(actual_head.clone())
    } else {
        checkout.resolve_ref(&format!("refs/heads/{}", session.branch))
    };
    let branch_drifted = actual_branch != session.branch;
    if branch_drifted || expected_head.is_some_and(|expected| expected != actual_head) {
        return Err(BrokerOpError::SessionCheckoutDrift {
            session_id: session.id,
            recorded_branch: session.branch.clone().into_boxed_str(),
            actual_branch: actual_branch.into_boxed_str(),
            expected_head: expected_head.map(|head| head.to_string().into_boxed_str()),
            actual_head: actual_head.into_boxed_str(),
            recorded_branch_head: recorded_branch_head.map(String::into_boxed_str),
        });
    }
    Ok(actual_head)
}

fn promotion_subject(session_id: i64, task: Option<&str>) -> String {
    let prefix = format!("chore(broker): promote session {session_id} (");
    let suffix = ")";
    // A subject is one line: a multi-line task contributes its first line.
    let task = task
        .and_then(|task| task.lines().next())
        .map(str::trim)
        .filter(|task| !task.is_empty())
        .unwrap_or("no task");
    let task_budget =
        PROMOTION_SUBJECT_MAX_CHARS.saturating_sub(prefix.chars().count() + suffix.chars().count());
    let task = if task.chars().count() <= task_budget {
        task.to_string()
    } else {
        // ASCII, so a hook that counts UTF-16 units sees the same length.
        let marker = "...";
        let visible_chars = task_budget.saturating_sub(marker.len());
        let shortened = task.chars().take(visible_chars).collect::<String>();
        format!("{}{marker}", shortened.trim_end())
    };

    format!("{prefix}{task}{suffix}")
}

/// How many merge verifications may run at once in one repository. Each slot
/// is a full checkout, and gates that share a build cache still serialise on
/// its resource lease, so this bounds disk and contention rather than promising
/// that many times the throughput.
const MERGE_VERIFICATION_SLOTS: usize = 4;

/// What a repository does with work that passes verification.
///
/// `VerifyOnly` exists because promotion is not universal. Where a repository
/// ships through pull requests, the integration branch is simultaneously the
/// default base for every session and a destination nothing reads, so it
/// accumulates drift that leaks into new sessions while contributing nothing
/// (#290). Such a repository still wants what `submit` is good at -- cross
/// session conflict detection before a pull request exists, and affected gates
/// rather than a full batch -- without a second copy of the work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PromoteMode {
    /// Verified work moves to the integration branch immediately.
    Auto,
    /// Verified work waits for an explicit `broker promote`.
    Manual,
    /// Nothing is ever promoted. `submit` verifies and reports; the integration
    /// branch is never moved.
    VerifyOnly,
}

impl PromoteMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Manual => "manual",
            Self::VerifyOnly => "verify-only",
        }
    }

    /// Whether any queue entry in this repository can still be promoted.
    /// `finish` and the pending-layer accounting use this to avoid demanding a
    /// promotion the repository has opted out of.
    pub fn promotes_at_all(self) -> bool {
        !matches!(self, Self::VerifyOnly)
    }
}

/// Whether a single `submit` invocation is allowed to promote.
///
/// Separate from [`PromoteMode`] because the repository's policy and one
/// operator's intent are different facts: `--verify-only` narrows a promoting
/// repository for one run, and never widens one that has opted out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotionIntent {
    /// Follow whatever the repository configured.
    Configured,
    /// Verify and report; promote nothing, whatever the repository configured.
    VerifyOnly,
}

/// `[promote]` section of `.aethyme/config.toml`.
#[derive(Debug, Clone)]
pub struct PromoteConfig {
    pub branch: String,
    pub mode: PromoteMode,
}

fn invalid_promote_mode_warning(value: Option<&str>) -> String {
    let detail = value
        .map(|value| format!("unknown value `{value}`"))
        .unwrap_or_else(|| "expected a string".to_string());
    format!(
        "invalid [promote].mode ({detail}); using `verify-only` so it cannot enable promotion. \
         Set it to `auto`, `manual`, `verify-only`, `none`, or `off`."
    )
}

impl PromoteConfig {
    /// Load `[promote]` with the same trust rule as the push policy.
    ///
    /// The configuration committed on the fetched default branch wins when it
    /// exists; the main checkout's working-tree file is the fallback. Reading
    /// only the working tree meant a merged `mode = "verify-only"` did nothing
    /// in any repository whose main checkout had not pulled it, or had a
    /// feature branch checked out -- the policy on the default branch said one
    /// thing and the broker did another. As fresh as the last fetch; never
    /// fetches.
    pub fn load(main_root: &Path) -> Self {
        Self::load_with_warning(main_root).0
    }

    pub(crate) fn load_with_warning(main_root: &Path) -> (Self, Option<String>) {
        Self::from_text(repository_config_text(main_root).as_deref())
    }

    pub(crate) fn from_text(text: Option<&str>) -> (Self, Option<String>) {
        let mut config = Self {
            branch: DEFAULT_INTEGRATION_BRANCH.to_string(),
            mode: PromoteMode::Auto,
        };
        let mut warning = None;
        let Some(text) = text else {
            return (config, warning);
        };
        let Ok(value) = text.parse::<toml::Value>() else {
            return (config, warning);
        };
        if let Some(promote) = value.get("promote") {
            if let Some(branch) = promote.get("branch").and_then(|v| v.as_str()) {
                config.branch = branch.to_string();
            }
            if let Some(mode) = promote.get("mode") {
                // Decision for #691: preserve broker diagnostics while failing
                // closed. An unknown critical mode warns and disables promotion
                // instead of silently restoring the permissive `auto` default.
                config.mode = match mode.as_str() {
                    Some("auto") => PromoteMode::Auto,
                    Some("manual") => PromoteMode::Manual,
                    Some("verify-only" | "none" | "off") => PromoteMode::VerifyOnly,
                    Some(value) => {
                        warning = Some(invalid_promote_mode_warning(Some(value)));
                        PromoteMode::VerifyOnly
                    }
                    None => {
                        warning = Some(invalid_promote_mode_warning(None));
                        PromoteMode::VerifyOnly
                    }
                };
            }
        }
        (config, warning)
    }
}

/// The commit a session's own changes start after.
///
/// Its merge-base with integration, unless its merge-base with the fetched
/// default branch is more recent. That happens exactly when the session was cut
/// from upstream because integration had fallen behind: measured from
/// integration alone, everything upstream merged since would count as the
/// session's own diff and flood the lease and overlap reports.
pub(crate) fn session_baseline(
    checkout: &GitRepo,
    integration: &str,
    upstream: Option<&str>,
) -> Option<String> {
    let from_integration = checkout.merge_base(integration, "HEAD").ok()?;
    if let Some(from_upstream) = upstream.and_then(|tip| checkout.merge_base(tip, "HEAD").ok())
        && from_upstream != from_integration
        && checkout.is_ancestor(&from_integration, &from_upstream)
    {
        return Some(from_upstream);
    }
    Some(from_integration)
}

/// The repository's `.aethyme/config` (TOML or YAML, #1223) under the
/// broker's one trust rule: as committed on the fetched default branch when
/// that exists, else the main checkout's working-tree file. `[promote]` and
/// `[worktrees]` both read it. The text is TOML: a YAML file is validated and
/// rendered from the same value. An ambiguous or invalid file reads as absent
/// here; callers that must report it use [`repository_config_checked`].
pub(crate) fn repository_config_text(main_root: &Path) -> Option<String> {
    repository_config_with_source(main_root).map(|(text, _)| text)
}

/// [`repository_config_text`] and where it came from: `committed` (the
/// fetched default branch) or `working_copy` (the main checkout's file, used
/// when there is no fetched default branch or it does not hold the file).
pub(crate) fn repository_config_with_source(main_root: &Path) -> Option<(String, &'static str)> {
    repository_config_checked(main_root).ok().flatten()
}

/// [`repository_config_with_source`], keeping why the configuration could
/// not be resolved: two format twins, or YAML outside the strict subset.
pub(crate) fn repository_config_checked(
    main_root: &Path,
) -> Result<Option<(String, &'static str)>, aethyme_config::ConfigSourceError> {
    crate::status_reads::configuration(main_root, || {
        if let Some(text) = committed_config_text(main_root)? {
            return Ok(Some((text, "committed")));
        }
        Ok(
            aethyme_config::read_config_text(
                &main_root.join(aethyme_config::CONFIG_DIR),
                "config",
            )?
            .map(|text| {
                text.toml_text()
                    .map(|toml| (toml.into_owned(), "working_copy"))
            })
            .transpose()?,
        )
    })
}

/// `.aethyme/config` as committed on the fetched default branch, if there
/// is one and it holds the file, as TOML text.
fn committed_config_text(
    main_root: &Path,
) -> Result<Option<String>, aethyme_config::ConfigSourceError> {
    // The caller already supplies the main checkout root. Reuse that path
    // without another discovery process; ref and content reads remain fresh.
    let repo = GitRepo::at_known_root(main_root);
    let Some((_, commit)) = repo.upstream_default() else {
        return Ok(None);
    };
    match repo.config_at_commit(&commit, "config") {
        Ok(Some(text)) => Ok(Some(text.toml_text()?.into_owned())),
        Ok(None) => Ok(None),
        Err(crate::GitError::Config(error)) => Err(error),
        Err(_) => Ok(None),
    }
}

/// Result of one submit: the queue entry after simulate (+ gates when
/// clean, + promotion when auto mode and verified).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionGateVerificationStatus {
    NotRun,
    NoConfiguration,
    NoGatesTriggered,
    Passed,
    Failed,
    /// A selected gate could not judge the change because the host could not
    /// run it -- contended, out of disk, or killed. Not a verdict on the diff.
    Deferred,
}

/// What a submission's gates say about the change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GateVerdict {
    Passed,
    /// Every gate that did not pass was stopped by the host, so nothing judged
    /// the change. See `GateRunOutcome::is_host_fault`.
    Deferred,
    Failed,
}

/// Decide a submission from its gate outcomes. Graph integrity is advice and
/// never part of this verdict (#280).
///
/// A genuine failure always wins: deferral requires that *every* gate that
/// did not pass be a host fault, so it can never hide a real one.
fn gate_verdict(outcomes: &[crate::gates::GateRunOutcome]) -> GateVerdict {
    let mut not_passed = outcomes
        .iter()
        .filter(|o| o.status != crate::types::GateStatus::Pass)
        .peekable();
    if not_passed.peek().is_none() {
        GateVerdict::Passed
    } else if not_passed.all(crate::gates::GateRunOutcome::is_host_fault) {
        GateVerdict::Deferred
    } else {
        GateVerdict::Failed
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SubmissionGateVerification {
    pub status: SubmissionGateVerificationStatus,
    pub configured_gates: usize,
    pub selected_gates: usize,
    pub executed_gates: usize,
    pub cached_gates: usize,
}

impl SubmissionGateVerification {
    fn not_run() -> Self {
        Self {
            status: SubmissionGateVerificationStatus::NotRun,
            configured_gates: 0,
            selected_gates: 0,
            executed_gates: 0,
            cached_gates: 0,
        }
    }
}

#[derive(Debug, serde::Serialize)]
pub struct SubmitOutcome {
    pub entry: MergeQueueEntry,
    pub submission_plan: SubmissionPlan,
    pub conflicts: Vec<String>,
    pub conflict_details: Vec<SubmissionConflict>,
    pub gate_outcomes: Vec<GateRunOutcome>,
    /// Native deployment-integrity verification for repositories that
    /// explicitly declare committed graph fragments authoritative.
    pub graph_integrity: Option<crate::GraphIntegrityOutcome>,
    /// Queue status describes promotion eligibility; this field describes
    /// whether gates actually supplied verification evidence.
    pub gate_verification: SubmissionGateVerification,
    /// Why a verified submission was not promoted, when it was not. `None`
    /// when it was promoted, or when it did not pass.
    pub promotion_suppressed: Option<String>,
    /// True when every pending session-owned commit is already represented
    /// by integration or produces no tree change. No gate or promotion runs
    /// for this outcome.
    pub no_changes: bool,
    pub promoted: bool,
    /// Other sessions' leases on this submission's paths that did not block
    /// it, each with the severity and reason it was judged by.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lease_warnings: Vec<crate::LeaseBlocker>,
    /// The commit this submission was simulated and gated against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_against: Option<VerificationBase>,
}

/// What a submission is simulated and gated against.
///
/// A promoting repository verifies against its integration branch, because
/// that is what a verified entry is promoted onto. A verify-only repository
/// promotes nothing and delivers each session through its own pull request,
/// so the only base that predicts the merge is the fetched default branch;
/// integration is not consulted there at all.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct VerificationBase {
    /// `upstream` (the fetched default branch) or `integration`.
    pub source: &'static str,
    /// The ref verified against, e.g. `origin/main` or `aethyme/integration`.
    pub reference: String,
    pub commit: String,
    /// Why a verify-only repository fell back to integration: it has no
    /// fetched default branch to verify against.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
}

/// [`VerificationBase::source`] for the fetched default branch.
pub const VERIFIED_AGAINST_UPSTREAM: &str = "upstream";
/// [`VerificationBase::source`] for the integration branch.
pub const VERIFIED_AGAINST_INTEGRATION: &str = "integration";

/// Whether a commit belongs to the session's recorded work boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionCommitOwnership {
    SessionOwned,
    InheritedFromRecordedBaseline,
    Ambiguous,
}

impl SubmissionCommitOwnership {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionOwned => "session_owned",
            Self::InheritedFromRecordedBaseline => "inherited_from_recorded_baseline",
            Self::Ambiguous => "ambiguous",
        }
    }
}

/// How a commit relates to the current integration history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionIntegrationState {
    Pending,
    AlreadyIntegratedByAncestry,
    AlreadyIntegratedByStablePatchIdentity,
    Ambiguous,
}

/// Full-SHA provenance for one commit carried by a submission.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SubmissionCommitProvenance {
    pub commit: String,
    pub parents: Vec<String>,
    pub ownership: SubmissionCommitOwnership,
    pub integration_state: SubmissionIntegrationState,
    pub patch_id: Option<String>,
    pub matching_integration_commits: Vec<String>,
}

/// Read-only explanation of the history a submit would currently carry.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SubmissionPlan {
    pub session_id: i64,
    pub recorded_baseline: Option<String>,
    pub session_head: String,
    /// Effective commit used as the plan base: integration in promoting
    /// repositories, or the fetched default branch in verify-only mode.
    pub integration_head: String,
    pub safe: bool,
    pub commits: Vec<SubmissionCommitProvenance>,
    /// Repository-relative paths whose final content would differ after
    /// replaying the pending session-owned patches onto integration.
    pub merged_tree_paths: Vec<String>,
    pub warnings: Vec<String>,
}

/// Inputs that determine commit provenance in `build_submission_plan`.
/// Kept beside, rather than inside, the public plan so reusing one within a
/// single submit does not change the serialized plan contract.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SubmissionPlanInputs {
    session_id: i64,
    session_head: String,
    integration_head: String,
    accepted_session_head: Option<String>,
    diff_base: Option<String>,
    adoption_base: Option<String>,
}

impl SubmissionPlanInputs {
    fn new(session: &crate::types::Session, session_head: &str, integration_head: &str) -> Self {
        Self {
            session_id: session.id,
            session_head: session_head.to_string(),
            integration_head: integration_head.to_string(),
            accepted_session_head: session.accepted_session_head.clone(),
            diff_base: session.diff_base.clone(),
            adoption_base: session.adoption_base.clone(),
        }
    }
}

/// A preflight plan that may be reused only by the submit invocation that
/// produced it, and only after all of its inputs are reread unchanged.
#[derive(Debug)]
struct PreparedSubmissionPlan {
    inputs: SubmissionPlanInputs,
    plan: SubmissionPlan,
}

/// A plan that can be reused only while holding the repository lock shared
/// with coordinated local Git writes.
struct PreparedSubmissionReuse {
    prepared: PreparedSubmissionPlan,
    repository_write_lock: crate::operations::RepositoryWriteLock,
}

/// Base references resolved for one queue-entry simulation.
struct SimulationBase {
    base: String,
    pre_refresh: Option<String>,
}

impl PreparedSubmissionPlan {
    fn new(
        session: &crate::types::Session,
        session_head: &str,
        integration_head: &str,
        plan: SubmissionPlan,
    ) -> Self {
        Self {
            inputs: SubmissionPlanInputs::new(session, session_head, integration_head),
            plan,
        }
    }

    fn matches(
        &self,
        entry: &MergeQueueEntry,
        session: &crate::types::Session,
        session_head: &str,
        integration_head: &str,
    ) -> bool {
        entry.session_id == self.inputs.session_id
            && entry.head_commit == self.inputs.session_head
            && entry.base_commit == self.inputs.integration_head
            && self.inputs == SubmissionPlanInputs::new(session, session_head, integration_head)
    }
}

fn build_or_reuse_submission_plan(
    repo: &GitRepo,
    prepared: Option<PreparedSubmissionPlan>,
    entry: &MergeQueueEntry,
    session: &crate::types::Session,
    session_head: &str,
    integration_head: &str,
    build: impl FnOnce() -> Result<SubmissionPlan, BrokerOpError>,
) -> Result<SubmissionPlan, BrokerOpError> {
    if let Some(prepared) = prepared
        && prepared.matches(entry, session, session_head, integration_head)
        // An incomplete or overridden Git history can change the graph view
        // without changing these object IDs. Reuse only in the ordinary,
        // complete history view, checked again after the preflight pass.
        && repo.has_complete_unmodified_history().unwrap_or(false)
    {
        return Ok(prepared.plan);
    }
    build()
}

fn try_submission_plan_reuse_lock(
    main_root: &Path,
    repo: &GitRepo,
    store: &crate::BrokerStore,
) -> Option<crate::operations::RepositoryWriteLock> {
    let (repository, lock) =
        match crate::operations::try_acquire_local_repository_write_lock(main_root, repo) {
            Ok(Some(locked)) => locked,
            Ok(None) => return None,
            Err(error) => {
                crate::warn_unrecorded(
                    "acquire optional submission-plan history lock",
                    Err::<(), _>(error),
                );
                return None;
            }
        };

    // Do not plan under a history view that the coordinated writer lock does
    // not make complete. A shallow boundary, replace ref, or graft keeps the
    // established fresh-plan behavior.
    if repo.has_complete_unmodified_history().unwrap_or(false) {
        match crate::operations::has_unresolved_repository_wide_mutation(store, &repository) {
            Ok(false) => Some(lock),
            Ok(true) => {
                drop(lock);
                None
            }
            Err(error) => {
                drop(lock);
                crate::warn_unrecorded(
                    "inspect unresolved operations before submission-plan reuse",
                    Err::<(), _>(error),
                );
                None
            }
        }
    } else {
        drop(lock);
        None
    }
}

impl SubmissionPlan {
    pub(crate) fn pending_owned_commit_ids(&self) -> Vec<String> {
        self.commits
            .iter()
            .filter(|commit| {
                commit.ownership == SubmissionCommitOwnership::SessionOwned
                    && commit.integration_state == SubmissionIntegrationState::Pending
            })
            .map(|commit| commit.commit.clone())
            .collect()
    }

    pub(crate) fn preservation_commit_ids(&self) -> Vec<String> {
        let pending = self.pending_owned_commit_ids();
        if !pending.is_empty() {
            return pending;
        }
        self.commits
            .iter()
            .filter(|commit| {
                commit.ownership == SubmissionCommitOwnership::Ambiguous
                    || commit.integration_state == SubmissionIntegrationState::Ambiguous
            })
            .map(|commit| commit.commit.clone())
            .collect()
    }

    pub(crate) fn automatic_repair_upstream(&self) -> Result<Option<String>, String> {
        if !self.safe {
            return Err(if self.warnings.is_empty() {
                "submission provenance is ambiguous".into()
            } else {
                self.warnings.join("; ")
            });
        }

        let Some(first_pending) = self.commits.iter().position(|commit| {
            commit.ownership == SubmissionCommitOwnership::SessionOwned
                && commit.integration_state == SubmissionIntegrationState::Pending
        }) else {
            return Ok(None);
        };
        let pending = &self.commits[first_pending..];
        if pending.iter().any(|commit| {
            commit.ownership != SubmissionCommitOwnership::SessionOwned
                || commit.integration_state != SubmissionIntegrationState::Pending
        }) {
            return Err(
                "pending session commits are interleaved with already integrated or inherited commits"
                    .into(),
            );
        }

        let first = &pending[0];
        if first.parents.len() != 1 {
            return Err(format!(
                "pending commit {} has {} parents; automatic repair supports only a linear pending suffix",
                first.commit,
                first.parents.len()
            ));
        }
        for commits in pending.windows(2) {
            if commits[1].parents.len() != 1 || commits[1].parents[0] != commits[0].commit {
                return Err(format!(
                    "pending commit {} does not directly follow {}; automatic repair supports only a linear pending suffix",
                    commits[1].commit, commits[0].commit
                ));
            }
        }
        Ok(first.parents.first().cloned())
    }
}

/// Provenance and recovery guidance for one surviving replay conflict.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SubmissionConflict {
    pub path: String,
    pub originating_commit: String,
    pub ownership: SubmissionCommitOwnership,
    pub integration_side_commits: Vec<String>,
    pub remediation: String,
    pub commands: Vec<String>,
}

pub(crate) struct SubmissionReplay {
    /// A proven source DAG to retain as the verification commit's second parent.
    history_parent: Option<String>,
    pub(crate) tree: String,
    pub(crate) conflicts: Vec<String>,
    pub(crate) conflict_details: Vec<SubmissionConflict>,
}

impl SubmissionReplay {
    pub(crate) fn retains_source_history(&self) -> bool {
        self.history_parent.is_some()
    }
}

impl Broker {
    #[cfg(test)]
    fn observe_submission_plan_test_phase(&self, phase: crate::broker::SubmissionPlanTestPhase) {
        if let Some(observer) = &self.submission_plan_test_observer {
            observer(phase);
        }
    }

    /// Build the exact plan used by submission before creating a queue entry
    /// or running gates. Verify-only repositories use the same fetched-default
    /// base as submit, while the plan remains read-only when falling back to
    /// the integration branch. Replaying patches can create unreachable Git
    /// objects, but this operation changes neither refs nor worktrees.
    pub fn submission_plan(&mut self, session_id: i64) -> Result<SubmissionPlan, BrokerOpError> {
        let session = self.store().session(session_id)?;
        let checkout = GitRepo::discover(Path::new(&session.worktree_path))?;
        let session_head = checkout.head_commit()?;
        let config = PromoteConfig::load(&self.main_root_path());
        let (integration_reference, integration_head) = self.integration_head_snapshot()?;
        let base = if config.mode == PromoteMode::VerifyOnly {
            self.upstream_verification_base()
                .unwrap_or(VerificationBase {
                    source: VERIFIED_AGAINST_INTEGRATION,
                    reference: integration_reference,
                    commit: integration_head,
                    fallback_reason: Some(VERIFY_ONLY_FALLBACK_REASON.to_string()),
                })
        } else {
            VerificationBase {
                source: VERIFIED_AGAINST_INTEGRATION,
                reference: integration_reference,
                commit: integration_head,
                fallback_reason: None,
            }
        };
        let mut plan = self.build_submission_plan(&session, &session_head, &base.commit)?;
        let replay = self.replay_submission_plan(&plan)?;
        if replay.conflicts.is_empty() {
            plan.merged_tree_paths = self
                .repo_handle()
                .changed_between(&base.commit, &replay.tree)?;
        }
        Ok(plan)
    }

    /// Ensure the integration branch exists (created from the main
    /// checkout's HEAD with an explanatory event — issue #20) and return
    /// its current commit.
    pub fn integration_head(&mut self) -> Result<(String, String), BrokerOpError> {
        let config = PromoteConfig::load(&self.main_root_path());
        let repo = self.repo_handle();
        if let Some(commit) = repo.resolve_ref(&config.branch) {
            // Issue #40: the integration branch follows main. Fast-forward
            // it when it is an ancestor of the main checkout's HEAD — i.e.
            // everything it holds is already in main, so nothing promoted
            // can be lost. When it holds unmerged promotions (not
            // reachable from HEAD), it is left strictly alone.
            let head = repo.head_commit()?;
            if commit != head && repo.is_ancestor(&commit, &head) {
                repo.update_branch_ref(&config.branch, &head)?;
                let payload =
                    crate::events::integration_refreshed_payload(&config.branch, &commit, &head);
                self.store().append_event(
                    crate::events::MERGE_INTEGRATION_REFRESHED,
                    None,
                    Some(&payload),
                )?;
                return Ok((config.branch, head));
            }
            return Ok((config.branch, commit));
        }
        let head = repo.head_commit()?;
        repo.update_branch_ref(&config.branch, &head)?;
        self.store().append_event(
            crate::events::MERGE_INTEGRATION_BRANCH_CREATED,
            None,
            Some(&crate::events::integration_branch_created_payload(
                &config.branch,
                &head,
            )),
        )?;
        Ok((config.branch, head))
    }

    /// The base a submission is simulated and gated against.
    ///
    /// Verify-only repositories verify against the fetched default branch and
    /// never touch the integration branch: nothing is promoted there, and a
    /// pull request merged on the provider leaves integration behind, so it
    /// would verify the change against an old copy of the default branch.
    /// Without a fetched default branch they fall back to integration, and the
    /// outcome says why. Promoting repositories keep integration, which is
    /// what a verified entry is promoted onto. Never fetches.
    pub(crate) fn submission_base(&mut self) -> Result<VerificationBase, BrokerOpError> {
        let config = PromoteConfig::load(&self.main_root_path());
        if config.mode == PromoteMode::VerifyOnly
            && let Some(base) = self.upstream_verification_base()
        {
            return Ok(base);
        }
        let (reference, commit) = self.integration_head()?;
        Ok(VerificationBase {
            source: VERIFIED_AGAINST_INTEGRATION,
            reference,
            commit,
            fallback_reason: (config.mode == PromoteMode::VerifyOnly)
                .then(|| VERIFY_ONLY_FALLBACK_REASON.to_string()),
        })
    }

    fn upstream_verification_base(&self) -> Option<VerificationBase> {
        let (reference, commit) = self.repo_handle().upstream_default()?;
        Some(VerificationBase {
            source: VERIFIED_AGAINST_UPSTREAM,
            reference,
            commit,
            fallback_reason: None,
        })
    }

    /// Submit a session's committed head for promotion: queue (idempotent
    /// per head), simulate, gate on the merged tree, and auto-promote if
    /// configured. Uncommitted changes are NOT included — the head commit
    /// is the unit of promotion.
    pub fn submit(&mut self, session_id: i64) -> Result<SubmitOutcome, BrokerOpError> {
        self.submit_with_policy(session_id, crate::gates::CachePolicy::Use)
    }

    /// Submit, declaring whether this invocation may promote at all.
    pub fn submit_with_intent(
        &mut self,
        session_id: i64,
        cache_policy: crate::gates::CachePolicy,
        intent: PromotionIntent,
    ) -> Result<SubmitOutcome, BrokerOpError> {
        self.submit_inner(session_id, cache_policy, intent, None)
    }

    /// Submit only if the session head is still `expected_head` at the point
    /// submit pins it; otherwise refuse with
    /// [`BrokerOpError::CapturedHeadMoved`] before any queue entry exists.
    /// Everything after that point (simulation, gates, promotion) works from
    /// the pinned queue entry, so the submitted commit is exactly the one the
    /// caller vouched for (#660).
    pub fn submit_expecting_head(
        &mut self,
        session_id: i64,
        cache_policy: crate::gates::CachePolicy,
        intent: PromotionIntent,
        expected_head: &str,
    ) -> Result<SubmitOutcome, BrokerOpError> {
        self.submit_inner(session_id, cache_policy, intent, Some(expected_head))
    }

    /// Submit with an explicit policy for merged-tree gate cache lookup.
    pub fn submit_with_policy(
        &mut self,
        session_id: i64,
        cache_policy: crate::gates::CachePolicy,
    ) -> Result<SubmitOutcome, BrokerOpError> {
        self.submit_inner(session_id, cache_policy, PromotionIntent::Configured, None)
    }

    fn submit_inner(
        &mut self,
        session_id: i64,
        cache_policy: crate::gates::CachePolicy,
        intent: PromotionIntent,
        expected_head: Option<&str>,
    ) -> Result<SubmitOutcome, BrokerOpError> {
        // Refuse legacy and incomplete bindings before submission planning.
        // A fresh database may proceed, but its root is not created until the
        // non-root preflight has passed below.
        self.check_submission_root_before_planning()?;

        // Recorded from the first step: lease auditing and planning can take
        // minutes on a busy repository, and a silent submit looks hung.
        let progress = crate::submit_progress::SubmitProgress::start(
            &self.main_root_path(),
            session_id,
            crate::gates::heartbeat_interval(),
        );
        let session = self.store().session(session_id)?;
        let checkout = GitRepo::discover(Path::new(&session.worktree_path))?;
        let head = require_session_checkout_identity(&session, &checkout, None)?;
        // Compare-and-swap against the caller's commit at the point the head
        // is pinned; the identity re-check below keeps it pinned until the
        // queue entry records it.
        if let Some(expected) = expected_head
            && expected != head
        {
            return Err(BrokerOpError::CapturedHeadMoved {
                session_id,
                captured: expected.into(),
                actual: head.into_boxed_str(),
            });
        }
        // Planning uses the same policy-aware base as submit: fetched upstream
        // in verify-only mode, integration otherwise. This prevents upstream-only
        // commits from being counted as session work when integration is stale (#352).
        self.refresh_disposable_integration(crate::IntegrationRefreshTrigger::Submit);
        progress.phase("auditing lease ownership");
        let ownership = self.audit_submit_ownership(session_id)?;
        if !ownership.ok {
            return Err(BrokerOpError::OwnershipViolation {
                summary: ownership.failure_summary(),
                report: Box::new(ownership),
            });
        }
        let base = self.submission_base()?.commit;
        // The base's gates judge this submission. Refuse an untrusted policy
        // before any queue row exists, so the refusal leaves no residue.
        self.require_trusted_policy_at_commit(&base, Some(session_id))?;

        // Provenance and replay planning are deterministic and ref-free. Run
        // them before creating a live queue row so a fail-closed ownership
        // decision cannot leave behind misleading `submitted` residue.
        // Keep the plan only as submit-invocation state: simulation rereads
        // the current session, base, and history inputs, reusing this plan only
        // when they still match and rebuilding it otherwise.
        // A coordinated `git replace` write can change provenance without
        // changing captured commit IDs. Reuse is optional: when the shared
        // repository lock is busy or unavailable, keep the ordinary fresh
        // planning path instead.
        progress.phase("planning the submission");
        let reuse_lock = try_submission_plan_reuse_lock(
            &self.main_root_path(),
            self.repo_handle(),
            self.store_ref(),
        );
        let plan = self
            .build_submission_plan(&session, &head, &base)
            .and_then(|plan| self.replay_submission_plan(&plan).map(|_| plan));
        let plan = match plan {
            Ok(plan) => plan,
            Err(error) => {
                drop(reuse_lock);
                self.store().append_event(
                    crate::events::MERGE_SUBMISSION_PLANNING_FAILED,
                    Some(session_id),
                    Some(&crate::events::merge_submission_planning_failed_payload(
                        &head,
                        submission_planning_failure_class(&error),
                    )),
                )?;
                return Err(error);
            }
        };
        let prepared_plan = reuse_lock.map(|repository_write_lock| PreparedSubmissionReuse {
            prepared: PreparedSubmissionPlan::new(&session, &head, &base, plan),
            repository_write_lock,
        });
        #[cfg(test)]
        self.observe_submission_plan_test_phase(
            crate::broker::SubmissionPlanTestPhase::PreflightPlanReady,
        );

        // Planning can be expensive. Refuse if the checkout changed identity
        // while it ran so a branch switch or new head cannot redirect the
        // queued submission away from the session snapshot.
        require_session_checkout_identity(&session, &checkout, Some(&head))?;

        // Root creation is an explicit submit side effect. Keep it after the
        // ownership, policy, provenance, replay, and checkout checks, and
        // before the queue row records this submission.
        let lifecycle_claim = self.claim_worktree_lifecycle()?;
        self.prepare_worktree_root_for_submission()?;
        let entry = self.store().submit(session_id, &head, &base)?;
        drop(lifecycle_claim);
        // A new head supersedes this session's older in-flight entries:
        // without this, a stale conflicted entry would be re-simulated
        // (and re-announced) on every future promotion.
        let stale: Vec<i64> = self
            .store()
            .merge_queue()?
            .into_iter()
            .filter(|e| {
                e.session_id == session_id
                    && e.id != entry.id
                    && matches!(
                        e.status,
                        MergeStatus::Submitted
                            | MergeStatus::Simulating
                            | MergeStatus::Verified
                            | MergeStatus::Conflict
                            | MergeStatus::Rejected
                    )
            })
            .map(|e| e.id)
            .collect();
        for stale_id in stale {
            self.store()
                .set_merge_status(stale_id, MergeStatus::Superseded, None, None)?;
        }
        let mut outcome = self.simulate_and_gate_with_policy(
            entry.id,
            cache_policy,
            intent,
            Some(&progress),
            prepared_plan,
        )?;
        outcome.lease_warnings = ownership.warned_leases;
        Ok(outcome)
    }

    /// Whether `commit` is a promotion this session produced that no promoted
    /// queue entry claims.
    ///
    /// Both halves matter. The message link proves the commit is this session's
    /// work rather than someone else's landing first, and the unclaimed check
    /// keeps a normal already-landed submission -- where another entry legitimately
    /// owns the tip -- reporting content-empty as it should.
    fn integration_tip_is_unclaimed_promotion_for(
        &mut self,
        session_id: i64,
        commit: &str,
    ) -> Result<bool, BrokerOpError> {
        let Ok(message) = self.repo_handle().commit_message(commit) else {
            return Ok(false);
        };
        if !message.contains(&format!("promote session {session_id} ")) {
            return Ok(false);
        }
        let claimed = self.store().merge_queue()?.into_iter().any(|entry| {
            entry.status == MergeStatus::Promoted
                && entry
                    .details_json
                    .as_deref()
                    .and_then(|details| serde_json::from_str::<serde_json::Value>(details).ok())
                    .and_then(|details| {
                        details
                            .get("commit")
                            .and_then(|value| value.as_str().map(str::to_string))
                    })
                    .is_some_and(|claimed| claimed == commit)
        });
        Ok(!claimed)
    }

    /// Simulate (and, when clean, gate) one queue entry against the
    /// CURRENT integration head. Rebinds the entry's base if the branch
    /// moved since submission.
    pub fn simulate_and_gate(&mut self, entry_id: i64) -> Result<SubmitOutcome, BrokerOpError> {
        self.simulate_and_gate_with_policy(
            entry_id,
            crate::gates::CachePolicy::Use,
            PromotionIntent::Configured,
            None,
            None,
        )
    }

    fn simulate_and_gate_with_policy(
        &mut self,
        entry_id: i64,
        cache_policy: crate::gates::CachePolicy,
        intent: PromotionIntent,
        progress: Option<&crate::submit_progress::SubmitProgress>,
        prepared_plan: Option<PreparedSubmissionReuse>,
    ) -> Result<SubmitOutcome, BrokerOpError> {
        // #42: remember where the integration branch stood BEFORE the
        // follows-main refresh. Gate selection must diff against this —
        // for a main-checkout session the refresh fast-forwards the base
        // onto the session's own commits, and diffing against the
        // refreshed base yields an empty set (the observed `gates:[]`
        // vacuous verify). Diffing against the pre-refresh head selects
        // gates for the work actually being accepted; for worktree
        // sessions the two are equal (or the pre-refresh diff is a safe
        // superset when integration lagged main).
        let pre_refresh = {
            let branch = PromoteConfig::load(&self.main_root_path()).branch;
            self.repo_handle()
                .resolve_ref(&format!("refs/heads/{branch}"))
        };
        let verification = self.submission_base()?;
        // The follows-main refresh only concerns integration. Verified against
        // the default branch, the base is the base: nothing was refreshed.
        let pre_refresh =
            pre_refresh.filter(|_| verification.source == VERIFIED_AGAINST_INTEGRATION);
        let base = verification.commit.clone();
        let mut outcome = self.simulate_and_gate_against(
            entry_id,
            cache_policy,
            intent,
            SimulationBase { base, pre_refresh },
            progress,
            prepared_plan,
        )?;
        outcome.verified_against = Some(verification);
        Ok(outcome)
    }

    fn simulate_and_gate_against(
        &mut self,
        entry_id: i64,
        cache_policy: crate::gates::CachePolicy,
        intent: PromotionIntent,
        simulation_base: SimulationBase,
        progress: Option<&crate::submit_progress::SubmitProgress>,
        prepared_plan: Option<PreparedSubmissionReuse>,
    ) -> Result<SubmitOutcome, BrokerOpError> {
        let SimulationBase { base, pre_refresh } = simulation_base;
        let entry = self
            .store()
            .merge_queue()?
            .into_iter()
            .find(|e| e.id == entry_id)
            .ok_or(crate::BrokerError::SessionNotFound(entry_id))?;
        let session = self.store().session(entry.session_id)?;
        // Re-simulation reaches here without submit (queue drain after another
        // promotion), and the base may have moved onto a new policy. Checked
        // before the entry changes state, so a refusal leaves it as it was.
        self.require_trusted_policy_at_commit(&base, Some(entry.session_id))?;
        let (prepared_plan, repository_write_lock) = match prepared_plan {
            Some(reuse) => (Some(reuse.prepared), Some(reuse.repository_write_lock)),
            None => (None, None),
        };
        #[cfg(test)]
        self.observe_submission_plan_test_phase(
            crate::broker::SubmissionPlanTestPhase::ConsumerStarting,
        );
        let mut submission_plan = build_or_reuse_submission_plan(
            self.repo_handle(),
            prepared_plan,
            &entry,
            &session,
            &entry.head_commit,
            &base,
            || self.build_submission_plan(&session, &entry.head_commit, &base),
        )?;

        if let Some(progress) = progress {
            progress.phase("simulating the merge");
        }
        let simulation = self.replay_submission_plan(&submission_plan)?;
        if simulation.conflicts.is_empty() {
            submission_plan.merged_tree_paths = self
                .repo_handle()
                .changed_between(&base, &simulation.tree)?;
        }
        let base_tree = self.repo_handle().commit_tree_id(&base)?;
        #[cfg(test)]
        self.observe_submission_plan_test_phase(
            crate::broker::SubmissionPlanTestPhase::BaseTreeCaptured,
        );
        drop(repository_write_lock);
        let main_checkout_session = Path::new(&session.worktree_path) == self.main_root_path();
        self.store()
            .set_merge_status(entry.id, MergeStatus::Simulating, None, None)?;

        if !simulation.conflicts.is_empty() {
            // Conflict: reject before any gate runs, name the blocking
            // session when a lease matches, and message the agent via a
            // file drop in its worktree (#21).
            let blocking = self.blocking_sessions(entry.session_id, &simulation.conflicts)?;
            let details = serde_json::json!({
                "conflicts": simulation.conflicts,
                "conflict_details": simulation.conflict_details,
                "blocking_sessions": blocking,
                "base": base,
            });
            self.store().set_merge_status(
                entry.id,
                MergeStatus::Conflict,
                Some(&simulation.tree),
                Some(&details.to_string()),
            )?;
            write_action_required(
                Path::new(&session.worktree_path),
                &entry,
                &simulation.conflict_details,
                &blocking,
                &base,
            );
            let entry = self.queue_entry(entry.id)?;
            return Ok(SubmitOutcome {
                entry,
                submission_plan,
                conflicts: simulation.conflicts,
                conflict_details: simulation.conflict_details,
                gate_outcomes: Vec::new(),
                graph_integrity: None,
                gate_verification: SubmissionGateVerification::not_run(),
                no_changes: false,
                lease_warnings: Vec::new(),
                verified_against: None,
                promoted: false,
                promotion_suppressed: None,
            });
        }

        // A clean replay that leaves integration's tree unchanged has
        // nothing to verify or promote. This is a normal terminal outcome
        // when every owned patch already landed (or is content-empty), and
        // a vital safety signal when a corrupted ownership baseline hid real
        // work: never manufacture a successful parent-tree promotion.
        // Primary-checkout sessions are the deliberate exception: the
        // follows-main refresh has already advanced integration to their
        // HEAD, and submit still needs to gate and record that externally
        // visible main movement.
        if simulation.tree == base_tree && !main_checkout_session {
            let details = serde_json::json!({
                "base": base,
                "reason": "submission produces no content change",
                "pending_session_owned_commits": submission_plan.pending_owned_commit_ids(),
            });
            // Ask why it is empty before recording it as nothing. If this very
            // session already produced the integration tip and no promoted entry
            // claims it, a previous attempt promoted and lost its response:
            // superseding here would strand that commit as unrecorded and refuse
            // publication (issue #135). Claim it instead, which is idempotent.
            if self.integration_tip_is_unclaimed_promotion_for(session.id, &base)? {
                let recovered = serde_json::json!({
                    "commit": base,
                    "reason": "retry claimed an unrecorded promotion this session produced",
                });
                let promotion_parent = self.repo_handle().first_parent(&base)?;
                let promoted_paths = self
                    .repo_handle()
                    .changed_between(&promotion_parent, &base)?;
                let integration_ref = PromoteConfig::load(&self.main_root_path()).branch;
                self.store().record_recovered_promotion(
                    entry.id,
                    &base,
                    &simulation.tree,
                    &integration_ref,
                    &promoted_paths,
                    &recovered.to_string(),
                )?;
                clear_action_required(Path::new(&session.worktree_path));
                return Ok(SubmitOutcome {
                    entry: self.queue_entry(entry.id)?,
                    submission_plan,
                    conflicts: Vec::new(),
                    conflict_details: Vec::new(),
                    gate_outcomes: Vec::new(),
                    graph_integrity: None,
                    gate_verification: SubmissionGateVerification::not_run(),
                    no_changes: true,
                    lease_warnings: Vec::new(),
                    verified_against: None,
                    promoted: true,
                    promotion_suppressed: None,
                });
            }
            self.store().record_content_empty_supersession(
                entry.id,
                &base,
                &simulation.tree,
                &details.to_string(),
            )?;
            clear_action_required(Path::new(&session.worktree_path));
            return Ok(SubmitOutcome {
                entry: self.queue_entry(entry.id)?,
                submission_plan,
                conflicts: Vec::new(),
                conflict_details: Vec::new(),
                gate_outcomes: Vec::new(),
                graph_integrity: None,
                gate_verification: SubmissionGateVerification::not_run(),
                no_changes: true,
                lease_warnings: Vec::new(),
                verified_against: None,
                promoted: false,
                promotion_suppressed: None,
            });
        }

        // Clean merge: materialize as a commit and run affected gates in the
        // stable, locked verification slot. The checkout is still disposable,
        // but its source path remains constant so build tools can reuse safe
        // path-sensitive fingerprints across queue entries.
        // Credit everyone in the chain: the human authors, the broker commits,
        // and the agent that did the work is named in a trailer.
        //
        // The session row is the ONLY source here. Promotion can run from a
        // different process than the agent's (another session's submit, or a
        // queue drain), so AETHYME_AGENT in this process would credit whoever
        // happened to trigger the merge. `start`/`adopt` resolve the env
        // fallback once, while it is still the right agent's environment.
        let agent = session
            .agent_identity
            .as_deref()
            .and_then(crate::attribution::Identity::parse);
        let attribution = crate::attribution::for_promote(&self.main_root_path(), agent);
        // The subject is a conventional commit so that
        // `aethyme repo lint-commit-message` accepts a broker promotion.
        // `chore` is honest: the substantive change is the session's own
        // commits, which the merge carries, not this integration commit.
        let mut verification_message = format!(
            "{}{}",
            promotion_subject(session.id, session.task.as_deref()),
            attribution.trailer_block()
        );
        let inputs = submission_plan.pending_owned_commit_ids();
        let pending_messages = inputs
            .iter()
            .map(|commit| self.repo_handle().commit_message(commit))
            .collect::<Result<Vec<_>, _>>()?
            .join("\n");
        if let Some(decision) = crate::contract_check::parse_contract_decision(&pending_messages) {
            verification_message.push_str("\n\nContract decision: ");
            verification_message.push_str(decision.label());
            // The contract gate reads this commit, not the session's. Dropping
            // the justification made a justified `none` unpassable here.
            if let Some(reason) =
                crate::contract_check::parse_contract_justification(&pending_messages)
            {
                verification_message.push_str("\nContract justification: ");
                verification_message.push_str(&reason);
            }
        }
        let candidate = mint_verification_candidate(
            self.repo_handle(),
            &base,
            inputs,
            &simulation.tree,
            &verification_message,
            &attribution,
            simulation.history_parent.as_deref(),
        )?;
        let merge_commit = candidate.commit.as_str();
        let verify_base = pre_refresh.as_deref().unwrap_or(&base);
        let changed = self
            .repo_handle()
            .gate_scope_changed_between(verify_base, merge_commit)?;
        // One of a small pool of stable slots, not one repository-wide slot:
        // two sessions verifying different merged trees have no reason to
        // exclude each other, and a single slot made verification serial no
        // matter how many sessions were submitting. Promotion stays safe
        // because `promote` advances the ref only from the base it verified.
        //
        // The two other slot users (`graph-integrity`, `gate-doctor-probe`)
        // keep the repository-scoped slot: each verifies a single tree it has
        // just chosen, and serialising them against each other is correct --
        // they are diagnostics, not throughput.
        if let Some(progress) = progress {
            progress.phase(crate::submit_progress::PHASE_WAITING_FOR_SLOT);
        }
        let main_root = self.main_root_path();
        let worktree_root_plan = self.worktree_root_plan()?;
        let mut verification_slot =
            crate::verification::ExactTreeVerificationSlot::acquire_pooled_reporting_with_worktree_root_plan(
                &main_root,
                "merge-sim",
                MERGE_VERIFICATION_SLOTS,
                &worktree_root_plan,
                crate::gates::heartbeat_interval(),
                &mut |waited| {
                    if let Some(progress) = progress {
                        progress.report_slot_wait(&main_root, waited);
                    }
                },
            )?;
        if let Some(progress) = progress {
            progress.phase(crate::submit_progress::PHASE_VERIFYING);
        }
        let sim_worktree = verification_slot.materialize(self.repo_handle(), merge_commit)?;
        // Verification policy comes from the base the change lands on, never
        // from the merged tree: otherwise a session could weaken or delete the
        // gates and graph policy that judge it. A policy change a session makes
        // applies to the submissions after it lands.
        let graph_policy =
            crate::graph_integrity::load_graph_policy_at_commit(self.repo_handle(), &base)?;
        let graph_policy_changed =
            crate::graph_integrity::load_graph_policy_at_commit(self.repo_handle(), merge_commit)
                .map_or(true, |merged| merged != graph_policy);
        let graph_integrity =
            crate::graph_integrity::verify_disposable_checkout(&sim_worktree, &graph_policy);
        if graph_integrity.enforced {
            self.store().append_event(
                crate::events::GRAPH_INTEGRITY_CHECKED,
                Some(entry.session_id),
                Some(&crate::events::graph_integrity_checked_payload(
                    &graph_integrity,
                )),
            )?;
        }
        // Conflict-only brokering is valid: a repo with no gates.toml gets
        // textual merge simulation and promotion on clean merges, with zero
        // verification — recorded explicitly so nobody mistakes it for a
        // passing check run. A *malformed* gates.toml at the base stays a
        // hard error (broken intent, not absent intent).
        let (gates, gate_configuration_present) = match self.load_and_sync_gates_at_commit(&base)? {
            Some(gates) => (gates, true),
            None => (Vec::new(), false),
        };
        let gate_policy_changed = self.repo_handle().config_toml_at_commit(&base, "gates")?
            != self
                .repo_handle()
                .config_toml_at_commit(merge_commit, "gates")?;
        if gate_policy_changed || graph_policy_changed {
            self.store().append_event(
                crate::events::MERGE_POLICY_DEFERRED,
                Some(entry.session_id),
                Some(&crate::events::merge_policy_deferred_payload(
                    &base,
                    gate_policy_changed,
                    graph_policy_changed,
                )),
            )?;
        }
        let configured_gates = gates.len();
        // A stale or unverifiable graph is recorded above and reported with
        // the submission; it never stops the gates or the promotion (#280).
        let storage = self.storage_context().gate_storage_locations();
        let gate_outcomes = crate::gates::run_affected_for_submit_in(
            self.store(),
            &main_root,
            &sim_worktree,
            &gates,
            &changed,
            Some(entry.session_id),
            cache_policy,
            progress,
            storage,
        );
        verification_slot.cleanup();
        // Promotion may re-simulate another in-flight entry, and a stale
        // verification may recursively re-simulate this entry. Both paths
        // acquire the same stable slot. Release the non-reentrant flock
        // before either path is reachable or one process deadlocks itself.
        drop(verification_slot);
        if let Some(progress) = progress {
            progress.phase("recording the verdict");
        }
        let gate_outcomes = gate_outcomes?;

        // A gate the host stopped from judging the change is not a verdict on
        // the change. Collapsing it into `Rejected` tells the agent its correct
        // work is broken, which is untrue and teaches agents to stop reading
        // rejections. Measured over 30 days: 34% of all gate failures on this
        // repository were host faults, and 40% of submits failed.
        //
        // The submission is deferred only when *every* gate that did not pass
        // is a host fault the broker observed itself (see
        // `GateRunOutcome::is_host_fault`). A genuine failure anywhere is still a
        // rejection, so a deferral can never hide one. The finding is still
        // printed and recorded in the entry's details -- `Deferred`, not
        // silent success.
        let verdict = gate_verdict(&gate_outcomes);
        let deferred = verdict == GateVerdict::Deferred;
        let all_pass = verdict == GateVerdict::Passed;
        let gate_verification = SubmissionGateVerification {
            status: if !gate_configuration_present {
                SubmissionGateVerificationStatus::NoConfiguration
            } else if gate_outcomes.is_empty() {
                SubmissionGateVerificationStatus::NoGatesTriggered
            } else if deferred {
                SubmissionGateVerificationStatus::Deferred
            } else if all_pass {
                SubmissionGateVerificationStatus::Passed
            } else {
                SubmissionGateVerificationStatus::Failed
            },
            configured_gates,
            selected_gates: gate_outcomes.len(),
            executed_gates: gate_outcomes
                .iter()
                .filter(|outcome| !outcome.cached)
                .count(),
            cached_gates: gate_outcomes
                .iter()
                .filter(|outcome| outcome.cached)
                .count(),
        };
        let mut details = serde_json::json!({
            "merge_commit": candidate.commit.as_str(),
            "base": candidate.base.as_str(),
            "gates": gate_outcomes.iter().map(|o| {
                let mut gate = serde_json::json!({
                    "gate": o.gate,
                    "tree_hash": o.tree_hash,
                    "status": o.status,
                    "failure_class": o.failure_class,
                    "cached": o.cached,
                });
                // Only when true, like `GateRunOutcome::host_fault`: these
                // details become the `merge.verified` payload, whose gate
                // fields are frozen (contract_v1), and a verified entry never
                // has a host fault.
                if o.is_host_fault() {
                    gate["host_fault"] = serde_json::Value::Bool(true);
                }
                gate
            }).collect::<Vec<_>>(),
        });
        if deferred {
            // The explicit marker is what `status` and `finish` read to tell a
            // deferred entry from one still in flight; both are `Submitted`.
            details["deferred"] = serde_json::Value::Bool(true);
            // The host could not judge the change. Leave the entry submitted so
            // a later `submit` -- or the re-simulation that follows another
            // entry's promotion -- runs it again, and say exactly which gate
            // could not run: the next action is to free the resource, not to
            // edit code that has not been shown to be wrong.
            self.store().set_merge_status_with_candidate(
                entry.id,
                MergeStatus::Submitted,
                Some(candidate.tree.as_str()),
                Some(&details.to_string()),
                &candidate,
            )?;
        } else if all_pass {
            self.store().set_merge_status_with_candidate(
                entry.id,
                MergeStatus::Verified,
                Some(candidate.tree.as_str()),
                Some(&details.to_string()),
                &candidate,
            )?;
            let verified_entry = self.queue_entry(entry.id)?;
            self.record_review_submission(&verified_entry)?;
        } else {
            self.store().set_merge_status_with_candidate(
                entry.id,
                MergeStatus::Rejected,
                Some(candidate.tree.as_str()),
                Some(&details.to_string()),
                &candidate,
            )?;
        }

        let (promote_config, promotion_config_warning) =
            PromoteConfig::load_with_warning(&self.main_root_path());
        let configured = promote_config.mode;
        let effective = match intent {
            PromotionIntent::Configured => configured,
            PromotionIntent::VerifyOnly => PromoteMode::VerifyOnly,
        };
        let mut promoted = false;
        let mut promotion_suppressed = promotion_config_warning.clone();
        if all_pass {
            match effective {
                PromoteMode::Auto => {
                    self.promote(entry.id)?;
                    promoted = true;
                }
                PromoteMode::Manual => {
                    promotion_suppressed = Some(format!(
                        "verified; promotion is manual for this repository — \
                         run `aethyme broker submit promote --entry {}`",
                        entry.id
                    ));
                }
                PromoteMode::VerifyOnly => {
                    let reason = match intent {
                        PromotionIntent::VerifyOnly => {
                            "verified; --verify-only, so nothing was promoted".to_string()
                        }
                        PromotionIntent::Configured => {
                            "verified; promotion is off for this repository, so the work was \
                             checked and nothing was moved"
                                .to_string()
                        }
                    };
                    promotion_suppressed = Some(match promotion_config_warning.as_deref() {
                        Some(warning) => format!("{reason}; {warning}"),
                        None => reason,
                    });
                }
            }
        }
        if promoted {
            clear_action_required(Path::new(&session.worktree_path));
        }
        let entry = self.queue_entry(entry.id)?;
        Ok(SubmitOutcome {
            entry,
            submission_plan,
            conflicts: Vec::new(),
            conflict_details: Vec::new(),
            gate_outcomes,
            graph_integrity: Some(graph_integrity),
            gate_verification,
            no_changes: false,
            lease_warnings: Vec::new(),
            verified_against: None,
            promoted,
            promotion_suppressed,
        })
    }

    pub(crate) fn replay_submission_plan(
        &self,
        plan: &SubmissionPlan,
    ) -> Result<SubmissionReplay, BrokerOpError> {
        if !plan.safe {
            let mut reason = if plan.warnings.is_empty() {
                "commit provenance is ambiguous".into()
            } else {
                plan.warnings.join("; ")
            };
            if plan
                .warnings
                .iter()
                .any(|warning| warning.starts_with("accepted session checkpoint "))
                && let Some(recorded_baseline) = plan.recorded_baseline.as_deref()
            {
                let recovery_branch = format!(
                    "aethyme/recovery/session-{}-{}",
                    plan.session_id,
                    plan.session_head.get(..12).unwrap_or(&plan.session_head)
                );
                reason.push_str(&format!(
                    "\nSafe recovery:\n\
                       1. Review `aethyme broker advanced checkpoint plan --session {}` and apply its exact digest if the plan is safe.\n\
                       2. If automatic recovery refuses an amended promoted commit, preserve it first:\n\
                          git branch {recovery_branch} {}\n\
                          git reset --hard {recorded_baseline}\n\
                          git diff --binary {recorded_baseline} {recovery_branch} | git apply --index\n\
                          git commit\n\
                          aethyme broker submit --session {}\n\
                     Never reset before creating the preservation branch.",
                    plan.session_id, plan.session_head, plan.session_id
                ));
            }
            return Err(BrokerOpError::UnsafeSubmissionPlan {
                session_id: plan.session_id,
                reason,
            });
        }

        let repo = self.repo_handle();
        if self.can_preserve_owned_merge_history(plan)? {
            return Ok(SubmissionReplay {
                history_parent: Some(plan.session_head.clone()),
                tree: repo.commit_tree_id(&plan.session_head)?,
                conflicts: Vec::new(),
                conflict_details: Vec::new(),
            });
        }
        let mut current = plan.integration_head.clone();
        for commit in &plan.commits {
            if commit.ownership != SubmissionCommitOwnership::SessionOwned
                || commit.integration_state != SubmissionIntegrationState::Pending
            {
                continue;
            }
            if commit.parents.len() != 1 {
                let short_commit = commit.commit.get(..12).unwrap_or(&commit.commit);
                return Err(BrokerOpError::UnsupportedSubmissionCommit {
                    session_id: plan.session_id,
                    commit: commit.commit.clone(),
                    parent_count: commit.parents.len(),
                    recorded_baseline: plan
                        .recorded_baseline
                        .clone()
                        .unwrap_or_else(|| "<missing>".into()),
                    session_head: plan.session_head.clone(),
                    recovery_branch: format!(
                        "aethyme/recovery/session-{}-{short_commit}",
                        plan.session_id
                    ),
                });
            }
            let simulation =
                repo.merge_tree_with_base(&commit.parents[0], &current, &commit.commit)?;
            if !simulation.conflicts.is_empty() {
                let conflict_details = simulation
                    .conflicts
                    .iter()
                    .map(|path| SubmissionConflict {
                        path: path.clone(),
                        originating_commit: commit.commit.clone(),
                        ownership: commit.ownership,
                        integration_side_commits: self
                            .integration_commits_touching_path(
                                &commit.parents[0],
                                &plan.integration_head,
                                path,
                            ),
                        remediation: format!(
                            "rebase session {} onto integration {}, resolve {} while replaying {}, then resubmit",
                            plan.session_id, plan.integration_head, path, commit.commit
                        ),
                        commands: vec![
                            format!("git fetch . {}", plan.integration_head),
                            format!("git rebase {}", plan.integration_head),
                            format!("aethyme broker submit --session {}", plan.session_id),
                        ],
                    })
                    .collect();
                return Ok(SubmissionReplay {
                    history_parent: None,
                    tree: simulation.tree,
                    conflicts: simulation.conflicts,
                    conflict_details,
                });
            }
            current = repo.commit_tree(
                &simulation.tree,
                &[&current],
                &format!(
                    "chore(broker): replay {} for session {}",
                    &commit.commit[..12],
                    plan.session_id
                ),
                &crate::attribution::Attribution::broker_only(),
            )?;
        }
        Ok(SubmissionReplay {
            history_parent: None,
            tree: repo.commit_tree_id(&current)?,
            conflicts: Vec::new(),
            conflict_details: Vec::new(),
        })
    }

    /// Retain a merge DAG only when its entire exclusive history is owned,
    /// pending, and based on the exact current integration. First-parent
    /// patch replay cannot preserve a merge's conflict-resolution edits.
    /// Missing, rewritten, inherited or patch-equivalent provenance keeps
    /// using the existing replay/refusal path instead of importing history.
    fn can_preserve_owned_merge_history(
        &self,
        plan: &SubmissionPlan,
    ) -> Result<bool, BrokerOpError> {
        if !plan.commits.iter().any(|c| c.parents.len() == 2) {
            return Ok(false);
        }
        let repo = self.repo_handle();
        let Some(baseline) = plan.recorded_baseline.as_deref() else {
            return Ok(false);
        };
        if !plan.safe
            || !repo.has_complete_unmodified_history()?
            || !repo.is_ancestor_checked(baseline, &plan.integration_head)?
            || !repo.is_ancestor_checked(baseline, &plan.session_head)?
            || !repo.is_ancestor_checked(&plan.integration_head, &plan.session_head)?
        {
            return Ok(false);
        }
        let carried = repo
            .commits_excluding_oldest(&plan.session_head, &plan.integration_head)?
            .into_iter()
            .collect::<BTreeSet<_>>();
        let pending = plan.pending_owned_commit_ids();
        if carried.is_empty()
            || pending.len() != carried.len()
            || pending.into_iter().collect::<BTreeSet<_>>() != carried
        {
            return Ok(false);
        }
        let mut has_merge = false;
        for commit in plan.commits.iter().filter(|c| carried.contains(&c.commit)) {
            let parents = repo.commit_parents(&commit.commit)?;
            if parents != commit.parents || !(1..=2).contains(&parents.len()) {
                return Ok(false);
            }
            has_merge |= parents.len() == 2;
        }
        Ok(has_merge)
    }

    pub(crate) fn validate_submission_plan(
        &self,
        plan: &SubmissionPlan,
    ) -> Result<(), BrokerOpError> {
        self.replay_submission_plan(plan).map(|_| ())
    }

    fn integration_commits_touching_path(
        &self,
        session_parent: &str,
        integration_head: &str,
        path: &str,
    ) -> Vec<String> {
        let repo = self.repo_handle();
        let Ok(base) = repo.merge_base(session_parent, integration_head) else {
            return Vec::new();
        };
        let Ok(commits) = repo.first_parent_commits_between_oldest(&base, integration_head) else {
            return Vec::new();
        };
        commits
            .into_iter()
            .filter(|commit| {
                repo.first_parent(commit)
                    .and_then(|parent| repo.changed_between(&parent, commit))
                    .is_ok_and(|paths| paths.iter().any(|candidate| candidate == path))
            })
            .collect()
    }

    pub(crate) fn build_submission_plan(
        &self,
        session: &crate::types::Session,
        session_head: &str,
        integration_head: &str,
    ) -> Result<SubmissionPlan, BrokerOpError> {
        let repo = self.repo_handle();
        // Once a contribution has been promoted, ownership begins after the
        // last accepted session HEAD. `diff_base` remains the operational
        // fallback for a session that has never contributed.
        let accepted_checkpoint = session.accepted_session_head.as_deref();
        let accepted_is_ancestor = accepted_checkpoint
            .is_some_and(|checkpoint| repo.is_ancestor(checkpoint, session_head));
        let repaired_checkpoint = session.diff_base.as_deref().filter(|checkpoint| {
            accepted_checkpoint.is_some()
                && !accepted_is_ancestor
                && session.adoption_base.as_deref() != Some(*checkpoint)
                && repo.is_ancestor(checkpoint, session_head)
        });
        let selected_checkpoint = if accepted_is_ancestor {
            accepted_checkpoint
        } else {
            repaired_checkpoint.or_else(|| {
                if accepted_checkpoint.is_none() {
                    session.diff_base.as_deref()
                } else {
                    accepted_checkpoint
                }
            })
        };
        let Some(recorded_baseline) = selected_checkpoint else {
            return Ok(SubmissionPlan {
                session_id: session.id,
                recorded_baseline: None,
                session_head: session_head.to_string(),
                integration_head: integration_head.to_string(),
                safe: false,
                commits: Vec::new(),
                merged_tree_paths: Vec::new(),
                warnings: vec![
                    "session has no recorded baseline; commit ownership is ambiguous".into(),
                ],
            });
        };

        let mut warnings = Vec::new();
        if let (Some(accepted), Some(repaired)) = (accepted_checkpoint, repaired_checkpoint) {
            warnings.push(format!(
                "accepted session checkpoint {accepted} was rewritten by broker repair; ownership resumes from the broker-recorded repair baseline {repaired}"
            ));
        }
        // Reuse the exact exclusive walk when it is needed for both inherited
        // ownership and the later ancestry membership check. This cache is
        // local to this plan and these captured head OIDs; a later plan build
        // performs its own walk. Keep the ownership use strict (`?`) and the
        // later-only use best-effort with its existing ancestry fallback.
        let mut outside_integration: Option<BTreeSet<String>> = None;
        let (owned, inherited) = if repo.is_ancestor(recorded_baseline, session_head) {
            let owned = repo.commits_between_oldest(recorded_baseline, session_head)?;
            let inherited = if accepted_checkpoint.is_some() {
                // The checkpoint and everything before it already passed
                // promotion. A replayed integration commit commonly has a
                // different SHA, so ancestry alone must not reintroduce that
                // proven contribution as inherited work.
                Vec::new()
            } else {
                let owned_set = owned.iter().cloned().collect::<BTreeSet<_>>();
                let outside = repo.commits_excluding_oldest(session_head, integration_head)?;
                outside_integration = Some(outside.iter().cloned().collect());
                outside
                    .into_iter()
                    .filter(|commit| !owned_set.contains(commit))
                    .collect::<Vec<_>>()
            };
            (owned, inherited)
        } else if accepted_checkpoint.is_some() {
            return Ok(SubmissionPlan {
                session_id: session.id,
                recorded_baseline: Some(recorded_baseline.to_string()),
                session_head: session_head.to_string(),
                integration_head: integration_head.to_string(),
                safe: false,
                commits: repo
                    .commits_excluding_oldest(session_head, integration_head)?
                    .into_iter()
                    .map(|commit| {
                        let parents = repo.commit_parents(&commit)?;
                        Ok(SubmissionCommitProvenance {
                            commit,
                            parents,
                            ownership: SubmissionCommitOwnership::Ambiguous,
                            integration_state: SubmissionIntegrationState::Ambiguous,
                            patch_id: None,
                            matching_integration_commits: Vec::new(),
                        })
                    })
                    .collect::<Result<Vec<_>, BrokerOpError>>()?,
                merged_tree_paths: Vec::new(),
                warnings: vec![format!(
                    "accepted session checkpoint {recorded_baseline} is not an ancestor of session HEAD {session_head}; follow-up ownership must remain {recorded_baseline}..{session_head}, so integration HEAD {integration_head} cannot replace it as the ownership boundary"
                )],
            });
        } else if repo.is_ancestor(integration_head, session_head) {
            warnings.push(format!(
                "recorded baseline {recorded_baseline} was rewritten; ownership uses the unambiguous rebased range {integration_head}..{session_head}"
            ));
            (
                repo.commits_between_oldest(integration_head, session_head)?,
                Vec::new(),
            )
        } else {
            return Ok(SubmissionPlan {
                session_id: session.id,
                recorded_baseline: Some(recorded_baseline.to_string()),
                session_head: session_head.to_string(),
                integration_head: integration_head.to_string(),
                safe: false,
                commits: repo
                    .commits_excluding_oldest(session_head, integration_head)?
                    .into_iter()
                    .map(|commit| {
                        let parents = repo.commit_parents(&commit)?;
                        Ok(SubmissionCommitProvenance {
                            commit,
                            parents,
                            ownership: SubmissionCommitOwnership::Ambiguous,
                            integration_state: SubmissionIntegrationState::Ambiguous,
                            patch_id: None,
                            matching_integration_commits: Vec::new(),
                        })
                    })
                    .collect::<Result<Vec<_>, BrokerOpError>>()?,
                merged_tree_paths: Vec::new(),
                warnings: vec![format!(
                    "recorded baseline {recorded_baseline} is not an ancestor of session HEAD {session_head}; commit ownership is ambiguous"
                )],
            });
        };

        // Every question below is asked of the whole commit list at once:
        // asking it once per commit spawned several git processes each, and
        // a session cut from a main far ahead of integration carries every
        // commit of that drift, so submit grew to 45 minutes (#463).
        let ordered = inherited
            .into_iter()
            .map(|commit| {
                (
                    commit,
                    SubmissionCommitOwnership::InheritedFromRecordedBaseline,
                )
            })
            .chain(
                owned
                    .into_iter()
                    .map(|commit| (commit, SubmissionCommitOwnership::SessionOwned)),
            )
            .collect::<Vec<_>>();
        let names = ordered
            .iter()
            .map(|(commit, _)| commit.clone())
            .collect::<Vec<_>>();
        let shapes = repo.commit_shapes(&names)?;
        let patch_ids = repo.first_parent_patch_ids(&with_parent(&names, &shapes))?;

        // Integration patch identities can affect provenance only for a
        // session commit that has a patch identity. Root and empty-diff
        // commits have none, so avoid walking and hashing integration history
        // when the session has no patch identities to compare. Ancestry and
        // exact-merge classification below still runs for every session.
        let mut integration_by_patch: BTreeMap<String, Vec<String>> = BTreeMap::new();
        if !patch_ids.is_empty() {
            let integration_candidates =
                repo.first_parent_commits_excluding_oldest(integration_head, recorded_baseline)?;
            // The session batch commonly already contains commits shared with
            // integration. Reuse their exact object-identity results so an
            // overlapping first-parent history is not diffed twice. Keep
            // scanning every integration candidate absent from the session
            // batch: patch-equivalent commits still populate the public match
            // list even when ancestry already proves a session commit is integrated.
            let unseen_candidates = integration_candidates
                .iter()
                .filter(|commit| !shapes.contains_key(*commit))
                .cloned()
                .collect::<Vec<_>>();
            let candidate_shapes = repo.commit_shapes(&unseen_candidates)?;
            let candidate_patch_ids =
                repo.first_parent_patch_ids(&with_parent(&unseen_candidates, &candidate_shapes))?;
            for commit in integration_candidates {
                if let Some(patch_id) = patch_ids
                    .get(&commit)
                    .or_else(|| candidate_patch_ids.get(&commit))
                {
                    integration_by_patch
                        .entry(patch_id.clone())
                        .or_default()
                        .push(commit);
                }
            }
        }

        // Each commit here, and each parent of one, is reachable from the
        // session HEAD, so it is an ancestor of integration exactly when it
        // is not among the commits the session HEAD has and integration
        // lacks. One rev-list answers that for all of them; if it cannot run,
        // ask per commit as before.
        let outside_integration = outside_integration.or_else(|| {
            repo.commits_excluding_oldest(session_head, integration_head)
                .ok()
                .map(|commits| commits.into_iter().collect::<BTreeSet<_>>())
        });
        let reaches_integration = |commit: &str| match &outside_integration {
            Some(outside) => !outside.contains(commit),
            None => repo.is_ancestor(commit, integration_head),
        };
        let synced_second_parents = shapes
            .values()
            .filter(|shape| shape.parents.len() == 2 && reaches_integration(&shape.parents[1]))
            .map(|shape| shape.parents[1].clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let second_parent_shapes = repo.commit_shapes(&synced_second_parents)?;

        let mut commits = Vec::with_capacity(ordered.len());
        for (commit, ownership) in ordered {
            let shape = &shapes[&commit];
            let parents = shape.parents.clone();
            let patch_id = patch_ids.get(&commit).cloned();
            let matching_integration_commits = patch_id
                .as_ref()
                .and_then(|patch| integration_by_patch.get(patch))
                .cloned()
                .unwrap_or_default();
            let exact_integration_sync = parents.len() == 2
                && second_parent_shapes
                    .get(&parents[1])
                    .is_some_and(|second| second.tree == shape.tree);
            let integration_state = if reaches_integration(&commit) || exact_integration_sync {
                SubmissionIntegrationState::AlreadyIntegratedByAncestry
            } else {
                match matching_integration_commits.len() {
                    0 => SubmissionIntegrationState::Pending,
                    1 => SubmissionIntegrationState::AlreadyIntegratedByStablePatchIdentity,
                    _ => SubmissionIntegrationState::Ambiguous,
                }
            };
            commits.push(SubmissionCommitProvenance {
                commit,
                parents,
                ownership,
                integration_state,
                patch_id,
                matching_integration_commits,
            });
        }

        let ambiguous = commits.iter().any(|commit| {
            commit.ownership == SubmissionCommitOwnership::Ambiguous
                || commit.integration_state == SubmissionIntegrationState::Ambiguous
        });
        if ambiguous {
            warnings.push(
                "one or more commits have ambiguous provenance; normalized replay must refuse this plan"
                    .into(),
            );
        }
        Ok(SubmissionPlan {
            session_id: session.id,
            recorded_baseline: Some(recorded_baseline.to_string()),
            session_head: session_head.to_string(),
            integration_head: integration_head.to_string(),
            safe: !ambiguous,
            commits,
            merged_tree_paths: Vec::new(),
            warnings,
        })
    }

    /// Prefer the typed candidate when it agrees with the stable legacy
    /// projection. Older writers can update queue details without knowing
    /// about the additive table, so a disagreement falls back to that copy.
    fn verification_candidate_for_entry(
        &mut self,
        entry: &MergeQueueEntry,
    ) -> Result<Option<VerificationCandidate>, BrokerOpError> {
        let typed = self.store().verification_candidate(entry.id)?;
        let legacy = legacy_verification_candidate(entry);
        let Some(typed) = typed else {
            return Ok(legacy);
        };
        let tree_matches = entry
            .merged_tree
            .as_deref()
            .is_none_or(|tree| tree == typed.tree.as_str());
        let legacy_matches = legacy.as_ref().is_none_or(|legacy| {
            legacy.base.as_str() == typed.base.as_str()
                && legacy.commit.as_str() == typed.commit.as_str()
        });
        if tree_matches && legacy_matches {
            Ok(Some(typed))
        } else {
            Ok(legacy)
        }
    }

    /// Complete the durable half of a promotion when the integration ref
    /// already names a verified entry's exact merge commit. This is the
    /// only ref/database ordering gap in normal promotion: the ref moves
    /// first, so reopening the broker must checkpoint the accepted session
    /// HEAD before any follow-up submission is planned.
    pub(crate) fn recover_interrupted_promotion(&mut self) -> Result<(), BrokerOpError> {
        let verified = self
            .store()
            .promotion_recovery_entries(MergeStatus::Verified)?;
        if verified.is_empty() {
            return Ok(());
        }
        let config = PromoteConfig::load(&self.main_root_path());
        let Some(integration_head) = self.repo_handle().resolve_ref(&config.branch) else {
            return Ok(());
        };
        let mut candidate = None;
        for entry in verified {
            let Some(verification_candidate) = self.verification_candidate_for_entry(&entry)?
            else {
                continue;
            };
            if verification_candidate.commit.as_str() == integration_head.as_str() {
                candidate = Some((entry, verification_candidate));
                break;
            }
        }
        if let Some((entry, verification_candidate)) = candidate {
            let promoted_base = if verification_candidate.base.is_empty() {
                self.repo_handle().first_parent(&integration_head)?
            } else {
                verification_candidate.base
            };
            let promoted_paths = self
                .repo_handle()
                .changed_between(&promoted_base, &integration_head)?;
            self.store().record_merge_promotion(
                entry.id,
                &integration_head,
                &config.branch,
                &promoted_paths,
                &crate::events::merge_promoted_payload(&config.branch, &integration_head),
            )?;
        }
        Ok(())
    }

    /// Schema v17 compatibility: reconstruct exact outstanding exposure for
    /// promotions recorded before exposure storage existed. Only currently
    /// promoted entries qualify; externally landed and superseded history is
    /// already terminal and must not be resurrected.
    pub(crate) fn backfill_promoted_path_exposures(&mut self) -> Result<(), BrokerOpError> {
        let promoted_entries = self
            .store()
            .promotion_recovery_entries(MergeStatus::Promoted)?;
        for entry in promoted_entries {
            if self.store().entry_path_exposure(entry.id)?.is_some() {
                continue;
            }
            let promotion_sha = entry
                .details_json
                .as_deref()
                .and_then(|details| serde_json::from_str::<serde_json::Value>(details).ok())
                .and_then(|details| details.get("commit")?.as_str().map(str::to_string))
                .ok_or_else(|| BrokerOpError::ShipPlanUnavailable {
                    what: "promotion commit",
                    reason: format!(
                        "promoted queue entry {} has no commit detail for exposure backfill",
                        entry.id
                    ),
                })?;
            let parent = self.repo_handle().first_parent(&promotion_sha)?;
            let promoted_paths = self
                .repo_handle()
                .changed_between(&parent, &promotion_sha)?;
            self.store()
                .backfill_entry_path_exposure(entry.id, &promotion_sha, &promoted_paths)?;
        }
        Ok(())
    }

    /// Advance the integration branch to a verified entry's merge commit,
    /// then re-simulate every other non-terminal entry whose base moved.
    /// Publication is a separate, explicit `broker ship` operation.
    /// Re-simulate an entry whose verified base is no longer the integration
    /// tip, and promote it if it still verifies.
    fn reverify_and_promote(&mut self, entry_id: i64) -> Result<(), BrokerOpError> {
        let outcome = self.simulate_and_gate(entry_id)?;
        if outcome.entry.status != MergeStatus::Verified {
            return Err(BrokerOpError::NotVerified {
                entry: entry_id,
                status: outcome.entry.status.as_str(),
            });
        }
        self.promote(entry_id)
    }

    pub fn promote(&mut self, entry_id: i64) -> Result<(), BrokerOpError> {
        let entry = self.queue_entry(entry_id)?;
        if entry.status != MergeStatus::Verified {
            return Err(BrokerOpError::NotVerified {
                entry: entry_id,
                status: entry.status.as_str(),
            });
        }
        // Every promotion, automatic or explicit, passes here. Under
        // `[collaboration] capture = "required"` only a head with an
        // acknowledged required capture may move integration (#660); with
        // capture off or advisory this reads the config and nothing else.
        crate::collaboration_submit::require_capture_for_promotion(
            self,
            entry_id,
            &entry.head_commit,
        )?;
        let candidate = self
            .verification_candidate_for_entry(&entry)?
            .ok_or(crate::BrokerError::SessionNotFound(entry_id))?;
        let base_at_verify = candidate.base;
        let merge_commit = candidate.commit;

        let (branch, current_base) = self.integration_head()?;
        if current_base != base_at_verify {
            // Base moved since verification: verification is stale —
            // re-simulate instead of promoting a stale merge.
            return self.reverify_and_promote(entry_id);
        }

        // Capture the exact promoted path set before moving the ref. This is
        // durable publication state, so unlike notification projection it is
        // part of the promotion transaction and cannot be best-effort.
        let promoted_paths = self
            .repo_handle()
            .changed_between(&current_base, &merge_commit)?;
        // The check above and this update are not atomic, and merge
        // verifications run concurrently: two entries verified against the
        // same base can both pass the check. An unconditional update would let
        // the second silently overwrite the first while the first stays
        // recorded as promoted. Moving the ref only from the verified base
        // turns that race into one more stale verification.
        if !advance_branch_from(self.repo_handle(), &branch, &current_base, &merge_commit)? {
            return self.reverify_and_promote(entry_id);
        }
        self.store().record_merge_promotion(
            entry_id,
            &merge_commit,
            &branch,
            &promoted_paths,
            &crate::events::merge_promoted_payload(&branch, &merge_commit),
        )?;
        self.persist_promotion_lease_advisories(&entry, &merge_commit, &promoted_paths);

        // Requeue: everything still in flight was verified/conflicted
        // against a base that just moved (#23).
        let stale: Vec<i64> = self
            .store()
            .merge_queue()?
            .into_iter()
            .filter(|e| {
                e.id != entry_id
                    && matches!(
                        e.status,
                        MergeStatus::Submitted
                            | MergeStatus::Simulating
                            | MergeStatus::Verified
                            | MergeStatus::Conflict
                    )
            })
            .map(|e| e.id)
            .collect();
        for stale_id in stale {
            // Promoting an entry can recursively re-simulate and promote
            // another queued entry. Re-read both queue and session state so
            // the outer stale snapshot cannot promote that entry twice, and
            // never resurrect abandoned work owned by a cleaned session.
            let Ok(stale_entry) = self.queue_entry(stale_id) else {
                continue;
            };
            if !matches!(
                stale_entry.status,
                MergeStatus::Submitted
                    | MergeStatus::Simulating
                    | MergeStatus::Verified
                    | MergeStatus::Conflict
            ) {
                continue;
            }
            let Ok(stale_session) = self.store().session(stale_entry.session_id) else {
                continue;
            };
            if stale_session.status.is_closed() {
                crate::warn_unrecorded(
                    "mark a closed session's queue entry superseded",
                    self.store().set_merge_status(
                        stale_id,
                        MergeStatus::Superseded,
                        None,
                        Some("{\"reason\":\"session cleaned before queue revalidation\"}"),
                    ),
                );
                continue;
            }
            // Best effort: a failure re-simulating one entry must not
            // abort the promotion that already happened.
            let _ = self.simulate_and_gate(stale_id);
        }
        Ok(())
    }

    /// Persist one idempotent advisory for every *other* live session whose
    /// explicit or implicit lease intersects the newly promoted paths.
    ///
    /// The integration ref and promotion row are already authoritative when
    /// this runs. Every failure is therefore reported as a warning, never
    /// returned: notifications may be
    /// repaired or acknowledged, but they can never roll back or block a
    /// verified promotion and they never mutate another worktree.
    fn persist_promotion_lease_advisories(
        &mut self,
        entry: &MergeQueueEntry,
        integration_sha: &str,
        promoted_paths: &[String],
    ) {
        if promoted_paths.is_empty() {
            return;
        }
        let Ok(live_sessions) = self.store().live_sessions() else {
            return;
        };
        let live_session_ids: BTreeSet<i64> = live_sessions
            .into_iter()
            .filter(|session| session.id != entry.session_id)
            .map(|session| session.id)
            .collect();
        let Ok(leases) = self.store().active_leases() else {
            return;
        };
        let mut paths_by_session: BTreeMap<i64, BTreeSet<String>> = BTreeMap::new();
        let mut lease_evidence_by_session: BTreeMap<i64, BTreeSet<String>> = BTreeMap::new();
        for lease in leases {
            if !live_session_ids.contains(&lease.session_id) {
                continue;
            }
            for promoted_path in promoted_paths {
                if crate::leases::paths_overlap(&lease.path, promoted_path) {
                    paths_by_session
                        .entry(lease.session_id)
                        .or_default()
                        .insert(promoted_path.clone());
                    lease_evidence_by_session
                        .entry(lease.session_id)
                        .or_default()
                        .insert(format!(
                            "{} lease {:?} overlaps promoted path {:?}",
                            lease.kind.as_str(),
                            lease.path,
                            promoted_path
                        ));
                }
            }
        }

        for (session_id, paths) in paths_by_session {
            let path_count = paths.len();
            let paths = paths.into_iter().take(100).collect::<Vec<_>>();
            let lease_evidence = lease_evidence_by_session
                .remove(&session_id)
                .unwrap_or_default();
            let lease_evidence_count = lease_evidence.len();
            let mut evidence = vec![AdvisoryEvidence {
                kind: "promotion_intersects_lease".into(),
                summary: format!(
                    "integration {integration_sha} changed paths leased by session {session_id}; the broker did not rebase or modify that worktree"
                ),
            }];
            evidence.extend(
                lease_evidence
                    .into_iter()
                    .take(97)
                    .map(|summary| AdvisoryEvidence {
                        kind: "lease_overlap".into(),
                        summary,
                    }),
            );
            evidence.push(AdvisoryEvidence {
                kind: "bounded_result".into(),
                summary: format!(
                    "recorded {} of {path_count} intersecting promoted paths and {} of {lease_evidence_count} lease overlaps",
                    paths.len(),
                    lease_evidence_count.min(97),
                ),
            });
            evidence.push(AdvisoryEvidence {
                kind: "safe_next_action".into(),
                summary: "aethyme broker status --json".into(),
            });
            let persisted = self.persist_advisory(NewAdvisory {
                identity: format!("promotion_lease_intersection:{integration_sha}:{session_id}"),
                audience: crate::AdvisoryAudience::Session,
                producer: crate::AdvisoryProducer::Coordination,
                session_id: Some(session_id),
                severity: AdvisorySeverity::Warning,
                queue_entry_id: Some(entry.id),
                integration_sha: Some(integration_sha.to_string()),
                paths,
                evidence,
            });
            crate::warn_unrecorded("persist a lease-intersection advisory", persisted);
        }
    }

    pub(crate) fn queue_entry(&mut self, entry_id: i64) -> Result<MergeQueueEntry, BrokerOpError> {
        self.store()
            .merge_queue()?
            .into_iter()
            .find(|e| e.id == entry_id)
            .ok_or(crate::BrokerError::SessionNotFound(entry_id).into())
    }

    /// Sessions (other than `submitter`) holding active leases on any of
    /// the conflicted paths — the "who am I fighting with" signal.
    fn blocking_sessions(
        &mut self,
        submitter: i64,
        conflicts: &[String],
    ) -> Result<Vec<i64>, BrokerOpError> {
        let leases = self.store().active_leases()?;
        let mut blocking: Vec<i64> = leases
            .iter()
            .filter(|lease| lease.session_id != submitter)
            .filter(|lease| {
                conflicts.iter().any(|conflict| {
                    conflict == &lease.path
                        || (lease.path.ends_with('/') && conflict.starts_with(&lease.path))
                })
            })
            .map(|lease| lease.session_id)
            .collect();
        blocking.sort_unstable();
        blocking.dedup();
        Ok(blocking)
    }
}

/// The commits in `commits` that have a parent, in order: a root commit has
/// no first-parent diff, and so no patch id.
fn with_parent(
    commits: &[String],
    shapes: &BTreeMap<String, crate::git::CommitShape>,
) -> Vec<String> {
    commits
        .iter()
        .filter(|commit| {
            shapes
                .get(*commit)
                .is_some_and(|shape| !shape.parents.is_empty())
        })
        .cloned()
        .collect()
}

fn submission_planning_failure_class(error: &BrokerOpError) -> &'static str {
    match error {
        BrokerOpError::UnsafeSubmissionPlan { .. } => "unsafe_provenance",
        BrokerOpError::UnsupportedSubmissionCommit { .. } => "unsupported_commit_shape",
        _ => "planning_error",
    }
}

/// The agent-facing conflict message (#21): machine- and human-readable,
/// dropped into the session's worktree. The Aethyme-generated AGENTS.md
/// can point agents at this path; no vendor-specific injection.
impl Broker {
    /// The commit a session's own changes should be measured against:
    /// `merge-base(session HEAD, integration)`. Unlike the stored
    /// adoption-time `diff_base`, this self-heals after a conflict-rebase
    /// (#41): promoted commits brought in by the rebase move into the
    /// common ancestry instead of inflating the session's apparent diff
    /// (the phantom-lease symptom). Read-only — never refreshes the
    /// integration branch.
    pub fn session_change_base(&mut self, session_checkout: &GitRepo) -> Option<String> {
        let integration = self.integration_tip()?;
        let upstream = self
            .repo_handle()
            .upstream_default()
            .map(|(_, commit)| commit);
        session_baseline(session_checkout, &integration, upstream.as_deref())
    }

    /// The integration branch's current commit, without creating or
    /// refreshing the branch (read-only, unlike [`Self::integration_head`]).
    /// Invariant across sessions — resolve once and reuse when deriving
    /// change bases in a loop (`refresh_leases` did this per session,
    /// the dominant redundant git call in `broker status`).
    pub(crate) fn integration_tip(&self) -> Option<String> {
        let branch = PromoteConfig::load(&self.main_root_path()).branch;
        self.repo_handle()
            .resolve_ref(&format!("refs/heads/{branch}"))
    }

    /// Resolve the integration view without creating or fast-forwarding its
    /// ref. Before the first promotion, a snapshot reports main as the
    /// effective integration base while leaving the ref absent.
    pub(crate) fn integration_head_snapshot(&self) -> Result<(String, String), BrokerOpError> {
        let branch = PromoteConfig::load(&self.main_root_path()).branch;
        let commit = self
            .repo_handle()
            .resolve_ref(&format!("refs/heads/{branch}"))
            .map(Ok)
            .unwrap_or_else(|| self.repo_handle().head_commit())?;
        Ok((branch, commit))
    }
}

/// Move `branch` from `expected` to `commit`, and only from `expected`.
///
/// `Ok(false)` means the branch had already moved away from `expected`, so the
/// caller's verification is stale. Any other failure is an error.
fn advance_branch_from(
    repo: &GitRepo,
    branch: &str,
    expected: &str,
    commit: &str,
) -> Result<bool, BrokerOpError> {
    match repo.update_branch_ref_checked(branch, commit, expected) {
        Ok(()) => Ok(true),
        Err(error) => {
            if repo.resolve_ref(&format!("refs/heads/{branch}")).as_deref() == Some(expected) {
                Err(error.into())
            } else {
                Ok(false)
            }
        }
    }
}

/// Remove a stale action-required drop once the session's work promotes
/// (#41 follow-on, reported by agent A4: the file survived success with
/// outdated blocking info). Best-effort — the worktree may be gone.
fn clear_action_required(worktree: &Path) {
    let _ = std::fs::remove_file(worktree.join(ACTION_REQUIRED_RELPATH));
}

fn write_action_required(
    worktree: &Path,
    entry: &MergeQueueEntry,
    conflicts: &[SubmissionConflict],
    blocking: &[i64],
    base: &str,
) {
    let blocking_note = if blocking.is_empty() {
        "No live session currently holds leases on these paths.".to_string()
    } else {
        format!(
            "Blocking session(s): {} — coordinate or wait for their promotion.",
            blocking
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let body = format!(
        "# Broker: action required — merge conflict\n\n\
         Your submission (commit `{head}`) conflicts with the integration\n\
         branch (base `{base}`) and was rejected before any CI ran.\n\n\
         Conflicting files:\n{files}\n\n{blocking_note}\n\n\
         To resolve, in this worktree:\n\n\
         1. `git fetch . {base}` (the base is a local commit; no network)\n\
         2. `git rebase {base}` and resolve the conflicts above\n\
            (headless agents: if the rebase pauses, continue with\n\
            `GIT_EDITOR=true git rebase --continue` — never rely on an\n\
            interactive editor)\n\
         3. resubmit: `aethyme broker submit --session {session}`\n\n\
         This file is regenerated on each conflicted submission; it is\n\
         gitignored broker state (delete freely).\n",
        head = entry.head_commit,
        base = base,
        files = conflicts
            .iter()
            .map(|conflict| {
                let integration = if conflict.integration_side_commits.is_empty() {
                    "unknown".to_string()
                } else {
                    conflict.integration_side_commits.join(", ")
                };
                format!(
                    "- `{path}` — session commit `{origin}` ({ownership}); integration commit(s): {integration}",
                    path = conflict.path,
                    origin = conflict.originating_commit,
                    ownership = conflict.ownership.as_str(),
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        blocking_note = blocking_note,
        session = entry.session_id,
    );
    // The queue entry already records the conflict; this file is how the
    // agent learns of it, so a failed write is reported rather than dropped.
    let path = worktree.join(ACTION_REQUIRED_RELPATH);
    let written = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&path, body));
    crate::warn_unrecorded(&format!("write {}", path.display()), written);
}

#[cfg(test)]
mod verdict_tests {
    use super::{GateVerdict, gate_verdict};
    use crate::gates::GateRunOutcome;
    use crate::types::{GateEnvironment, GateFailureClass, GateStatus};

    fn outcome(
        status: GateStatus,
        class: Option<GateFailureClass>,
        host_fault: bool,
    ) -> GateRunOutcome {
        GateRunOutcome {
            gate: "g".into(),
            run_id: None,
            tree_hash: "t".into(),
            definition_hash: "d".into(),
            cache_provenance: None,
            resource_lease: None,
            managed_cache: None,
            broker_database: None,
            status,
            failure_class: class,
            cached: false,
            exit_code: None,
            duration_ms: Some(0),
            wait_duration_ms: None,
            first_output_ms: None,
            output_bytes: None,
            log_path: None,
            environment: GateEnvironment::default(),
            host_fault,
        }
    }

    fn pass() -> GateRunOutcome {
        outcome(GateStatus::Pass, None, false)
    }

    fn host() -> GateRunOutcome {
        outcome(
            GateStatus::Error,
            Some(GateFailureClass::ResourceContention),
            true,
        )
    }

    fn real() -> GateRunOutcome {
        outcome(GateStatus::Fail, Some(GateFailureClass::TestFailure), false)
    }

    #[test]
    fn a_host_fault_alone_defers() {
        assert_eq!(gate_verdict(&[pass(), host()]), GateVerdict::Deferred);
    }

    /// The property deferral must never break: a genuine failure anywhere in
    /// the outcomes is a rejection, whatever else the host did.
    #[test]
    fn a_real_failure_is_never_masked_by_a_host_fault() {
        assert_eq!(gate_verdict(&[host(), real()]), GateVerdict::Failed);
        assert_eq!(gate_verdict(&[real(), host()]), GateVerdict::Failed);
    }

    /// Classified like a host problem from its log, but not observed by the
    /// broker: still a verdict.
    #[test]
    fn a_host_looking_class_without_the_observed_flag_fails() {
        let inferred = outcome(GateStatus::Error, Some(GateFailureClass::Timeout), false);
        assert_eq!(gate_verdict(&[inferred]), GateVerdict::Failed);
    }

    #[test]
    fn passing_gates_pass() {
        assert_eq!(gate_verdict(&[pass(), pass()]), GateVerdict::Passed);
    }
}

#[cfg(test)]
mod promotion_ref_tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.test")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.test")
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn commit(dir: &Path, name: &str) -> String {
        std::fs::write(dir.join(name), name).unwrap();
        git(dir, &["add", name]);
        git(dir, &["commit", "-q", "-m", name]);
        git(dir, &["rev-parse", "HEAD"])
    }

    /// Two promotions verified against the same base: the first advances
    /// integration, and the second must see that its base moved instead of
    /// overwriting the first promotion.
    #[test]
    fn a_second_promotion_from_the_same_base_does_not_overwrite_the_first() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        let base = commit(dir, "base");
        git(dir, &["branch", "integration", &base]);
        let first = commit(dir, "first");
        git(dir, &["reset", "-q", "--hard", &base]);
        let second = commit(dir, "second");
        let repo = GitRepo::discover(dir).unwrap();

        assert!(advance_branch_from(&repo, "integration", &base, &first).unwrap());
        assert!(
            !advance_branch_from(&repo, "integration", &base, &second).unwrap(),
            "the second promotion did not notice its base had moved"
        );
        assert_eq!(
            git(dir, &["rev-parse", "integration"]),
            first,
            "the first promotion was overwritten"
        );
    }
}

#[cfg(test)]
mod promotion_subject_tests {
    use super::{PROMOTION_SUBJECT_MAX_CHARS, promotion_subject};

    #[test]
    fn keeps_short_task_names_intact() {
        assert_eq!(
            promotion_subject(42, Some("fix lease refresh")),
            "chore(broker): promote session 42 (fix lease refresh)"
        );
    }

    #[test]
    fn caps_long_task_names_at_the_conventional_subject_limit() {
        let subject = promotion_subject(42, Some(&"long task ".repeat(20)));

        assert!(subject.chars().count() <= PROMOTION_SUBJECT_MAX_CHARS);
        // The cut never leaves a space before the marker.
        assert!(subject.ends_with("...)"), "{subject}");
        assert!(!subject.ends_with(" ...)"), "{subject}");
    }

    #[test]
    fn a_multi_line_task_contributes_only_its_first_line() {
        assert_eq!(
            promotion_subject(42, Some("fix lease refresh\n\nlong details")),
            "chore(broker): promote session 42 (fix lease refresh)"
        );
    }

    #[test]
    fn truncates_unicode_task_names_on_character_boundaries() {
        let subject = promotion_subject(42, Some(&"🚀".repeat(100)));

        assert_eq!(subject.chars().count(), PROMOTION_SUBJECT_MAX_CHARS);
        assert!(subject.ends_with("...)"));
    }
}

#[cfg(test)]
mod verification_candidate_tests {
    use super::mint_verification_candidate;
    use crate::attribution::Attribution;
    use crate::git::GitRepo;
    use std::path::Path;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn commit_file(dir: &Path, name: &str, contents: &str) -> String {
        std::fs::write(dir.join(name), contents).unwrap();
        git(dir, &["add", name]);
        git(dir, &["commit", "-q", "-m", name]);
        git(dir, &["rev-parse", "HEAD"])
    }

    #[test]
    fn mints_a_typed_candidate_without_gate_configuration_or_execution() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        git(root, &["init", "-q", "-b", "main"]);
        git(root, &["config", "user.name", "Candidate Test"]);
        git(root, &["config", "user.email", "candidate@example.invalid"]);
        let base = commit_file(root, "base.txt", "base\n");
        let input = commit_file(root, "change.txt", "change\n");
        let tree = git(root, &["write-tree"]);
        assert!(!aethyme_config::config_exists(
            &root.join(aethyme_config::CONFIG_DIR),
            "gates"
        ));

        let repo = GitRepo::discover(root).unwrap();
        let candidate = mint_verification_candidate(
            &repo,
            &base,
            vec![input.clone()],
            &tree,
            "test verification candidate",
            &Attribution::broker_only(),
            None,
        )
        .unwrap();

        assert_eq!(candidate.base, base);
        assert_eq!(candidate.inputs, vec![input]);
        assert_eq!(candidate.tree, tree);
        assert_eq!(
            repo.commit_tree_id(&candidate.commit).unwrap(),
            candidate.tree
        );
        assert_eq!(
            repo.first_parent(&candidate.commit).unwrap(),
            candidate.base
        );
        assert_eq!(
            repo.commit_message(&candidate.commit).unwrap(),
            "test verification candidate"
        );
    }
}

#[cfg(test)]
mod owned_merge_history_tests {
    use super::*;
    use std::path::PathBuf;

    fn git(root: &Path, args: &[&str]) -> String {
        let mut command = std::process::Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key
                .to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("GIT_")
            {
                command.env_remove(key);
            }
        }
        let output = command
            .args(args)
            .current_dir(root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_SYSTEM", root.join("missing-system-gitconfig"))
            .env("GIT_CONFIG_GLOBAL", root.join("missing-global-gitconfig"))
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("GIT_AUTHOR_NAME", "History Test")
            .env("GIT_AUTHOR_EMAIL", "history@example.invalid")
            .env("GIT_COMMITTER_NAME", "History Test")
            .env("GIT_COMMITTER_EMAIL", "history@example.invalid")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn edit(root: &Path, file: &str, content: &str) {
        std::fs::write(root.join(file), content).unwrap();
        git(root, &["add", file]);
        git(root, &["commit", "-qm", file]);
    }

    fn fixture() -> (tempfile::TempDir, Broker, SubmissionPlan) {
        fixture_with_root_and_legacy_mode(None, false)
    }

    fn fixture_with_worktree_root(
        worktree_root: Option<PathBuf>,
    ) -> (tempfile::TempDir, Broker, SubmissionPlan) {
        fixture_with_root_and_legacy_mode(worktree_root, false)
    }

    fn fixture_with_legacy_binding() -> (tempfile::TempDir, Broker, SubmissionPlan) {
        fixture_with_root_and_legacy_mode(None, true)
    }

    fn fixture_with_root_and_legacy_mode(
        worktree_root: Option<PathBuf>,
        legacy_binding: bool,
    ) -> (tempfile::TempDir, Broker, SubmissionPlan) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        git(root, &["init", "-q", "-b", "main"]);
        edit(root, "base.txt", "base\n");
        let base = git(root, &["rev-parse", "HEAD"]);
        if legacy_binding {
            // Opening an existing broker database through the ordinary schema
            // path creates the valid legacy-unbound state used for refusal.
            let database_path = crate::broker_db_path(root).unwrap();
            drop(crate::BrokerStore::open(&database_path).unwrap());
        }
        let mut broker = match worktree_root {
            Some(worktree_root) => Broker::open_with_options(
                root,
                crate::BrokerOpenOptions::default().with_worktree_root(worktree_root),
            )
            .unwrap(),
            None => Broker::open(root).unwrap(),
        };
        let source = root.join(".aethyme/worktrees/source");
        git(
            root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "agent/source",
                source.to_str().unwrap(),
            ],
        );
        let session = broker.adopt(&source, None).unwrap();
        git(&source, &["branch", "side"]);
        edit(&source, "left.txt", "left\n");
        git(&source, &["checkout", "-q", "side"]);
        edit(&source, "right.txt", "right\n");
        git(&source, &["checkout", "-q", "agent/source"]);
        git(
            &source,
            &["merge", "-q", "--no-ff", "side", "-m", "owned merge"],
        );
        let head = git(&source, &["rev-parse", "HEAD"]);
        let plan = broker
            .build_submission_plan(&session, &head, &base)
            .unwrap();
        assert!(broker.can_preserve_owned_merge_history(&plan).unwrap());
        (tmp, broker, plan)
    }

    #[test]
    fn submit_binds_a_fresh_root_after_preflight_before_queueing() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };

        let root_owner = tempfile::tempdir().unwrap();
        let worktree_root = root_owner.path().join("managed-worktrees");
        let (tmp, mut broker, plan) = fixture_with_worktree_root(Some(worktree_root.clone()));
        let root = tmp.path().canonicalize().unwrap();
        let session_id = plan.session_id;
        let expected_head = plan.session_head.clone();
        let fresh_unbound =
            crate::store::repository_binding::RepositoryWorktreeBindingState::FreshUnbound;
        assert_eq!(
            broker.store().repository_worktree_binding().unwrap().state,
            fresh_unbound
        );

        let observer_root = root.clone();
        let lifecycle_repository_root = root.clone();
        let observer_worktree_root = worktree_root.clone();
        let preflight_observed = Arc::new(AtomicBool::new(false));
        let preflight_observed_by_hook = Arc::clone(&preflight_observed);
        let lifecycle_observed = Arc::new(AtomicBool::new(false));
        let lifecycle_observed_by_hook = Arc::clone(&lifecycle_observed);
        let root_before_lifecycle_claim = worktree_root.clone();
        let preflight_before_lifecycle_claim = Arc::clone(&preflight_observed);
        broker.worktree_lifecycle_test_observer = Some(Arc::new(move |main_root| {
            assert_eq!(main_root, lifecycle_repository_root);
            assert!(
                preflight_before_lifecycle_claim.load(Ordering::SeqCst),
                "submit must finish refresh and preflight before claiming lifecycle"
            );
            assert!(
                !root_before_lifecycle_claim.exists(),
                "submit prepared its root before the lifecycle claim"
            );
            lifecycle_observed_by_hook.store(true, Ordering::SeqCst);
        }));
        broker.submission_plan_test_observer = Some(Arc::new(move |phase| {
            if phase == crate::broker::SubmissionPlanTestPhase::PreflightPlanReady {
                preflight_observed_by_hook.store(true, Ordering::SeqCst);
                // The non-root preflight has finished, but submit must not
                // reserve or bind a root until its final checkout check passes.
                let mut observer = Broker::open_with_options(
                    &observer_root,
                    crate::BrokerOpenOptions::default()
                        .with_worktree_root(observer_worktree_root.clone()),
                )
                .unwrap();
                assert_eq!(
                    observer
                        .store()
                        .repository_worktree_binding()
                        .unwrap()
                        .state,
                    fresh_unbound
                );
                assert!(observer.store().merge_queue().unwrap().is_empty());
            }
        }));

        let outcome = broker
            .submit_with_intent(
                session_id,
                crate::gates::CachePolicy::Bypass,
                crate::PromotionIntent::VerifyOnly,
            )
            .unwrap();

        assert!(
            preflight_observed.load(Ordering::SeqCst),
            "submit must reach the preflight boundary"
        );
        assert!(
            lifecycle_observed.load(Ordering::SeqCst),
            "fresh root preparation must claim the repository lifecycle lock"
        );
        assert_eq!(outcome.entry.session_id, session_id);
        assert_eq!(outcome.entry.head_commit, expected_head);
        assert_eq!(
            broker.store().repository_worktree_binding().unwrap().state,
            crate::store::repository_binding::RepositoryWorktreeBindingState::Bound
        );
        assert!(
            broker
                .store()
                .merge_queue()
                .unwrap()
                .iter()
                .any(|entry| entry.id == outcome.entry.id)
        );
    }

    #[test]
    fn submit_refuses_legacy_unbound_before_planning_or_queueing() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };

        let (_tmp, mut broker, plan) = fixture_with_legacy_binding();
        assert_eq!(
            broker.store().repository_worktree_binding().unwrap().state,
            crate::store::repository_binding::RepositoryWorktreeBindingState::LegacyUnbound
        );
        let preflight_observed = Arc::new(AtomicBool::new(false));
        let preflight_observed_by_hook = Arc::clone(&preflight_observed);
        broker.submission_plan_test_observer = Some(Arc::new(move |phase| {
            if phase == crate::broker::SubmissionPlanTestPhase::PreflightPlanReady {
                preflight_observed_by_hook.store(true, Ordering::SeqCst);
            }
        }));

        let result = broker.submit_with_intent(
            plan.session_id,
            crate::gates::CachePolicy::Bypass,
            crate::PromotionIntent::VerifyOnly,
        );

        assert!(matches!(
            result,
            Err(BrokerOpError::WorktreeRootUnavailable { .. })
        ));
        assert!(
            !preflight_observed.load(Ordering::SeqCst),
            "legacy-unbound submit must refuse before planning"
        );
        assert_eq!(
            broker.store().repository_worktree_binding().unwrap().state,
            crate::store::repository_binding::RepositoryWorktreeBindingState::LegacyUnbound
        );
        assert!(broker.store().merge_queue().unwrap().is_empty());
    }

    #[test]
    fn rejected_expected_head_does_not_bind_a_fresh_worktree_root() {
        let (_tmp, mut broker, plan) = fixture();
        let wrong_head = format!("{}-moved", plan.session_head);

        let error = broker
            .submit_expecting_head(
                plan.session_id,
                crate::gates::CachePolicy::Bypass,
                crate::PromotionIntent::VerifyOnly,
                &wrong_head,
            )
            .unwrap_err();

        assert!(matches!(error, BrokerOpError::CapturedHeadMoved { .. }));
        assert_eq!(
            broker.store().repository_worktree_binding().unwrap().state,
            crate::store::repository_binding::RepositoryWorktreeBindingState::FreshUnbound
        );
        assert!(broker.store().merge_queue().unwrap().is_empty());
    }

    #[test]
    fn prepared_submission_plan_rebuilds_after_base_or_session_inputs_move() {
        let (tmp, mut broker, original) = fixture();
        let root = tmp.path();
        let session = broker.store().session(original.session_id).unwrap();
        let entry = broker
            .store()
            .submit(
                session.id,
                &original.session_head,
                &original.integration_head,
            )
            .unwrap();
        let base = original.integration_head.clone();

        let unchanged = build_or_reuse_submission_plan(
            broker.repo_handle(),
            Some(PreparedSubmissionPlan::new(
                &session,
                &original.session_head,
                &base,
                original.clone(),
            )),
            &entry,
            &session,
            &original.session_head,
            &base,
            || panic!("unchanged inputs must reuse the preflight plan"),
        )
        .unwrap();
        assert_eq!(unchanged, original);

        for (label, changed_session, changed_head, changed_base) in [
            (
                "session id",
                {
                    let mut changed = session.clone();
                    changed.id += 1;
                    changed
                },
                original.session_head.clone(),
                base.clone(),
            ),
            (
                "accepted checkpoint",
                {
                    let mut changed = session.clone();
                    changed.accepted_session_head = Some("new accepted checkpoint".into());
                    changed
                },
                original.session_head.clone(),
                base.clone(),
            ),
            (
                "diff base",
                {
                    let mut changed = session.clone();
                    changed.diff_base = Some("new diff base".into());
                    changed
                },
                original.session_head.clone(),
                base.clone(),
            ),
            (
                "adoption base",
                {
                    let mut changed = session.clone();
                    changed.adoption_base = Some("new adoption base".into());
                    changed
                },
                original.session_head.clone(),
                base.clone(),
            ),
            (
                "session head",
                session.clone(),
                format!("{}-moved", original.session_head),
                base.clone(),
            ),
            (
                "integration base",
                session.clone(),
                original.session_head.clone(),
                format!("{}-moved", base),
            ),
        ] {
            let build_calls = std::cell::Cell::new(0);
            let rebuilt = build_or_reuse_submission_plan(
                broker.repo_handle(),
                Some(PreparedSubmissionPlan::new(
                    &session,
                    &original.session_head,
                    &base,
                    original.clone(),
                )),
                &entry,
                &changed_session,
                &changed_head,
                &changed_base,
                || {
                    build_calls.set(build_calls.get() + 1);
                    Ok(original.clone())
                },
            )
            .unwrap();
            assert_eq!(build_calls.get(), 1, "{label} changes require a fresh plan");
            assert_eq!(rebuilt, original);
        }

        // Move the actual integration ref after the cached plan was prepared.
        // The caller's freshly resolved base must therefore take the builder
        // path and produce provenance bound to that new commit.
        git(root, &["switch", "-q", "main"]);
        edit(root, "fresh-integration-base.txt", "fresh base\n");
        let moved_base = git(root, &["rev-parse", "HEAD"]);
        git(
            root,
            &["update-ref", "refs/heads/aethyme/integration", &moved_base],
        );
        assert_eq!(
            broker
                .repo_handle()
                .resolve_ref("refs/heads/aethyme/integration")
                .as_deref(),
            Some(moved_base.as_str())
        );
        let build_calls = std::cell::Cell::new(0);
        let rebuilt = build_or_reuse_submission_plan(
            broker.repo_handle(),
            Some(PreparedSubmissionPlan::new(
                &session,
                &original.session_head,
                &base,
                original.clone(),
            )),
            &entry,
            &session,
            &entry.head_commit,
            &moved_base,
            || {
                build_calls.set(build_calls.get() + 1);
                broker.build_submission_plan(&session, &entry.head_commit, &moved_base)
            },
        )
        .unwrap();
        assert_eq!(build_calls.get(), 1);
        assert_eq!(rebuilt.integration_head, moved_base);
    }

    #[test]
    fn prepared_submission_plan_rebuilds_when_git_history_view_is_overridden() {
        let (tmp, mut broker, original) = fixture();
        let root = tmp.path().canonicalize().unwrap();
        git(
            &root,
            &["remote", "add", "origin", "git@github.com:example/repo.git"],
        );
        let session = broker.store().session(original.session_id).unwrap();
        let base = original.integration_head.clone();
        let entry = broker
            .store()
            .submit(session.id, &original.session_head, &base)
            .unwrap();
        assert!(
            broker
                .repo_handle()
                .has_complete_unmodified_history()
                .unwrap(),
            "fixture starts with the ordinary Git history view"
        );
        assert!(
            try_submission_plan_reuse_lock(&root, broker.repo_handle(), broker.store_ref())
                .is_some(),
            "a complete history with canonical origin should allow plan reuse"
        );

        // Replace the pinned base with a same-tree root commit. The plan's
        // object IDs remain unchanged, but Git now exposes a different graph.
        let replacement = broker
            .repo_handle()
            .commit_tree(
                &broker.repo_handle().commit_tree_id(&base).unwrap(),
                &[],
                "same-tree history-view replacement",
                &crate::attribution::Attribution::broker_only(),
            )
            .unwrap();
        git(&root, &["replace", &base, &replacement]);
        assert!(
            !broker
                .repo_handle()
                .has_complete_unmodified_history()
                .unwrap()
        );
        assert!(
            try_submission_plan_reuse_lock(&root, broker.repo_handle(), broker.store_ref())
                .is_none(),
            "a persistent replacement ref must disable the reuse lease"
        );

        let build_calls = std::cell::Cell::new(0);
        let rebuilt = build_or_reuse_submission_plan(
            broker.repo_handle(),
            Some(PreparedSubmissionPlan::new(
                &session,
                &original.session_head,
                &base,
                original.clone(),
            )),
            &entry,
            &session,
            &entry.head_commit,
            &base,
            || {
                build_calls.set(build_calls.get() + 1);
                Ok(original.clone())
            },
        )
        .unwrap();

        assert_eq!(
            build_calls.get(),
            1,
            "history overrides require a fresh plan"
        );
        assert_eq!(rebuilt, original);
    }

    fn wait_for_writer_waiter(
        root: &Path,
        repository: &str,
        session_id: i64,
        timeout: std::time::Duration,
    ) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let waiting = crate::waiters::current_waiters(root, crate::clock::epoch_ms())
                .into_iter()
                .any(|waiter| {
                    waiter.alive
                        && waiter.session_id == Some(session_id)
                        && waiter.kind == crate::waiters::WAIT_COORDINATED_WRITE_LOCK
                        && waiter.resource == repository
                });
            if waiting {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    fn try_reuse_lock_is_busy(root: &Path, repo: &GitRepo) -> Result<bool, String> {
        match crate::operations::try_acquire_local_repository_write_lock(root, repo) {
            Ok(None) => Ok(true),
            Ok(Some((_repository, lock))) => {
                drop(lock);
                Ok(false)
            }
            Err(error) => Err(error.to_string()),
        }
    }

    fn join_before<T>(
        handle: std::thread::JoinHandle<T>,
        timeout: std::time::Duration,
        worker: &str,
    ) -> Result<T, String> {
        let deadline = std::time::Instant::now() + timeout;
        while !handle.is_finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if !handle.is_finished() {
            return Err(format!(
                "{worker} thread did not finish before its deadline"
            ));
        }
        handle
            .join()
            .map_err(|_| format!("{worker} thread panicked"))
    }

    #[test]
    fn submit_holds_the_reuse_lock_until_base_tree_capture_before_coordinated_replace() {
        use std::sync::{Arc, Mutex, mpsc};
        use std::time::Duration;

        let managed_root_owner = tempfile::tempdir().unwrap();
        let managed_root = managed_root_owner.path().join("worktrees");
        let (tmp, mut submit_broker, plan) = fixture_with_worktree_root(Some(managed_root));
        let owner = Arc::new(tmp);
        let root = owner.path().canonicalize().unwrap();
        git(
            &root,
            &["remote", "add", "origin", "git@github.com:example/repo.git"],
        );
        let session_id = plan.session_id;
        let mut writer_broker = Broker::open(&root).unwrap();
        let checkout = GitRepo::discover(&root).unwrap();
        // This writer queues at the common-directory lane before the remote
        // compatibility lane. Observe the actual held lock, not the old lane.
        let repository =
            crate::operations::local_git_coordination_lock_key(checkout.root()).unwrap();
        let initial_reuse_lock =
            try_submission_plan_reuse_lock(&root, &checkout, submit_broker.store_ref())
                .expect("the fixture has a canonical origin and no unresolved writer");
        drop(initial_reuse_lock);
        let base = plan.integration_head.clone();
        let base_tree = checkout.commit_tree_id(&base).unwrap();
        let replacement = checkout
            .commit_tree(
                &base_tree,
                &[],
                "same-tree replacement from coordinated writer",
                &crate::attribution::Attribution::broker_only(),
            )
            .unwrap();
        let replace_ref = format!("refs/replace/{base}");
        assert!(checkout.resolve_ref(&replace_ref).is_none());

        let (phase_sender, phase_receiver) = mpsc::channel();
        let (resume_sender, resume_receiver) = mpsc::channel();
        let resume_receiver = Arc::new(Mutex::new(resume_receiver));
        submit_broker.submission_plan_test_observer = Some(Arc::new(move |phase| {
            let _ = phase_sender.send(phase);
            if let Ok(receiver) = resume_receiver.lock() {
                let _ = receiver.recv_timeout(Duration::from_secs(8));
            }
        }));
        let submit_owner = Arc::clone(&owner);
        let submit = std::thread::Builder::new()
            .name("submit-with-reuse-lock-test".into())
            .spawn(move || {
                let _owner = submit_owner;
                submit_broker.submit_with_intent(
                    session_id,
                    crate::gates::CachePolicy::Bypass,
                    crate::PromotionIntent::VerifyOnly,
                )
            })
            .unwrap();

        let mut failures = Vec::new();
        let mut writer: Option<std::thread::JoinHandle<Result<_, _>>> = None;
        let first_phase = phase_receiver.recv_timeout(Duration::from_secs(6));
        if first_phase != Ok(crate::broker::SubmissionPlanTestPhase::PreflightPlanReady) {
            failures.push(format!(
                "submit did not pause after its preflight plan: {first_phase:?}"
            ));
        } else {
            let writer_owner = Arc::clone(&owner);
            let request = crate::CoordinatedCommand {
                session_id,
                provider: crate::OperationProvider::Git,
                repository: None,
                resolved_target: None,
                scope: None,
                declared_effect: Some(crate::OperationEffect::Write),
                destructive_confirmed: false,
                cross_session: None,
                ref_write_acknowledged: false,
                authorization_reason: Some(
                    "serialize the test replacement against submission provenance".into(),
                ),
                args: vec!["replace".into(), base.clone(), replacement.clone()],
            };
            match std::thread::Builder::new()
                .name("coordinated-replace-writer-test".into())
                .spawn(move || {
                    let _owner = writer_owner;
                    writer_broker
                        .run_coordinated_operation_with_wait(request, crate::QueueWait::Seconds(8))
                }) {
                Ok(handle) => writer = Some(handle),
                Err(error) => failures.push(format!("could not start coordinated writer: {error}")),
            }
            if let Some(handle) = writer.as_ref() {
                if !wait_for_writer_waiter(&root, &repository, session_id, Duration::from_secs(3)) {
                    failures.push(
                        "coordinated writer never appeared in the repository lock waiter list"
                            .into(),
                    );
                }
                if handle.is_finished() {
                    failures
                        .push("coordinated writer finished before submit released the lock".into());
                }
                match try_reuse_lock_is_busy(&root, &checkout) {
                    Ok(true) => {}
                    Ok(false) => failures.push(
                        "submit's shared repository lock was available during preflight pause"
                            .into(),
                    ),
                    Err(error) => failures.push(format!("could not probe reuse lock: {error}")),
                }
                if checkout.resolve_ref(&replace_ref).is_some() {
                    failures
                        .push("replace ref appeared before submit captured its base tree".into());
                }
            }
        }
        let _ = resume_sender.send(());

        for expected in [
            crate::broker::SubmissionPlanTestPhase::ConsumerStarting,
            crate::broker::SubmissionPlanTestPhase::BaseTreeCaptured,
        ] {
            match phase_receiver.recv_timeout(Duration::from_secs(6)) {
                Ok(actual) => {
                    if actual != expected {
                        failures.push(format!(
                            "submit phase order changed: expected {expected:?}, observed {actual:?}"
                        ));
                    }
                    if let Some(handle) = writer.as_ref() {
                        let waiting =
                            crate::waiters::current_waiters(&root, crate::clock::epoch_ms())
                                .into_iter()
                                .any(|waiter| {
                                    waiter.alive
                                        && waiter.session_id == Some(session_id)
                                        && waiter.kind
                                            == crate::waiters::WAIT_COORDINATED_WRITE_LOCK
                                        && waiter.resource == repository
                                });
                        if !waiting || handle.is_finished() {
                            failures.push(format!(
                                "coordinated writer was not still blocked at {actual:?}"
                            ));
                        }
                        match try_reuse_lock_is_busy(&root, &checkout) {
                            Ok(true) => {}
                            Ok(false) => failures.push(format!(
                                "submit's shared repository lock was available at {actual:?}"
                            )),
                            Err(error) => failures
                                .push(format!("could not probe reuse lock at {actual:?}: {error}")),
                        }
                        if checkout.resolve_ref(&replace_ref).is_some() {
                            failures.push(format!(
                                "replace ref appeared before submit released the lock at {actual:?}"
                            ));
                        }
                    }
                    let _ = resume_sender.send(());
                }
                Err(error) => failures.push(format!(
                    "submit did not reach expected phase {expected:?}: {error}"
                )),
            }
        }

        // Release any phase callbacks the test failed to observe before
        // joining. Each callback has a timeout as a backstop, but explicit
        // tokens keep failure paths from leaving either test thread paused.
        for _ in 0..3 {
            let _ = resume_sender.send(());
        }

        match join_before(submit, Duration::from_secs(20), "submit") {
            Ok(Ok(_outcome)) => {}
            Ok(Err(error)) => failures.push(format!("submit failed: {error}")),
            Err(error) => failures.push(error),
        }
        match writer {
            Some(handle) => match join_before(handle, Duration::from_secs(20), "writer") {
                Ok(Ok(report)) if report.ok() => {}
                Ok(Ok(report)) => failures.push(format!(
                    "coordinated replace did not succeed: {:?}",
                    report.operation.status
                )),
                Ok(Err(error)) => failures.push(format!("coordinated replace failed: {error}")),
                Err(error) => failures.push(error),
            },
            None => failures.push("coordinated writer was not started".into()),
        }
        if checkout.resolve_ref(&replace_ref).is_none() {
            failures.push(
                "coordinated writer did not install the replacement ref after release".into(),
            );
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn submission_plan_reuse_falls_back_without_canonical_origin_or_when_lock_is_busy() {
        let (tmp, broker, _plan) = fixture();
        let root = tmp.path().canonicalize().unwrap();
        git(
            &root,
            &["remote", "add", "origin", "git@github.com:example/repo.git"],
        );
        let checkout = GitRepo::discover(&root).unwrap();
        let (repository, held) =
            crate::operations::try_acquire_local_repository_write_lock(&root, &checkout)
                .unwrap()
                .expect("fixture origin provides the canonical writer key");
        assert_eq!(repository, "github.com/example/repo");
        assert!(
            try_submission_plan_reuse_lock(&root, &checkout, broker.store_ref()).is_none(),
            "a busy coordinated writer must make optional plan reuse fall back"
        );
        drop(held);
        git(&root, &["remote", "remove", "origin"]);
        let checkout_without_origin = GitRepo::discover(&root).unwrap();
        assert!(
            try_submission_plan_reuse_lock(&root, &checkout_without_origin, broker.store_ref())
                .is_none(),
            "without a canonical origin, submit must not guess a lock key"
        );
    }

    #[test]
    fn submission_plan_reuse_refuses_unresolved_repo_wide_writers_but_not_failed_rows() {
        use crate::{
            NewCoordinatedOperation, OperationEffect, OperationIdentityProvenance,
            OperationProvider, OperationStatus,
        };

        for status in [
            OperationStatus::Prepared,
            OperationStatus::Running,
            OperationStatus::OutcomeUnknown,
        ] {
            let (tmp, mut broker, plan) = fixture();
            let root = tmp.path().canonicalize().unwrap();
            git(
                &root,
                &["remote", "add", "origin", "git@github.com:example/repo.git"],
            );
            let checkout = GitRepo::discover(&root).unwrap();
            let repository = checkout
                .resolve_remote_target("origin", None)
                .unwrap()
                .coordination_key;
            let session_id = plan.session_id;
            let operation = broker
                .store()
                .create_coordinated_operation(&NewCoordinatedOperation {
                    session_id,
                    provider: OperationProvider::Git,
                    repository,
                    scope: "repository".into(),
                    effect: OperationEffect::Write,
                    authorization_reason: Some("seed an unresolved test writer".into()),
                    command_json: r#"["replace","old","new"]"#.into(),
                    pid: i64::from(std::process::id()),
                    host_operation_id: None,
                    identity_provenance: OperationIdentityProvenance::VerifiedCanonical,
                })
                .unwrap();
            if status != OperationStatus::Prepared {
                broker
                    .store()
                    .transition_coordinated_operation(
                        operation.id,
                        OperationStatus::Running,
                        None,
                        None,
                    )
                    .unwrap();
            }
            if status == OperationStatus::OutcomeUnknown {
                broker
                    .store()
                    .transition_coordinated_operation(
                        operation.id,
                        OperationStatus::OutcomeUnknown,
                        None,
                        None,
                    )
                    .unwrap();
            }
            let lock = try_submission_plan_reuse_lock(&root, &checkout, broker.store_ref());
            if status == OperationStatus::Prepared {
                assert!(
                    lock.is_some(),
                    "a queued Prepared operation has not spawned a Git writer"
                );
                drop(lock);
            } else {
                assert!(
                    lock.is_none(),
                    "{status:?} repository-wide operation may have changed history"
                );
            }
        }

        let (tmp, mut broker, plan) = fixture();
        let root = tmp.path().canonicalize().unwrap();
        git(
            &root,
            &["remote", "add", "origin", "git@github.com:example/repo.git"],
        );
        let checkout = GitRepo::discover(&root).unwrap();
        let repository = checkout
            .resolve_remote_target("origin", None)
            .unwrap()
            .coordination_key;
        let operation = broker
            .store()
            .create_coordinated_operation(&NewCoordinatedOperation {
                session_id: plan.session_id,
                provider: OperationProvider::Git,
                repository,
                scope: "repository".into(),
                effect: OperationEffect::Write,
                authorization_reason: Some("seed a terminal test writer".into()),
                command_json: r#"["replace","old","new"]"#.into(),
                pid: i64::from(std::process::id()),
                host_operation_id: None,
                identity_provenance: OperationIdentityProvenance::VerifiedCanonical,
            })
            .unwrap();
        broker
            .store()
            .transition_coordinated_operation(operation.id, OperationStatus::Running, None, None)
            .unwrap();
        broker
            .store()
            .transition_coordinated_operation(operation.id, OperationStatus::Failed, Some(1), None)
            .unwrap();
        let lock = try_submission_plan_reuse_lock(&root, &checkout, broker.store_ref());
        assert!(
            lock.is_some(),
            "terminal Failed must not veto reuse forever"
        );
        drop(lock);
    }

    #[test]
    fn owned_merge_proof_requires_exact_complete_pending_history() {
        let (_tmp, broker, original) = fixture();
        let merge = original
            .commits
            .iter()
            .position(|c| c.parents.len() == 2)
            .unwrap();
        let mut cases = Vec::new();
        let mut plan = original.clone();
        plan.safe = false;
        cases.push(("unsafe", plan));
        let mut plan = original.clone();
        plan.recorded_baseline = None;
        cases.push(("missing boundary", plan));
        let mut plan = original.clone();
        plan.recorded_baseline = Some(original.session_head.clone());
        cases.push(("boundary outside integration", plan));
        let mut plan = original.clone();
        plan.commits[0].ownership = SubmissionCommitOwnership::InheritedFromRecordedBaseline;
        cases.push(("inherited side ancestor", plan));
        let mut plan = original.clone();
        plan.commits[0].integration_state =
            SubmissionIntegrationState::AlreadyIntegratedByStablePatchIdentity;
        cases.push(("patch identity is not ancestry", plan));
        let mut plan = original.clone();
        plan.commits[merge].parents.reverse();
        cases.push(("ordered parent mismatch", plan));
        let mut plan = original.clone();
        plan.commits.remove(0);
        cases.push(("omitted ancestor", plan));
        let mut plan = original.clone();
        plan.commits.push(plan.commits[0].clone());
        cases.push(("duplicated provenance", plan));
        for (label, plan) in cases {
            assert!(
                !broker.can_preserve_owned_merge_history(&plan).unwrap(),
                "{label}"
            );
        }
        assert!(broker.can_preserve_owned_merge_history(&original).unwrap());
    }

    #[test]
    fn owned_merge_proof_refuses_shallow_replacement_and_grafted_history() {
        for control in ["shallow", "replace", "graft"] {
            let (tmp, broker, plan) = fixture();
            let root = tmp.path();
            match control {
                "shallow" => std::fs::write(
                    root.join(".git/shallow"),
                    format!("{}\n", plan.session_head),
                )
                .unwrap(),
                "replace" => {
                    // Even an unrelated replacement must disable this path;
                    // otherwise another ref could change the history view.
                    let unrelated = broker
                        .repo_handle()
                        .commit_tree(
                            &broker
                                .repo_handle()
                                .commit_tree_id(&plan.integration_head)
                                .unwrap(),
                            &[&plan.integration_head],
                            "unreachable history-view control",
                            &crate::attribution::Attribution::broker_only(),
                        )
                        .unwrap();
                    git(root, &["replace", &unrelated, &plan.integration_head]);
                }
                "graft" => std::fs::write(
                    root.join(".git/info/grafts"),
                    format!("{}\n", plan.session_head),
                )
                .unwrap(),
                _ => unreachable!(),
            }
            assert!(
                !broker.can_preserve_owned_merge_history(&plan).unwrap(),
                "{control}"
            );
        }
    }
}

#[cfg(test)]
mod submission_plan_empty_patch_tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::Duration;

    fn git(root: &Path, args: &[&str]) -> String {
        let mut command = Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key
                .to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("GIT_")
            {
                command.env_remove(key);
            }
        }
        let output = command
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "Empty Patch Test")
            .env("GIT_AUTHOR_EMAIL", "empty-patch@example.invalid")
            .env("GIT_COMMITTER_NAME", "Empty Patch Test")
            .env("GIT_COMMITTER_EMAIL", "empty-patch@example.invalid")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn patch_reuse_broker_options(storage_root: &Path) -> crate::BrokerOpenOptions {
        let host_state = storage_root.join("host-state");
        crate::BrokerOpenOptions::default()
            .with_host_state_dir(&host_state)
            .with_host_cache_dir(storage_root.join("host-cache"))
            .with_host_operation_database(host_state.join("host-operations.db"))
            .with_worktree_root(storage_root.join("worktrees"))
            .with_collaboration_state(storage_root.join("collaboration"))
            .with_preparation_cache_root(storage_root.join("preparation-cache"))
    }

    #[test]
    fn empty_and_no_patch_sessions_do_not_scan_integration_patch_history() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["config", "gc.auto", "0"]);
        let base_file = root.join("base.txt");
        std::fs::write(&base_file, "base\n").unwrap();
        git(&root, &["add", "base.txt"]);
        git(&root, &["commit", "-qm", "base"]);
        let base = git(&root, &["rev-parse", "HEAD"]);

        let mut broker = Broker::open(&root).unwrap();
        let worktree = root.join(".aethyme/worktrees/empty-patch-probe");
        let worktree_arg = worktree.to_string_lossy().into_owned();
        git(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "agent/empty-patch-probe",
                &worktree_arg,
                &base,
            ],
        );
        let session = broker
            .adopt(&worktree, Some("empty patch id regression"))
            .unwrap();
        assert_eq!(session.diff_base.as_deref(), Some(base.as_str()));

        // Make the integration-only candidate's commit and tree readable, but
        // remove its loose content blob. Planning a session with no patch IDs
        // does not need the candidate diff; an unconditional integration patch
        // scan tries to read this blob and fails.
        let integration_file = root.join("integration-only.txt");
        std::fs::write(&integration_file, "integration only\n").unwrap();
        git(&root, &["add", "integration-only.txt"]);
        git(&root, &["commit", "-qm", "integration-only candidate"]);
        let integration_head = git(&root, &["rev-parse", "HEAD"]);
        let blob = git(
            &root,
            &[
                "rev-parse",
                &format!("{integration_head}:integration-only.txt"),
            ],
        );
        let common = GitRepo::discover(&root).unwrap().git_common_dir().unwrap();
        let blob_object = common.join("objects").join(&blob[..2]).join(&blob[2..]);
        assert!(
            blob_object.is_file(),
            "fixture blob should be loose at {}",
            blob_object.display()
        );
        std::fs::remove_file(blob_object).unwrap();

        let empty_plan = broker
            .build_submission_plan(&session, &base, &integration_head)
            .unwrap();
        assert!(empty_plan.safe);
        assert_eq!(empty_plan.recorded_baseline.as_deref(), Some(base.as_str()));
        assert_eq!(empty_plan.session_head, base);
        assert_eq!(empty_plan.integration_head, integration_head);
        assert!(empty_plan.commits.is_empty());
        assert!(empty_plan.warnings.is_empty());
        assert_eq!(
            serde_json::to_value(&empty_plan).unwrap()["commits"],
            serde_json::json!([])
        );

        // A session with a real commit but no first-parent diff has the same
        // empty patch-ID map and must retain its ordinary provenance fields.
        git(
            &worktree,
            &["commit", "--allow-empty", "-qm", "empty session marker"],
        );
        let no_patch_head = git(&worktree, &["rev-parse", "HEAD"]);
        let no_patch_plan = broker
            .build_submission_plan(&session, &no_patch_head, &integration_head)
            .unwrap();
        assert!(no_patch_plan.safe);
        assert_eq!(no_patch_plan.commits.len(), 1);
        let provenance = &no_patch_plan.commits[0];
        assert_eq!(provenance.commit, no_patch_head);
        assert_eq!(
            provenance.ownership,
            SubmissionCommitOwnership::SessionOwned
        );
        assert_eq!(
            provenance.integration_state,
            SubmissionIntegrationState::Pending
        );
        assert_eq!(provenance.patch_id, None);
        assert!(provenance.matching_integration_commits.is_empty());
        let provenance_json = serde_json::to_value(provenance).unwrap();
        assert!(provenance_json["patch_id"].is_null());
        assert_eq!(
            provenance_json["matching_integration_commits"],
            serde_json::json!([])
        );
    }

    #[test]
    fn exact_shared_candidates_reuse_patch_ids_and_unseen_candidates_are_scanned() {
        const ROOT: &str = "AETHYME_EXACT_PATCH_REUSE_TEST_ROOT";
        const MODE: &str = "AETHYME_EXACT_PATCH_REUSE_TEST_MODE";
        const SESSION_HEAD: &str = "AETHYME_EXACT_PATCH_REUSE_SESSION_HEAD";
        const INTEGRATION_HEAD: &str = "AETHYME_EXACT_PATCH_REUSE_INTEGRATION_HEAD";
        const SESSION_ID: &str = "AETHYME_EXACT_PATCH_REUSE_SESSION_ID";
        const EXPECTED_MATCHES: &str = "AETHYME_EXACT_PATCH_REUSE_EXPECTED_MATCHES";
        const STORAGE_ROOT: &str = "AETHYME_EXACT_PATCH_REUSE_STORAGE_ROOT";
        const TEST: &str = "merge::submission_plan_empty_patch_tests::exact_shared_candidates_reuse_patch_ids_and_unseen_candidates_are_scanned";

        if let Some(root) = std::env::var_os(ROOT) {
            let root = PathBuf::from(root);
            let mode = std::env::var(MODE).unwrap();
            let session_head = std::env::var(SESSION_HEAD).unwrap();
            let integration_head = std::env::var(INTEGRATION_HEAD).unwrap();
            let session_id = std::env::var(SESSION_ID).unwrap().parse::<i64>().unwrap();
            let expected_matches: Vec<String> =
                serde_json::from_str(&std::env::var(EXPECTED_MATCHES).unwrap()).unwrap();
            let storage_root = PathBuf::from(std::env::var_os(STORAGE_ROOT).unwrap());
            let mut broker =
                Broker::open_with_options(&root, patch_reuse_broker_options(&storage_root))
                    .unwrap();
            let session = broker.store().session(session_id).unwrap();
            let plan = broker
                .build_submission_plan(&session, &session_head, &integration_head)
                .unwrap();

            assert!(plan.safe);
            assert_eq!(plan.commits.len(), 1);
            let provenance = &plan.commits[0];
            assert!(provenance.patch_id.is_some());
            assert_eq!(provenance.matching_integration_commits, expected_matches);
            if mode == "ancestry" || mode == "partial_overlap" {
                assert_eq!(
                    provenance.integration_state,
                    SubmissionIntegrationState::AlreadyIntegratedByAncestry
                );
            } else {
                assert_eq!(mode, "cherry_pick");
                assert_eq!(
                    provenance.integration_state,
                    SubmissionIntegrationState::AlreadyIntegratedByStablePatchIdentity
                );
            }
            return;
        }

        for mode in ["ancestry", "cherry_pick", "partial_overlap"] {
            let repository_owner = tempfile::tempdir().unwrap();
            let root = repository_owner.path().join("repository");
            std::fs::create_dir(&root).unwrap();
            let root = root.canonicalize().unwrap();
            let storage = crate::test_storage::TestStorage::new();
            std::fs::create_dir_all(storage.worktree_root()).unwrap();
            git(&root, &["init", "-q", "-b", "main"]);
            git(&root, &["config", "gc.auto", "0"]);
            std::fs::write(root.join("base.txt"), "base\n").unwrap();
            git(&root, &["add", "base.txt"]);
            git(&root, &["commit", "-qm", "base"]);
            let base = git(&root, &["rev-parse", "HEAD"]);

            let worktree = storage.worktree_root().join("patch-reuse-probe");
            let worktree_arg = worktree.to_string_lossy().into_owned();
            git(
                &root,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    "agent/patch-reuse-probe",
                    &worktree_arg,
                    &base,
                ],
            );
            let mut setup_broker = storage.open(&root).unwrap();
            let session = setup_broker
                .adopt(&worktree, Some("exact patch identity reuse regression"))
                .unwrap();
            std::fs::write(worktree.join("session.txt"), "session patch\n").unwrap();
            git(&worktree, &["add", "session.txt"]);
            git(&worktree, &["commit", "-qm", "session patch"]);
            let session_head = git(&worktree, &["rev-parse", "HEAD"]);
            let (integration_head, expected_matches) = match mode {
                "ancestry" => (session_head.clone(), vec![session_head.clone()]),
                "cherry_pick" => {
                    git(&root, &["cherry-pick", "-x", &session_head]);
                    let integration_head = git(&root, &["rev-parse", "HEAD"]);
                    (integration_head.clone(), vec![integration_head])
                }
                "partial_overlap" => {
                    git(&root, &["merge", "--ff-only", &session_head]);
                    git(&root, &["revert", "--no-edit", &session_head]);
                    git(&root, &["cherry-pick", "-x", &session_head]);
                    let first_equivalent = git(&root, &["rev-parse", "HEAD"]);
                    git(&root, &["revert", "--no-edit", "HEAD"]);
                    git(&root, &["cherry-pick", "-x", &session_head]);
                    let second_equivalent = git(&root, &["rev-parse", "HEAD"]);
                    let integration_head = git(&root, &["rev-parse", "HEAD"]);
                    (
                        integration_head,
                        vec![session_head.clone(), first_equivalent, second_equivalent],
                    )
                }
                _ => unreachable!("known patch reuse fixture mode"),
            };

            let trace = root.join("patch-reuse-trace.json");
            let mut child = Command::new(std::env::current_exe().unwrap());
            child.args(["--exact", TEST, "--nocapture"]);
            for (key, _) in std::env::vars_os() {
                if key
                    .to_string_lossy()
                    .to_ascii_uppercase()
                    .starts_with("GIT_")
                {
                    child.env_remove(key);
                }
            }
            child
                .env(ROOT, &root)
                .env(MODE, mode)
                .env(SESSION_HEAD, &session_head)
                .env(INTEGRATION_HEAD, &integration_head)
                .env(SESSION_ID, session.id.to_string())
                .env(
                    EXPECTED_MATCHES,
                    serde_json::to_string(&expected_matches).unwrap(),
                )
                .env(STORAGE_ROOT, storage.root())
                .env("AETHYME_WORKTREE_ROOT", storage.worktree_root())
                .env("AETHYME_HOST_STATE_DIR", storage.host_state_dir())
                .env("AETHYME_HOST_CACHE_DIR", storage.root().join("host-cache"))
                .env(
                    "AETHYME_PREPARATION_CACHE_ROOT",
                    storage.root().join("preparation-cache"),
                )
                .env(
                    "AETHYME_BROKER_OPEN_HOST_OPERATION_DB",
                    storage.host_state_dir().join("host-operations.db"),
                )
                .env(
                    "AETHYME_BROKER_OPEN_COLLABORATION_ROOT",
                    storage.root().join("collaboration"),
                )
                .env_remove("AETHYME_BROKER_DB")
                .env_remove("AETHYME_GATE_BROKER_DATABASES")
                .env_remove("AETHYME_BROKER_OPEN_HOST_STATE_EXPLICIT")
                .env_remove("AETHYME_BROKER_OPEN_WORKTREE_ROOT")
                .env_remove("AETHYME_BROKER_STORAGE_CONTEXT_V1")
                .env("GIT_TRACE2_EVENT", &trace)
                .current_dir(&root);
            let output = match crate::bounded_output::output_limited(
                &mut child,
                Duration::from_secs(30),
                1024 * 1024,
            )
            .unwrap()
            {
                crate::bounded_output::LimitedOutput::Completed(output) => output,
                crate::bounded_output::LimitedOutput::Deadline => {
                    panic!("patch reuse test child exceeded its 30-second budget ({mode})")
                }
                crate::bounded_output::LimitedOutput::OutputLimit => {
                    panic!("patch reuse test child exceeded its 1 MiB output cap ({mode})")
                }
            };
            assert!(
                output.status.success(),
                "mode={mode}\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );

            let events = std::fs::read_to_string(trace).unwrap();
            let (mut patch_logs, mut patch_id_processes, mut shape_queries, mut exclusive_walks) =
                (0, 0, 0, 0);
            for line in events.lines() {
                let event: serde_json::Value = serde_json::from_str(line).unwrap();
                if event["event"] != "start" {
                    continue;
                }
                let Some(argv) = event["argv"].as_array() else {
                    continue;
                };
                let argv = argv
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<Vec<_>>();
                if argv.windows(5).any(|args| {
                    args[0] == "rev-list"
                        && args[1] == "--reverse"
                        && args[2] == session_head.as_str()
                        && args[3] == "--not"
                        && args[4] == integration_head.as_str()
                }) {
                    exclusive_walks += 1;
                }
                if argv.contains(&"--diff-merges=first-parent") {
                    patch_logs += 1;
                }
                if argv.contains(&"patch-id") {
                    patch_id_processes += 1;
                }
                if argv.contains(&"--format=%H %T %P") {
                    shape_queries += 1;
                }
            }
            assert_eq!(
                exclusive_walks, 1,
                "one exclusive walk for captured heads in {mode}"
            );
            let expected_streams = if mode == "ancestry" { 1 } else { 2 };
            assert_eq!(patch_logs, expected_streams, "log streams for {mode}");
            assert_eq!(
                patch_id_processes, expected_streams,
                "patch-id processes for {mode}"
            );
            assert_eq!(shape_queries, expected_streams, "shape queries for {mode}");
        }
    }
}
