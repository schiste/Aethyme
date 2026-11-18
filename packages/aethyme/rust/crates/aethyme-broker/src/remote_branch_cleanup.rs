//! Broker-owned deletion of merged remote session branches.
//!
//! GitHub's "delete branch on merge" is deliberately off: deleting a branch
//! the moment its pull request merges can pull it out from under an agent
//! still working on it, or out from under a pull request stacked on it. The
//! broker deletes a session's remote branch itself, only when every rule
//! holds:
//!
//! 1. its pull request merged and the remote tip is exactly that pull
//!    request's merged head (nothing was pushed after the merge);
//! 2. every session that worked on the branch is cleaned, not merely merged;
//! 3. no open pull request uses the branch as its head, and any open pull
//!    request using it as its base is first retargeted to the base the merged
//!    pull request landed in, confirmed by a fresh read;
//! 4. no open session is stacked on, or was started from, the branch.
//!
//! The delete is a `git push --force-with-lease=<ref>:<merged head>` of an
//! empty source through the coordinated write path, so it fails if the remote
//! tip moved. GitHub documents that deleting a merged head branch retargets
//! open pull requests based on it, and that deleting a branch associated
//! with an open pull request closes it; it documents neither for deletion by
//! push or the refs API. Rule 3 retargets first, so no open pull request is
//! based on the branch when it is deleted and the answer cannot matter.
//!
//! A local branch may already be gone: nothing here reads or deletes one.
//! The background worker (`gc sweep`) runs a paced, bounded pass; `gc
//! remote-branches plan` previews the same decisions without writing.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::session_stack::{
    GhCliPullRequestReader, PullRequestBaseEffects, PullRequestFact, PullRequestReader,
    PullRequestState, retarget_confirmed,
};
use crate::{Broker, CoordinatedCommand, OperationEffect, OperationProvider, QueueWait};

/// Branches the lane considers per run, by default.
pub(crate) const DEFAULT_SCAN_PER_RUN: usize = 25;
/// Remote deletions per run, by default.
pub(crate) const DEFAULT_MAX_DELETIONS_PER_RUN: usize = 25;
/// The ceiling either per-run bound may be configured to.
pub(crate) const MAX_PER_RUN: usize = 200;
/// The first backoff after a failed run; it doubles per consecutive failure.
const BACKOFF_BASE_MS: i64 = 15 * 60 * 1000;
/// The longest backoff.
const BACKOFF_MAX_MS: i64 = 6 * 60 * 60 * 1000;
const META_RETARGETS: &str = "remote_branch_cleanup.retargets";
const META_FAILURES: &str = "remote_branch_cleanup.failures";
const META_CONSECUTIVE_FAILURES: &str = "remote_branch_cleanup.consecutive_failures";
const META_BACKOFF_UNTIL: &str = "remote_branch_cleanup.backoff_until_ms";
const SESSION_BRANCH_PREFIX: &str = "agent/";

/// `[remote_branches]` in `.aethyme/config.toml`, read from the fetched
/// default branch so a session cannot change it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct RemoteBranchPolicy {
    /// Off unless the repository opts in.
    pub delete_merged: bool,
    pub scan_per_run: usize,
    pub max_deletions_per_run: usize,
}

impl Default for RemoteBranchPolicy {
    fn default() -> Self {
        Self {
            delete_merged: false,
            scan_per_run: DEFAULT_SCAN_PER_RUN,
            max_deletions_per_run: DEFAULT_MAX_DELETIONS_PER_RUN,
        }
    }
}

impl RemoteBranchPolicy {
    pub(crate) fn from_config_text(text: &str) -> Result<Self, String> {
        let value = text
            .parse::<toml::Value>()
            .map_err(|error| format!(".aethyme/config.toml is invalid: {error}"))?;
        let Some(table) = value.get("remote_branches") else {
            return Ok(Self::default());
        };
        let table = table
            .as_table()
            .ok_or_else(|| "[remote_branches] must be a table".to_string())?;
        let mut policy = Self::default();
        for (key, value) in table {
            match key.as_str() {
                "delete_merged" => {
                    policy.delete_merged = value.as_bool().ok_or_else(|| {
                        "[remote_branches] delete_merged must be true or false".to_string()
                    })?;
                }
                "scan_per_run" | "max_deletions_per_run" => {
                    let bound = value
                        .as_integer()
                        .and_then(|bound| usize::try_from(bound).ok())
                        .filter(|bound| (1..=MAX_PER_RUN).contains(bound))
                        .ok_or_else(|| {
                            format!(
                                "[remote_branches] {key} must be an integer from 1 to {MAX_PER_RUN}"
                            )
                        })?;
                    if key == "scan_per_run" {
                        policy.scan_per_run = bound;
                    } else {
                        policy.max_deletions_per_run = bound;
                    }
                }
                other => return Err(format!("[remote_branches] has unsupported key {other:?}")),
            }
        }
        Ok(policy)
    }
}

/// Why a branch is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum KeepReason {
    NotASessionBranch,
    SessionNotCleaned,
    StackedSessionOpen,
    OpenPullRequestUsesBranchAsHead,
    NoMergedPullRequest,
    RemoteTipNotMergedHead,
    LookupFailed,
}

impl KeepReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::NotASessionBranch => "not_a_session_branch",
            Self::SessionNotCleaned => "session_not_cleaned",
            Self::StackedSessionOpen => "stacked_session_open",
            Self::OpenPullRequestUsesBranchAsHead => "open_pull_request_uses_branch_as_head",
            Self::NoMergedPullRequest => "no_merged_pull_request",
            Self::RemoteTipNotMergedHead => "remote_tip_not_merged_head",
            Self::LookupFailed => "lookup_failed",
        }
    }
}

/// An open pull request based on the branch, and where it must point first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PlannedRetarget {
    pub pull_request: i64,
    pub to: String,
}

/// What the rules say about one branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BranchDecision {
    Delete {
        expected_oid: String,
        pr_number: i64,
        retargets: Vec<PlannedRetarget>,
    },
    /// The remote branch is already gone.
    Absent { pr_number: Option<i64> },
    Keep {
        reason: KeepReason,
        detail: Option<String>,
    },
}

/// Everything the rules read about one branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BranchFacts {
    pub session_id: i64,
    pub branch: String,
    pub all_sessions_cleaned: bool,
    pub stacked_open_sessions: Vec<i64>,
    /// `None`: the remote has no such branch.
    pub remote_tip: Option<String>,
    pub head_pull_requests: Result<Vec<PullRequestFact>, String>,
    pub base_pull_requests: Result<Vec<PullRequestFact>, String>,
}

fn keep(reason: KeepReason, detail: Option<String>) -> BranchDecision {
    BranchDecision::Keep { reason, detail }
}

/// Apply the four rules to one branch. Pure: the facts are read elsewhere.
pub(crate) fn decide(facts: &BranchFacts) -> BranchDecision {
    if !facts.branch.starts_with(SESSION_BRANCH_PREFIX) {
        return keep(KeepReason::NotASessionBranch, None);
    }
    if !facts.all_sessions_cleaned {
        return keep(KeepReason::SessionNotCleaned, None);
    }
    if !facts.stacked_open_sessions.is_empty() {
        let ids: Vec<String> = facts
            .stacked_open_sessions
            .iter()
            .map(i64::to_string)
            .collect();
        return keep(
            KeepReason::StackedSessionOpen,
            Some(format!("open session(s) {}", ids.join(", "))),
        );
    }
    let head_prs = match &facts.head_pull_requests {
        Ok(prs) => prs,
        Err(error) => return keep(KeepReason::LookupFailed, Some(error.clone())),
    };
    let own = || head_prs.iter().filter(|pr| pr.head_branch == facts.branch);
    if let Some(open) = own().find(|pr| pr.state == PullRequestState::Open) {
        return keep(
            KeepReason::OpenPullRequestUsesBranchAsHead,
            Some(format!("#{}", open.number)),
        );
    }
    let newest_merged = own()
        .filter(|pr| pr.state == PullRequestState::Merged)
        .max_by_key(|pr| pr.number);
    let Some(tip) = &facts.remote_tip else {
        return BranchDecision::Absent {
            pr_number: newest_merged.map(|pr| pr.number),
        };
    };
    if newest_merged.is_none() {
        return keep(KeepReason::NoMergedPullRequest, None);
    }
    let Some(merged) = own()
        .filter(|pr| pr.state == PullRequestState::Merged && &pr.head_oid == tip)
        .max_by_key(|pr| pr.number)
    else {
        return keep(
            KeepReason::RemoteTipNotMergedHead,
            Some(format!("remote tip {tip}")),
        );
    };
    let base_prs = match &facts.base_pull_requests {
        Ok(prs) => prs,
        Err(error) => return keep(KeepReason::LookupFailed, Some(error.clone())),
    };
    let retargets = base_prs
        .iter()
        .filter(|pr| pr.state == PullRequestState::Open && pr.base_branch == facts.branch)
        .map(|pr| PlannedRetarget {
            pull_request: pr.number,
            to: merged.base_branch.clone(),
        })
        .collect();
    BranchDecision::Delete {
        expected_oid: tip.clone(),
        pr_number: merged.number,
        retargets,
    }
}

/// The writes the executor makes, behind a seam tests replace.
pub(crate) trait CleanupEffects: PullRequestBaseEffects {
    /// Delete `refs/heads/<branch>` only if the remote holds `expected_oid`.
    fn delete_remote_branch(
        &mut self,
        session_id: i64,
        branch: &str,
        expected_oid: &str,
    ) -> Result<(), String>;
    /// The live session that owns `branch`, for attributing a retarget base.
    fn branch_owner(&self, branch: &str) -> Option<i64>;
}

/// One retarget made (or planned) before a deletion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetargetReport {
    pub pull_request: i64,
    pub to: String,
    /// `retargeted`, `already_targeted`, `failed` or `planned`.
    pub outcome: String,
}

/// What happened to one branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CleanupItemReport {
    pub session_id: i64,
    pub branch: String,
    /// `deleted`, `absent`, `kept`, `failed`, `would_delete`, or `deferred`
    /// (bounded out of this run, or after the run stopped).
    pub outcome: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_oid: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub retargets: Vec<RetargetReport>,
}

/// The executor's result: each item's outcome, and why the run stopped early.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Execution {
    pub items: Vec<CleanupItemReport>,
    pub stopped: Option<String>,
    pub retargets: u64,
}

fn item(
    session_id: i64,
    branch: &str,
    outcome: &str,
    reason: &str,
    detail: Option<String>,
) -> CleanupItemReport {
    CleanupItemReport {
        session_id,
        branch: branch.to_string(),
        outcome: outcome.to_string(),
        reason: reason.to_string(),
        detail,
        pr_number: None,
        expected_oid: None,
        retargets: Vec::new(),
    }
}

/// Carry out (or, without `apply`, only describe) the decisions in order:
/// retarget every based pull request and confirm it, then delete under the
/// lease. At most `max_deletions` deletions; the first failure stops the run
/// so the caller backs off, and every later deletion is deferred.
pub(crate) fn execute<E: CleanupEffects + ?Sized>(
    effects: &mut E,
    repository: &str,
    decisions: Vec<(i64, String, BranchDecision)>,
    max_deletions: usize,
    apply: bool,
) -> Execution {
    let mut items = Vec::new();
    let mut stopped: Option<String> = None;
    let mut deletions = 0;
    let mut retarget_count = 0;
    for (session_id, branch, decision) in decisions {
        match decision {
            BranchDecision::Keep { reason, detail } => {
                items.push(item(session_id, &branch, "kept", reason.as_str(), detail));
            }
            BranchDecision::Absent { pr_number } => {
                let mut absent = item(session_id, &branch, "absent", "remote_absent", None);
                absent.pr_number = pr_number;
                items.push(absent);
            }
            BranchDecision::Delete {
                expected_oid,
                pr_number,
                retargets,
            } => {
                let mut report = item(session_id, &branch, "would_delete", "merged", None);
                report.pr_number = Some(pr_number);
                report.expected_oid = Some(expected_oid.clone());
                report.retargets = retargets
                    .iter()
                    .map(|planned| RetargetReport {
                        pull_request: planned.pull_request,
                        to: planned.to.clone(),
                        outcome: "planned".into(),
                    })
                    .collect();
                if !apply {
                    items.push(report);
                    continue;
                }
                if stopped.is_some() || deletions >= max_deletions {
                    report.outcome = "deferred".into();
                    report.reason = if stopped.is_some() {
                        "run_stopped".into()
                    } else {
                        "run_bound_reached".into()
                    };
                    items.push(report);
                    continue;
                }
                let mut failure = None;
                for retarget in &mut report.retargets {
                    let owner = effects.branch_owner(&retarget.to);
                    match retarget_confirmed(
                        effects,
                        session_id,
                        repository,
                        retarget.pull_request,
                        &retarget.to,
                        owner,
                    ) {
                        Ok(outcome) => {
                            retarget.outcome = match outcome {
                                crate::session_stack::RetargetOutcome::Retargeted => {
                                    retarget_count += 1;
                                    "retargeted".into()
                                }
                                crate::session_stack::RetargetOutcome::AlreadyTargeted => {
                                    "already_targeted".into()
                                }
                            };
                        }
                        Err(error) => {
                            retarget.outcome = "failed".into();
                            failure = Some(format!(
                                "retargeting #{} to {} was not confirmed: {error}",
                                retarget.pull_request, retarget.to
                            ));
                            break;
                        }
                    }
                }
                let result = match failure {
                    Some(failure) => Err(failure),
                    None => effects.delete_remote_branch(session_id, &branch, &expected_oid),
                };
                match result {
                    Ok(()) => {
                        deletions += 1;
                        report.outcome = "deleted".into();
                    }
                    Err(error) => {
                        report.outcome = "failed".into();
                        report.reason = "write_failed".into();
                        report.detail = Some(error.clone());
                        stopped = Some(error);
                    }
                }
                items.push(report);
            }
        }
    }
    Execution {
        items,
        stopped,
        retargets: retarget_count,
    }
}

/// Totals over every recorded decision and the lane's counters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CleanupTotals {
    pub deleted: u64,
    pub absent: u64,
    pub kept_by_reason: BTreeMap<String, u64>,
    pub failed: u64,
    pub retargets: u64,
    pub failures: u64,
}

/// The report of one pass or preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RemoteBranchCleanupReport {
    pub schema: &'static str,
    pub enabled: bool,
    /// False for a preview: nothing was written.
    pub applied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// Cleaned session branches still awaiting a final decision.
    pub backlog: u64,
    pub scan_per_run: usize,
    pub max_deletions_per_run: usize,
    pub items: Vec<CleanupItemReport>,
    /// This run's items by outcome.
    pub outcomes: BTreeMap<String, u64>,
    /// This run's kept items by reason.
    pub kept_by_reason: BTreeMap<String, u64>,
    pub totals: CleanupTotals,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
}

/// How long to wait after `consecutive` failed runs.
pub(crate) fn backoff_ms(consecutive: u32) -> i64 {
    let exponent = consecutive.saturating_sub(1).min(16);
    BACKOFF_BASE_MS
        .saturating_mul(1_i64 << exponent)
        .min(BACKOFF_MAX_MS)
}

/// The production effects: coordinated writes made on behalf of the cleaned
/// session whose branch is deleted.
struct BrokerCleanupEffects<'a> {
    broker: &'a mut Broker,
    reader: &'a dyn PullRequestReader,
    remote: String,
    target: crate::ResolvedRemoteTarget,
}

impl PullRequestBaseEffects for BrokerCleanupEffects<'_> {
    fn current_base(&self, repository: &str, number: i64) -> Result<String, String> {
        self.reader
            .pull_request(repository, number)
            .map(|pr| pr.base_branch)
    }

    fn set_base(
        &mut self,
        session_id: i64,
        repository: &str,
        number: i64,
        base: &str,
        cross_session: Option<i64>,
    ) -> Result<(), String> {
        crate::session_stack::BrokerPullRequestBases {
            broker: &mut *self.broker,
            reader: self.reader,
        }
        .set_base(session_id, repository, number, base, cross_session)
    }
}

impl CleanupEffects for BrokerCleanupEffects<'_> {
    fn delete_remote_branch(
        &mut self,
        session_id: i64,
        branch: &str,
        expected_oid: &str,
    ) -> Result<(), String> {
        let branch_ref = format!("refs/heads/{branch}");
        let main_root = self.broker.main_root().to_path_buf();
        let report = self
            .broker
            .run_coordinated_operation_at_with_wait(
                CoordinatedCommand {
                    session_id,
                    provider: OperationProvider::Git,
                    repository: None,
                    resolved_target: Some(self.target.clone()),
                    scope: Some(format!("remote-branch-cleanup:{branch_ref}")),
                    declared_effect: Some(OperationEffect::Destructive),
                    // The lease is the confirmation: the delete can only
                    // remove the exact merged head of a cleaned session.
                    destructive_confirmed: true,
                    cross_session: None,
                    ref_write_acknowledged: false,
                    authorization_reason: Some(
                        "repository policy remote_branches.delete_merged".into(),
                    ),
                    args: vec![
                        "push".into(),
                        format!("--force-with-lease={branch_ref}:{expected_oid}"),
                        self.remote.clone(),
                        format!(":{branch_ref}"),
                    ],
                },
                &main_root,
                QueueWait::Seconds(60),
            )
            .map_err(|error| error.to_string())?;
        if report.ok() {
            Ok(())
        } else {
            Err(format!(
                "deleting {branch} failed (operation {}, {})",
                report.operation.id,
                report.operation.status.as_str()
            ))
        }
    }

    fn branch_owner(&self, branch: &str) -> Option<i64> {
        self.broker.live_branch_owner(branch, 0)
    }
}

impl Broker {
    /// The trusted `[remote_branches]` policy and the default branch it was
    /// read from.
    fn remote_branch_policy(
        &self,
    ) -> Result<(RemoteBranchPolicy, crate::session_push::TrackedDefault), String> {
        let default = crate::session_push::tracked_default(self.repo_handle())
            .ok_or_else(|| "the main checkout tracks no fetched default branch".to_string())?;
        let text = self
            .repo_handle()
            .config_toml_at_commit(&default.commit, "config")
            .map_err(|error| error.to_string())?;
        let policy = match text {
            Some(text) => RemoteBranchPolicy::from_config_text(&text)?,
            None => RemoteBranchPolicy::default(),
        };
        Ok((policy, default))
    }

    /// Read every fact the rules need for up to `limit` candidates.
    fn remote_branch_decisions(
        &self,
        limit: usize,
        remote: &str,
        repository: &str,
        reader: &dyn PullRequestReader,
    ) -> Result<Vec<(i64, String, BranchDecision)>, String> {
        let store = self.store_ref();
        let candidates = store
            .remote_branch_cleanup_candidates(limit)
            .map_err(|error| error.to_string())?;
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let refs: Vec<String> = candidates
            .iter()
            .map(|(_, branch)| format!("refs/heads/{branch}"))
            .collect();
        let tips = self
            .repo_handle()
            .remote_ref_oids(remote, &refs)
            .map_err(|error| format!("reading remote branch tips failed: {error}"))?;
        let mut decisions = Vec::new();
        for (session_id, branch) in candidates {
            let all_sessions_cleaned = store
                .every_session_on_branch_cleaned(&branch)
                .map_err(|error| error.to_string())?;
            let stacked_open_sessions = store
                .open_sessions_stacked_on_branch(&branch)
                .map_err(|error| error.to_string())?;
            let mut facts = BranchFacts {
                session_id,
                remote_tip: tips.get(&format!("refs/heads/{branch}")).cloned().flatten(),
                all_sessions_cleaned,
                stacked_open_sessions,
                head_pull_requests: Ok(Vec::new()),
                base_pull_requests: Ok(Vec::new()),
                branch,
            };
            // Provider reads only where a local rule has not already decided.
            if facts.all_sessions_cleaned && facts.stacked_open_sessions.is_empty() {
                facts.head_pull_requests =
                    reader.pull_requests_with_head(repository, &facts.branch);
                if matches!(decide(&facts), BranchDecision::Delete { .. }) {
                    facts.base_pull_requests =
                        reader.open_pull_requests_with_base(repository, &facts.branch);
                }
            }
            let decision = decide(&facts);
            decisions.push((facts.session_id, facts.branch, decision));
        }
        Ok(decisions)
    }

    fn remote_branch_totals(&self) -> CleanupTotals {
        let store = self.store_ref();
        let mut totals = CleanupTotals::default();
        for (outcome, reason, count) in store.remote_branch_decision_counts().unwrap_or_default() {
            match outcome.as_str() {
                "deleted" => totals.deleted += count,
                "absent" => totals.absent += count,
                "failed" => totals.failed += count,
                _ => *totals.kept_by_reason.entry(reason).or_default() += count,
            }
        }
        let counter = |key: &str| {
            store
                .meta_get(key)
                .ok()
                .flatten()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0)
        };
        totals.retargets = counter(META_RETARGETS);
        totals.failures = counter(META_FAILURES);
        totals
    }

    fn add_to_counter(&mut self, key: &str, by: u64) {
        if by == 0 {
            return;
        }
        let current = self
            .store_ref()
            .meta_get(key)
            .ok()
            .flatten()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        crate::warn_unrecorded(
            "count a remote branch cleanup outcome",
            self.store()
                .meta_set(key, &current.saturating_add(by).to_string()),
        );
    }

    fn empty_cleanup_report(
        &self,
        policy: RemoteBranchPolicy,
        applied: bool,
    ) -> RemoteBranchCleanupReport {
        RemoteBranchCleanupReport {
            schema: "aethyme.remote-branch-cleanup/v1",
            enabled: policy.delete_merged,
            applied,
            repository: None,
            backlog: self
                .store_ref()
                .remote_branch_cleanup_backlog()
                .unwrap_or(0),
            scan_per_run: policy.scan_per_run,
            max_deletions_per_run: policy.max_deletions_per_run,
            items: Vec::new(),
            outcomes: BTreeMap::new(),
            kept_by_reason: BTreeMap::new(),
            totals: self.remote_branch_totals(),
            skipped: None,
            stopped: None,
            next_action: None,
        }
    }

    /// `gc remote-branches plan`: the decisions for up to `limit` candidates,
    /// with nothing written.
    pub(crate) fn plan_remote_branch_cleanup(
        &mut self,
        limit: Option<usize>,
    ) -> RemoteBranchCleanupReport {
        self.remote_branch_cleanup_pass(limit, false, &GhCliPullRequestReader)
    }

    /// The background worker's pass: skipped unless the policy is on and no
    /// backoff is pending; bounded by the policy; backs off after a failure.
    pub(crate) fn run_remote_branch_cleanup(&mut self) -> Option<RemoteBranchCleanupReport> {
        let (policy, _) = self.remote_branch_policy().ok()?;
        if !policy.delete_merged {
            return None;
        }
        let now = crate::clock::epoch_ms();
        let backoff_until = self
            .store_ref()
            .meta_get(META_BACKOFF_UNTIL)
            .ok()
            .flatten()
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        if now < backoff_until {
            let mut report = self.empty_cleanup_report(policy, true);
            report.skipped = Some(format!(
                "backing off after a failed run until {backoff_until} (epoch ms)"
            ));
            return Some(report);
        }
        Some(self.remote_branch_cleanup_pass(None, true, &GhCliPullRequestReader))
    }

    fn remote_branch_cleanup_pass(
        &mut self,
        limit: Option<usize>,
        apply: bool,
        reader: &dyn PullRequestReader,
    ) -> RemoteBranchCleanupReport {
        let (policy, default) = match self.remote_branch_policy() {
            Ok(found) => found,
            Err(error) => {
                let mut report = self.empty_cleanup_report(RemoteBranchPolicy::default(), apply);
                report.skipped = Some(error);
                return report;
            }
        };
        let mut report = self.empty_cleanup_report(policy, apply);
        let target = match self
            .repo_handle()
            .resolve_remote_target(&default.remote, None)
        {
            Ok(target) => target,
            Err(error) => {
                report.skipped = Some(format!("remote {:?}: {error}", default.remote));
                return report;
            }
        };
        report.repository = Some(target.display_slug.clone());
        let limit = limit.unwrap_or(policy.scan_per_run).clamp(1, MAX_PER_RUN);
        let decisions = match self.remote_branch_decisions(
            limit,
            &default.remote,
            &target.display_slug,
            reader,
        ) {
            Ok(decisions) => decisions,
            Err(error) => {
                if apply {
                    self.record_cleanup_failure(&error);
                }
                report.stopped = Some(error);
                return report;
            }
        };
        let repository = target.display_slug.clone();
        let execution = {
            let mut effects = BrokerCleanupEffects {
                broker: &mut *self,
                reader,
                remote: default.remote.clone(),
                target,
            };
            execute(
                &mut effects,
                &repository,
                decisions,
                policy.max_deletions_per_run,
                apply,
            )
        };
        if apply {
            for item in &execution.items {
                if matches!(
                    item.outcome.as_str(),
                    "deleted" | "absent" | "kept" | "failed"
                ) {
                    crate::warn_unrecorded(
                        "record a remote branch cleanup decision",
                        self.store().record_remote_branch_decision(
                            &crate::store::RemoteBranchDecisionRow {
                                session_id: item.session_id,
                                branch: &item.branch,
                                outcome: &item.outcome,
                                reason: &item.reason,
                                pr_number: item.pr_number,
                                expected_oid: item.expected_oid.as_deref(),
                            },
                        ),
                    );
                }
            }
            self.add_to_counter(META_RETARGETS, execution.retargets);
            match &execution.stopped {
                Some(error) => self.record_cleanup_failure(error),
                None => {
                    crate::warn_unrecorded(
                        "clear the remote branch cleanup backoff",
                        self.store().meta_set(META_CONSECUTIVE_FAILURES, "0"),
                    );
                }
            }
            report.backlog = self
                .store_ref()
                .remote_branch_cleanup_backlog()
                .unwrap_or(0);
            report.totals = self.remote_branch_totals();
        }
        for item in &execution.items {
            *report.outcomes.entry(item.outcome.clone()).or_default() += 1;
            if item.outcome == "kept" {
                *report
                    .kept_by_reason
                    .entry(item.reason.clone())
                    .or_default() += 1;
            }
        }
        report.items = execution.items;
        report.stopped = execution.stopped;
        report.next_action = cleanup_next_action(&report);
        report
    }

    fn record_cleanup_failure(&mut self, error: &str) {
        self.add_to_counter(META_FAILURES, 1);
        let consecutive = self
            .store_ref()
            .meta_get(META_CONSECUTIVE_FAILURES)
            .ok()
            .flatten()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0)
            .saturating_add(1);
        let until = crate::clock::epoch_ms().saturating_add(backoff_ms(consecutive));
        crate::warn_unrecorded(
            "record the remote branch cleanup backoff",
            self.store()
                .meta_set(META_CONSECUTIVE_FAILURES, &consecutive.to_string()),
        );
        crate::warn_unrecorded(
            "record the remote branch cleanup backoff",
            self.store()
                .meta_set(META_BACKOFF_UNTIL, &until.to_string()),
        );
        eprintln!("warning: remote branch cleanup stopped and backs off: {error}");
    }

    /// Repository status advice: the backlog, and what to run about it.
    /// `None` when there is no backlog.
    pub(crate) fn remote_branch_cleanup_advice(&self) -> Option<serde_json::Value> {
        let backlog = self.store_ref().remote_branch_cleanup_backlog().ok()?;
        if backlog == 0 {
            return None;
        }
        let enabled = self
            .remote_branch_policy()
            .map(|(policy, _)| policy.delete_merged)
            .unwrap_or(false);
        let next_action = if enabled {
            "the background worker (`aethyme broker gc sweep`) deletes merged ones in bounded \
             batches; preview with `aethyme broker gc remote-branches plan --json`"
        } else {
            "preview what would be deleted with `aethyme broker gc remote-branches plan --json`; \
             enable with `[remote_branches] delete_merged = true` in .aethyme/config.toml on the \
             default branch"
        };
        Some(serde_json::json!({
            "enabled": enabled,
            "backlog": backlog,
            "next_action": next_action,
        }))
    }
}

fn cleanup_next_action(report: &RemoteBranchCleanupReport) -> Option<String> {
    if report.stopped.is_some() {
        return Some(
            "the run stopped at a failed write and backs off; inspect the named operation with \
             `aethyme broker advanced operations` before the next run"
                .into(),
        );
    }
    if !report.enabled && report.outcomes.contains_key("would_delete") {
        return Some(
            "enable deletion with `[remote_branches] delete_merged = true` in \
             .aethyme/config.toml on the default branch"
                .into(),
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const MERGED_HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const LATER: &str = "cccccccccccccccccccccccccccccccccccccccc";

    fn pr(number: i64, state: PullRequestState, head: &str, base: &str) -> PullRequestFact {
        PullRequestFact {
            number,
            state,
            head_branch: head.into(),
            head_oid: MERGED_HEAD.into(),
            base_branch: base.into(),
            merge_commit: None,
        }
    }

    fn mergeable(branch: &str) -> BranchFacts {
        BranchFacts {
            session_id: 1,
            branch: branch.into(),
            all_sessions_cleaned: true,
            stacked_open_sessions: Vec::new(),
            remote_tip: Some(MERGED_HEAD.into()),
            head_pull_requests: Ok(vec![pr(10, PullRequestState::Merged, branch, "main")]),
            base_pull_requests: Ok(Vec::new()),
        }
    }

    fn kept(decision: BranchDecision) -> KeepReason {
        match decision {
            BranchDecision::Keep { reason, .. } => reason,
            other => panic!("expected keep, got {other:?}"),
        }
    }

    #[test]
    fn a_merged_cleaned_unstacked_branch_at_its_merged_head_is_deleted_under_that_lease() {
        assert_eq!(
            decide(&mergeable("agent/a")),
            BranchDecision::Delete {
                expected_oid: MERGED_HEAD.into(),
                pr_number: 10,
                retargets: Vec::new(),
            }
        );
    }

    #[test]
    fn each_rule_keeps_the_branch_on_its_own() {
        // Rule 1: a push after the merge.
        let mut moved = mergeable("agent/a");
        moved.remote_tip = Some(LATER.into());
        assert_eq!(kept(decide(&moved)), KeepReason::RemoteTipNotMergedHead);
        let mut unmerged = mergeable("agent/a");
        unmerged.head_pull_requests = Ok(vec![pr(10, PullRequestState::Closed, "agent/a", "main")]);
        assert_eq!(kept(decide(&unmerged)), KeepReason::NoMergedPullRequest);
        // Rule 2: merged is not enough; the session must be cleaned.
        let mut open = mergeable("agent/a");
        open.all_sessions_cleaned = false;
        assert_eq!(kept(decide(&open)), KeepReason::SessionNotCleaned);
        // Rule 3: an open pull request uses it as head.
        let mut head = mergeable("agent/a");
        head.head_pull_requests = Ok(vec![
            pr(10, PullRequestState::Merged, "agent/a", "main"),
            pr(11, PullRequestState::Open, "agent/a", "main"),
        ]);
        assert_eq!(
            kept(decide(&head)),
            KeepReason::OpenPullRequestUsesBranchAsHead
        );
        // Rule 4: an open session is stacked on it.
        let mut stacked = mergeable("agent/a");
        stacked.stacked_open_sessions = vec![7];
        assert_eq!(kept(decide(&stacked)), KeepReason::StackedSessionOpen);
        // Never a non-session branch.
        assert_eq!(
            kept(decide(&mergeable("main"))),
            KeepReason::NotASessionBranch
        );
        // An unanswered lookup keeps it.
        let mut unknown = mergeable("agent/a");
        unknown.base_pull_requests = Err("rate limited".into());
        assert_eq!(kept(decide(&unknown)), KeepReason::LookupFailed);
    }

    #[test]
    fn a_branch_already_gone_is_absent_whatever_a_local_branch_says() {
        let mut gone = mergeable("agent/a");
        gone.remote_tip = None;
        assert_eq!(
            decide(&gone),
            BranchDecision::Absent {
                pr_number: Some(10)
            }
        );
    }

    #[test]
    fn open_pull_requests_based_on_the_branch_are_retargeted_where_it_landed() {
        let mut based = mergeable("agent/a");
        based.head_pull_requests = Ok(vec![pr(
            10,
            PullRequestState::Merged,
            "agent/a",
            "agent/root",
        )]);
        based.base_pull_requests = Ok(vec![pr(12, PullRequestState::Open, "agent/b", "agent/a")]);
        match decide(&based) {
            BranchDecision::Delete { retargets, .. } => assert_eq!(
                retargets,
                vec![PlannedRetarget {
                    pull_request: 12,
                    to: "agent/root".into()
                }]
            ),
            other => panic!("expected delete, got {other:?}"),
        }
    }

    #[derive(Default)]
    struct Fake {
        bases: BTreeMap<i64, String>,
        log: RefCell<Vec<String>>,
        unconfirmed: bool,
        fail_delete: bool,
    }

    impl PullRequestBaseEffects for Fake {
        fn current_base(&self, _repository: &str, number: i64) -> Result<String, String> {
            self.log.borrow_mut().push(format!("read #{number}"));
            self.bases
                .get(&number)
                .cloned()
                .ok_or_else(|| "missing".to_string())
        }

        fn set_base(
            &mut self,
            _session_id: i64,
            _repository: &str,
            number: i64,
            base: &str,
            cross_session: Option<i64>,
        ) -> Result<(), String> {
            self.log
                .borrow_mut()
                .push(format!("retarget #{number} {base} {cross_session:?}"));
            if !self.unconfirmed {
                self.bases.insert(number, base.to_string());
            }
            Ok(())
        }
    }

    impl CleanupEffects for Fake {
        fn delete_remote_branch(
            &mut self,
            _session_id: i64,
            branch: &str,
            expected_oid: &str,
        ) -> Result<(), String> {
            self.log
                .borrow_mut()
                .push(format!("delete {branch} {expected_oid}"));
            if self.fail_delete {
                Err("lease rejected".into())
            } else {
                Ok(())
            }
        }

        fn branch_owner(&self, branch: &str) -> Option<i64> {
            (branch == "agent/root").then_some(3)
        }
    }

    fn delete(branch: &str, retargets: Vec<PlannedRetarget>) -> (i64, String, BranchDecision) {
        (
            1,
            branch.into(),
            BranchDecision::Delete {
                expected_oid: MERGED_HEAD.into(),
                pr_number: 10,
                retargets,
            },
        )
    }

    #[test]
    fn retargets_are_confirmed_before_the_delete_and_the_delete_carries_the_lease() {
        let mut fake = Fake::default();
        fake.bases.insert(12, "agent/a".into());
        let execution = execute(
            &mut fake,
            "o/r",
            vec![delete(
                "agent/a",
                vec![PlannedRetarget {
                    pull_request: 12,
                    to: "agent/root".into(),
                }],
            )],
            5,
            true,
        );
        assert_eq!(
            fake.log.borrow().as_slice(),
            [
                "read #12".to_string(),
                "retarget #12 agent/root Some(3)".into(),
                "read #12".into(),
                format!("delete agent/a {MERGED_HEAD}"),
            ]
        );
        assert_eq!(execution.items[0].outcome, "deleted");
        assert_eq!(execution.items[0].retargets[0].outcome, "retargeted");
        assert_eq!(execution.retargets, 1);
        assert_eq!(execution.stopped, None);
    }

    #[test]
    fn an_unconfirmed_retarget_never_deletes_and_stops_the_run() {
        let mut fake = Fake {
            unconfirmed: true,
            ..Fake::default()
        };
        fake.bases.insert(12, "agent/a".into());
        let execution = execute(
            &mut fake,
            "o/r",
            vec![
                delete(
                    "agent/a",
                    vec![PlannedRetarget {
                        pull_request: 12,
                        to: "main".into(),
                    }],
                ),
                delete("agent/b", Vec::new()),
            ],
            5,
            true,
        );
        assert!(
            fake.log
                .borrow()
                .iter()
                .all(|line| !line.starts_with("delete"))
        );
        assert_eq!(execution.items[0].outcome, "failed");
        assert_eq!(execution.items[1].outcome, "deferred");
        assert!(execution.stopped.is_some());
    }

    #[test]
    fn a_failed_delete_stops_the_run_and_the_bound_defers_the_rest() {
        let mut failing = Fake {
            fail_delete: true,
            ..Fake::default()
        };
        let execution = execute(
            &mut failing,
            "o/r",
            vec![delete("agent/a", Vec::new()), delete("agent/b", Vec::new())],
            5,
            true,
        );
        assert_eq!(execution.items[0].outcome, "failed");
        assert_eq!(execution.items[1].outcome, "deferred");
        assert_eq!(failing.log.borrow().len(), 1);

        let mut bounded = Fake::default();
        let execution = execute(
            &mut bounded,
            "o/r",
            vec![
                delete("agent/a", Vec::new()),
                delete("agent/b", Vec::new()),
                (
                    2,
                    "agent/c".into(),
                    keep(KeepReason::SessionNotCleaned, None),
                ),
            ],
            1,
            true,
        );
        let outcomes: Vec<&str> = execution
            .items
            .iter()
            .map(|item| item.outcome.as_str())
            .collect();
        assert_eq!(outcomes, ["deleted", "deferred", "kept"]);
    }

    #[test]
    fn a_preview_writes_nothing() {
        let mut fake = Fake::default();
        let execution = execute(
            &mut fake,
            "o/r",
            vec![delete(
                "agent/a",
                vec![PlannedRetarget {
                    pull_request: 12,
                    to: "main".into(),
                }],
            )],
            5,
            false,
        );
        assert!(fake.log.borrow().is_empty());
        assert_eq!(execution.items[0].outcome, "would_delete");
        assert_eq!(execution.items[0].retargets[0].outcome, "planned");
    }

    #[test]
    fn the_policy_is_off_by_default_and_validates_its_bounds() {
        assert_eq!(
            RemoteBranchPolicy::from_config_text("").unwrap(),
            RemoteBranchPolicy::default()
        );
        assert!(!RemoteBranchPolicy::default().delete_merged);
        let on = RemoteBranchPolicy::from_config_text(
            "[remote_branches]\ndelete_merged = true\nmax_deletions_per_run = 10\n",
        )
        .unwrap();
        assert!(on.delete_merged);
        assert_eq!(on.max_deletions_per_run, 10);
        assert_eq!(on.scan_per_run, DEFAULT_SCAN_PER_RUN);
        assert!(
            RemoteBranchPolicy::from_config_text("[remote_branches]\nscan_per_run = 0\n").is_err()
        );
        assert!(
            RemoteBranchPolicy::from_config_text("[remote_branches]\nscan_per_run = 999\n")
                .is_err()
        );
        assert!(RemoteBranchPolicy::from_config_text("[remote_branches]\nunknown = 1\n").is_err());
        assert!(
            RemoteBranchPolicy::from_config_text("[remote_branches]\ndelete_merged = 1\n").is_err()
        );
    }

    #[test]
    fn backoff_doubles_from_fifteen_minutes_to_a_six_hour_ceiling() {
        assert_eq!(backoff_ms(1), BACKOFF_BASE_MS);
        assert_eq!(backoff_ms(2), 2 * BACKOFF_BASE_MS);
        assert_eq!(backoff_ms(3), 4 * BACKOFF_BASE_MS);
        assert_eq!(backoff_ms(40), BACKOFF_MAX_MS);
        assert_eq!(backoff_ms(0), BACKOFF_BASE_MS);
    }
}
