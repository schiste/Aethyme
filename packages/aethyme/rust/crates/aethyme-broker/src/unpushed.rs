//! Committed work that exists on this machine and nowhere else.
//!
//! Measured on 2026-09-30 on one developer machine: 94 broker worktrees held
//! work found on no remote, about 75 of them belonging to *closed* sessions,
//! and one repository's local integration branch carried 74 commits no remote
//! had, the oldest five weeks old. Nothing reported any of it. The broker asked
//! "was this submitted?" and never "does a copy exist anywhere but this disk?",
//! so work piled up in worktrees and on integration until a cleanup or a dead
//! disk decided its fate.
//!
//! This answers the second question from local refs alone -- no fetch, no
//! network -- so `status` and `doctor` can afford it on every call. The answer
//! is "as of the last fetch", which errs toward reporting a pushed commit as
//! unpushed, never the reverse.

use std::collections::HashSet;

use crate::StatusAdviceSeverity;
use crate::git::{CherrySide, GitError, GitRepo};

/// Unpushed session commits younger than this are ordinary work in progress.
/// Four hours is roughly one working block: past it, the work is one worktree
/// cleanup or one failed disk away from gone, and the agent that wrote it may
/// already have moved on.
const PUSH_SOON_MS: i64 = 4 * 3_600_000;

/// Past a day the author's context is gone: the commits have outlived the
/// working session that could explain them. It matches the idle window the
/// artifact sweep uses before it treats an open session as abandoned (#428).
const PUSH_OVERDUE_MS: i64 = 24 * 3_600_000;

/// Commits only this machine holds, and when the oldest of them was made.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct OffRemoteWork {
    pub commits: u32,
    pub oldest_at_ms: Option<i64>,
}

/// Non-merge commits reachable from `head` that no remote-tracking ref and
/// none of `excluded` reaches, less any that are patch-equivalent to a commit
/// on `upstream`.
///
/// The patch filter is what keeps delivered work out of the count: a pull
/// request merged by squash or rebase lands the same change under another SHA,
/// and reporting it as at risk would teach operators to ignore the number
/// (#408).
pub(crate) fn off_remote_work(
    repo: &GitRepo,
    head: &str,
    excluded: &[&str],
    upstream: Option<&str>,
) -> Result<OffRemoteWork, GitError> {
    let mut commits = repo
        .own_commits_not_on_remotes(head, excluded)?
        .into_iter()
        .map(|commit| (commit.sha, commit.committed_at_ms))
        .collect::<Vec<_>>();
    if let Some(upstream) = upstream
        && !commits.is_empty()
    {
        let landed: HashSet<String> = repo
            .cherry_marked(upstream, head, CherrySide::Right)?
            .into_iter()
            .filter_map(|(commit, equivalent)| equivalent.then_some(commit))
            .collect();
        commits.retain(|(commit, _)| !landed.contains(commit));
    }
    Ok(OffRemoteWork {
        commits: u32::try_from(commits.len()).unwrap_or(u32::MAX),
        oldest_at_ms: commits.iter().map(|(_, at)| *at).min(),
    })
}

/// How loudly to report a session's unpushed commits.
///
/// Without the push lane nobody has promised to push, so the count is context
/// and never rises above a notice. With it, a day-old unpushed commit is
/// `blocked` in the literal sense: `finish` refuses to close the session until
/// it is pushed or explicitly abandoned.
pub(crate) fn session_severity(age_ms: i64, push_session_branches: bool) -> StatusAdviceSeverity {
    match (push_session_branches, age_ms) {
        (false, age) if age >= PUSH_OVERDUE_MS => StatusAdviceSeverity::Notice,
        (false, _) => StatusAdviceSeverity::Info,
        (true, age) if age >= PUSH_OVERDUE_MS => StatusAdviceSeverity::Blocked,
        (true, age) if age >= PUSH_SOON_MS => StatusAdviceSeverity::Warning,
        (true, _) => StatusAdviceSeverity::Info,
    }
}

/// How loudly to report integration running ahead of upstream. Nothing is
/// blocked by it, so it tops out at a warning; a day is when a promotion stops
/// being "about to ship" and starts being the pile-up.
pub(crate) fn integration_severity(age_ms: i64) -> StatusAdviceSeverity {
    if age_ms >= PUSH_OVERDUE_MS {
        StatusAdviceSeverity::Warning
    } else {
        StatusAdviceSeverity::Notice
    }
}

/// Human-readable age for advice text, coarse on purpose.
pub(crate) fn describe_age(age_ms: i64) -> String {
    let hours = age_ms.max(0) / 3_600_000;
    match hours {
        0 => format!("{} minute(s)", age_ms.max(0) / 60_000),
        1..=47 => format!("{hours} hour(s)"),
        _ => format!("{} day(s)", hours / 24),
    }
}

/// Unpushed work across a repository's live sessions and its integration
/// branch, as reported by `status` and `doctor`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct UnpushedWorkReport {
    /// Whether the repository opted into the push lane: the same trusted
    /// read `broker push` makes, `[delivery] push_session_branches = true`
    /// committed on the fetched default branch.
    pub push_session_branches: bool,
    /// Live sessions holding commits only their worktree has.
    pub sessions: Vec<UnpushedSessionWork>,
    /// Integration commits upstream lacks, when there are any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub integration: Option<UnpublishedIntegrationWork>,
}

impl UnpushedWorkReport {
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty() && self.integration.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UnpushedSessionWork {
    pub session_id: i64,
    pub branch: String,
    pub head: String,
    /// Commits reachable from the session head that no remote-tracking ref
    /// holds and the session did not inherit from its start or reuse base,
    /// less those patch-equivalent to upstream. Deliberately narrower than
    /// `SessionPushState::unpushed_commits`, which counts everything a push
    /// would send, inherited commits included: this counts only work that
    /// would be lost with the worktree.
    pub unpushed_commits: u32,
    /// Committer time of the oldest of them, Unix epoch milliseconds.
    pub oldest_unpushed_at_ms: Option<i64>,
    pub severity: StatusAdviceSeverity,
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UnpublishedIntegrationWork {
    pub branch: String,
    pub head: String,
    pub upstream_ref: String,
    /// Integration commits upstream lacks, less those patch-equivalent to it.
    pub unpublished_commits: u32,
    /// How many of those no remote-tracking ref holds at all: the ones a lost
    /// disk would take with it.
    pub on_no_remote: u32,
    pub oldest_unpublished_at_ms: Option<i64>,
    pub severity: StatusAdviceSeverity,
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: i64 = 3_600_000;

    #[test]
    fn without_the_push_lane_the_count_is_context_only() {
        assert_eq!(session_severity(HOUR, false), StatusAdviceSeverity::Info);
        assert_eq!(
            session_severity(48 * HOUR, false),
            StatusAdviceSeverity::Notice
        );
    }

    #[test]
    fn with_the_push_lane_severity_escalates_with_age() {
        assert_eq!(session_severity(HOUR, true), StatusAdviceSeverity::Info);
        assert_eq!(
            session_severity(5 * HOUR, true),
            StatusAdviceSeverity::Warning
        );
        assert_eq!(
            session_severity(25 * HOUR, true),
            StatusAdviceSeverity::Blocked
        );
    }
}
