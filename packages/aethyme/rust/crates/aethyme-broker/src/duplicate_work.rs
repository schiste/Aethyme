//! Live sessions working on the same pull request or branch.
//!
//! Several agents repairing one pull request at once conflict with each other
//! by construction: they edit the same lines toward the same goal. Measured in
//! one repository on 2026-10-01, pull request #1122 had three live sessions
//! ("Resolve merge conflicts and refresh PR #1122 against current…" twice, and
//! "Repair and merge PR1122 first…") and #1149 had two. Their lease overlaps
//! were real Git conflicts, so the broker reported them correctly. It just
//! never said why they kept happening.
//!
//! This names the duplication itself, from local facts only: the session
//! branches, the cached open-PR listing `broker push` records (#441), and PR
//! numbers written in the task text. It warns and never blocks. Two agents on
//! one PR can be intentional, and only they can decide which one stops.

use std::collections::{BTreeMap, BTreeSet};

use crate::{AgentView, Broker, Session, SessionStatus, StatusAdvice, StatusAdviceSeverity};

/// Why two sessions look like the same work, strongest first.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum DuplicateWorkReason {
    /// Both sessions work on one branch.
    SameBranch,
    /// One session's branch is the head of an open PR the other one targets.
    SamePr,
    /// Both tasks name the same PR number. A weaker signal: a task can
    /// mention a PR it does not change.
    TaskMentionsPr,
}

impl DuplicateWorkReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SameBranch => "same_branch",
            Self::SamePr => "same_pr",
            Self::TaskMentionsPr => "task_mentions_pr",
        }
    }

    fn describe(self, pull_request: Option<i64>) -> String {
        match (self, pull_request) {
            (Self::SameBranch, _) => "works on the same branch".into(),
            (Self::SamePr, Some(pr)) => format!("works on the same pull request #{pr}"),
            (Self::SamePr, None) => "works on the same pull request".into(),
            (Self::TaskMentionsPr, Some(pr)) => format!("has a task that also names PR #{pr}"),
            (Self::TaskMentionsPr, None) => "has a task that names the same PR".into(),
        }
    }
}

/// Another session that appears to do the same work.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DuplicateWork {
    pub session_id: i64,
    /// The other session's derived status: `stale` and `exited` sessions are
    /// reported too, but only live ones make the advice a warning.
    pub status: SessionStatus,
    pub task: Option<String>,
    pub reason: DuplicateWorkReason,
    pub pull_request: Option<i64>,
}

/// Whether a derived status means an agent is still working.
fn is_live(status: SessionStatus) -> bool {
    matches!(status, SessionStatus::Active | SessionStatus::Idle)
}

/// PR numbers written in a task: `PR #1122`, `PR1122`, `pr-1122`,
/// `pull request 1122`, or a bare `#1122`.
pub fn pr_numbers_in_task(task: &str) -> BTreeSet<i64> {
    let lower = task.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let at_word_start = |index: usize| index == 0 || !bytes[index - 1].is_ascii_alphanumeric();
    let skip = |mut index: usize, separators: &[u8]| {
        while index < bytes.len() && separators.contains(&bytes[index]) {
            index += 1;
        }
        index
    };
    let mut found = BTreeSet::new();
    for index in 0..bytes.len() {
        let digits_from = if bytes[index] == b'#' {
            Some(index + 1)
        } else if bytes[index..].starts_with(b"pull request") && at_word_start(index) {
            Some(skip(index + b"pull request".len(), b" #"))
        } else if bytes[index..].starts_with(b"pr") && at_word_start(index) {
            Some(skip(index + 2, b" #-_"))
        } else {
            None
        };
        let Some(start) = digits_from else {
            continue;
        };
        let end = start
            + bytes[start..]
                .iter()
                .take_while(|byte| byte.is_ascii_digit())
                .count();
        if end == start || (end < bytes.len() && bytes[end].is_ascii_alphabetic()) {
            continue;
        }
        if let Some(number) = std::str::from_utf8(&bytes[start..end])
            .ok()
            .and_then(|digits| digits.parse::<i64>().ok())
            .filter(|number| *number > 0)
        {
            found.insert(number);
        }
    }
    found
}

/// What one session works on, as far as local facts show.
struct Target {
    branch: String,
    /// The open PR whose head is this session's branch, from the cached
    /// listing `broker push` records. `status` never asks GitHub.
    own_pr: Option<i64>,
    task_prs: BTreeSet<i64>,
}

fn duplicate_target(session: &Session, prs_by_head: &BTreeMap<String, i64>) -> Target {
    Target {
        branch: session.branch.clone(),
        own_pr: prs_by_head.get(&session.branch).copied(),
        task_prs: session
            .task
            .as_deref()
            .map(pr_numbers_in_task)
            .unwrap_or_default(),
    }
}

impl Broker {
    /// Other sessions that look like the same work as `session`, strongest
    /// reason first. Closed sessions never count.
    pub(crate) fn duplicate_work_among(
        &self,
        session: &Session,
        agents: &[AgentView],
    ) -> Vec<DuplicateWork> {
        duplicates_among(session, agents, &self.cached_open_prs_by_head())
    }

    /// [`Broker::duplicate_work_among`] against every live session now.
    pub fn duplicate_work_for(&mut self, session: &Session) -> Vec<DuplicateWork> {
        let agents = self.agents(crate::clock::epoch_ms()).unwrap_or_default();
        self.duplicate_work_among(session, &agents)
    }

    /// One `session.duplicate-work` advice row per pair of sessions that look
    /// like the same work. A warning when both are live, info when one of
    /// them has gone quiet.
    pub(crate) fn duplicate_work_advice(&self, agents: &[AgentView]) -> Vec<StatusAdvice> {
        let prs_by_head = self.cached_open_prs_by_head();
        let mut advice = Vec::new();
        for agent in agents
            .iter()
            .filter(|agent| !agent.derived_status.is_closed())
        {
            for duplicate in duplicates_among(&agent.session, agents, &prs_by_head) {
                // One row per pair: the lower session id speaks for both.
                if duplicate.session_id < agent.session.id {
                    continue;
                }
                let both_live = is_live(agent.derived_status) && is_live(duplicate.status);
                advice.push(StatusAdvice {
                    id: "session.duplicate-work",
                    severity: if both_live {
                        StatusAdviceSeverity::Warning
                    } else {
                        StatusAdviceSeverity::Info
                    },
                    reason: "two sessions appear to work on the same pull request or branch",
                    summary: format!(
                        "session {} ({}) and session {} ({}): session {} {}; their changes will \
                         keep conflicting until one of them stops or they divide the work",
                        agent.session.id,
                        agent.derived_status.as_str(),
                        duplicate.session_id,
                        duplicate.status.as_str(),
                        duplicate.session_id,
                        duplicate.reason.describe(duplicate.pull_request),
                    ),
                    session_id: Some(agent.session.id),
                    queue_entry_id: None,
                    evidence: [
                        (agent.session.id, agent.session.task.as_deref()),
                        (duplicate.session_id, duplicate.task.as_deref()),
                    ]
                    .into_iter()
                    .map(|(id, task)| format!("session {id}: {}", task.unwrap_or("(no task)")))
                    .collect(),
                    commands: vec![
                        format!(
                            "aethyme broker advanced note send --session {} --to-session {} \
                             --message \"<who continues, who stops>\"",
                            agent.session.id, duplicate.session_id
                        ),
                        "aethyme broker finish --session <the session that should stop>"
                            .to_string(),
                    ],
                });
            }
        }
        advice
    }
}

/// Sessions in `agents` that look like the same work as `session`.
fn duplicates_among(
    session: &Session,
    agents: &[AgentView],
    prs_by_head: &BTreeMap<String, i64>,
) -> Vec<DuplicateWork> {
    let mine = duplicate_target(session, prs_by_head);
    let mut found = agents
        .iter()
        .filter(|other| other.session.id != session.id && !other.derived_status.is_closed())
        .filter_map(|other| {
            let theirs = duplicate_target(&other.session, prs_by_head);
            let (reason, pull_request) = compare(&mine, &theirs)?;
            Some(DuplicateWork {
                session_id: other.session.id,
                status: other.derived_status,
                task: other.session.task.clone(),
                reason,
                pull_request,
            })
        })
        .collect::<Vec<_>>();
    found.sort_by_key(|duplicate| {
        (
            duplicate.reason,
            !is_live(duplicate.status),
            duplicate.session_id,
        )
    });
    found
}

/// Why `theirs` looks like the same work as `mine`, if it does.
fn compare(mine: &Target, theirs: &Target) -> Option<(DuplicateWorkReason, Option<i64>)> {
    if mine.branch == theirs.branch {
        return Some((
            DuplicateWorkReason::SameBranch,
            mine.own_pr.or(theirs.own_pr),
        ));
    }
    let shared_pr = |own: Option<i64>, other_own: Option<i64>, other_task: &BTreeSet<i64>| {
        own.filter(|pr| other_own == Some(*pr) || other_task.contains(pr))
    };
    if let Some(pr) = shared_pr(mine.own_pr, theirs.own_pr, &theirs.task_prs)
        .or_else(|| shared_pr(theirs.own_pr, mine.own_pr, &mine.task_prs))
    {
        return Some((DuplicateWorkReason::SamePr, Some(pr)));
    }
    mine.task_prs
        .intersection(&theirs.task_prs)
        .next()
        .map(|pr| (DuplicateWorkReason::TaskMentionsPr, Some(*pr)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbers(task: &str) -> Vec<i64> {
        pr_numbers_in_task(task).into_iter().collect()
    }

    #[test]
    fn pr_numbers_are_read_from_the_usual_spellings() {
        assert_eq!(
            numbers("Resolve merge conflicts and refresh PR #1122"),
            vec![1122]
        );
        assert_eq!(numbers("Repair and merge PR1122 first"), vec![1122]);
        assert_eq!(numbers("repair-pr-1121-and-1122-contract"), vec![1121]);
        assert_eq!(numbers("follow up on pull request 1149"), vec![1149]);
        assert_eq!(numbers("fix #430 and #431"), vec![430, 431]);
    }

    #[test]
    fn words_that_start_with_pr_are_not_pull_requests() {
        assert!(numbers("prepare the v0.8.12 release").is_empty());
        assert!(numbers("improve 12 probes").is_empty());
        assert!(numbers("#12abc is not a number").is_empty());
    }

    fn target(branch: &str, own_pr: Option<i64>, task: &str) -> Target {
        Target {
            branch: branch.into(),
            own_pr,
            task_prs: pr_numbers_in_task(task),
        }
    }

    #[test]
    fn the_strongest_shared_target_wins() {
        let a = target("agent/a", None, "repair PR #1122");
        let b = target("agent/a", None, "something else");
        assert_eq!(
            compare(&a, &b),
            Some((DuplicateWorkReason::SameBranch, None))
        );

        let owner = target("agent/owner", Some(1122), "implement the feature");
        let repairer = target("agent/repair", None, "refresh PR #1122 against main");
        assert_eq!(
            compare(&repairer, &owner),
            Some((DuplicateWorkReason::SamePr, Some(1122)))
        );
        assert_eq!(
            compare(&owner, &repairer),
            Some((DuplicateWorkReason::SamePr, Some(1122)))
        );

        let x = target("agent/x", None, "Resolve conflicts in PR #1122");
        let y = target("agent/y", None, "Repair and merge PR1122 first");
        assert_eq!(
            compare(&x, &y),
            Some((DuplicateWorkReason::TaskMentionsPr, Some(1122)))
        );
    }

    #[test]
    fn unrelated_sessions_are_not_duplicates() {
        let a = target("agent/a", Some(10), "PR #11");
        let b = target("agent/b", Some(12), "PR #13");
        assert_eq!(compare(&a, &b), None);
    }
}
