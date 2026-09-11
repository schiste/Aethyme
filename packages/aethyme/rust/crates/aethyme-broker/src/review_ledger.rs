//! What the review router has already asked for.
//!
//! The decision plane in [`crate::review_trigger`] and [`crate::review_backend`]
//! is deliberately pure: it takes what has been spent and what is in flight as
//! arguments rather than reading them. This module is where those arguments
//! come from -- the durable record of every review the router requested, which
//! dimension it was, which commit it was bound to, and who was asked to do it.
//!
//! Two properties matter more than the shape of the rows.
//!
//! A review is identified by (repository, pull request, dimension, head), and
//! that tuple is unique in the database. Scheduling already declines to
//! re-request a review bound to the head it was requested for, but a decision
//! is not a guarantee: an executor that writes the row, then dies before
//! spawning the reviewer, re-runs from the start. Making the identity a unique
//! index turns "we decided not to" into "we cannot", so the crash costs a
//! missing review rather than two reviewers racing on one pull request. A
//! missing review is recoverable -- CI still runs, and the next push makes a
//! new head and a new request.
//!
//! In-flight is derived from the rows rather than counted into a column. A
//! counter has to be decremented by whoever finishes, and nothing guarantees
//! anyone does; a query over states cannot leak a slot.

use serde::{Deserialize, Serialize};

use crate::review_backend::InFlightReview;
use crate::review_trigger::ReviewSpend;

/// Where one requested review has got to.
///
/// The three terminal states are separate because they mean different things to
/// the next tick, and a reader a year from now has only the row to go on.
/// `Failed` was attempted and produced no verdict -- asking again without a new
/// head buys nothing. `Recorded` is the `record` backend's complete outcome:
/// the policy asked for nothing to be performed. `Abandoned` alone means nobody
/// was ever asked, which is the one case the router may ask about again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewRequestState {
    /// Recorded, not yet picked up.
    Requested,
    /// A reviewer is working on it.
    Running,
    /// A verdict landed.
    Satisfied,
    /// Attempted, no verdict.
    Failed,
    /// The `record` backend's whole outcome: the policy performs nothing for
    /// this dimension, so the row itself is the answer.
    Recorded,
    /// Nobody was ever asked -- the tab was gone, the mention could not be
    /// posted, the executor died before it got there.
    Abandoned,
}

impl ReviewRequestState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Running => "running",
            Self::Satisfied => "satisfied",
            Self::Failed => "failed",
            Self::Recorded => "recorded",
            Self::Abandoned => "abandoned",
        }
    }

    pub fn parse(label: &str) -> Option<Self> {
        match label {
            "requested" => Some(Self::Requested),
            "running" => Some(Self::Running),
            "satisfied" => Some(Self::Satisfied),
            "failed" => Some(Self::Failed),
            "recorded" => Some(Self::Recorded),
            "abandoned" => Some(Self::Abandoned),
            _ => None,
        }
    }

    /// Whether this state occupies one of the router's concurrency slots.
    ///
    /// Only the states where someone may still be working. A satisfied or
    /// abandoned review holds nothing, and treating it as if it did would
    /// starve a repository one review at a time until nothing dispatched.
    pub fn occupies_a_slot(self) -> bool {
        matches!(self, Self::Requested | Self::Running)
    }

    /// Whether the router may ask for this review again.
    ///
    /// Exactly one state qualifies. The unique index means a row is otherwise
    /// permanent for its head, so this is the only thing standing between a
    /// `gh` call that failed and a dimension that is never reviewed again --
    /// [`crate::spend_by_type`] therefore leaves a revivable row out of the
    /// spend it reports, and [`crate::BrokerStore::record_review_request`]
    /// reuses the row rather than colliding with it.
    pub fn is_revivable(self) -> bool {
        matches!(self, Self::Abandoned)
    }
}

/// One row of the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewRequest {
    pub id: i64,
    pub repository: String,
    pub pr_number: i64,
    pub review_type: String,
    pub head_commit: String,
    /// Who was asked, as `ReviewBackend`'s label. Kept as text because the
    /// answer to "why was there no review" has to survive a policy change that
    /// removes the backend the row names.
    pub backend: String,
    pub state: ReviewRequestState,
    pub detail: Option<String>,
    pub requested_at: i64,
    pub updated_at: i64,
}

/// What has been spent per dimension, for [`crate::schedule`].
///
/// `last_requested_commit` is the most recently requested head, by request
/// time. Ordering by time rather than by row id keeps this correct if rows are
/// ever backfilled out of order.
///
/// Revivable rows are not spend. `schedule` skips a dimension whose last
/// request is bound to the current head, so counting a review nobody was ever
/// asked to do would turn one failed `gh` call into a dimension that is never
/// reviewed again for that head.
pub fn spend_by_type(rows: &[ReviewRequest]) -> std::collections::BTreeMap<String, ReviewSpend> {
    let mut spend: std::collections::BTreeMap<String, ReviewSpend> =
        std::collections::BTreeMap::new();
    for row in rows {
        if row.state.is_revivable() {
            continue;
        }
        let entry = spend.entry(row.review_type.clone()).or_default();
        entry.requested_count += 1;
        if entry
            .last_requested_ms
            .is_none_or(|last| row.requested_at >= last)
        {
            entry.last_requested_ms = Some(row.requested_at);
            entry.last_requested_commit = Some(row.head_commit.clone());
        }
    }
    spend
}

/// The reviews still occupying a slot, for [`crate::dispatch_review`].
pub fn in_flight(rows: &[ReviewRequest]) -> Vec<InFlightReview> {
    rows.iter()
        .filter(|row| row.state.occupies_a_slot())
        .map(|row| InFlightReview {
            review_type: row.review_type.clone(),
            pull_request: row.pr_number,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(review_type: &str, head: &str, at: i64, state: ReviewRequestState) -> ReviewRequest {
        ReviewRequest {
            id: 0,
            repository: "owner/repo".into(),
            pr_number: 7,
            review_type: review_type.into(),
            head_commit: head.into(),
            backend: "chau7".into(),
            state,
            detail: None,
            requested_at: at,
            updated_at: at,
        }
    }

    #[test]
    fn spend_counts_every_request_and_reports_the_latest_head() {
        let rows = vec![
            row("security", "aaa", 100, ReviewRequestState::Satisfied),
            row("security", "bbb", 200, ReviewRequestState::Requested),
            row("code", "bbb", 150, ReviewRequestState::Satisfied),
        ];
        let spend = spend_by_type(&rows);
        assert_eq!(spend["security"].requested_count, 2);
        assert_eq!(
            spend["security"].last_requested_commit.as_deref(),
            Some("bbb")
        );
        assert_eq!(spend["security"].last_requested_ms, Some(200));
        assert_eq!(spend["code"].requested_count, 1);
    }

    /// Rows arriving newest-first must not make an old head look current: the
    /// scheduler reads `last_requested_commit` to decide whether this head has
    /// already had its review, and a stale answer there silently doubles the
    /// review budget or silently spends none.
    #[test]
    fn the_latest_head_does_not_depend_on_row_order() {
        let ascending = vec![
            row("security", "aaa", 100, ReviewRequestState::Satisfied),
            row("security", "bbb", 200, ReviewRequestState::Requested),
        ];
        let descending: Vec<_> = ascending.iter().cloned().rev().collect();
        assert_eq!(spend_by_type(&ascending), spend_by_type(&descending));
        assert_eq!(
            spend_by_type(&descending)["security"]
                .last_requested_commit
                .as_deref(),
            Some("bbb")
        );
    }

    /// A finished review must release its slot, or a repository that reviews
    /// steadily stops dispatching once its budget of historical rows is
    /// reached -- a failure that looks like the router having quietly died.
    #[test]
    fn only_unfinished_reviews_occupy_a_slot() {
        let rows = vec![
            row("security", "aaa", 100, ReviewRequestState::Satisfied),
            row("code", "bbb", 200, ReviewRequestState::Running),
            row("perf", "ccc", 300, ReviewRequestState::Requested),
            row("docs", "ddd", 400, ReviewRequestState::Abandoned),
            row("api", "eee", 500, ReviewRequestState::Failed),
        ];
        let flight = in_flight(&rows);
        let types: Vec<&str> = flight.iter().map(|r| r.review_type.as_str()).collect();
        assert_eq!(types, vec!["code", "perf"]);
    }

    /// The spend a revivable row reports is the difference between one failed
    /// `gh` call costing a tick and it costing the dimension permanently:
    /// `schedule` skips whatever is already bound to the current head.
    #[test]
    fn a_review_nobody_was_asked_for_is_not_spend() {
        let rows = vec![
            row("security", "aaa", 100, ReviewRequestState::Abandoned),
            row("code", "aaa", 100, ReviewRequestState::Recorded),
        ];
        let spend = spend_by_type(&rows);
        assert!(
            !spend.contains_key("security"),
            "an abandoned request must leave the head askable"
        );
        let code = spend.get("code").expect("a recorded review is spent");
        assert_eq!(code.last_requested_commit.as_deref(), Some("aaa"));
    }

    #[test]
    fn exactly_one_state_is_revivable() {
        let revivable: Vec<&str> = [
            ReviewRequestState::Requested,
            ReviewRequestState::Running,
            ReviewRequestState::Satisfied,
            ReviewRequestState::Failed,
            ReviewRequestState::Recorded,
            ReviewRequestState::Abandoned,
        ]
        .into_iter()
        .filter(|state| state.is_revivable())
        .map(|state| state.label())
        .collect();
        assert_eq!(revivable, vec!["abandoned"]);
    }

    #[test]
    fn every_state_round_trips_through_its_label() {
        for state in [
            ReviewRequestState::Requested,
            ReviewRequestState::Running,
            ReviewRequestState::Satisfied,
            ReviewRequestState::Failed,
            ReviewRequestState::Recorded,
            ReviewRequestState::Abandoned,
        ] {
            assert_eq!(ReviewRequestState::parse(state.label()), Some(state));
        }
        assert_eq!(ReviewRequestState::parse("in_progress"), None);
    }
}
