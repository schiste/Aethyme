//! What actually happened to a pull request, derived rather than assumed.
//!
//! The routing policy lets a rule fire on a lifecycle transition — a head that
//! was rewritten, a pull request that left draft, a base that moved. Deciding
//! which transition occurred needs two things the decision plane deliberately
//! does not have: the provider's current answer, and a memory of the last one.
//!
//! Both are gathered by the caller and passed in, so every derivation here is a
//! pure function over two snapshots. That is the same seam the rest of the
//! review plane uses: the broker decides, the caller performs the transport.
//!
//! **A transition nobody recorded is not a transition.** The first time Aethyme
//! looks at a pull request it has no previous observation, so the answer is
//! [`ReviewTrigger::PullRequestOpened`] whatever the pull request's real age.
//! Claiming `AdditionalCommit` for a pull request opened before Aethyme was
//! watching would assert a comparison that was never made.

use serde::{Deserialize, Serialize};

use crate::ReviewTrigger;

/// What the provider says about a pull request right now.
///
/// Field for field what one `gh pr view --json` call returns, normalised.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderPullRequest {
    pub head_commit: String,
    pub base_ref: String,
    pub is_draft: bool,
    /// `open`, `closed`, or `merged`, lowercased.
    pub state: String,
    /// The head branch lives in a different repository than the base.
    pub from_fork: bool,
    /// How many reviews the provider reports as dismissed.
    ///
    /// A count rather than a flag: dismissal is not a state a pull request is
    /// in, it is an event, and the only way to see an event in a snapshot is to
    /// notice that there are more of them than there were.
    pub dismissed_reviews: i64,
    /// The author's relationship to the repository, lowercased, when the
    /// provider said. `None` means unknown, which is never treated as "new".
    pub author_association: Option<String>,
}

/// What Aethyme saw the last time it looked at this pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestObservation {
    pub repository: String,
    pub pr_number: i64,
    pub head_commit: String,
    pub base_ref: String,
    pub is_draft: bool,
    pub state: String,
    pub dismissed_reviews: i64,
    pub observed_at: i64,
}

/// Which transition happened between two observations.
///
/// The order of the checks is the order of specificity, and it matters. A pull
/// request that just left draft almost always also has a head the last
/// observation never saw, and both facts are true; reporting `AdditionalCommit`
/// for it would answer with the smaller one. Each case below subsumes the cases
/// after it, so the first match is the most informative true answer.
///
/// `head_descends_from_previous` separates a commit added on top of the old head
/// from a head that replaced it. The caller answers it with
/// `git merge-base --is-ancestor`, because only a repository can: the provider
/// reports the new head and says nothing about how it came to be.
pub fn derive_trigger(
    previous: Option<&PullRequestObservation>,
    current: &ProviderPullRequest,
    head_descends_from_previous: bool,
) -> ReviewTrigger {
    let Some(previous) = previous else {
        return ReviewTrigger::PullRequestOpened;
    };
    if previous.state != "open" && current.state == "open" {
        return ReviewTrigger::Reopened;
    }
    if previous.is_draft && !current.is_draft {
        return ReviewTrigger::ReadyForReview;
    }
    if current.dismissed_reviews > previous.dismissed_reviews {
        return ReviewTrigger::ReviewDismissed;
    }
    if previous.base_ref != current.base_ref {
        return ReviewTrigger::BaseRetargeted;
    }
    if previous.head_commit != current.head_commit {
        return if head_descends_from_previous {
            ReviewTrigger::AdditionalCommit
        } else {
            ReviewTrigger::ReplacementCommit
        };
    }
    // Nothing moved. A sweep still counts as an occasion to reconsider, which
    // is what `Scheduled` exists for -- rules that catch what events missed.
    ReviewTrigger::Scheduled
}

/// Whether the author has not previously landed a change here.
///
/// GitHub's `authorAssociation` is the provider's own answer, so this does not
/// go counting merged pull requests. Unknown is not new: a missing association
/// means the call did not report one, and inventing "first-time contributor"
/// from silence would apply the strictest rules to everyone the moment the
/// provider changed a field name.
pub fn first_time_contributor(association: Option<&str>) -> bool {
    matches!(
        association.map(str::trim),
        Some("none") | Some("first_time_contributor") | Some("first_timer")
    )
}

impl ReviewTrigger {
    /// The configuration spelling, as an `on = [...]` entry writes it.
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewTrigger::PullRequestOpened => "pull_request_opened",
            ReviewTrigger::ReadyForReview => "ready_for_review",
            ReviewTrigger::Reopened => "reopened",
            ReviewTrigger::ReplacementCommit => "replacement_commit",
            ReviewTrigger::AdditionalCommit => "additional_commit",
            ReviewTrigger::BaseRetargeted => "base_retargeted",
            ReviewTrigger::ReviewDismissed => "review_dismissed",
            ReviewTrigger::MergeQueueEntered => "merge_queue_entered",
            ReviewTrigger::Scheduled => "scheduled",
            ReviewTrigger::Manual => "manual",
        }
    }

    /// Whether a tick can ever report this trigger.
    ///
    /// Everything [`derive_trigger`] can conclude from two snapshots, plus
    /// `Manual`, which `review request` supplies directly. `MergeQueueEntered`
    /// is the one transition no `gh pr view` field exposes; a rule that waits
    /// for it would wait forever, so the policy loader refuses it by name
    /// rather than letting it sit in the file looking configured.
    pub fn is_observable(self) -> bool {
        !matches!(self, ReviewTrigger::MergeQueueEntered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every variant, so a new one cannot be added without deciding whether a
    /// tick can report it.
    const ALL_TRIGGERS: [ReviewTrigger; 10] = [
        ReviewTrigger::PullRequestOpened,
        ReviewTrigger::ReadyForReview,
        ReviewTrigger::Reopened,
        ReviewTrigger::ReplacementCommit,
        ReviewTrigger::AdditionalCommit,
        ReviewTrigger::BaseRetargeted,
        ReviewTrigger::ReviewDismissed,
        ReviewTrigger::MergeQueueEntered,
        ReviewTrigger::Scheduled,
        ReviewTrigger::Manual,
    ];

    fn seen(head: &str) -> PullRequestObservation {
        PullRequestObservation {
            repository: "owner/name".into(),
            pr_number: 7,
            head_commit: head.into(),
            base_ref: "main".into(),
            is_draft: false,
            state: "open".into(),
            dismissed_reviews: 0,
            observed_at: 0,
        }
    }

    fn now(head: &str) -> ProviderPullRequest {
        ProviderPullRequest {
            head_commit: head.into(),
            base_ref: "main".into(),
            is_draft: false,
            state: "open".into(),
            from_fork: false,
            dismissed_reviews: 0,
            author_association: None,
        }
    }

    #[test]
    fn a_pull_request_nobody_has_observed_is_treated_as_newly_opened() {
        assert_eq!(
            derive_trigger(None, &now("aaa"), false),
            ReviewTrigger::PullRequestOpened
        );
    }

    #[test]
    fn a_commit_on_top_of_the_observed_head_is_not_a_rewrite() {
        assert_eq!(
            derive_trigger(Some(&seen("aaa")), &now("bbb"), true),
            ReviewTrigger::AdditionalCommit
        );
        assert_eq!(
            derive_trigger(Some(&seen("aaa")), &now("bbb"), false),
            ReviewTrigger::ReplacementCommit
        );
    }

    #[test]
    fn leaving_draft_outranks_the_commits_that_came_with_it() {
        let mut before = seen("aaa");
        before.is_draft = true;
        // Both true: the head moved and the pull request became reviewable.
        // The second is the one a rule wants to hear about.
        assert_eq!(
            derive_trigger(Some(&before), &now("bbb"), true),
            ReviewTrigger::ReadyForReview
        );
    }

    #[test]
    fn reopening_outranks_everything_else() {
        let mut before = seen("aaa");
        before.state = "closed".into();
        before.is_draft = true;
        assert_eq!(
            derive_trigger(Some(&before), &now("bbb"), false),
            ReviewTrigger::Reopened
        );
    }

    #[test]
    fn a_moved_base_is_its_own_transition() {
        let mut current = now("aaa");
        current.base_ref = "release".into();
        assert_eq!(
            derive_trigger(Some(&seen("aaa")), &current, true),
            ReviewTrigger::BaseRetargeted
        );
    }

    #[test]
    fn a_dismissal_is_visible_only_as_an_increase() {
        let mut before = seen("aaa");
        before.dismissed_reviews = 1;
        let mut current = now("aaa");
        current.dismissed_reviews = 2;
        assert_eq!(
            derive_trigger(Some(&before), &current, true),
            ReviewTrigger::ReviewDismissed
        );
        // A dismissal that was already there on the last look is not news.
        current.dismissed_reviews = 1;
        assert_eq!(
            derive_trigger(Some(&before), &current, true),
            ReviewTrigger::Scheduled
        );
    }

    #[test]
    fn an_unchanged_pull_request_is_a_sweep_not_an_event() {
        assert_eq!(
            derive_trigger(Some(&seen("aaa")), &now("aaa"), true),
            ReviewTrigger::Scheduled
        );
    }

    #[test]
    fn silence_about_the_author_is_not_a_claim_that_they_are_new() {
        assert!(first_time_contributor(Some("none")));
        assert!(first_time_contributor(Some("first_time_contributor")));
        assert!(!first_time_contributor(Some("member")));
        assert!(!first_time_contributor(Some("owner")));
        assert!(!first_time_contributor(None));
    }

    /// `as_str` and the `rename_all = "snake_case"` that parses `on = [...]`
    /// are two spellings of one vocabulary. Nothing in the type system ties
    /// them together, so this does: if they ever disagree, a rule fires on a
    /// name the error message does not use.
    #[test]
    fn the_spelling_a_rule_writes_is_the_spelling_an_error_names() {
        for trigger in ALL_TRIGGERS {
            assert_eq!(
                serde_json::to_string(&trigger).unwrap(),
                format!("\"{}\"", trigger.as_str()),
            );
        }
    }

    #[test]
    fn every_trigger_a_rule_may_wait_for_is_one_a_tick_can_report() {
        // The whole point of the check: a spelling that parses but can never
        // fire is dead configuration, and dead configuration is silent.
        for trigger in ALL_TRIGGERS {
            assert_eq!(
                trigger.is_observable(),
                trigger != ReviewTrigger::MergeQueueEntered,
                "{}",
                trigger.as_str(),
            );
        }
    }
}
