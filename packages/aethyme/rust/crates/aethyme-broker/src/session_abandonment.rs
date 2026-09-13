//! When the broker may conclude that a session's agent is gone.
//!
//! Every retention lane in the broker is downstream of this question. A
//! session that is still open pins its worktree and its session branch, and
//! `cleanup_plan` only ever considers sessions whose status has already
//! reached `cleaned`. So a session that is merely *quiet* holds its disk
//! forever: nothing moves it toward a terminal state, and no retention policy
//! can reach it. That is #176 -- 16 of 19 sessions stale, one worktree of 38
//! eligible, and 99.99% of the retained bytes reported as blocked.
//!
//! The shape here is deliberately the same as [`crate::expired`] for review
//! requests: a bounded wait, then a state change, rather than an unbounded
//! hold. The decision is a pure function over observations so it can be
//! tested without a process table or a database, and so the rule that
//! releases someone's worktree is readable in one place.

/// What the broker observed about one session, with liveness already
/// resolved by the caller.
///
/// `agent_alive` is deliberately a three-way value. `Some(true)` means a pid
/// was checked and answered; `Some(false)` means it was checked and is gone;
/// `None` means there is nothing to check -- an adopted session the broker
/// never spawned. Collapsing `None` into either answer is how this goes
/// wrong: treat it as alive and adopted sessions pin their disk forever,
/// treat it as dead and a working agent loses its worktree mid-task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionActivity {
    pub session_id: i64,
    /// Unix epoch milliseconds of the most recent evidence of work.
    pub last_activity_at: i64,
    /// Unix epoch milliseconds the session was created.
    ///
    /// The window is measured from whichever is later. Today the store writes
    /// `last_activity_at` at insert, so this floor never binds; it is here
    /// because a row that loses its activity timestamp -- an older binary, a
    /// hand-repaired database -- would otherwise read as infinitely idle, and
    /// the cost of being wrong in that direction is somebody's worktree.
    pub created_at: i64,
    /// `None` when the broker has no pid to interrogate.
    pub agent_alive: Option<bool>,
    /// Already terminal in broker state; nothing to abandon.
    pub closed: bool,
}

/// Why one session was left alone, or was not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbandonmentVerdict {
    /// The agent answered. Quiet is not gone.
    AgentAlive,
    /// Already terminal in broker state.
    AlreadyClosed,
    /// Inside the window; the broker is still waiting.
    Waiting { remaining_ms: i64 },
    /// The window elapsed with no evidence of an agent.
    Abandoned { idle_ms: i64 },
}

impl AbandonmentVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AgentAlive => "agent_alive",
            Self::AlreadyClosed => "already_closed",
            Self::Waiting { .. } => "waiting",
            Self::Abandoned { .. } => "abandoned",
        }
    }

    pub fn is_abandoned(self) -> bool {
        matches!(self, Self::Abandoned { .. })
    }
}

/// One session, judged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbandonmentDecision {
    pub session_id: i64,
    pub verdict: AbandonmentVerdict,
}

/// Decide whether a single session's agent should be treated as gone.
///
/// A live agent is never abandoned regardless of how long it has been quiet:
/// an agent thinking for three hours is not an agent that left, and taking
/// its worktree would destroy work in progress. That check comes first for
/// exactly that reason.
pub fn decide(activity: &SessionActivity, now_ms: i64, window_ms: i64) -> AbandonmentVerdict {
    if activity.closed {
        return AbandonmentVerdict::AlreadyClosed;
    }
    if activity.agent_alive == Some(true) {
        return AbandonmentVerdict::AgentAlive;
    }
    // A window of zero disables the lane outright. It is the documented way
    // for a repository to keep the old unbounded-hold behaviour, so it must
    // never be read as "abandon immediately".
    if window_ms <= 0 {
        return AbandonmentVerdict::Waiting {
            remaining_ms: i64::MAX,
        };
    }
    let last_seen = activity.last_activity_at.max(activity.created_at);
    let idle_ms = now_ms.saturating_sub(last_seen);
    // A clock that runs backwards (a restored snapshot, a corrected system
    // clock) yields a negative idle time. Treating that as "very idle" would
    // reap live sessions, so it counts as inside the window.
    if idle_ms >= window_ms {
        AbandonmentVerdict::Abandoned { idle_ms }
    } else {
        AbandonmentVerdict::Waiting {
            remaining_ms: window_ms.saturating_sub(idle_ms),
        }
    }
}

/// Judge every observed session, preserving input order.
pub fn survey(
    activities: &[SessionActivity],
    now_ms: i64,
    window_ms: i64,
) -> Vec<AbandonmentDecision> {
    activities
        .iter()
        .map(|activity| AbandonmentDecision {
            session_id: activity.session_id,
            verdict: decide(activity, now_ms, window_ms),
        })
        .collect()
}

/// The session ids whose agents the broker may treat as gone.
pub fn abandoned(activities: &[SessionActivity], now_ms: i64, window_ms: i64) -> Vec<i64> {
    survey(activities, now_ms, window_ms)
        .into_iter()
        .filter(|decision| decision.verdict.is_abandoned())
        .map(|decision| decision.session_id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: i64 = 60 * 60 * 1000;

    fn quiet(session_id: i64, last_activity_at: i64) -> SessionActivity {
        SessionActivity {
            session_id,
            last_activity_at,
            created_at: last_activity_at,
            agent_alive: None,
            closed: false,
        }
    }

    #[test]
    fn a_living_agent_is_never_abandoned_however_quiet() {
        let mut activity = quiet(1, 0);
        activity.agent_alive = Some(true);
        // Ten times the window. Liveness still wins: an agent that is
        // thinking is not an agent that left.
        assert_eq!(
            decide(&activity, 720 * HOUR, 72 * HOUR),
            AbandonmentVerdict::AgentAlive
        );
    }

    #[test]
    fn a_dead_agent_past_the_window_is_abandoned() {
        let mut activity = quiet(1, 0);
        activity.agent_alive = Some(false);
        assert_eq!(
            decide(&activity, 73 * HOUR, 72 * HOUR),
            AbandonmentVerdict::Abandoned { idle_ms: 73 * HOUR }
        );
    }

    #[test]
    fn an_unspawned_session_still_ages_out() {
        // agent_alive is None: the broker never spawned it, so there is no
        // pid to ask. It must still be reachable by policy, or adopted
        // sessions pin their worktrees forever.
        assert!(decide(&quiet(1, 0), 100 * HOUR, 72 * HOUR).is_abandoned());
    }

    #[test]
    fn inside_the_window_the_broker_reports_what_it_is_waiting_for() {
        assert_eq!(
            decide(&quiet(1, 0), 70 * HOUR, 72 * HOUR),
            AbandonmentVerdict::Waiting {
                remaining_ms: 2 * HOUR
            }
        );
    }

    #[test]
    fn the_boundary_belongs_to_abandonment() {
        assert!(decide(&quiet(1, 0), 72 * HOUR, 72 * HOUR).is_abandoned());
        assert!(!decide(&quiet(1, 0), 72 * HOUR - 1, 72 * HOUR).is_abandoned());
    }

    #[test]
    fn a_zero_window_disables_the_lane_rather_than_reaping_everything() {
        // The failure mode this guards is catastrophic: reading 0 as "no
        // wait required" would abandon every session on the machine at once.
        let verdict = decide(&quiet(1, 0), 10_000 * HOUR, 0);
        assert!(!verdict.is_abandoned());
        assert_eq!(verdict.as_str(), "waiting");
    }

    #[test]
    fn a_backwards_clock_does_not_reap_a_live_session() {
        // last_activity_at in the future: idle_ms goes negative.
        let verdict = decide(&quiet(1, 100 * HOUR), HOUR, 72 * HOUR);
        assert!(!verdict.is_abandoned());
    }

    #[test]
    fn a_row_that_lost_its_activity_timestamp_is_judged_on_its_age() {
        // last_activity_at of 0 would otherwise read as ~57 years idle.
        let mut activity = quiet(1, 0);
        activity.created_at = 99 * HOUR;
        assert!(!decide(&activity, 100 * HOUR, 72 * HOUR).is_abandoned());
        activity.created_at = 10 * HOUR;
        assert!(decide(&activity, 100 * HOUR, 72 * HOUR).is_abandoned());
    }

    #[test]
    fn a_closed_session_is_left_alone() {
        let mut activity = quiet(1, 0);
        activity.closed = true;
        assert_eq!(
            decide(&activity, 10_000 * HOUR, 72 * HOUR),
            AbandonmentVerdict::AlreadyClosed
        );
    }

    #[test]
    fn the_survey_judges_each_session_separately_and_keeps_order() {
        let mut live = quiet(2, 0);
        live.agent_alive = Some(true);
        let mut closed = quiet(3, 0);
        closed.closed = true;
        let activities = vec![quiet(1, 0), live, closed, quiet(4, 99 * HOUR)];
        let decisions = survey(&activities, 100 * HOUR, 72 * HOUR);
        let labels: Vec<_> = decisions
            .iter()
            .map(|d| (d.session_id, d.verdict.as_str()))
            .collect();
        assert_eq!(
            labels,
            vec![
                (1, "abandoned"),
                (2, "agent_alive"),
                (3, "already_closed"),
                (4, "waiting"),
            ]
        );
        assert_eq!(abandoned(&activities, 100 * HOUR, 72 * HOUR), vec![1]);
    }

    #[test]
    fn no_two_verdicts_read_alike() {
        let words = [
            AbandonmentVerdict::AgentAlive.as_str(),
            AbandonmentVerdict::AlreadyClosed.as_str(),
            AbandonmentVerdict::Waiting { remaining_ms: 1 }.as_str(),
            AbandonmentVerdict::Abandoned { idle_ms: 1 }.as_str(),
        ];
        let mut unique = words.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), words.len(), "each verdict needs its own word");
    }
}
