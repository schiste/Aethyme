//! `broker push`: publish a session's own branch while the work is under way.
//!
//! Work that exists only in a worktree is lost with it. Measured 2026-09-30:
//! 94 broker worktrees held commits or edits found on no remote, most of them
//! in closed sessions, and one repository's local integration branch was 74
//! unpublished commits ahead of its remote. Submission promotes locally and
//! publication was a separate act nobody was authorized to take, so work piled
//! up where only one disk held it.
//!
//! This lane makes the cheap, safe publication routine. It pushes exactly one
//! ref -- the session's own `agent/*` branch -- and nothing that another
//! session or the default branch depends on. Authorization is durable and
//! repository-level (`[delivery] push_session_branches = true` on the default
//! branch), so an agent can push after every commit without asking each time,
//! and a repository that has not opted in is refused with the key to set.

use std::path::Path;

use crate::{
    Broker, BrokerOpError, CoordinatedCommand, CoordinatedOperationReport, GitRepo,
    OperationEffect, OperationProvider, QueueWait, RepositoryDeliveryConfig,
    UnknownOutcomeRecovery,
};

/// The configuration key that authorizes `broker push`, as printed in refusals.
pub const SESSION_PUSH_POLICY_KEY: &str = "delivery.push_session_branches";

/// Prefix every broker session branch carries. `broker push` publishes only
/// refs under it, so the default branch, `aethyme/integration` and tags are
/// out of reach by construction rather than by a list of exceptions.
const SESSION_BRANCH_PREFIX: &str = "agent/";

/// `meta` key recording the last oid this broker pushed for one session.
fn pushed_oid_key(session_id: i64) -> String {
    format!("session_push.{session_id}.oid")
}

/// How long one push or GitHub call may queue for the repository write lock.
fn push_operation_wait() -> QueueWait {
    QueueWait::Seconds(60)
}

/// How long a default-branch refresh may queue for the repository lock.
///
/// Short on purpose: `start` and `sync` run it before an agent begins, and a
/// busy lane must cost seconds, not the minute a push may wait. Falling back
/// to the last fetched copy is always safe; the result says so.
fn fetch_operation_wait() -> QueueWait {
    QueueWait::Seconds(10)
}

/// Where a session's branch stands relative to its remote, from local refs.
///
/// Read-only and never fetches, so it is as fresh as the last fetch or push.
/// `unpushed_commits` counts commits reachable from the branch that no
/// remote-tracking ref holds -- work that exists only on this machine.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionPushState {
    pub branch: String,
    pub head_oid: String,
    pub remote_oid: Option<String>,
    pub unpushed_commits: u32,
    pub oldest_unpushed_at_ms: Option<i64>,
}

/// The pull request `broker push --pr` found or opened.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionPullRequest {
    pub url: String,
    pub number: i64,
    pub state: String,
    /// True when this invocation created it.
    pub created: bool,
}

/// What `broker push` did.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionPushReport {
    pub session_id: i64,
    pub branch: String,
    pub remote: String,
    pub pushed_oid: String,
    /// The remote branch before this push, as this broker last knew it.
    pub previous_remote_oid: Option<String>,
    /// Commits the push made available that no remote held before.
    pub commits_pushed: u32,
    /// Uncommitted entries left in the worktree; only commits are pushed.
    /// `uncommitted.modified + uncommitted.untracked_entries`: an untracked
    /// directory counts once, as `git status` shows it.
    pub uncommitted_files: u32,
    pub uncommitted: UncommittedCounts,
    pub pr: Option<SessionPullRequest>,
    /// Open pull requests whose change overlaps this session's: same files,
    /// and whether the changed lines overlap or nearly touch. Advisory only.
    pub pr_overlaps: Vec<crate::PrOverlap>,
    /// Open pull requests whose change could not be read, so their overlap
    /// is unknown rather than absent.
    pub pr_overlaps_unknown: Vec<i64>,
    /// Other sessions that look like the same work: the same branch, the same
    /// open PR, or a task naming the same PR. Omitted when none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub duplicate_work: Vec<crate::DuplicateWork>,
    /// The pushed head compared with the default branch, fetched just before
    /// the comparison: how far behind it is and whether merging would
    /// conflict. `None` when Git could not answer. Advisory only.
    pub default_branch: Option<crate::DefaultBranchDrift>,
    /// Why `default_branch` is missing or may be stale, e.g. the fetch failed
    /// and the comparison used the last fetched copy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_branch_note: Option<String>,
}

/// How many paths `broker push` left behind, and a few of them.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct UncommittedCounts {
    /// Tracked paths with staged or unstaged changes.
    pub modified: u32,
    /// Untracked entries; an untracked directory is one entry.
    pub untracked_entries: u32,
    /// Up to [`UNCOMMITTED_SAMPLE`] paths, modified first.
    pub sample: Vec<String>,
}

/// How many uncommitted paths a push report names.
pub const UNCOMMITTED_SAMPLE: usize = 5;

impl From<&crate::UncommittedSummary> for UncommittedCounts {
    fn from(summary: &crate::UncommittedSummary) -> Self {
        let count = |paths: &Vec<String>| u32::try_from(paths.len()).unwrap_or(u32::MAX);
        Self {
            modified: count(&summary.modified),
            untracked_entries: count(&summary.untracked),
            sample: summary
                .modified
                .iter()
                .chain(&summary.untracked)
                .take(UNCOMMITTED_SAMPLE)
                .cloned()
                .collect(),
        }
    }
}

/// The remote and default branch the main checkout tracks, and the commit its
/// remote-tracking ref points at.
pub(crate) struct TrackedDefault {
    pub(crate) remote: String,
    pub(crate) branch: String,
    pub(crate) tracking_ref: String,
    pub(crate) commit: String,
}

pub(crate) fn tracked_default(repo: &GitRepo) -> Option<TrackedDefault> {
    let (upstream, commit) = repo.upstream_default()?;
    let (remote, branch) = upstream
        .strip_prefix("refs/remotes/")
        .unwrap_or(&upstream)
        .split_once('/')
        .filter(|(remote, branch)| !remote.is_empty() && !branch.is_empty())?;
    Some(TrackedDefault {
        remote: remote.to_string(),
        branch: branch.to_string(),
        tracking_ref: format!("refs/remotes/{remote}/{branch}"),
        commit,
    })
}

fn refused(reason: impl Into<String>) -> BrokerOpError {
    BrokerOpError::SessionPushRefused {
        reason: reason.into(),
    }
}

fn operation_failure(phase: &'static str, report: &CoordinatedOperationReport) -> BrokerOpError {
    if report.operation.status == crate::OperationStatus::OutcomeUnknown {
        BrokerOpError::CoordinatedOperationBlocked {
            repository: report.operation.repository.clone(),
            operation_id: report.operation.id,
            recovery: UnknownOutcomeRecovery::from_operation(&report.operation),
        }
    } else {
        BrokerOpError::SessionPushFailed {
            phase,
            operation_id: report.operation.id,
            status: report.operation.status.as_str(),
            stderr: report.stderr.trim().to_string(),
        }
    }
}

/// Why `branch` may not be published by `broker push`, if it may not.
fn branch_refusal(branch: &str, default_branch: &str) -> Option<String> {
    let Some(name) = branch.strip_prefix(SESSION_BRANCH_PREFIX) else {
        return Some(format!(
            "{branch:?} is not a session branch; broker push publishes only \
             {SESSION_BRANCH_PREFIX}* branches"
        ));
    };
    if name.is_empty() {
        return Some(format!("{branch:?} is not a session branch"));
    }
    if branch == default_branch || branch.starts_with("refs/") || branch.contains("..") {
        return Some(format!("{branch:?} is not a publishable session branch"));
    }
    None
}

/// Whether the trusted repository policy authorizes session-branch pushes.
///
/// Read from the committed `.aethyme/config.toml` at the default branch's
/// remote-tracking ref -- never from a worktree or the session's own branch,
/// which the pushing agent could have edited. This is the same rule `ship`
/// applies to its delivery policy (a copy of the default branch), taken from
/// the last fetch rather than a network read.
/// [`push_authorized`] for readers that only report or tighten: `status`,
/// `doctor`, `finish`. No tracked default branch, or a policy that cannot be
/// read, means "not opted in" rather than an error, so those commands keep
/// working in a repository `broker push` would refuse.
pub(crate) fn session_push_enabled(repo: &GitRepo) -> bool {
    tracked_default(repo).is_some_and(|default| push_authorized(repo, &default).unwrap_or(false))
}

fn push_authorized(repo: &GitRepo, default: &TrackedDefault) -> Result<bool, BrokerOpError> {
    let Some(text) = repo.file_at_commit(&default.commit, ".aethyme/config.toml")? else {
        return Ok(false);
    };
    let config = RepositoryDeliveryConfig::from_config_text(&text).map_err(refused)?;
    Ok(config.is_some_and(|config| config.push_session_branches))
}

impl Broker {
    /// Where a session's branch stands against its remote-tracking ref.
    /// See [`SessionPushState`]; this never fetches or writes.
    pub fn session_push_state(
        &mut self,
        session_id: i64,
    ) -> Result<SessionPushState, BrokerOpError> {
        let session = self.store().session(session_id)?;
        let repo = self.repo_handle();
        let remote = tracked_default(repo)
            .map(|default| default.remote)
            .unwrap_or_else(|| "origin".to_string());
        let head = repo
            .resolve_ref(&format!("refs/heads/{}", session.branch))
            .ok_or_else(|| BrokerOpError::SessionBranchMissing {
                session_id,
                branch: session.branch.clone(),
            })?;
        let remote_oid = repo.resolve_ref(&format!("refs/remotes/{remote}/{}", session.branch));
        let unpushed = repo.commits_not_on_remotes(&head)?;
        Ok(SessionPushState {
            branch: session.branch,
            head_oid: head,
            remote_oid,
            unpushed_commits: u32::try_from(unpushed.len()).unwrap_or(u32::MAX),
            oldest_unpushed_at_ms: unpushed.last().map(|commit| commit.committed_at_ms),
        })
    }

    /// Publish a live session's own branch, and optionally open a draft pull
    /// request for it. See the module documentation for the authorization.
    pub fn push_session(
        &mut self,
        session_id: i64,
        open_pr: bool,
    ) -> Result<SessionPushReport, BrokerOpError> {
        let session = self.store().session(session_id)?;
        if session.status.is_closed() {
            return Err(BrokerOpError::ClosedSessionOperation {
                session_id,
                repository_root: self.main_root().display().to_string(),
            });
        }
        let main_root = self.main_root().to_path_buf();
        let repo = self.repo_handle();
        let default = tracked_default(repo).ok_or_else(|| {
            refused(
                "the main checkout's branch has no fetched upstream, so there is no trusted \
                 default branch to read the push policy from; set one with \
                 `git branch --set-upstream-to <remote>/<branch>`",
            )
        })?;
        if let Some(reason) = branch_refusal(&session.branch, &default.branch) {
            return Err(refused(reason));
        }
        if !push_authorized(repo, &default)? {
            return Err(refused(format!(
                "repository policy {SESSION_PUSH_POLICY_KEY} is not enabled on {} ({}); add \
                 `push_session_branches = true` under `[delivery]` in .aethyme/config.toml on \
                 the default branch, or publish explicitly with `aethyme broker advanced git \
                 --session {session_id} --reason <authorization> -- push ...`",
                default.tracking_ref,
                &default.commit[..default.commit.len().min(12)],
            )));
        }
        let target = repo
            .resolve_remote_target(&default.remote, None)
            .map_err(|error| refused(format!("remote {:?}: {error}", default.remote)))?;
        let branch_ref = format!("refs/heads/{}", session.branch);
        let head =
            repo.resolve_ref(&branch_ref)
                .ok_or_else(|| BrokerOpError::SessionBranchMissing {
                    session_id,
                    branch: session.branch.clone(),
                })?;
        let tracking = repo.resolve_ref(&format!(
            "refs/remotes/{}/{}",
            default.remote, session.branch
        ));
        let new_commits = repo.commits_not_on_remotes(&head)?.len();
        let uncommitted = Path::new(&session.worktree_path)
            .is_dir()
            .then(|| GitRepo::discover(Path::new(&session.worktree_path)).ok())
            .flatten()
            .and_then(|worktree| worktree.uncommitted_summary().ok())
            .unwrap_or_default();
        let recorded = self.store().meta_get(&pushed_oid_key(session_id))?;
        let repo = self.repo_handle();

        // A plain push when the remote only moves forward: Git itself refuses
        // anything else. A rewritten branch (rebase, amend) needs a lease, and
        // the lease is only ever against an oid this broker recorded pushing
        // for this session -- never one merely seen in a fetch, which could be
        // someone else's work under the same name.
        let known = recorded.clone().or_else(|| tracking.clone());
        let fast_forward = known
            .as_deref()
            .is_none_or(|known| known == head || repo.is_ancestor(known, &head));
        let (args, effect) = if fast_forward {
            (
                vec![
                    "push".to_string(),
                    default.remote.clone(),
                    format!("{head}:{branch_ref}"),
                ],
                OperationEffect::Write,
            )
        } else if let Some(pushed) = recorded.as_deref() {
            (
                vec![
                    "push".to_string(),
                    format!("--force-with-lease={branch_ref}:{pushed}"),
                    default.remote.clone(),
                    format!("{head}:{branch_ref}"),
                ],
                OperationEffect::Destructive,
            )
        } else {
            return Err(refused(format!(
                "remote branch {} is at {}, which this session's branch does not contain and \
                 this broker never pushed; inspect it before replacing it",
                session.branch,
                known.unwrap_or_default()
            )));
        };
        let push = self.run_coordinated_operation_at_with_wait(
            CoordinatedCommand {
                session_id,
                provider: OperationProvider::Git,
                repository: None,
                resolved_target: Some(target.clone()),
                scope: Some(format!("session-push:{branch_ref}")),
                declared_effect: Some(effect),
                // The lease is the confirmation: it can only replace this
                // session's own last push, on this session's own branch.
                destructive_confirmed: effect == OperationEffect::Destructive,
                authorization_reason: Some(format!("repository policy {SESSION_PUSH_POLICY_KEY}")),
                args,
            },
            &main_root,
            push_operation_wait(),
        )?;
        if !push.ok() {
            return Err(operation_failure("push", &push));
        }
        self.store().meta_set(&pushed_oid_key(session_id), &head)?;

        let pr = if open_pr {
            Some(self.open_session_pull_request(
                session_id,
                &session,
                &default,
                &target.display_slug,
                &head,
                &main_root,
            )?)
        } else {
            None
        };

        // Advisory and best-effort, like the overlap check below: the default
        // branch moved under this session whenever another PR merged, and
        // that is only visible after fetching it.
        let (default_branch, default_branch_note) = self.fetch_and_measure_default_branch(
            session_id,
            &session.branch,
            &default,
            Some(&target),
            &head,
            &main_root,
        );

        // Advisory and best-effort: a listing or diff that cannot be read
        // degrades to "unknown" and never fails a push that already happened.
        let overlap = self.check_pr_overlaps_for_push(
            session_id,
            &session.branch,
            &head,
            &target.display_slug,
            &main_root,
            crate::clock::epoch_ms(),
        );

        let payload = serde_json::json!({
            "session_id": session_id,
            "branch": session.branch,
            "oid": head,
            "pr_url": pr.as_ref().map(|pr| pr.url.clone()),
        })
        .to_string();
        self.store().append_event(
            crate::events::BROKER_SESSION_PUSHED,
            Some(session_id),
            Some(&payload),
        )?;

        let duplicate_work = self.duplicate_work_for(&session);
        Ok(SessionPushReport {
            session_id,
            branch: session.branch,
            remote: default.remote,
            pushed_oid: head,
            previous_remote_oid: recorded.or(tracking),
            commits_pushed: u32::try_from(new_commits).unwrap_or(u32::MAX),
            uncommitted_files: u32::try_from(uncommitted.len()).unwrap_or(u32::MAX),
            uncommitted: UncommittedCounts::from(&uncommitted),
            pr,
            pr_overlaps: overlap.overlaps,
            pr_overlaps_unknown: overlap.unknown_prs,
            duplicate_work,
            default_branch,
            default_branch_note,
        })
    }

    /// Fetch exactly the default branch into its remote-tracking ref through
    /// the coordinated operation lane, on behalf of `session_id`.
    ///
    /// The fetch moves only that one remote-tracking ref, like every other
    /// remote read that moves a ref. `Err` carries a note saying why the
    /// caller is working from the last fetched copy; a failed fetch is never
    /// fatal to `push`, `start` or `sync`.
    pub(crate) fn fetch_default_branch(
        &mut self,
        session_id: i64,
        default: &TrackedDefault,
        target: Option<&crate::ResolvedRemoteTarget>,
        main_root: &Path,
    ) -> Result<(), String> {
        let target = match target {
            Some(target) => target.clone(),
            None => self
                .repo_handle()
                .resolve_remote_target(&default.remote, None)
                .map_err(|error| {
                    format!(
                        "remote {:?} cannot be resolved ({error}); used the last fetched copy",
                        default.remote
                    )
                })?,
        };
        let source = format!("refs/heads/{}", default.branch);
        let fetch = self.run_coordinated_operation_at_with_wait(
            CoordinatedCommand {
                session_id,
                provider: OperationProvider::Git,
                repository: None,
                resolved_target: Some(target),
                scope: Some(format!("ref:{}", default.tracking_ref)),
                declared_effect: None,
                destructive_confirmed: false,
                authorization_reason: Some(
                    "refresh the default branch to compare a session with it".into(),
                ),
                args: vec![
                    "fetch".into(),
                    default.remote.clone(),
                    format!("{source}:{}", default.tracking_ref),
                ],
            },
            main_root,
            fetch_operation_wait(),
        );
        match fetch {
            Ok(fetch) if fetch.ok() => Ok(()),
            Ok(fetch) => Err(format!(
                "fetching {} failed (operation {}); used the last fetched copy",
                default.tracking_ref, fetch.operation.id
            )),
            Err(error) => Err(format!(
                "fetching {} failed ({error}); used the last fetched copy",
                default.tracking_ref
            )),
        }
    }

    /// Fetch exactly the default branch, then compare `head` with it.
    ///
    /// The result is cached so `status` can show it without the network. A
    /// failed fetch falls back to the last fetched copy and says so.
    pub(crate) fn fetch_and_measure_default_branch(
        &mut self,
        session_id: i64,
        branch: &str,
        default: &TrackedDefault,
        target: Option<&crate::ResolvedRemoteTarget>,
        head: &str,
        main_root: &Path,
    ) -> (Option<crate::DefaultBranchDrift>, Option<String>) {
        let mut note = self
            .fetch_default_branch(session_id, default, target, main_root)
            .err();
        let repo = self.repo_handle();
        let Some(commit) = repo.resolve_ref(&default.tracking_ref) else {
            return (
                None,
                Some(format!("{} is not fetched", default.tracking_ref)),
            );
        };
        let reference = format!("{}/{}", default.remote, default.branch);
        let catch_up = crate::main_drift::CatchUp::for_branch(repo, &default.remote, branch);
        let drift = crate::main_drift::measure(repo, &reference, &commit, head, catch_up);
        match &drift {
            Some(drift) => {
                if let Ok(raw) = serde_json::to_string(drift) {
                    crate::warn_unrecorded(
                        "cache the default-branch comparison",
                        self.store()
                            .meta_set(&crate::main_drift::cache_key(session_id), &raw),
                    );
                }
            }
            None => {
                note.get_or_insert_with(|| {
                    format!("could not compare the session with {reference}")
                });
            }
        }
        (drift, note)
    }

    /// Find the open pull request for the session branch, or open a draft.
    /// Never marks one ready and never merges.
    fn open_session_pull_request(
        &mut self,
        session_id: i64,
        session: &crate::Session,
        default: &TrackedDefault,
        repository: &str,
        head: &str,
        cwd: &Path,
    ) -> Result<SessionPullRequest, BrokerOpError> {
        if let Some(existing) =
            self.find_session_pull_request(session_id, repository, &session.branch, cwd)?
        {
            return Ok(existing);
        }
        let commits = self
            .repo_handle()
            .commits_between(&default.tracking_ref, head)?;
        let title = commits
            .last()
            .map(|commit| commit.subject.clone())
            .filter(|subject| !subject.trim().is_empty())
            .or_else(|| session.task.clone())
            .unwrap_or_else(|| session.branch.clone());
        let mut body = String::new();
        if let Some(task) = session.task.as_deref() {
            body.push_str(task.trim());
            body.push_str("\n\n");
        }
        body.push_str("Commits:\n");
        for commit in commits.iter().rev() {
            body.push_str(&format!(
                "- {} {}\n",
                &commit.sha[..commit.sha.len().min(10)],
                commit.subject
            ));
        }
        body.push_str("\nOpened by aethyme broker push as a draft.\n");
        let create = self.run_coordinated_operation_at_with_wait(
            CoordinatedCommand {
                session_id,
                provider: OperationProvider::Github,
                repository: Some(repository.to_string()),
                resolved_target: None,
                scope: Some(format!("session-push:pr-create:{}", session.branch)),
                declared_effect: Some(OperationEffect::Write),
                destructive_confirmed: false,
                authorization_reason: Some(format!("repository policy {SESSION_PUSH_POLICY_KEY}")),
                args: vec![
                    "pr".into(),
                    "create".into(),
                    "--draft".into(),
                    "--head".into(),
                    session.branch.clone(),
                    "--base".into(),
                    default.branch.clone(),
                    "--title".into(),
                    title,
                    "--body".into(),
                    body,
                ],
            },
            cwd,
            push_operation_wait(),
        )?;
        if !create.ok() {
            return Err(operation_failure("pull request creation", &create));
        }
        // The provider, not gh's output text, is the evidence the PR exists.
        let mut created = self
            .find_session_pull_request(session_id, repository, &session.branch, cwd)?
            .ok_or_else(|| BrokerOpError::SessionPushFailed {
                phase: "pull request lookup after creation",
                operation_id: create.operation.id,
                status: create.operation.status.as_str(),
                stderr: format!(
                    "gh reported success but lists no open pull request for {}",
                    session.branch
                ),
            })?;
        created.created = true;
        // This push opened the pull request, so its opening is known now, not
        // only once someone watches it — and PR monitoring is off by default,
        // so without this most pull requests would never get a lifetime in
        // `broker advanced insights`. Recorded after the provider confirmed the
        // pull request exists, so the instant is never before the real one; a
        // later watch poll that reads the provider's `createdAt` can only move
        // it earlier (milestones keep the earliest sighting). Best-effort: the
        // push already succeeded and must not fail over telemetry.
        crate::warn_unrecorded(
            "record when the pull request opened",
            self.store().record_pull_request_opened(
                repository,
                created.number,
                Some(session_id),
                crate::clock::epoch_ms(),
            ),
        );
        Ok(created)
    }

    fn find_session_pull_request(
        &mut self,
        session_id: i64,
        repository: &str,
        branch: &str,
        cwd: &Path,
    ) -> Result<Option<SessionPullRequest>, BrokerOpError> {
        let list = self.run_coordinated_operation_at_with_wait(
            CoordinatedCommand {
                session_id,
                provider: OperationProvider::Github,
                repository: Some(repository.to_string()),
                resolved_target: None,
                scope: Some(format!("session-push:pr-list:{branch}")),
                declared_effect: Some(OperationEffect::Read),
                destructive_confirmed: false,
                authorization_reason: None,
                args: vec![
                    "pr".into(),
                    "list".into(),
                    "--head".into(),
                    branch.to_string(),
                    "--state".into(),
                    "open".into(),
                    "--json".into(),
                    "number,url,state,headRefName".into(),
                ],
            },
            cwd,
            push_operation_wait(),
        )?;
        if !list.ok() {
            return Err(operation_failure("pull request lookup", &list));
        }
        Ok(parse_pull_request_list(&list.stdout, branch))
    }
}

/// The open pull request whose head is exactly `branch`, from `gh pr list`
/// JSON. Anything unparseable is "none found" -- creation then fails loudly
/// on the post-create lookup rather than trusting a guess.
fn parse_pull_request_list(stdout: &str, branch: &str) -> Option<SessionPullRequest> {
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    value.as_array()?.iter().find_map(|pr| {
        (pr["headRefName"].as_str() == Some(branch)
            && pr["state"]
                .as_str()
                .is_some_and(|state| state.eq_ignore_ascii_case("open")))
        .then(|| SessionPullRequest {
            url: pr["url"].as_str().unwrap_or_default().to_string(),
            number: pr["number"].as_i64().unwrap_or_default(),
            state: pr["state"].as_str().unwrap_or_default().to_string(),
            created: false,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_session_branches_are_publishable() {
        assert_eq!(branch_refusal("agent/fix-thing", "main"), None);
        for branch in [
            "main",
            "aethyme/integration",
            "v1.0.0",
            "feature/x",
            "refs/heads/agent/x",
        ] {
            assert!(
                branch_refusal(branch, "main").is_some(),
                "{branch} was allowed"
            );
        }
        assert!(branch_refusal("agent/", "main").is_some());
        assert!(branch_refusal("agent/../main", "main").is_some());
        assert!(branch_refusal("agent/x", "agent/x").is_some());
    }

    #[test]
    fn a_pull_request_listing_matches_the_exact_head_branch() {
        let listing = r#"[
            {"number":3,"url":"https://github.com/a/b/pull/3","state":"OPEN","headRefName":"agent/other"},
            {"number":7,"url":"https://github.com/a/b/pull/7","state":"OPEN","headRefName":"agent/mine"}
        ]"#;
        let found = parse_pull_request_list(listing, "agent/mine").unwrap();
        assert_eq!(found.number, 7);
        assert!(!found.created);
        assert_eq!(parse_pull_request_list(listing, "agent/none"), None);
        assert_eq!(parse_pull_request_list("not json", "agent/mine"), None);
    }
}
