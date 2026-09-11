//! Turning review decisions into work, in an order that survives a crash.
//!
//! Everything upstream of this module is a pure judgement: eligibility, then
//! scheduling, then routing, then projection. Nothing performs. This module is
//! where the judgements become a sequence of effects -- ledger rows, GitHub
//! writes, and prompts handed to an adapter -- and its whole substance is the
//! order they happen in and what a partial failure leaves behind.
//!
//! The ordering rule is **record before perform**. A review is written to the
//! ledger before anyone is asked to do it, so the identity index in
//! `review_requests` is already claimed when the spawn happens. A process that
//! dies after recording leaves a review nobody performed; one that died after
//! performing but before recording would leave a review nobody knows about, and
//! the next tick would ask for it again. The first failure is a missed review,
//! which CI still covers. The second is two reviewers on one pull request,
//! which costs an agent and confuses the comment thread.
//!
//! The plan is a value rather than a loop for the usual reason: the ordering is
//! the part worth testing, and a loop that calls `gh` cannot be tested without
//! `gh`.

use serde::Serialize;

use crate::pr_projection::PrProjectionAction;
use crate::review_backend::ReviewDispatchAction;
use crate::review_ledger::ReviewRequestState;

/// One review's entry in the ledger, before anything is performed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LedgerWrite {
    pub review_type: String,
    /// Who was asked, spelled the way `.aethyme/config.toml` spells it.
    ///
    /// The ledger exists to answer "why is there no review on this pull
    /// request" months later, and the only vocabulary the operator reading it
    /// shares with the router is the one they wrote in their own config.
    pub backend: &'static str,
    /// Where the request lands once recorded.
    ///
    /// A routed review is left `Requested` for its performer to advance. A
    /// record-only one is closed immediately: recording it is the entire
    /// intent, and leaving it open would hold a concurrency slot forever
    /// against a reviewer who is never coming.
    pub state: ReviewRequestState,
    pub detail: Option<String>,
}

/// One coordinated GitHub call, as arguments for the `gh` lane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GhCall {
    /// Which review this call asks for, and `None` for the projection write.
    ///
    /// A failed call has to be attributable: the mention that did not post is
    /// the review nobody was asked for, and that row must be reopened. The
    /// projection write carries `None` because it describes every review at
    /// once, and failing it means the pull request is stale rather than that
    /// any particular reviewer was missed.
    pub review_type: Option<String>,
    /// What this call is for, for the operation's authorization reason and for
    /// a human reading the audit log later.
    pub purpose: String,
    pub args: Vec<String>,
}

/// One review to be started by an adapter with Chau7 access.
///
/// The broker does not talk to Chau7, here or anywhere else: it decides which
/// workspace and which prompt, and a caller that has the transport performs it.
/// That seam is why the routing decision is testable without Chau7 running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Chau7Handoff {
    pub review_type: String,
    pub pull_request: i64,
    pub workspace: String,
    pub prompt: String,
}

/// Everything one tick should do, in the order it should do it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ReviewExecutionPlan {
    /// Written first, so the identity is claimed before anyone is asked.
    pub ledger: Vec<LedgerWrite>,
    /// Bot mentions and the pull request's projection, in that order.
    pub gh: Vec<GhCall>,
    /// Handed to the adapter last.
    pub chau7: Vec<Chau7Handoff>,
    /// Eligible, not started, not recorded. The next tick reconsiders, which
    /// is exactly why these are absent from `ledger`: a deferred review that
    /// left a row behind would look spent forever.
    pub deferred: Vec<DeferredReview>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeferredReview {
    pub review_type: String,
    pub why: String,
}

/// Build the plan for one tick.
///
/// `projection` is appended after the dispatch calls so the pull request's
/// comment is written once, after the requests it describes have been recorded.
/// Writing it first would announce reviews that a crash then prevented, and the
/// comment is the only thing a human reads.
pub fn plan_execution(
    dispatch: &[ReviewDispatchAction],
    projection: &[PrProjectionAction],
    pull_request: i64,
) -> ReviewExecutionPlan {
    let mut plan = ReviewExecutionPlan::default();
    for action in dispatch {
        match action {
            ReviewDispatchAction::SpawnChau7Review {
                review_type,
                pull_request,
                workspace,
                prompt,
            } => {
                plan.ledger.push(LedgerWrite {
                    review_type: review_type.clone(),
                    backend: "chau7",
                    state: ReviewRequestState::Requested,
                    detail: Some(format!("workspace {workspace}")),
                });
                plan.chau7.push(Chau7Handoff {
                    review_type: review_type.clone(),
                    pull_request: *pull_request,
                    workspace: workspace.clone(),
                    prompt: prompt.clone(),
                });
            }
            ReviewDispatchAction::MentionOnPullRequest { review_type, .. } => {
                plan.ledger.push(LedgerWrite {
                    review_type: review_type.clone(),
                    backend: "provider_comment",
                    state: ReviewRequestState::Requested,
                    detail: None,
                });
                if let Some(args) = action.gh_args() {
                    plan.gh.push(GhCall {
                        review_type: Some(review_type.clone()),
                        purpose: format!("request the {review_type} review from the provider bot"),
                        args,
                    });
                }
            }
            ReviewDispatchAction::RecordOnly { review_type, why } => {
                // Settled on arrival, and deliberately not `Abandoned`: the
                // policy performs nothing here, so there is nothing for a later
                // tick to retry. Spelling it `Recorded` is what keeps the one
                // revivable state meaning only "nobody was ever asked".
                plan.ledger.push(LedgerWrite {
                    review_type: review_type.clone(),
                    backend: "record",
                    state: ReviewRequestState::Recorded,
                    detail: Some(why.clone()),
                });
            }
            ReviewDispatchAction::Defer { review_type, why } => {
                plan.deferred.push(DeferredReview {
                    review_type: review_type.clone(),
                    why: why.clone(),
                });
            }
        }
    }
    for action in projection {
        plan.gh.push(GhCall {
            review_type: None,
            purpose: "project the review record onto the pull request".into(),
            args: action.gh_args(pull_request),
        });
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spawn(review_type: &str) -> ReviewDispatchAction {
        ReviewDispatchAction::SpawnChau7Review {
            review_type: review_type.into(),
            pull_request: 12,
            workspace: "/w/review".into(),
            prompt: "review it".into(),
        }
    }

    /// The ledger row has to exist before the reviewer does. If a spawn were
    /// planned first, a crash between the two would leave a running reviewer
    /// the next tick knows nothing about, and it would start a second one.
    #[test]
    fn a_chau7_review_is_recorded_before_it_is_handed_out() {
        let plan = plan_execution(&[spawn("security")], &[], 12);
        assert_eq!(plan.ledger.len(), 1);
        assert_eq!(plan.ledger[0].state, ReviewRequestState::Requested);
        assert_eq!(plan.chau7.len(), 1);
        assert_eq!(plan.chau7[0].workspace, "/w/review");
    }

    /// A record-only review holds no slot: nobody is coming, and an open row
    /// would throttle the next real review and every one after it.
    #[test]
    fn a_record_only_review_is_closed_the_moment_it_is_written() {
        let plan = plan_execution(
            &[ReviewDispatchAction::RecordOnly {
                review_type: "docs".into(),
                why: "no backend routes docs".into(),
            }],
            &[],
            12,
        );
        assert_eq!(plan.ledger[0].state, ReviewRequestState::Recorded);
        assert!(!plan.ledger[0].state.occupies_a_slot());
        assert!(
            !plan.ledger[0].state.is_revivable(),
            "a record-only review is settled; retrying it would rewrite the row every tick"
        );
        assert!(plan.chau7.is_empty());
        assert!(plan.gh.is_empty());
    }

    /// Deferral means "ask again later", and a ledger row is how this system
    /// remembers "already asked". Writing one here would turn every deferral
    /// into a permanent skip.
    #[test]
    fn a_deferred_review_leaves_no_trace_in_the_ledger() {
        let plan = plan_execution(
            &[ReviewDispatchAction::Defer {
                review_type: "security".into(),
                why: "all slots busy".into(),
            }],
            &[],
            12,
        );
        assert!(plan.ledger.is_empty());
        assert_eq!(plan.deferred.len(), 1);
        assert_eq!(plan.deferred[0].why, "all slots busy");
    }

    /// The comment describes requests that must already be recorded when a
    /// reader sees it.
    #[test]
    fn the_projection_is_written_after_the_requests_it_describes() {
        let plan = plan_execution(
            &[ReviewDispatchAction::MentionOnPullRequest {
                review_type: "code".into(),
                pull_request: 12,
                body: "@bot please review".into(),
            }],
            &[PrProjectionAction::CreateComment {
                body: "status".into(),
            }],
            12,
        );
        assert_eq!(plan.gh.len(), 2);
        assert!(plan.gh[0].purpose.contains("provider bot"));
        assert!(plan.gh[1].purpose.contains("project"));

        // A mention that fails is a review nobody was asked for, and the
        // executor reopens that row -- which it can only do if the call says
        // which review it was. The projection write names none, because
        // failing it says nothing about any one reviewer.
        assert_eq!(plan.gh[0].review_type.as_deref(), Some("code"));
        assert_eq!(plan.gh[1].review_type, None);
    }

    #[test]
    fn an_empty_decision_set_plans_nothing() {
        assert_eq!(plan_execution(&[], &[], 12), ReviewExecutionPlan::default());
    }
}
