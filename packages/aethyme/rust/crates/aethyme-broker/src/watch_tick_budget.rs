//! A single foreground tick's provider work budget. No timers or background work.

use std::cell::Cell;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::{
    GithubCliPullRequestWatchProvider, GithubCliRepositoryWatchProvider, PullRequestSnapshot,
    PullRequestWatchError, PullRequestWatchProvider, PullRequestWatchRequest,
    RepositoryPullRequest, RepositoryWatchProvider,
};

const TICK_DEADLINE: Duration = Duration::from_secs(30);
const CALL_DEADLINE: Duration = Duration::from_secs(3);
const CALLS_PER_PHASE: usize = 4;

#[derive(Clone, Copy)]
pub(crate) enum Phase {
    PullRequests,
    Repositories,
}

pub(crate) struct WatchTickBudget {
    started: Instant,
    pr_calls: Cell<usize>,
    repository_calls: Cell<usize>,
    refusals: Cell<usize>,
}

#[derive(Serialize)]
pub(crate) struct Report {
    pub schema_version: u32,
    pub calls: usize,
    pub max_calls: usize,
    pub max_calls_per_phase: usize,
    pub deadline_ms: u64,
    pub deferred_calls: usize,
}

impl WatchTickBudget {
    pub(crate) fn new() -> Self {
        Self {
            started: Instant::now(),
            pr_calls: Cell::new(0),
            repository_calls: Cell::new(0),
            refusals: Cell::new(0),
        }
    }

    pub(crate) fn provider(&self, phase: Phase) -> BudgetedProvider<'_> {
        BudgetedProvider {
            budget: self,
            phase,
        }
    }

    fn claim(&self, phase: Phase) -> Result<Duration, PullRequestWatchError> {
        self.claim_at(phase, self.started.elapsed())
    }

    fn claim_at(&self, phase: Phase, elapsed: Duration) -> Result<Duration, PullRequestWatchError> {
        let calls = match phase {
            Phase::PullRequests => &self.pr_calls,
            Phase::Repositories => &self.repository_calls,
        };
        let remaining = TICK_DEADLINE.saturating_sub(elapsed);
        if calls.get() >= CALLS_PER_PHASE || remaining.is_zero() {
            self.refusals.set(self.refusals.get().saturating_add(1));
            return Err(PullRequestWatchError::TickBudgetExhausted);
        }
        calls.set(calls.get() + 1);
        Ok(CALL_DEADLINE.min(remaining))
    }

    pub(crate) fn report(&self) -> Report {
        Report {
            schema_version: 1,
            calls: self.pr_calls.get() + self.repository_calls.get(),
            max_calls: CALLS_PER_PHASE * 2,
            max_calls_per_phase: CALLS_PER_PHASE,
            deadline_ms: TICK_DEADLINE.as_millis() as u64,
            deferred_calls: self.refusals.get(),
        }
    }
}

pub(crate) struct BudgetedProvider<'a> {
    budget: &'a WatchTickBudget,
    phase: Phase,
}

impl PullRequestWatchProvider for BudgetedProvider<'_> {
    fn inspect(
        &self,
        request: &PullRequestWatchRequest,
    ) -> Result<PullRequestSnapshot, PullRequestWatchError> {
        let deadline = self.budget.claim(self.phase)?;
        GithubCliPullRequestWatchProvider::inspect_with_deadline(request, deadline)
    }
}

impl RepositoryWatchProvider for BudgetedProvider<'_> {
    fn list_open(
        &self,
        repository: &str,
        limit: usize,
    ) -> Result<Vec<RepositoryPullRequest>, PullRequestWatchError> {
        let deadline = self.budget.claim(self.phase)?;
        GithubCliRepositoryWatchProvider::list_with_deadline(repository, limit, deadline)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_phase_has_reserved_capacity_and_refusal_never_charges_a_call() {
        let budget = WatchTickBudget::new();
        for _ in 0..CALLS_PER_PHASE {
            assert_eq!(
                budget
                    .claim_at(Phase::PullRequests, Duration::ZERO)
                    .unwrap(),
                CALL_DEADLINE
            );
        }
        assert!(matches!(
            budget.claim_at(Phase::PullRequests, Duration::ZERO),
            Err(PullRequestWatchError::TickBudgetExhausted)
        ));
        // A busy PR inventory cannot consume the repository phase's allowance.
        for _ in 0..CALLS_PER_PHASE {
            assert!(budget.claim_at(Phase::Repositories, Duration::ZERO).is_ok());
        }
        assert!(
            budget
                .claim_at(Phase::Repositories, Duration::ZERO)
                .is_err()
        );
        let report = budget.report();
        assert_eq!(report.calls, 8);
        assert_eq!(report.deferred_calls, 2);
    }

    #[test]
    fn shared_deadline_clamps_the_final_call_and_blocks_both_phases() {
        let budget = WatchTickBudget::new();
        assert_eq!(
            budget
                .claim_at(Phase::PullRequests, Duration::from_millis(29_950))
                .unwrap(),
            Duration::from_millis(50)
        );
        for phase in [Phase::PullRequests, Phase::Repositories] {
            assert!(matches!(
                budget.claim_at(phase, TICK_DEADLINE),
                Err(PullRequestWatchError::TickBudgetExhausted)
            ));
        }
        assert_eq!(budget.report().calls, 1);
    }
}
