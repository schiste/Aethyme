//! Owner resolution (#1238): which sessions produced the landed commits in a
//! range, narrowed to the commits that touched given paths, and whether each
//! of those sessions is still alive to be told about a failure.
//!
//! Resolution is local and read-only. Each matching commit is traced to the
//! first-parent commit that landed it; a GitHub merge subject names the pull
//! request and branch, a squash subject names the pull request. The pull
//! request maps to sessions through the recorded watch/ship links, and the
//! branch through the session table, which keeps finished sessions.

use std::collections::BTreeMap;

use super::{Broker, BrokerOpError};
use crate::types::SessionStatus;

/// Most commits one query resolves; a wider range reports `truncated`.
pub const MAX_OWNER_COMMITS: usize = 200;

/// One matching commit and where it came from.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct OwnedCommit {
    pub sha: String,
    pub subject: String,
    /// The first-parent commit that brought `sha` onto the range's head.
    pub landing: String,
    pub pull_request: Option<i64>,
    pub branch: Option<String>,
    pub session_ids: Vec<i64>,
}

/// A session that produced at least one matching commit.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CommitOwner {
    pub session_id: i64,
    /// Liveness as `broker advanced agents` derives it.
    pub status: SessionStatus,
    /// Still able to receive a note: neither exited nor closed.
    pub live: bool,
    pub branch: String,
    pub pull_request: Option<i64>,
    pub task: Option<String>,
    pub agent_identity: Option<String>,
    pub commits: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct OwnersReport {
    pub range: String,
    pub paths: Vec<String>,
    pub repository: Option<String>,
    pub commits: Vec<OwnedCommit>,
    pub owners: Vec<CommitOwner>,
    /// Commits no session could be found for.
    pub unowned: Vec<String>,
    /// More than [`MAX_OWNER_COMMITS`] commits matched; only the newest are
    /// resolved.
    pub truncated: bool,
}

/// The pull request and branch a landing commit's subject names: GitHub's
/// `Merge pull request #N from owner/branch`, or a squash `subject (#N)`.
pub(crate) fn landing_reference(subject: &str) -> (Option<i64>, Option<String>) {
    if let Some(rest) = subject.strip_prefix("Merge pull request #") {
        let (number, rest) = rest.split_once(' ').unwrap_or((rest, ""));
        let pull_request = number.parse().ok();
        let branch = rest
            .strip_prefix("from ")
            .and_then(|head| head.split_once('/'))
            .map(|(_, branch)| branch.trim().to_string())
            .filter(|branch| !branch.is_empty());
        return (pull_request, branch);
    }
    let pull_request = subject
        .trim_end()
        .strip_suffix(')')
        .and_then(|rest| rest.rsplit_once("(#"))
        .and_then(|(_, number)| number.parse().ok());
    (pull_request, None)
}

/// Split `from..to` (or a single commit, read as the commits it brought in:
/// `commit^..commit`) into its endpoints. `to` defaults to `HEAD`.
pub(crate) fn split_range(range: &str) -> Result<(String, String), String> {
    let range = range.trim();
    if range.is_empty() {
        return Err("--commits requires a range such as <from>..<to>".into());
    }
    if range.contains("...") {
        return Err(
            "--commits takes a two-dot range <from>..<to>, not a symmetric difference".into(),
        );
    }
    if range.starts_with('-') || range.contains("..-") {
        return Err("--commits endpoints must be revisions, not options".into());
    }
    match range.split_once("..") {
        Some(("", _)) => Err("--commits range needs a start: <from>..<to>".into()),
        Some((from, "")) => Ok((from.to_string(), "HEAD".to_string())),
        Some((from, to)) => Ok((from.to_string(), to.to_string())),
        None => Ok((format!("{range}^"), range.to_string())),
    }
}

impl Broker {
    /// Resolve the sessions that produced the commits in `range` touching any
    /// of `paths` (every commit when empty). Read-only.
    pub fn owners(
        &self,
        range: &str,
        paths: &[String],
        now_ms: i64,
    ) -> Result<OwnersReport, BrokerOpError> {
        let invalid = |reason: String| BrokerOpError::InvalidOwnersRequest { reason };
        let (from, to) = split_range(range).map_err(invalid)?;
        for path in paths {
            if path.trim().is_empty() || path.starts_with('/') {
                return Err(invalid(format!(
                    "--path {path:?} must be a non-empty repository-relative glob"
                )));
            }
        }
        let head = self
            .repo
            .resolve_ref(&to)
            .ok_or_else(|| invalid(format!("--commits end {to:?} does not name a commit")))?;
        let base = self
            .repo
            .resolve_ref(&from)
            .ok_or_else(|| invalid(format!("--commits start {from:?} does not name a commit")))?;
        let mut matched = self
            .repo
            .commits_touching(&base, &head, paths, MAX_OWNER_COMMITS + 1)?;
        let truncated = matched.len() > MAX_OWNER_COMMITS;
        if truncated {
            matched.remove(0);
        }
        let repository = self
            .repo
            .remote_url("origin")
            .ok()
            .and_then(|url| crate::environments::budget_outcomes::github_slug(&url));

        let mut commits = Vec::with_capacity(matched.len());
        let mut owners: BTreeMap<i64, CommitOwner> = BTreeMap::new();
        let mut unowned = Vec::new();
        for sha in matched {
            let subject = self.repo.commit_subject(&sha)?;
            let landing = self.repo.landing_commit(&sha, &head)?;
            let landing_subject = if landing == sha {
                subject.clone()
            } else {
                self.repo.commit_subject(&landing)?
            };
            let (pull_request, branch) = landing_reference(&landing_subject);
            let mut sessions = Vec::new();
            if let (Some(repository), Some(number)) = (repository.as_deref(), pull_request) {
                for id in self.store.pull_request_sessions(repository, number)? {
                    sessions.push(self.store.session(id)?);
                }
            }
            if sessions.is_empty()
                && let Some(branch) = branch.as_deref()
            {
                sessions = self.store.sessions_on_branch(branch)?;
            }
            let session_ids: Vec<i64> = sessions.iter().map(|session| session.id).collect();
            if sessions.is_empty() {
                unowned.push(sha.clone());
            }
            for view in self.agent_views(sessions, now_ms)? {
                let owner = owners
                    .entry(view.session.id)
                    .or_insert_with(|| CommitOwner {
                        session_id: view.session.id,
                        status: view.derived_status,
                        live: !view.derived_status.is_closed()
                            && view.derived_status != SessionStatus::Exited,
                        branch: view.session.branch.clone(),
                        pull_request,
                        task: view.session.task.clone(),
                        agent_identity: view.session.agent_identity.clone(),
                        commits: Vec::new(),
                    });
                owner.commits.push(sha.clone());
            }
            commits.push(OwnedCommit {
                sha,
                subject,
                landing,
                pull_request,
                branch,
                session_ids,
            });
        }
        Ok(OwnersReport {
            range: range.to_string(),
            paths: paths.to_vec(),
            repository,
            commits,
            owners: owners.into_values().collect(),
            unowned,
            truncated,
        })
    }
}

#[cfg(test)]
#[path = "owners_tests.rs"]
mod tests;
