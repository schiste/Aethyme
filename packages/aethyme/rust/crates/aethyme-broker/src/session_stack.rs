//! Explicit session stacking: a session started on top of another session's
//! unmerged branch (`broker start --stack-on <session>`).
//!
//! The relationship is recorded once, at start (`session_stacks`, schema
//! v66): the parent session, the parent branch and the parent tip the child
//! started from. From then on the broker, not the agent, manages the child's
//! pull-request base:
//!
//! - `push --pr` opens the child's pull request against the parent branch and
//!   says in its body that it is stacked on the parent pull request;
//! - a parent `push` leaves the child a note, and the child's `sync` merges
//!   the parent branch instead of the default branch;
//! - when the parent pull request merges (identified by GitHub's own `MERGED`
//!   state and merged head, never by ancestry, so squash merges count), the
//!   child's pull request is retargeted to its nearest unmerged ancestor
//!   through the coordinated GitHub write path, confirmed by reading it back,
//!   and the child's next `sync` integrates that branch;
//! - when the parent pull request closes unmerged, or the parent session is
//!   abandoned, nothing is retargeted: the child is told and `status` shows
//!   the choice (`sync --unstack`, or finish).

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Value, json};

use crate::session_push::{TrackedDefault, tracked_default};
use crate::{
    Broker, BrokerOpError, CoordinatedCommand, OperationEffect, OperationProvider, QueueWait,
};

/// Sender of every note this module leaves for a child session.
pub(crate) const NOTE_SENDER: &str = "automation:session-stack";
/// Ancestors walked when looking for the nearest unmerged one.
const MAX_CHAIN_DEPTH: usize = 16;
/// The pull-request fields every provider read requests.
const PR_FIELDS: &str = "number,state,headRefName,headRefOid,baseRefName,mergeCommit";
/// The most open pull requests one base-branch listing may return before it
/// is treated as incomplete.
const BASE_LISTING_LIMIT: usize = 100;

/// What became of the parent session's pull request, as last observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParentState {
    /// Unmerged (or not yet opened): the child stays on the parent branch.
    Open,
    /// The parent pull request merged.
    Merged,
    /// The parent pull request closed without merging.
    Closed,
    /// The parent session closed with no pull request merged or open.
    Abandoned,
}

impl ParentState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Merged => "merged",
            Self::Closed => "closed",
            Self::Abandoned => "abandoned",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "open" => Some(Self::Open),
            "merged" => Some(Self::Merged),
            "closed" => Some(Self::Closed),
            "abandoned" => Some(Self::Abandoned),
            _ => None,
        }
    }
}

/// One recorded stack edge: `session_id` was started on top of
/// `parent_session_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionStack {
    pub session_id: i64,
    pub parent_session_id: i64,
    pub parent_branch: String,
    /// The parent tip the child was started from.
    pub parent_tip: String,
    /// The branch the child's pull request targets and `sync` follows: the
    /// parent branch, until the parent merges or the child is unstacked.
    pub base_branch: String,
    pub parent_state: ParentState,
    /// The parent tip the child was last told about.
    pub last_parent_tip: Option<String>,
    pub parent_pr_number: Option<i64>,
    pub parent_merged_head: Option<String>,
    pub parent_merge_commit: Option<String>,
    pub retargeted_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// What `start --stack-on` resolved before creating the worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StackStartPlan {
    pub parent_session_id: i64,
    pub parent_branch: String,
    pub parent_tip: String,
    /// Whether the remote-tracking ref holds exactly `parent_tip`. A child's
    /// pull request can only be opened once the parent branch is pushed.
    pub parent_pushed: bool,
}

/// A provider's pull-request state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PullRequestState {
    Open,
    Merged,
    Closed,
}

/// The provider facts the stacking and branch-cleanup lanes decide from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PullRequestFact {
    pub number: i64,
    pub state: PullRequestState,
    pub head_branch: String,
    /// For a merged pull request, the head commit GitHub merged.
    pub head_oid: String,
    pub base_branch: String,
    pub merge_commit: Option<String>,
}

/// Parse `gh pr list|view --json number,state,headRefName,headRefOid,
/// baseRefName,mergeCommit` output: one object or an array of them.
pub(crate) fn parse_pull_request_facts(stdout: &str) -> Result<Vec<PullRequestFact>, String> {
    let value: Value = serde_json::from_str(stdout.trim())
        .map_err(|error| format!("unreadable pull request JSON: {error}"))?;
    let items = match value {
        Value::Array(items) => items,
        object @ Value::Object(_) => vec![object],
        _ => return Err("pull request JSON is neither an object nor an array".into()),
    };
    items
        .iter()
        .map(|item| {
            let text = |field: &str| item[field].as_str().unwrap_or_default().to_string();
            let number = item["number"]
                .as_i64()
                .filter(|number| *number > 0)
                .ok_or_else(|| "a pull request has no number".to_string())?;
            let state = match item["state"]
                .as_str()
                .map(str::to_ascii_uppercase)
                .as_deref()
            {
                Some("OPEN") => PullRequestState::Open,
                Some("MERGED") => PullRequestState::Merged,
                Some("CLOSED") => PullRequestState::Closed,
                other => return Err(format!("pull request #{number} has state {other:?}")),
            };
            let head_oid = text("headRefOid").to_ascii_lowercase();
            if head_oid.len() != 40 || !head_oid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(format!("pull request #{number} has no full head commit"));
            }
            Ok(PullRequestFact {
                number,
                state,
                head_branch: text("headRefName"),
                head_oid,
                base_branch: text("baseRefName"),
                merge_commit: item["mergeCommit"]["oid"]
                    .as_str()
                    .filter(|oid| !oid.is_empty())
                    .map(str::to_ascii_lowercase),
            })
        })
        .collect()
}

/// Read-only provider access. Reads need no coordination; every write goes
/// through [`PullRequestBaseEffects`].
pub(crate) trait PullRequestReader {
    /// Every pull request (any state) whose head is exactly `branch`.
    fn pull_requests_with_head(
        &self,
        repository: &str,
        branch: &str,
    ) -> Result<Vec<PullRequestFact>, String>;
    /// Every open pull request whose base is exactly `branch`; an incomplete
    /// listing is an error, never a short answer.
    fn open_pull_requests_with_base(
        &self,
        repository: &str,
        branch: &str,
    ) -> Result<Vec<PullRequestFact>, String>;
    /// One pull request, read fresh.
    fn pull_request(&self, repository: &str, number: i64) -> Result<PullRequestFact, String>;
}

/// The production reader: bounded `gh` reads.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct GhCliPullRequestReader;

impl GhCliPullRequestReader {
    fn run(args: &[String]) -> Result<String, String> {
        let mut command = std::process::Command::new("gh");
        command.args(args);
        match crate::bounded_output::output_limited(
            &mut command,
            std::time::Duration::from_secs(20),
            4 * 1024 * 1024,
        ) {
            Ok(crate::bounded_output::LimitedOutput::Completed(output))
                if output.status.success() =>
            {
                Ok(String::from_utf8_lossy(&output.stdout).into_owned())
            }
            Ok(crate::bounded_output::LimitedOutput::Completed(output)) => Err(format!(
                "gh {} failed: {}",
                args.first().map(String::as_str).unwrap_or_default(),
                String::from_utf8_lossy(&output.stderr)
                    .lines()
                    .next()
                    .unwrap_or("no error text")
            )),
            Ok(crate::bounded_output::LimitedOutput::Deadline) => {
                Err("gh pull request read exceeded its 20-second deadline".into())
            }
            Ok(crate::bounded_output::LimitedOutput::OutputLimit) => {
                Err("gh pull request read exceeded its output budget".into())
            }
            Err(error) => Err(format!("gh could not run: {error}")),
        }
    }

    fn list(repository: &str, selector: [&str; 2], state: &str, limit: usize) -> Vec<String> {
        [
            "pr",
            "list",
            "--repo",
            repository,
            selector[0],
            selector[1],
            "--state",
            state,
            "--limit",
            &limit.to_string(),
            "--json",
            PR_FIELDS,
        ]
        .iter()
        .map(|arg| (*arg).to_string())
        .collect()
    }
}

impl PullRequestReader for GhCliPullRequestReader {
    fn pull_requests_with_head(
        &self,
        repository: &str,
        branch: &str,
    ) -> Result<Vec<PullRequestFact>, String> {
        let stdout = Self::run(&Self::list(repository, ["--head", branch], "all", 30))?;
        Ok(parse_pull_request_facts(&stdout)?
            .into_iter()
            .filter(|pr| pr.head_branch == branch)
            .collect())
    }

    fn open_pull_requests_with_base(
        &self,
        repository: &str,
        branch: &str,
    ) -> Result<Vec<PullRequestFact>, String> {
        let stdout = Self::run(&Self::list(
            repository,
            ["--base", branch],
            "open",
            BASE_LISTING_LIMIT,
        ))?;
        let prs = parse_pull_request_facts(&stdout)?;
        if prs.len() >= BASE_LISTING_LIMIT {
            return Err(format!(
                "the open pull requests based on {branch} reached the {BASE_LISTING_LIMIT}-item \
                 listing limit, so the listing may be incomplete"
            ));
        }
        Ok(prs
            .into_iter()
            .filter(|pr| pr.base_branch == branch && pr.state == PullRequestState::Open)
            .collect())
    }

    fn pull_request(&self, repository: &str, number: i64) -> Result<PullRequestFact, String> {
        let args: Vec<String> = [
            "pr",
            "view",
            &number.to_string(),
            "--repo",
            repository,
            "--json",
            PR_FIELDS,
        ]
        .iter()
        .map(|arg| (*arg).to_string())
        .collect();
        parse_pull_request_facts(&Self::run(&args)?)?
            .into_iter()
            .next()
            .ok_or_else(|| format!("pull request #{number} was not returned"))
    }
}

/// What the parent's pull requests say about the parent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParentObservation {
    pub state: ParentState,
    pub pull_request: Option<PullRequestFact>,
}

/// Decide the parent's state from every pull request whose head is its
/// branch. An open pull request wins (the parent can still merge); then the
/// newest merged one (GitHub's `MERGED` state and merged head, so squash and
/// merge commits are treated alike); then a closed one. With none, a closed
/// parent session is abandoned and an open one simply has no pull request yet.
pub(crate) fn observe_parent(
    prs: &[PullRequestFact],
    parent_branch: &str,
    parent_session_closed: bool,
) -> ParentObservation {
    let newest = |state: PullRequestState| {
        prs.iter()
            .filter(|pr| pr.head_branch == parent_branch && pr.state == state)
            .max_by_key(|pr| pr.number)
            .cloned()
    };
    if let Some(pr) = newest(PullRequestState::Open) {
        return ParentObservation {
            state: ParentState::Open,
            pull_request: Some(pr),
        };
    }
    if let Some(pr) = newest(PullRequestState::Merged) {
        return ParentObservation {
            state: ParentState::Merged,
            pull_request: Some(pr),
        };
    }
    if let Some(pr) = newest(PullRequestState::Closed) {
        return ParentObservation {
            state: ParentState::Closed,
            pull_request: Some(pr),
        };
    }
    ParentObservation {
        state: if parent_session_closed {
            ParentState::Abandoned
        } else {
            ParentState::Open
        },
        pull_request: None,
    }
}

/// The nearest unmerged ancestor branch, starting from `start` (the branch
/// the merged parent landed in). `merged_into(branch)` answers where
/// `branch`'s own pull request merged, or `None` while it is unmerged. The
/// default branch always ends the walk; so does a cycle or a chain deeper
/// than [`MAX_CHAIN_DEPTH`], at the default branch.
pub(crate) fn nearest_unmerged_base(
    start: &str,
    default_branch: &str,
    merged_into: impl Fn(&str) -> Option<String>,
) -> String {
    let mut candidate = start.to_string();
    let mut seen = vec![candidate.clone()];
    for _ in 0..MAX_CHAIN_DEPTH {
        if candidate == default_branch {
            return candidate;
        }
        match merged_into(&candidate) {
            None => return candidate,
            Some(next) if seen.contains(&next) => return default_branch.to_string(),
            Some(next) => {
                seen.push(next.clone());
                candidate = next;
            }
        }
    }
    default_branch.to_string()
}

/// The pull-request base write and its confirmation read.
pub(crate) trait PullRequestBaseEffects {
    /// The base branch the provider reports now.
    fn current_base(&self, repository: &str, number: i64) -> Result<String, String>;
    /// Set the base through the coordinated write path.
    fn set_base(
        &mut self,
        session_id: i64,
        repository: &str,
        number: i64,
        base: &str,
        cross_session: Option<i64>,
    ) -> Result<(), String>;
}

/// How a confirmed retarget ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RetargetOutcome {
    /// The pull request already targeted the base; nothing was written.
    AlreadyTargeted,
    /// The base was written and a fresh read confirmed it.
    Retargeted,
}

/// Retarget pull request `number` to `base`, then re-read it: success only
/// when the provider reports the new base. Nothing is written when it is
/// already there.
pub(crate) fn retarget_confirmed<E: PullRequestBaseEffects + ?Sized>(
    effects: &mut E,
    session_id: i64,
    repository: &str,
    number: i64,
    base: &str,
    cross_session: Option<i64>,
) -> Result<RetargetOutcome, String> {
    if effects.current_base(repository, number)? == base {
        return Ok(RetargetOutcome::AlreadyTargeted);
    }
    effects.set_base(session_id, repository, number, base, cross_session)?;
    let after = effects.current_base(repository, number)?;
    if after != base {
        return Err(format!(
            "pull request #{number} still targets {after} after retargeting it to {base}"
        ));
    }
    Ok(RetargetOutcome::Retargeted)
}

/// The production effects: a coordinated `gh pr edit --base` (journaled as an
/// operation record) and a fresh provider read.
pub(crate) struct BrokerPullRequestBases<'a> {
    pub broker: &'a mut Broker,
    pub reader: &'a dyn PullRequestReader,
}

impl PullRequestBaseEffects for BrokerPullRequestBases<'_> {
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
        let main_root = self.broker.main_root().to_path_buf();
        let report = self
            .broker
            .run_coordinated_operation_at_with_wait(
                CoordinatedCommand {
                    session_id,
                    provider: OperationProvider::Github,
                    repository: Some(repository.to_string()),
                    resolved_target: None,
                    scope: Some(format!("pull-request:{number}")),
                    declared_effect: Some(OperationEffect::Write),
                    destructive_confirmed: false,
                    cross_session,
                    ref_write_acknowledged: false,
                    authorization_reason: Some(
                        "broker-managed stacked pull request base (session stacking)".into(),
                    ),
                    args: vec![
                        "pr".into(),
                        "edit".into(),
                        number.to_string(),
                        "--base".into(),
                        base.to_string(),
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
                "retargeting pull request #{number} to {base} failed (operation {}, {})",
                report.operation.id,
                report.operation.status.as_str()
            ))
        }
    }
}

/// The base `push --pr` opens a stacked session's pull request against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StackedPullRequestBase {
    pub base_branch: String,
    /// The remote-tracking ref of `base_branch`: what the commits listed in
    /// the body are counted from.
    pub compare_ref: String,
    pub parent_session_id: i64,
    pub parent_pr: Option<i64>,
    /// The live session that owns `base_branch`, named so the coordinated
    /// write may attribute the base to it.
    pub cross_session: Option<i64>,
}

/// The body of a stacked pull request: a stack banner above the composed body.
pub(crate) fn stacked_pull_request_body(
    body: &str,
    base: &StackedPullRequestBase,
    default_branch: &str,
) -> String {
    let parent = match base.parent_pr {
        Some(number) => format!("#{number} (`{}`)", base.base_branch),
        None => format!(
            "`{}` (session {})",
            base.base_branch, base.parent_session_id
        ),
    };
    format!(
        "> Stacked on {parent}: review and merge that pull request first. When it merges, the \
         broker retargets this pull request onto `{default_branch}` (or the nearest unmerged \
         ancestor).\n\n{body}"
    )
}

/// The one next action a stacked session needs, if any.
pub(crate) fn stack_next_action(
    stack: &SessionStack,
    diff_base: Option<&str>,
    default_branch: &str,
) -> Option<String> {
    let id = stack.session_id;
    let parent = stack.parent_session_id;
    let still_on_parent = stack.base_branch == stack.parent_branch;
    let on_parent_tip = diff_base.is_some_and(|base| {
        base == stack.parent_tip || Some(base) == stack.last_parent_tip.as_deref()
    });
    match stack.parent_state {
        ParentState::Closed | ParentState::Abandoned if still_on_parent => Some(format!(
            "parent session {parent}'s pull request was closed without merging or the session was \
             abandoned; nothing was retargeted. Choose: carry its commits onto {default_branch} \
             with `aethyme broker sync --session {id} --unstack`, or stop this work with \
             `aethyme broker finish --session {id}`"
        )),
        ParentState::Merged if on_parent_tip => Some(format!(
            "parent session {parent}'s pull request merged and this session now targets {}; run \
             `aethyme broker sync --session {id}`, then `aethyme broker push --session {id}`",
            stack.base_branch
        )),
        ParentState::Open
            if still_on_parent
                && stack
                    .last_parent_tip
                    .as_deref()
                    .is_some_and(|tip| Some(tip) != diff_base) =>
        {
            Some(format!(
                "parent session {parent} pushed new commits; run `aethyme broker sync --session \
                 {id}` to bring them in"
            ))
        }
        _ => None,
    }
}

impl Broker {
    /// The default branch and the `owner/name` slug of its remote.
    fn stack_repository(&self) -> Result<(TrackedDefault, String), String> {
        let default = tracked_default(self.repo_handle())
            .ok_or_else(|| "the main checkout tracks no fetched default branch".to_string())?;
        let target = self
            .repo_handle()
            .resolve_remote_target(&default.remote, None)
            .map_err(|error| format!("remote {:?}: {error}", default.remote))?;
        Ok((default, target.display_slug))
    }

    /// The one live session other than `except` working on `branch`.
    pub(crate) fn live_branch_owner(&self, branch: &str, except: i64) -> Option<i64> {
        let owners: Vec<i64> = self
            .store_ref()
            .live_sessions()
            .ok()?
            .into_iter()
            .filter(|session| {
                session.id != except && !session.status.is_closed() && session.branch == branch
            })
            .map(|session| session.id)
            .collect();
        match owners.as_slice() {
            [owner] => Some(*owner),
            _ => None,
        }
    }

    /// Resolve `start --stack-on <parent>` before any worktree exists.
    pub(crate) fn stack_start_plan(
        &self,
        parent_session_id: i64,
    ) -> Result<StackStartPlan, BrokerOpError> {
        let parent = self.store_ref().session(parent_session_id)?;
        if parent.status.is_closed() {
            return Err(BrokerOpError::StartBaseUnavailable {
                reason: format!(
                    "--stack-on {parent_session_id}: that session is closed; stack only on an \
                     open session. No worktree was created"
                ),
            });
        }
        let repo = self.repo_handle();
        let parent_tip = repo
            .resolve_ref(&format!("refs/heads/{}", parent.branch))
            .ok_or_else(|| BrokerOpError::StartBaseUnavailable {
                reason: format!(
                    "--stack-on {parent_session_id}: its branch {} does not exist. No worktree \
                     was created",
                    parent.branch
                ),
            })?;
        let parent_pushed = tracked_default(repo).is_some_and(|default| {
            repo.resolve_ref(&format!(
                "refs/remotes/{}/{}",
                default.remote, parent.branch
            ))
            .as_deref()
                == Some(parent_tip.as_str())
        });
        Ok(StackStartPlan {
            parent_session_id,
            parent_branch: parent.branch,
            parent_tip,
            parent_pushed,
        })
    }

    /// Record the stack edge for a session `start --stack-on` just created.
    pub(crate) fn record_session_stack(
        &mut self,
        session_id: i64,
        plan: &StackStartPlan,
    ) -> Result<SessionStack, BrokerOpError> {
        Ok(self.store().insert_session_stack(
            session_id,
            plan.parent_session_id,
            &plan.parent_branch,
            &plan.parent_tip,
        )?)
    }

    /// The base a stacked session's new pull request targets, or `None` for
    /// an unstacked session (or one already moved onto the default branch).
    pub(crate) fn stacked_pull_request_base(
        &self,
        session_id: i64,
        default: &TrackedDefault,
    ) -> Result<Option<StackedPullRequestBase>, BrokerOpError> {
        let Some(stack) = self.store_ref().session_stack(session_id)? else {
            return Ok(None);
        };
        if stack.base_branch == default.branch {
            return Ok(None);
        }
        let compare_ref = format!("refs/remotes/{}/{}", default.remote, stack.base_branch);
        if self.repo_handle().resolve_ref(&compare_ref).is_none() {
            return Err(BrokerOpError::SessionPushRefused {
                reason: format!(
                    "session {session_id} is stacked on {} (session {}), which is not on {} yet, \
                     so its pull request has no base to target; publish the parent first with \
                     `aethyme broker push --session {}`, then retry. Nothing was pushed",
                    stack.base_branch,
                    stack.parent_session_id,
                    default.remote,
                    stack.parent_session_id
                ),
            });
        }
        let parent_pr = self
            .store_ref()
            .session_pull_request_merges(stack.parent_session_id)
            .ok()
            .and_then(|links| {
                links
                    .iter()
                    .filter(|(_, _, merged)| merged.is_none())
                    .map(|(_, number, _)| *number)
                    .max()
                    .or(stack.parent_pr_number)
            });
        Ok(Some(StackedPullRequestBase {
            cross_session: self.live_branch_owner(&stack.base_branch, session_id),
            base_branch: stack.base_branch,
            compare_ref,
            parent_session_id: stack.parent_session_id,
            parent_pr,
        }))
    }

    /// After a parent session pushed `head`, tell each open child still on
    /// the parent branch, once per new tip. Best-effort: the push happened.
    pub(crate) fn note_stack_children_after_push(&mut self, parent_session_id: i64, head: &str) {
        let Ok(children) = self.store_ref().session_stack_children(parent_session_id) else {
            return;
        };
        for child in children {
            if child.base_branch != child.parent_branch
                || child.last_parent_tip.as_deref() == Some(head)
                || !self
                    .store_ref()
                    .session(child.session_id)
                    .is_ok_and(|session| !session.status.is_closed())
            {
                continue;
            }
            let message = format!(
                "Parent session {parent_session_id} pushed {} (new tip {head}). Bring its commits \
                 in with `aethyme broker sync --session {}`, then `aethyme broker push --session \
                 {}`.",
                child.parent_branch, child.session_id, child.session_id
            );
            crate::warn_unrecorded(
                "note a parent push to a stacked session",
                self.store()
                    .record_automation_note(
                        NOTE_SENDER,
                        child.session_id,
                        &message,
                        Some(&format!("stack-parent-advance:{head}")),
                    )
                    .map(|_| ()),
            );
            crate::warn_unrecorded(
                "record the parent tip a stacked session was told about",
                self.store()
                    .set_session_stack_parent_tip_seen(child.session_id, head),
            );
        }
    }

    /// The branch `sync` follows for a stacked session, and whether it must
    /// merge (never rebase) because the parent merged under it.
    pub(crate) fn stack_sync_branch(&self, session_id: i64) -> Option<(String, bool)> {
        let stack = self.store_ref().session_stack(session_id).ok()??;
        Some((stack.base_branch, stack.parent_state == ParentState::Merged))
    }

    /// Re-read one stacked session's parent and act on what changed. `Ok(None)`
    /// when the session is not stacked or nothing changed.
    pub(crate) fn refresh_session_stack(
        &mut self,
        session_id: i64,
        reader: &dyn PullRequestReader,
    ) -> Result<Option<String>, String> {
        let Some(stack) = self
            .store_ref()
            .session_stack(session_id)
            .map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        self.reconcile_stacked_child(&stack, reader)
    }

    /// Re-read the parent of every open child of `parent_session_id`.
    /// Returns one line per failure.
    pub(crate) fn reconcile_stack_children(
        &mut self,
        parent_session_id: i64,
        reader: &dyn PullRequestReader,
    ) -> Vec<String> {
        let children = match self.store_ref().session_stack_children(parent_session_id) {
            Ok(children) => children,
            Err(error) => return vec![error.to_string()],
        };
        self.reconcile_stacks(children, reader)
    }

    /// Re-read the parent of every open stacked session still on its parent
    /// branch: the background worker's pass. Returns one line per failure.
    pub(crate) fn reconcile_open_session_stacks(
        &mut self,
        reader: &dyn PullRequestReader,
    ) -> Vec<String> {
        let stacks = match self.store_ref().open_session_stacks() {
            Ok(stacks) => stacks,
            Err(error) => return vec![error.to_string()],
        };
        self.reconcile_stacks(stacks, reader)
    }

    fn reconcile_stacks(
        &mut self,
        stacks: Vec<SessionStack>,
        reader: &dyn PullRequestReader,
    ) -> Vec<String> {
        let mut failures = Vec::new();
        for stack in stacks {
            let open = self
                .store_ref()
                .session(stack.session_id)
                .is_ok_and(|session| !session.status.is_closed());
            if !open || stack.base_branch != stack.parent_branch {
                continue;
            }
            if let Err(error) = self.reconcile_stacked_child(&stack, reader) {
                failures.push(format!("session {}: {error}", stack.session_id));
            }
        }
        failures
    }

    /// The core transition for one child still on its parent branch.
    fn reconcile_stacked_child(
        &mut self,
        stack: &SessionStack,
        reader: &dyn PullRequestReader,
    ) -> Result<Option<String>, String> {
        if stack.base_branch != stack.parent_branch {
            return Ok(None);
        }
        let (default, repository) = self.stack_repository()?;
        let parent_closed = self
            .store_ref()
            .session(stack.parent_session_id)
            .map(|session| session.status.is_closed())
            .unwrap_or(true);
        let prs = reader.pull_requests_with_head(&repository, &stack.parent_branch)?;
        let observed = observe_parent(&prs, &stack.parent_branch, parent_closed);
        let id = stack.session_id;
        match observed.state {
            ParentState::Open => {
                if stack.parent_state != ParentState::Open {
                    self.store()
                        .set_session_stack_parent_state(id, ParentState::Open, None, None, None)
                        .map_err(|error| error.to_string())?;
                }
                Ok(None)
            }
            ParentState::Closed | ParentState::Abandoned => {
                if stack.parent_state == observed.state {
                    return Ok(None);
                }
                let number = observed.pull_request.as_ref().map(|pr| pr.number);
                self.store()
                    .set_session_stack_parent_state(id, observed.state, number, None, None)
                    .map_err(|error| error.to_string())?;
                let what = match number {
                    Some(number) => format!("pull request #{number} was closed without merging"),
                    None => "session closed with no merged pull request".to_string(),
                };
                let message = format!(
                    "Parent session {}'s {what}. Nothing was retargeted: this session still \
                     targets {}. Choose: carry the parent's commits onto {} with `aethyme broker \
                     sync --session {id} --unstack`, or stop this work with `aethyme broker \
                     finish --session {id}`.",
                    stack.parent_session_id, stack.parent_branch, default.branch
                );
                self.leave_stack_note(id, &message, observed.state.as_str());
                Ok(Some(message))
            }
            ParentState::Merged => {
                let merged = observed
                    .pull_request
                    .ok_or_else(|| "a merged parent without its pull request".to_string())?;
                let new_base =
                    nearest_unmerged_base(&merged.base_branch, &default.branch, |branch| {
                        let prs = reader.pull_requests_with_head(&repository, branch).ok()?;
                        let observed = observe_parent(&prs, branch, false);
                        if observed.state == ParentState::Merged {
                            observed.pull_request.map(|pr| pr.base_branch)
                        } else {
                            None
                        }
                    });
                let child_branch = self
                    .store_ref()
                    .session(id)
                    .map(|session| session.branch)
                    .map_err(|error| error.to_string())?;
                let child_pr = reader
                    .pull_requests_with_head(&repository, &child_branch)?
                    .into_iter()
                    .filter(|pr| pr.state == PullRequestState::Open)
                    .max_by_key(|pr| pr.number);
                let cross_session = if new_base == default.branch {
                    None
                } else {
                    self.live_branch_owner(&new_base, id)
                };
                if let Some(child_pr) = &child_pr {
                    let mut effects = BrokerPullRequestBases {
                        broker: &mut *self,
                        reader,
                    };
                    retarget_confirmed(
                        &mut effects,
                        id,
                        &repository,
                        child_pr.number,
                        &new_base,
                        cross_session,
                    )?;
                }
                self.store()
                    .set_session_stack_parent_state(
                        id,
                        ParentState::Merged,
                        Some(merged.number),
                        Some(&merged.head_oid),
                        merged.merge_commit.as_deref(),
                    )
                    .map_err(|error| error.to_string())?;
                self.store()
                    .set_session_stack_base(id, &new_base)
                    .map_err(|error| error.to_string())?;
                let retargeted = match &child_pr {
                    Some(pr) => format!(
                        "This session's pull request #{} now targets {new_base} (was {}).",
                        pr.number, stack.parent_branch
                    ),
                    None => format!(
                        "This session has no open pull request; its next one will target \
                         {new_base}."
                    ),
                };
                let message = format!(
                    "Parent session {}'s pull request #{} merged (merged head {}, merge commit \
                     {}). {retargeted} Run `aethyme broker sync --session {id}` to integrate \
                     {new_base}, then `aethyme broker push --session {id}`.",
                    stack.parent_session_id,
                    merged.number,
                    merged.head_oid,
                    merged.merge_commit.as_deref().unwrap_or("unknown"),
                );
                self.leave_stack_note(id, &message, "merged");
                Ok(Some(message))
            }
        }
    }

    fn leave_stack_note(&mut self, session_id: i64, message: &str, kind: &str) {
        crate::warn_unrecorded(
            "leave a stacked session its parent's outcome",
            self.store()
                .record_automation_note(
                    NOTE_SENDER,
                    session_id,
                    message,
                    Some(&format!("stack-parent-{kind}")),
                )
                .map(|_| ()),
        );
    }

    /// `sync --unstack`: move a stacked session onto the default branch,
    /// retargeting its open pull request (confirmed by a fresh read). The
    /// parent's commits stay in this session's branch.
    pub(crate) fn unstack_session(
        &mut self,
        session_id: i64,
        reader: &dyn PullRequestReader,
    ) -> Result<String, BrokerOpError> {
        let refused = |reason: String| BrokerOpError::SessionSyncRefused { reason };
        let stack = self
            .store_ref()
            .session_stack(session_id)?
            .ok_or_else(|| refused(format!("session {session_id} is not stacked on a session")))?;
        let (default, repository) = self.stack_repository().map_err(refused)?;
        if stack.base_branch == default.branch {
            return Ok(format!(
                "session {session_id} already targets {}",
                default.branch
            ));
        }
        let branch = self.store_ref().session(session_id)?.branch;
        let child_pr = reader
            .pull_requests_with_head(&repository, &branch)
            .map_err(refused)?
            .into_iter()
            .filter(|pr| pr.state == PullRequestState::Open)
            .max_by_key(|pr| pr.number);
        if let Some(pr) = &child_pr {
            let mut effects = BrokerPullRequestBases {
                broker: &mut *self,
                reader,
            };
            retarget_confirmed(
                &mut effects,
                session_id,
                &repository,
                pr.number,
                &default.branch,
                None,
            )
            .map_err(refused)?;
        }
        self.store()
            .set_session_stack_base(session_id, &default.branch)?;
        Ok(match child_pr {
            Some(pr) => format!(
                "unstacked from {}: pull request #{} now targets {}",
                stack.parent_branch, pr.number, default.branch
            ),
            None => format!(
                "unstacked from {}: the next pull request will target {}",
                stack.parent_branch, default.branch
            ),
        })
    }

    /// The stack around one session for `status --json`, or `None` when it
    /// is neither stacked nor a parent of an open session. Read-only and
    /// offline: pull-request states are the broker's recorded ones.
    pub(crate) fn session_stack_view(&self, session_id: i64) -> Option<Value> {
        let store = self.store_ref();
        let stack = store.session_stack(session_id).ok().flatten();
        let children: Vec<SessionStack> = store
            .session_stack_children(session_id)
            .ok()
            .unwrap_or_default()
            .into_iter()
            .filter(|child| {
                store
                    .session(child.session_id)
                    .is_ok_and(|session| !session.status.is_closed())
            })
            .collect();
        if stack.is_none() && children.is_empty() {
            return None;
        }
        let default_branch = tracked_default(self.repo_handle())
            .map(|default| default.branch)
            .unwrap_or_default();
        let recorded_pr = |id: i64| -> Value {
            let links = store
                .session_pull_request_merges(id)
                .ok()
                .unwrap_or_default();
            match links.iter().max_by_key(|(_, number, _)| *number) {
                Some((_, number, merged)) => {
                    let state = if merged.is_some() {
                        "merged"
                    } else {
                        "unmerged"
                    };
                    json!({"number": number, "state": state})
                }
                None => Value::Null,
            }
        };
        let session = store.session(session_id).ok();
        let child_records: Vec<Value> = children
            .iter()
            .map(|child| {
                let branch = store
                    .session(child.session_id)
                    .map(|session| session.branch)
                    .unwrap_or_default();
                json!({
                    "session_id": child.session_id,
                    "branch": branch,
                    "base_branch": child.base_branch,
                    "pull_request": recorded_pr(child.session_id),
                })
            })
            .collect();
        let mut view = json!({
            "pull_request": recorded_pr(session_id),
            "children": child_records,
            "data": "recorded",
        });
        if let Some(stack) = &stack {
            let parent_pr = match stack.parent_pr_number {
                Some(number) => json!({"number": number, "state": stack.parent_state}),
                None => recorded_pr(stack.parent_session_id),
            };
            view["parent"] = json!({
                "session_id": stack.parent_session_id,
                "branch": stack.parent_branch,
                "started_from": stack.parent_tip,
                "state": stack.parent_state,
                "pull_request": parent_pr,
                "merged_head": stack.parent_merged_head,
                "merge_commit": stack.parent_merge_commit,
            });
            view["base_branch"] = json!(stack.base_branch);
            view["retargeted_at"] = json!(stack.retargeted_at);
            if let Some(action) = stack_next_action(
                stack,
                session.as_ref().and_then(|s| s.diff_base.as_deref()),
                &default_branch,
            ) {
                view["next_action"] = json!(action);
            }
        }
        Some(view)
    }

    /// Every open stacked session for the repository view of `status`, or
    /// `None` when there is none.
    pub(crate) fn repository_stack_view(&self) -> Option<Value> {
        let store = self.store_ref();
        let stacks = store.open_session_stacks().ok()?;
        if stacks.is_empty() {
            return None;
        }
        let default_branch = tracked_default(self.repo_handle())
            .map(|default| default.branch)
            .unwrap_or_default();
        let mut by_parent: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
        let records: Vec<Value> = stacks
            .iter()
            .map(|stack| {
                by_parent
                    .entry(stack.parent_session_id)
                    .or_default()
                    .push(stack.session_id);
                let diff_base = store
                    .session(stack.session_id)
                    .ok()
                    .and_then(|session| session.diff_base);
                let mut record = json!({
                    "session_id": stack.session_id,
                    "parent_session_id": stack.parent_session_id,
                    "parent_branch": stack.parent_branch,
                    "base_branch": stack.base_branch,
                    "parent_state": stack.parent_state,
                });
                if let Some(action) =
                    stack_next_action(stack, diff_base.as_deref(), &default_branch)
                {
                    record["next_action"] = json!(action);
                }
                record
            })
            .collect();
        let attention = records
            .iter()
            .filter(|record| record.get("next_action").is_some())
            .count();
        Some(json!({
            "count": records.len(),
            "needs_attention": attention,
            "parents": by_parent.len(),
            "records": records,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const OID_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OID_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn fact(number: i64, state: PullRequestState, head: &str, base: &str) -> PullRequestFact {
        PullRequestFact {
            number,
            state,
            head_branch: head.into(),
            head_oid: OID_A.into(),
            base_branch: base.into(),
            merge_commit: (state == PullRequestState::Merged).then(|| OID_B.to_string()),
        }
    }

    fn stack(state: ParentState, base: &str) -> SessionStack {
        SessionStack {
            session_id: 2,
            parent_session_id: 1,
            parent_branch: "agent/parent".into(),
            parent_tip: "tip0".into(),
            base_branch: base.into(),
            parent_state: state,
            last_parent_tip: Some("tip0".into()),
            parent_pr_number: None,
            parent_merged_head: None,
            parent_merge_commit: None,
            retargeted_at: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn provider_json_parses_merge_and_squash_alike_and_refuses_partial_heads() {
        let json = format!(
            r#"[{{"number":7,"state":"MERGED","headRefName":"agent/a","headRefOid":"{OID_A}",
                "baseRefName":"main","mergeCommit":{{"oid":"{OID_B}"}}}},
               {{"number":8,"state":"OPEN","headRefName":"agent/b","headRefOid":"{OID_B}",
                "baseRefName":"agent/a","mergeCommit":null}}]"#
        );
        let facts = parse_pull_request_facts(&json).unwrap();
        assert_eq!(facts[0].state, PullRequestState::Merged);
        assert_eq!(facts[0].merge_commit.as_deref(), Some(OID_B));
        assert_eq!(facts[1].merge_commit, None);
        let single = format!(
            r#"{{"number":9,"state":"CLOSED","headRefName":"x","headRefOid":"{OID_A}","baseRefName":"main"}}"#
        );
        assert_eq!(parse_pull_request_facts(&single).unwrap().len(), 1);
        let partial = r#"[{"number":9,"state":"OPEN","headRefName":"x","headRefOid":"abc","baseRefName":"main"}]"#;
        assert!(parse_pull_request_facts(partial).is_err());
        assert!(parse_pull_request_facts("not json").is_err());
    }

    #[test]
    fn the_parent_state_is_decided_by_pull_request_state_not_ancestry() {
        let parent = "agent/parent";
        let merged = fact(3, PullRequestState::Merged, parent, "main");
        let open = fact(4, PullRequestState::Open, parent, "main");
        let closed = fact(2, PullRequestState::Closed, parent, "main");
        let other = fact(9, PullRequestState::Merged, "agent/other", "main");

        let observed = observe_parent(&[merged.clone(), closed.clone()], parent, false);
        assert_eq!(observed.state, ParentState::Merged);
        assert_eq!(observed.pull_request.unwrap().number, 3);
        // A reopened or follow-up pull request keeps the child waiting.
        assert_eq!(
            observe_parent(&[merged, open], parent, false).state,
            ParentState::Open
        );
        assert_eq!(
            observe_parent(std::slice::from_ref(&closed), parent, false).state,
            ParentState::Closed
        );
        // Another branch's pull request says nothing about the parent.
        assert_eq!(
            observe_parent(std::slice::from_ref(&other), parent, false).state,
            ParentState::Open
        );
        assert_eq!(
            observe_parent(&[other], parent, true).state,
            ParentState::Abandoned
        );
    }

    #[test]
    fn the_nearest_unmerged_ancestor_walks_merged_links_and_stops_at_default() {
        let links: BTreeMap<&str, &str> = [("agent/b", "agent/a"), ("agent/a", "main")]
            .into_iter()
            .collect();
        let merged_into = |branch: &str| links.get(branch).map(|next| (*next).to_string());
        // b and a both merged: all the way to main.
        assert_eq!(
            nearest_unmerged_base("agent/b", "main", merged_into),
            "main"
        );
        // Only b merged (into a, which is open): a.
        let open_a = |branch: &str| (branch == "agent/b").then(|| "agent/a".to_string());
        assert_eq!(nearest_unmerged_base("agent/b", "main", open_a), "agent/a");
        assert_eq!(nearest_unmerged_base("main", "main", |_| None), "main");
        // A cycle ends at the default branch rather than looping.
        let cycle = |branch: &str| {
            Some(
                if branch == "agent/x" {
                    "agent/y"
                } else {
                    "agent/x"
                }
                .to_string(),
            )
        };
        assert_eq!(nearest_unmerged_base("agent/x", "main", cycle), "main");
    }

    #[derive(Default)]
    struct FakeBases {
        bases: RefCell<BTreeMap<i64, String>>,
        writes: Vec<(i64, String, Option<i64>)>,
        ignore_writes: bool,
        fail_write: bool,
    }

    impl PullRequestBaseEffects for FakeBases {
        fn current_base(&self, _repository: &str, number: i64) -> Result<String, String> {
            self.bases
                .borrow()
                .get(&number)
                .cloned()
                .ok_or_else(|| format!("no pull request #{number}"))
        }

        fn set_base(
            &mut self,
            _session_id: i64,
            _repository: &str,
            number: i64,
            base: &str,
            cross_session: Option<i64>,
        ) -> Result<(), String> {
            if self.fail_write {
                return Err("denied".into());
            }
            self.writes.push((number, base.to_string(), cross_session));
            if !self.ignore_writes {
                self.bases.borrow_mut().insert(number, base.to_string());
            }
            Ok(())
        }
    }

    #[test]
    fn a_retarget_writes_once_and_succeeds_only_when_the_read_back_confirms_it() {
        let mut effects = FakeBases::default();
        effects.bases.borrow_mut().insert(5, "agent/parent".into());
        assert_eq!(
            retarget_confirmed(&mut effects, 2, "o/r", 5, "main", None),
            Ok(RetargetOutcome::Retargeted)
        );
        assert_eq!(effects.writes, vec![(5, "main".to_string(), None)]);
        // Already there: nothing is written.
        assert_eq!(
            retarget_confirmed(&mut effects, 2, "o/r", 5, "main", None),
            Ok(RetargetOutcome::AlreadyTargeted)
        );
        assert_eq!(effects.writes.len(), 1);

        let mut unconfirmed = FakeBases {
            ignore_writes: true,
            ..FakeBases::default()
        };
        unconfirmed
            .bases
            .borrow_mut()
            .insert(5, "agent/parent".into());
        assert!(retarget_confirmed(&mut unconfirmed, 2, "o/r", 5, "main", Some(1)).is_err());

        let mut denied = FakeBases {
            fail_write: true,
            ..FakeBases::default()
        };
        denied.bases.borrow_mut().insert(5, "agent/parent".into());
        assert_eq!(
            retarget_confirmed(&mut denied, 2, "o/r", 5, "main", None),
            Err("denied".into())
        );
    }

    #[test]
    fn the_stacked_body_names_the_parent_pull_request_and_the_landing_branch() {
        let base = StackedPullRequestBase {
            base_branch: "agent/parent".into(),
            compare_ref: "refs/remotes/origin/agent/parent".into(),
            parent_session_id: 1,
            parent_pr: Some(41),
            cross_session: Some(1),
        };
        let body = stacked_pull_request_body("## Summary\n", &base, "main");
        assert!(
            body.starts_with("> Stacked on #41 (`agent/parent`)"),
            "{body}"
        );
        assert!(body.contains("onto `main`"));
        assert!(body.ends_with("## Summary\n"));
        let unopened = StackedPullRequestBase {
            parent_pr: None,
            ..base
        };
        assert!(stacked_pull_request_body("b", &unopened, "main").contains("(session 1)"));
    }

    #[test]
    fn each_stack_state_has_at_most_one_next_action() {
        // Fresh child on the parent tip: nothing to do.
        assert_eq!(
            stack_next_action(
                &stack(ParentState::Open, "agent/parent"),
                Some("tip0"),
                "main"
            ),
            None
        );
        // The parent pushed: sync.
        let mut advanced = stack(ParentState::Open, "agent/parent");
        advanced.last_parent_tip = Some("tip1".into());
        assert!(
            stack_next_action(&advanced, Some("tip0"), "main")
                .unwrap()
                .contains("pushed new commits")
        );
        // Merged and retargeted, not yet synced: sync then push.
        let merged = stack(ParentState::Merged, "main");
        assert!(
            stack_next_action(&merged, Some("tip0"), "main")
                .unwrap()
                .contains("--session 2`, then")
        );
        // Synced onto main: nothing.
        assert_eq!(stack_next_action(&merged, Some("main-tip"), "main"), None);
        // Closed unmerged: the choice, never an automatic retarget.
        let closed = stack(ParentState::Closed, "agent/parent");
        let action = stack_next_action(&closed, Some("tip0"), "main").unwrap();
        assert!(
            action.contains("--unstack") && action.contains("finish"),
            "{action}"
        );
        // Unstacked after the close: nothing.
        assert_eq!(
            stack_next_action(&stack(ParentState::Abandoned, "main"), Some("x"), "main"),
            None
        );
    }

    #[test]
    fn parent_states_round_trip_their_stored_strings() {
        for state in [
            ParentState::Open,
            ParentState::Merged,
            ParentState::Closed,
            ParentState::Abandoned,
        ] {
            assert_eq!(ParentState::parse(state.as_str()), Some(state));
        }
        assert_eq!(ParentState::parse("gone"), None);
    }
}
