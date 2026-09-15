//! What the review router asked for and what providers completed.
//!
//! The decision plane in [`crate::review_trigger`] and [`crate::review_backend`]
//! is deliberately pure: it takes what has been spent and what is in flight as
//! arguments rather than reading them. This module is where those arguments
//! come from -- the durable record of every review the router requested or
//! provider completed, which dimension it was, which commits the two facts
//! name, and who was asked to do it or actually answered.
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
/// The terminal states are separate because they mean different things to the
/// next tick, and a reader a year from now has only the row to go on.
/// `Failed` was attempted and concluded without a verdict -- asking again on
/// the same head buys nothing, because the attempt itself is the answer.
/// `Recorded` is the `record` backend's complete outcome: the policy asked for
/// nothing to be performed. `Abandoned` is the absence of any conclusion at
/// all -- nobody was ever asked, or whoever was asked never came back -- which
/// is the one case where asking again is worth something, and therefore the one
/// case the router may ask about again.
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
    /// No conclusion is coming -- nobody was ever asked (the tab was gone, the
    /// mention could not be posted, the executor died before it got there), or
    /// whoever was asked never reported back inside `stale_after_minutes`.
    Abandoned,
    /// A person decided this dimension does not have to happen for this head.
    ///
    /// Separate from `Satisfied` because it is a different claim about the
    /// world: `Satisfied` says a review happened, `Waived` says one was
    /// excused. Collapsing them is #172 -- the only way to unblock one stuck
    /// dimension was to assert a review that never ran, after which no reader
    /// could tell the two apart. The row carries a [`ReviewWaiver`] in
    /// `detail`, so the decision keeps its author and its reason.
    Waived,
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
            Self::Waived => "waived",
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
            "waived" => Some(Self::Waived),
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

/// The provider's typed conclusion about one completed review.
///
/// This is intentionally separate from [`ReviewRequestState`]. `Satisfied`
/// says that a reviewer returned an outcome; this says what that outcome was.
/// A missing value is therefore not an implicit pass, especially for rows
/// written by an older binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    /// The review found no issue that blocks the reviewed dimension.
    Pass,
    /// The review found a blocking issue.
    Fail,
    /// The provider explicitly asked the author to make changes.
    ChangesRequested,
    /// The provider returned observations without an approval or rejection.
    Commented,
}

impl ReviewVerdict {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::ChangesRequested => "changes_requested",
            Self::Commented => "commented",
        }
    }

    /// Parse the stable CLI/database spelling. Provider-native aliases are
    /// accepted at the boundary and normalized to the shared vocabulary.
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "pass" | "passed" | "approve" | "approved" => Self::Pass,
            "fail" | "failed" | "reject" | "rejected" => Self::Fail,
            "changes_requested" | "request_changes" => Self::ChangesRequested,
            "commented" | "comment" => Self::Commented,
            _ => return None,
        })
    }
}

/// The identity of the system that produced a completion.
///
/// `backend` on [`ReviewRequest`] remains the route Aethyme selected. This is
/// the provider/model that actually produced the result, so a completion that
/// arrived through a different path cannot be misattributed to the route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewerIdentity {
    pub provider: String,
    /// `None` means the provider did not expose a model identity. It is kept
    /// unknown rather than guessed from a backend label.
    #[serde(default)]
    pub model: Option<String>,
}

#[cfg(test)]
mod completion_fact_tests {
    use super::*;
    use crate::review_trigger::ReviewTrigger;

    #[test]
    fn verdicts_round_trip_to_stable_labels() {
        for verdict in [
            ReviewVerdict::Pass,
            ReviewVerdict::Fail,
            ReviewVerdict::ChangesRequested,
            ReviewVerdict::Commented,
        ] {
            assert_eq!(ReviewVerdict::parse(verdict.label()), Some(verdict));
        }
        assert_eq!(ReviewVerdict::parse("approved"), Some(ReviewVerdict::Pass));
        assert_eq!(
            ReviewVerdict::parse("changes_requested"),
            Some(ReviewVerdict::ChangesRequested)
        );
        assert_eq!(ReviewVerdict::parse("unknown"), None);
    }

    #[test]
    fn an_unsolicited_completion_uses_completion_facts_for_spend_without_a_request_time() {
        let row = ReviewRequest {
            id: 1,
            repository: "o/r".into(),
            pr_number: 7,
            review_type: "security".into(),
            head_commit: "provider-head".into(),
            requested_for_commit: None,
            base_commit: None,
            backend: "unsolicited".into(),
            trigger: Some(ReviewTrigger::Unsolicited),
            state: ReviewRequestState::Satisfied,
            detail: Some("no findings".into()),
            requested_at: None,
            completed_at: Some(200),
            completed_for_commit: Some("provider-head".into()),
            verdict: Some(ReviewVerdict::Pass),
            reviewer: Some(ReviewerIdentity {
                provider: "github".into(),
                model: None,
            }),
            updated_at: 200,
        };
        let spend = spend_by_type(&[row]);
        assert_eq!(spend["security"].requested_count, 1);
        assert_eq!(
            spend["security"].last_requested_commit.as_deref(),
            Some("provider-head")
        );
        assert_eq!(spend["security"].last_requested_ms, Some(200));
    }

    #[test]
    fn an_old_ledger_json_row_defaults_new_optional_facts() {
        let row: ReviewRequest = serde_json::from_str(
            r#"{
                "id": 1,
                "repository": "o/r",
                "pr_number": 7,
                "review_type": "security",
                "head_commit": "old-head",
                "base_commit": null,
                "backend": "chau7",
                "state": "requested",
                "detail": null,
                "requested_at": 100,
                "updated_at": 100
            }"#,
        )
        .unwrap();
        assert_eq!(row.requested_for_commit, None);
        assert_eq!(row.completed_for_commit, None);
        assert_eq!(row.verdict, None);
        assert_eq!(row.reviewer, None);
    }
}

/// Why a provider would not do the review it was asked for.
///
/// Scraped, not typed: no provider returns a machine-readable reason, so this
/// is pattern matching over prose that a provider may reword tomorrow. Two
/// consequences shape the type. The raw text is kept *beside* the
/// classification rather than replaced by it, so a misfire costs precision and
/// never the evidence. And [`Self::Unknown`] is an ordinary outcome, not a bug:
/// "a provider refused, here is what it said" already answers the question this
/// exists to answer -- why a gate will not clear -- even when nothing matched.
///
/// #173: the refusal was discarded entirely, so quota exhaustion looked exactly
/// like "not requested yet" and like "still running". Ten pull requests in
/// `Aeptus/mockup` sat unmergeable for about 48 hours on 2026-09-11, and the
/// cause was only ever found by someone who already suspected it.
// `Ord` so a class can key a configuration map: `[..on_refusal]` is written
// per class, and a BTreeMap keeps the parsed table in a stable order for
// round-tripping and for error messages that name several classes at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalClass {
    /// The budget is spent. Waiting helps only when it refills.
    QuotaExhausted,
    /// Too many requests too quickly. Waiting helps.
    RateLimited,
    /// The provider failed rather than declined. Retrying may help.
    ProviderError,
    /// Refused, and nothing in the text said why.
    Unknown,
}

impl RefusalClass {
    pub fn label(self) -> &'static str {
        match self {
            Self::QuotaExhausted => "quota_exhausted",
            Self::RateLimited => "rate_limited",
            Self::ProviderError => "provider_error",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(label: &str) -> Option<Self> {
        match label {
            "quota_exhausted" => Some(Self::QuotaExhausted),
            "rate_limited" => Some(Self::RateLimited),
            "provider_error" => Some(Self::ProviderError),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }

    /// Read a classification out of what the provider said.
    ///
    /// Ordered most specific first, because the vocabularies overlap: a
    /// provider that refuses on a spent budget often says when the budget
    /// refills, and "retry after" is rate-limit vocabulary. Reading a
    /// refill-bound wait as a retriable one is the error that sends an
    /// operator straight back to the same wall, so the budget wins the tie.
    pub fn classify(text: &str) -> Self {
        let text = text.to_ascii_lowercase();
        const QUOTA: &[&str] = &[
            "quota",
            "usage limit",
            "credit balance",
            "insufficient credit",
            "out of credit",
            "billing",
        ];
        const RATE: &[&str] = &[
            "rate limit",
            "too many requests",
            "429",
            "retry after",
            "slow down",
        ];
        const ERROR: &[&str] = &[
            "internal server error",
            "service unavailable",
            "bad gateway",
            "timed out",
            "timeout",
            "connection reset",
            "500",
            "502",
            "503",
        ];
        if QUOTA.iter().any(|needle| text.contains(needle)) {
            return Self::QuotaExhausted;
        }
        if RATE.iter().any(|needle| text.contains(needle)) {
            return Self::RateLimited;
        }
        if ERROR.iter().any(|needle| text.contains(needle)) {
            return Self::ProviderError;
        }
        Self::Unknown
    }
}

/// What a provider said when it would not do the review, and what that means.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewRefusal {
    pub class: RefusalClass,
    /// The provider's own words, bounded and flattened to one line.
    pub text: String,
}

/// How much of a refusal is kept.
///
/// A provider can answer with an HTML error page, and the ledger's `detail` is
/// rendered into a pull-request comment. Enough to recognise the refusal, not
/// enough to bury the row it explains.
const REFUSAL_TEXT_LIMIT: usize = 300;

impl ReviewRefusal {
    /// Build a refusal from what a failed provider call left behind.
    ///
    /// `stderr` first: a CLI that fails says why on stderr, and stdout in that
    /// case is usually a partial or empty payload. Falling back to stdout
    /// covers the providers that report refusals as ordinary output.
    pub fn from_provider_output(stdout: &str, stderr: &str, fallback: &str) -> Self {
        let source = [stderr, stdout, fallback]
            .into_iter()
            .map(str::trim)
            .find(|candidate| !candidate.is_empty())
            .unwrap_or("");
        Self {
            class: RefusalClass::classify(source),
            text: flatten(source),
        }
    }

    /// The ledger `detail` for this refusal: classification, then evidence.
    ///
    /// One string rather than two columns because every reader of a review row
    /// already renders `detail` -- the pull-request comment, the run report,
    /// the ledger dump. A column nothing displays would state the cause in a
    /// place the operator in #173 was never going to look. [`Self::parse`]
    /// reads it back, so this stays a typed outcome rather than prose.
    pub fn detail(&self) -> String {
        if self.text.is_empty() {
            return self.class.label().to_string();
        }
        format!("{}: {}", self.class.label(), self.text)
    }

    /// Recover a refusal from a `detail` written by [`Self::detail`].
    ///
    /// `None` for any other detail -- the column carries rule names and
    /// debounce windows too, and reporting one of those as an `unknown`
    /// refusal would invent a provider refusal that never happened.
    pub fn parse(detail: &str) -> Option<Self> {
        let (label, text) = match detail.split_once(": ") {
            Some((label, text)) => (label, text),
            None => (detail, ""),
        };
        Some(Self {
            class: RefusalClass::parse(label)?,
            text: text.to_string(),
        })
    }
}

/// One line, bounded, with the truncation visible.
fn flatten(text: &str) -> String {
    let single: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if single.chars().count() <= REFUSAL_TEXT_LIMIT {
        return single;
    }
    let kept: String = single.chars().take(REFUSAL_TEXT_LIMIT).collect();
    format!("{kept}…")
}

/// Who excused a review dimension, and why.
///
/// A waiver is the one ledger outcome no machine produces, so it is the one
/// that must carry a person. `ReviewRefusal` records what a provider said;
/// this records what somebody decided, and the two are kept in the same
/// `detail` column for the same reason -- every existing reader of a review row
/// already renders `detail`, so a decision stored anywhere else is a decision
/// the operator reading the pull request never sees.
///
/// `reason` is stored as text rather than as the SHA-256 digest the broker's
/// coordinated operations use for their `--reason`. Those digests exist to
/// prove an authorization was given without retaining it; a waiver's reason is
/// the opposite -- it is addressed to the next person who asks why this
/// dimension is green, and a digest would answer them with nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewWaiver {
    /// The operator's own identity, as `--agent` or `AETHYME_AGENT` gave it.
    pub who: String,
    /// Why this dimension does not have to happen for this head.
    pub reason: String,
}

/// How a waiver is spelled in `detail`, and what tells it apart from a refusal.
const WAIVER_PREFIX: &str = "waived by ";

/// What is recorded when the operator could not be identified.
///
/// Never an error: refusing to waive because `AETHYME_AGENT` is unset would
/// send the operator to `review state --state satisfied`, which is the
/// unattributable escape hatch this type exists to replace. An anonymous
/// waiver that says so is strictly better evidence than a forged review.
const WAIVER_UNKNOWN_WHO: &str = "an unidentified operator";

impl ReviewWaiver {
    /// Build a waiver, bounding both fields the way a refusal bounds its text.
    pub fn new(who: Option<&str>, reason: &str) -> Self {
        let who = who.map(str::trim).filter(|who| !who.is_empty());
        Self {
            who: who.map_or_else(|| WAIVER_UNKNOWN_WHO.to_string(), flatten),
            reason: flatten(reason),
        }
    }

    /// The ledger `detail` for this waiver.
    pub fn detail(&self) -> String {
        format!("{WAIVER_PREFIX}{}: {}", self.who, self.reason)
    }

    /// Recover a waiver from a `detail` written by [`Self::detail`].
    ///
    /// `None` for anything else. The column also carries refusals, rule names
    /// and debounce windows, and reading one of those as a waiver would invent
    /// a human decision that nobody made -- the precise failure this type is
    /// here to prevent, inverted.
    pub fn parse(detail: &str) -> Option<Self> {
        let rest = detail.strip_prefix(WAIVER_PREFIX)?;
        let (who, reason) = rest.split_once(": ")?;
        if who.trim().is_empty() || reason.trim().is_empty() {
            return None;
        }
        Some(Self {
            who: who.to_string(),
            reason: reason.to_string(),
        })
    }
}

/// The waiver excusing one dimension at exactly this head, if there is one.
///
/// Scoped on all three of repository-local rows, `review_type` and `head`
/// together. A waiver recorded against an earlier head is not returned and
/// cannot be: that is what makes this a per-dimension, per-head decision rather
/// than a standing exemption, and it is why nothing has to remember to withdraw
/// one when the branch moves.
pub fn waiver_for(rows: &[ReviewRequest], review_type: &str, head: &str) -> Option<ReviewWaiver> {
    rows.iter()
        .filter(|row| row.review_type == review_type && row.head_commit == head)
        .filter(|row| row.state == ReviewRequestState::Waived)
        .find_map(|row| ReviewWaiver::parse(row.detail.as_deref()?))
}

/// One row of the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewRequest {
    pub id: i64,
    pub repository: String,
    pub pr_number: i64,
    pub review_type: String,
    /// The stable row identity. For a requested row it is the requested head;
    /// for an unsolicited completion it is the completion head, which lets a
    /// later request for that exact commit reconcile with this row.
    pub head_commit: String,
    /// The commit Aethyme asked the reviewer to inspect. This is nullable for
    /// an unsolicited completion, where no request fact exists.
    #[serde(default)]
    pub requested_for_commit: Option<String>,
    /// The base commit this review was requested against.
    ///
    /// `None` for a row written before the ledger recorded one. Read as "the
    /// base cannot be proven unchanged" rather than "unchanged", so a
    /// `head_and_base` dimension re-reviews instead of trusting a comparison
    /// nobody made (#172).
    #[serde(default)]
    pub base_commit: Option<String>,
    /// Who was asked, as `ReviewBackend`'s label. Kept as text because the
    /// answer to "why was there no review" has to survive a policy change that
    /// removes the backend the row names.
    pub backend: String,
    /// The lifecycle event that caused the request, or `unsolicited` for a
    /// provider completion that arrived without an Aethyme request.
    #[serde(default)]
    pub trigger: Option<crate::ReviewTrigger>,
    pub state: ReviewRequestState,
    pub detail: Option<String>,
    /// The time Aethyme recorded the request. Unsolicited completions have no
    /// request and therefore keep this null.
    #[serde(default)]
    pub requested_at: Option<i64>,
    /// The time the provider reported a completion, when one exists.
    #[serde(default)]
    pub completed_at: Option<i64>,
    /// The commit the provider actually reviewed. It is independent from the
    /// request binding because a provider can complete work for another head.
    #[serde(default)]
    pub completed_for_commit: Option<String>,
    #[serde(default)]
    pub verdict: Option<ReviewVerdict>,
    #[serde(default)]
    pub reviewer: Option<ReviewerIdentity>,
    pub updated_at: i64,
}

/// What has been spent per dimension, for [`crate::schedule`].
///
/// `last_requested_commit` is the most recently requested or externally
/// completed head, by the timestamp available for that fact. Including a
/// completed unsolicited row prevents the scheduler from asking twice for a
/// review the provider already performed, while its row still makes clear
/// that no Aethyme request existed.
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
        let at = row
            .requested_at
            .or(row.completed_at)
            .unwrap_or(row.updated_at);
        if entry.last_requested_ms.is_none_or(|last| at >= last) {
            entry.last_requested_ms = Some(at);
            entry.last_requested_commit = row
                .requested_for_commit
                .clone()
                .or_else(|| row.completed_for_commit.clone())
                .or_else(|| Some(row.head_commit.clone()));
            entry.last_requested_base = row.base_commit.clone();
        }
    }
    spend
}

/// One review the router has stopped waiting for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExpiredReview {
    pub id: i64,
    pub review_type: String,
    pub pull_request: i64,
    /// What the row's `detail` becomes, phrased for whoever reads the ledger
    /// months later and was not here when it happened.
    pub why: String,
}

/// The open reviews the router should stop waiting for.
///
/// A slot is released by whoever reports the outcome, and nothing guarantees
/// anyone does. The in-flight count is derived from states rather than a
/// counter precisely so a crash cannot leak a slot -- but a row that stays
/// `running` forever leaks one just as effectively, and a repository that has
/// quietly stopped dispatching reviews looks exactly like a repository whose
/// policy asks for none.
///
/// The window is per route because `max_concurrent` is: a security review that
/// legitimately takes hours and a docs review that should take minutes cannot
/// share one number without the short one waiting on the long one's patience.
/// `0` disables expiry for that route, for an operator who would rather wedge
/// than re-ask.
///
/// Pure, and separate from the write, so the executor can show what it would
/// expire before it expires anything.
pub fn expired(
    policy: &crate::ReviewRoutingPolicy,
    rows: &[ReviewRequest],
    now: i64,
) -> Vec<ExpiredReview> {
    rows.iter()
        .filter(|row| row.state.occupies_a_slot())
        .filter_map(|row| {
            let minutes = policy.route_for(&row.review_type).stale_after_minutes;
            if minutes == 0 {
                return None;
            }
            let window = i64::from(minutes) * 60_000;
            // From the last time anything happened to this row, not from when
            // it was requested: a reviewer that reported `running` an hour ago
            // is working, and restarting it would duplicate that hour.
            let idle = now.saturating_sub(row.updated_at);
            if idle < window {
                return None;
            }
            Some(ExpiredReview {
                id: row.id,
                review_type: row.review_type.clone(),
                pull_request: row.pr_number,
                why: format!(
                    "no report in {minutes} minutes while {}; the router stopped waiting",
                    row.state.label()
                ),
            })
        })
        .collect()
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

/// How the most recent attempt at one dimension ended, when it ended in a
/// classified refusal.
///
/// Deliberately only the *most recent* row. A quota refusal from last week says
/// nothing about the provider's budget today, and a routing edge that read it
/// would pin the dimension to the expensive backend for good. Once any later
/// attempt exists -- requested, running, or completed -- the refusal is history
/// and this returns `None`, so the edge un-takes itself the moment the provider
/// answers again (#175).
///
/// An `unknown` refusal returns `Some(RefusalClass::Unknown)` rather than
/// `None`: it genuinely refused, and whether that is worth an escape is the
/// policy's decision to make, not this function's.
pub fn last_refusal(rows: &[ReviewRequest], review_type: &str) -> Option<RefusalClass> {
    let latest = rows
        .iter()
        .filter(|row| row.review_type == review_type)
        .max_by_key(|row| {
            (
                row.requested_at
                    .or(row.completed_at)
                    .unwrap_or(row.updated_at),
                row.id,
            )
        })?;
    if latest.state != ReviewRequestState::Abandoned {
        return None;
    }
    ReviewRefusal::parse(latest.detail.as_deref()?).map(|refusal| refusal.class)
}

#[cfg(test)]
mod last_refusal_tests {
    use super::*;

    fn row(
        id: i64,
        review_type: &str,
        state: ReviewRequestState,
        detail: Option<&str>,
    ) -> ReviewRequest {
        ReviewRequest {
            id,
            repository: "o/r".into(),
            pr_number: 7,
            review_type: review_type.into(),
            head_commit: "abc123".into(),
            requested_for_commit: Some("abc123".into()),
            base_commit: None,
            backend: "provider_comment".into(),
            trigger: None,
            state,
            detail: detail.map(str::to_string),
            // `requested_at` tracks `id` so ordering is unambiguous; the
            // production ordering breaks ties on `id` for exactly the case
            // where it does not.
            requested_at: Some(id * 1_000),
            completed_at: None,
            completed_for_commit: None,
            verdict: None,
            reviewer: None,
            updated_at: id * 1_000,
        }
    }

    const QUOTA: &str = "quota_exhausted: You have exceeded your usage limit";

    /// The case the routing edge exists for.
    #[test]
    fn the_previous_attempt_refusing_on_quota_is_reported() {
        let rows = vec![row(1, "code", ReviewRequestState::Abandoned, Some(QUOTA))];
        assert_eq!(
            last_refusal(&rows, "code"),
            Some(RefusalClass::QuotaExhausted)
        );
    }

    /// The expiry rule, and the reason this reads one row rather than scanning
    /// for any refusal: once the provider answers again, the edge must stop
    /// being taken. Without this a single quota refusal would pin the
    /// dimension to the expensive backend for the life of the pull request.
    #[test]
    fn a_refusal_a_later_attempt_replaced_is_history() {
        let rows = vec![
            row(1, "code", ReviewRequestState::Abandoned, Some(QUOTA)),
            row(2, "code", ReviewRequestState::Satisfied, None),
        ];
        assert_eq!(last_refusal(&rows, "code"), None);
    }

    /// A later attempt that is merely *running* also clears it. The edge asks
    /// "did the last attempt refuse", not "has anything ever refused", so a
    /// review in progress must not be second-guessed by spawning a rival.
    #[test]
    fn an_attempt_still_running_is_not_a_refusal() {
        let rows = vec![
            row(1, "code", ReviewRequestState::Abandoned, Some(QUOTA)),
            row(2, "code", ReviewRequestState::Running, None),
        ];
        assert_eq!(last_refusal(&rows, "code"), None);
    }

    /// Dimensions are independent: a spent provider on `code` says nothing
    /// about `security`, which may be routed to a different backend entirely.
    #[test]
    fn a_refusal_on_one_dimension_does_not_reroute_another() {
        let rows = vec![row(1, "code", ReviewRequestState::Abandoned, Some(QUOTA))];
        assert_eq!(last_refusal(&rows, "security"), None);
    }

    /// An abandoned row whose detail is not a refusal -- a router timeout, say
    /// -- is not a provider refusal and must not take a refusal edge.
    #[test]
    fn an_abandonment_that_is_not_a_refusal_is_not_one() {
        let rows = vec![row(
            1,
            "code",
            ReviewRequestState::Abandoned,
            Some("no report in 360 minutes while in_progress"),
        )];
        assert_eq!(last_refusal(&rows, "code"), None);
    }

    /// `unknown` is returned rather than swallowed. Whether an unreadable
    /// refusal is worth an escape is the policy's call; this only reports.
    #[test]
    fn an_unknown_refusal_is_reported_as_unknown() {
        let rows = vec![row(
            1,
            "code",
            ReviewRequestState::Abandoned,
            Some("unknown: ?"),
        )];
        assert_eq!(last_refusal(&rows, "code"), Some(RefusalClass::Unknown));
    }
}

#[cfg(test)]
mod refusal_tests {
    use super::*;

    /// The refusal that caused #173. A spent budget is the one class where
    /// retrying is not the answer, so reading it as anything else sends an
    /// operator back to the same wall.
    #[test]
    fn a_spent_budget_reads_as_quota_exhausted() {
        assert_eq!(
            RefusalClass::classify(
                "Error: your usage limit has been reached for this billing period"
            ),
            RefusalClass::QuotaExhausted
        );
    }

    /// Both vocabularies contain "limit", and the two mean opposite things to
    /// whoever is deciding whether to wait. Ordering the scrape is the whole
    /// defence, so the overlapping case is the one worth pinning.
    #[test]
    fn a_rate_limit_is_not_read_as_a_spent_budget() {
        assert_eq!(
            RefusalClass::classify(
                "You have exceeded a secondary rate limit. Please retry after 60s"
            ),
            RefusalClass::RateLimited
        );
    }

    /// The tie the ordering exists to settle. A provider that refuses on a
    /// spent budget and then says when it refills speaks both vocabularies at
    /// once, and reading it as a rate limit tells the operator to wait
    /// minutes for something that will not come back for hours.
    #[test]
    fn a_quota_refusal_that_names_a_refill_time_is_not_a_rate_limit() {
        assert_eq!(
            RefusalClass::classify(
                "You have exceeded your usage limit. Please retry after your quota resets at 00:00 UTC"
            ),
            RefusalClass::QuotaExhausted
        );
    }

    #[test]
    fn a_provider_failure_reads_as_a_provider_error() {
        assert_eq!(
            RefusalClass::classify("HTTP 503: Service Unavailable"),
            RefusalClass::ProviderError
        );
    }

    /// `unknown` is an outcome, not a bug: the refusal still answers "why is
    /// this gate unclearable" because the provider's words travel with it.
    #[test]
    fn an_unrecognised_refusal_is_unknown_rather_than_guessed() {
        assert_eq!(
            RefusalClass::classify("the reviewer declined"),
            RefusalClass::Unknown
        );
    }

    /// A CLI that fails says why on stderr; stdout in that case is usually a
    /// truncated payload that classifies as nothing.
    #[test]
    fn stderr_outranks_stdout_as_the_refusal_source() {
        let refusal = ReviewRefusal::from_provider_output(
            "{}",
            "quota exceeded",
            "the coordinated GitHub write failed",
        );
        assert_eq!(refusal.class, RefusalClass::QuotaExhausted);
        assert_eq!(refusal.text, "quota exceeded");
    }

    /// A provider that says nothing at all still has to produce a row, or the
    /// silent case goes back to being indistinguishable from "never asked".
    #[test]
    fn a_silent_refusal_falls_back_to_what_the_caller_was_doing() {
        let refusal =
            ReviewRefusal::from_provider_output("", "   ", "requesting a security review");
        assert_eq!(refusal.class, RefusalClass::Unknown);
        assert_eq!(refusal.text, "requesting a security review");
    }

    /// The classification is precision; the text is evidence. A misfire must
    /// cost the first and never the second, so the words survive verbatim.
    #[test]
    fn the_providers_words_survive_a_misclassification() {
        let refusal = ReviewRefusal::from_provider_output("", "the reviewer declined", "");
        assert_eq!(refusal.class, RefusalClass::Unknown);
        assert_eq!(refusal.detail(), "unknown: the reviewer declined");
    }

    /// `detail` is rendered into pull-request comments, and a provider can
    /// answer with an HTML error page.
    #[test]
    fn an_enormous_refusal_is_bounded_and_says_so() {
        let refusal = ReviewRefusal::from_provider_output("", &"x".repeat(5_000), "");
        assert_eq!(refusal.text.chars().count(), REFUSAL_TEXT_LIMIT + 1);
        assert!(refusal.text.ends_with('\u{2026}'));
    }

    #[test]
    fn a_multiline_refusal_becomes_one_line() {
        let refusal = ReviewRefusal::from_provider_output("", "quota\n  exceeded\n", "");
        assert_eq!(refusal.text, "quota exceeded");
    }

    #[test]
    fn a_refusal_round_trips_through_the_detail_column() {
        let refusal = ReviewRefusal::from_provider_output("", "API rate limit exceeded", "");
        assert_eq!(ReviewRefusal::parse(&refusal.detail()), Some(refusal));
    }

    /// The column also carries rule names and debounce windows. Reading one of
    /// those back as a refusal would invent a provider refusal that never
    /// happened -- exactly the fabricated cause #173 exists to avoid.
    #[test]
    fn a_detail_that_is_not_a_refusal_reads_back_as_none() {
        assert_eq!(
            ReviewRefusal::parse("debounced: another review ran 4 minutes ago"),
            None
        );
        assert_eq!(
            ReviewRefusal::parse("the coordinated GitHub write failed"),
            None
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn routing(toml: &str) -> crate::ReviewRoutingPolicy {
        toml::from_str(toml).expect("test routing policy should parse")
    }

    const MINUTE: i64 = 60_000;

    /// The slot budget is only a budget if slots come back. Nothing guarantees
    /// a reviewer reports, so without this a repository dispatches
    /// `max_concurrent` reviews and then looks exactly like a repository whose
    /// policy asks for none.
    #[test]
    fn a_reviewer_that_never_reports_stops_holding_its_slot() {
        let policy = routing(
            "enabled = true\n[route.security]\nbackend = \"chau7\"\nstale_after_minutes = 60\n",
        );
        let now = 1_000 * MINUTE;
        let fresh = row(
            "security",
            "abc",
            now - 30 * MINUTE,
            ReviewRequestState::Running,
        );
        let stale = ReviewRequest {
            id: 2,
            ..row(
                "security",
                "def",
                now - 90 * MINUTE,
                ReviewRequestState::Running,
            )
        };
        let expired = expired(&policy, &[fresh, stale], now);
        assert_eq!(expired.len(), 1, "only the one past its window");
        assert_eq!(expired[0].id, 2);
        assert!(
            expired[0].why.contains("60 minutes"),
            "the row has to say why it was given up on: {}",
            expired[0].why
        );
    }

    /// Measured from the last update, not the request. A reviewer that reported
    /// `running` is working, and restarting it duplicates everything it has
    /// done since.
    #[test]
    fn progress_resets_the_window() {
        let policy = routing(
            "enabled = true\n[route.security]\nbackend = \"chau7\"\nstale_after_minutes = 60\n",
        );
        let now = 1_000 * MINUTE;
        let mut working = row(
            "security",
            "abc",
            now - 600 * MINUTE,
            ReviewRequestState::Running,
        );
        working.updated_at = now - 10 * MINUTE;
        assert!(
            expired(&policy, &[working], now).is_empty(),
            "a long review that is still reporting is not a dead one"
        );
    }

    /// Expiry is about slots, and only an open row holds one. Rewriting a
    /// settled row would turn every tick into a write and lose the outcome it
    /// already recorded.
    #[test]
    fn a_settled_review_is_never_expired() {
        let policy = routing(
            "enabled = true\n[route.security]\nbackend = \"chau7\"\nstale_after_minutes = 1\n",
        );
        let now = 1_000 * MINUTE;
        for settled in [
            ReviewRequestState::Satisfied,
            ReviewRequestState::Failed,
            ReviewRequestState::Recorded,
            ReviewRequestState::Abandoned,
        ] {
            let old = row("security", "abc", now - 500 * MINUTE, settled);
            assert!(
                expired(&policy, &[old], now).is_empty(),
                "{settled:?} is an outcome, not a slot"
            );
        }
    }

    /// The window is per route for the same reason the budget is: a security
    /// review that takes hours and a docs review that takes minutes cannot
    /// share one number without the short one waiting on the long one.
    #[test]
    fn each_route_waits_its_own_length_and_zero_waits_forever() {
        let policy = routing(
            "enabled = true\n             [route.security]\nbackend = \"chau7\"\nstale_after_minutes = 0\n             [route.docs]\nbackend = \"chau7\"\nstale_after_minutes = 10\n",
        );
        let now = 1_000 * MINUTE;
        let patient = row(
            "security",
            "abc",
            now - 900 * MINUTE,
            ReviewRequestState::Running,
        );
        let impatient = ReviewRequest {
            id: 2,
            ..row(
                "docs",
                "abc",
                now - 11 * MINUTE,
                ReviewRequestState::Requested,
            )
        };
        let expired = expired(&policy, &[patient, impatient], now);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].review_type, "docs");
    }

    /// An expired review has to be askable again, or the window trades a wedged
    /// repository for a silently unreviewed one.
    #[test]
    fn what_expiry_writes_is_the_one_revivable_state() {
        assert!(ReviewRequestState::Abandoned.is_revivable());
        assert!(!ReviewRequestState::Abandoned.occupies_a_slot());
    }

    fn row(review_type: &str, head: &str, at: i64, state: ReviewRequestState) -> ReviewRequest {
        ReviewRequest {
            id: 0,
            repository: "owner/repo".into(),
            pr_number: 7,
            review_type: review_type.into(),
            head_commit: head.into(),
            requested_for_commit: Some(head.into()),
            base_commit: None,
            backend: "chau7".into(),
            trigger: None,
            state,
            detail: None,
            requested_at: Some(at),
            completed_at: None,
            completed_for_commit: None,
            verdict: None,
            reviewer: None,
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

#[cfg(test)]
mod waiver_tests {
    use super::*;

    fn row(
        review_type: &str,
        head: &str,
        state: ReviewRequestState,
        detail: &str,
    ) -> ReviewRequest {
        ReviewRequest {
            id: 1,
            repository: "owner/repo".into(),
            pr_number: 7,
            review_type: review_type.into(),
            head_commit: head.into(),
            requested_for_commit: Some(head.into()),
            base_commit: None,
            backend: "waiver".into(),
            trigger: None,
            state,
            detail: Some(detail.to_string()),
            requested_at: Some(100),
            completed_at: None,
            completed_for_commit: None,
            verdict: None,
            reviewer: None,
            updated_at: 100,
        }
    }

    fn waived(review_type: &str, head: &str) -> ReviewRequest {
        row(
            review_type,
            head,
            ReviewRequestState::Waived,
            &ReviewWaiver::new(Some("Ada <ada@example.com>"), "hotfix, reviewed offline").detail(),
        )
    }

    /// The provenance has to survive the column, or it is not provenance.
    #[test]
    fn a_waiver_round_trips_through_the_detail_column() {
        let waiver = ReviewWaiver::new(Some("Ada <ada@example.com>"), "hotfix, reviewed offline");
        assert_eq!(ReviewWaiver::parse(&waiver.detail()), Some(waiver));
    }

    /// `detail` also carries refusals, rule names and debounce windows. Reading
    /// any of those as a waiver would invent a human decision nobody made.
    #[test]
    fn only_a_waiver_detail_parses_as_a_waiver() {
        for detail in [
            "quota_exhausted: You have exceeded your usage limit",
            "debounced: another review ran 4 minutes ago",
            "waived by : no author",
            "waived by Ada <ada@example.com>",
            "waived by Ada <ada@example.com>:    ",
        ] {
            assert_eq!(ReviewWaiver::parse(detail), None, "{detail:?}");
        }
    }

    /// An anonymous waiver that says it is anonymous beats sending the operator
    /// back to `review state --state satisfied`, which attributes nothing and
    /// claims a review instead.
    #[test]
    fn an_unidentified_operator_still_records_a_readable_waiver() {
        for who in [None, Some(""), Some("   ")] {
            let waiver = ReviewWaiver::new(who, "shipping without a docs review");
            assert_eq!(waiver.who, "an unidentified operator");
            assert_eq!(ReviewWaiver::parse(&waiver.detail()), Some(waiver));
        }
    }

    /// The whole claim of #172: waiving one dimension waives one dimension.
    #[test]
    fn a_waiver_does_not_reach_another_dimension() {
        let rows = vec![waived("code", "aaa")];
        assert!(waiver_for(&rows, "code", "aaa").is_some());
        assert_eq!(waiver_for(&rows, "security", "aaa"), None);
    }

    /// Bound to the head, so it expires when the branch moves without anyone
    /// remembering to withdraw it.
    #[test]
    fn a_waiver_does_not_reach_another_head() {
        let rows = vec![waived("code", "aaa")];
        assert_eq!(waiver_for(&rows, "code", "bbb"), None);
    }

    /// A row in some other state is not a waiver however its detail reads --
    /// otherwise a satisfied review whose note happened to start "waived by"
    /// would be reported as excused.
    #[test]
    fn only_a_waived_row_yields_a_waiver() {
        let detail = ReviewWaiver::new(Some("Ada"), "excused").detail();
        let rows = vec![row("code", "aaa", ReviewRequestState::Satisfied, &detail)];
        assert_eq!(waiver_for(&rows, "code", "aaa"), None);
    }

    /// Waived is settled, not retryable: the router must not re-ask for a
    /// dimension a person just excused, which would make the waiver useless.
    #[test]
    fn a_waived_dimension_is_settled_and_holds_no_slot() {
        assert!(!ReviewRequestState::Waived.is_revivable());
        assert!(!ReviewRequestState::Waived.occupies_a_slot());

        let spend = spend_by_type(&[waived("code", "aaa")]);
        assert_eq!(spend["code"].requested_count, 1);
        assert_eq!(spend["code"].last_requested_commit.as_deref(), Some("aaa"));
    }

    #[test]
    fn the_waived_state_round_trips_its_label() {
        assert_eq!(
            ReviewRequestState::parse(ReviewRequestState::Waived.label()),
            Some(ReviewRequestState::Waived)
        );
    }
}
