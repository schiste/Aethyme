//! Deciding which reviews a change needs, and whether to spend one now.
//!
//! Two questions that look like one and are not. *Eligibility* is a property of
//! the change: does this diff touch something that wants a security review.
//! *Scheduling* is a property of the moment: a pull request under active
//! development raises the same eligibility on every push, and a provider whose
//! review quota is finite cannot answer all of them. Fusing the two is what
//! always-on review already does badly -- it fires every type on every trigger
//! until the quota is gone, and then the dimension that stopped producing
//! evidence is the one nobody chose to sacrifice.
//!
//! So the policy is evaluated in two passes. [`eligible_types`] answers the
//! first question from facts alone and is a pure function of the change.
//! [`schedule`] answers the second from what has already been spent, and is the
//! only place a decision depends on history.
//!
//! Nothing here performs a review or talks to a provider. The output is a
//! [`ReviewTriggerDecision`] -- a value an adapter executes and a test can
//! assert on without a network.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Current shape of the `[review.trigger]` table.
///
/// Separate from `REVIEW_POLICY_SCHEMA_VERSION` on purpose: the lifecycle
/// policy and the trigger policy are versioned independently so adding a
/// predicate does not force a migration on repositories that only use the
/// lifecycle.
pub const REVIEW_TRIGGER_SCHEMA_VERSION: u32 = 1;

/// A review dimension. Free-form because the set is repository policy, not a
/// property of the broker: a repository that wants `performance` alongside
/// `code` and `security` declares one, and every rule keys off the same string.
pub type ReviewType = String;

// ---------------------------------------------------------------------------
// What the author told us
// ---------------------------------------------------------------------------

/// Classification the author declared, read from commit-message trailers.
///
/// The agent that wrote the change already knows whether it touched
/// authentication. Asking it later costs a whole turn and re-derives, less
/// accurately, what was known at the time; a trailer on a commit message the
/// agent is already composing costs nothing. So this is captured at the source
/// and is why [`ChangeFacts`] carries it rather than inferring it.
///
/// **A declaration may escalate a review, never waive one.** Every field here
/// is author-supplied and therefore unverified -- it can be mistaken, stale, or
/// in a hostile case chosen to dodge a dimension. [`eligible_types`] unions
/// declarations with the path-derived floor rather than intersecting, so a
/// wrong declaration costs an unnecessary review and can never remove a
/// required one. That is what makes trusting an unverified signal safe.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitClassification {
    /// `Area:` -- broad part of the system, e.g. `backend`, `frontend`.
    #[serde(default)]
    pub areas: BTreeSet<String>,
    /// `Surface:` -- named sensitive surfaces, e.g. `auth`, `storage`.
    #[serde(default)]
    pub surfaces: BTreeSet<String>,
    /// `Risk:` -- the author's own severity call.
    #[serde(default)]
    pub risk: Option<String>,
    /// `Review:` -- a dimension the author explicitly asked for.
    #[serde(default)]
    pub requested: BTreeSet<String>,
    /// `Model:` -- the model that wrote the change.
    ///
    /// Declared rather than inferred. A `Co-Authored-By` line is not a
    /// reliable substitute: it is written by convention, carries a human-facing
    /// name rather than a stable identifier, and appears on commits a model only
    /// helped with. A rule that refuses to let a model review its own output
    /// must not fire on a guess, so this is populated only when an author says
    /// so outright.
    #[serde(default)]
    pub model: Option<String>,
}

impl CommitClassification {
    /// Whether the author declared anything at all.
    ///
    /// Distinguishing "declared nothing" from "declared empty" matters for
    /// [`classification_conflicts`]: a commit with no trailers is the ordinary
    /// case and must not be reported as disagreeing with its own diff.
    ///
    /// `model` is deliberately not counted. This gates conflict detection,
    /// which compares declarations against the diff, and there is nothing in a
    /// diff that can contradict who wrote it.
    pub fn is_empty(&self) -> bool {
        self.areas.is_empty()
            && self.surfaces.is_empty()
            && self.risk.is_none()
            && self.requested.is_empty()
    }

    /// Merge the classifications of every commit in a change.
    ///
    /// Union, never intersection, for the same reason declarations may only
    /// escalate: one commit touching `auth` makes the change touch `auth`,
    /// whatever the others say. The highest declared risk wins.
    pub fn merge(parts: impl IntoIterator<Item = Self>) -> Self {
        let mut merged = Self::default();
        for part in parts {
            merged.areas.extend(part.areas);
            merged.surfaces.extend(part.surfaces);
            merged.requested.extend(part.requested);
            // First declaration wins. Commits are merged oldest-first and a
            // change has one author, so a later commit that omits the trailer
            // is silence, not a retraction.
            merged.model = merged.model.take().or(part.model);
            merged.risk = match (merged.risk.take(), part.risk) {
                (Some(current), Some(other)) => Some(if risk_rank(&other) > risk_rank(&current) {
                    other
                } else {
                    current
                }),
                (current, other) => current.or(other),
            };
        }
        merged
    }
}

/// Order the known risk words. An unrecognised word ranks above `low` but below
/// `high`, so a typo escalates rather than silently downgrading.
fn risk_rank(risk: &str) -> u8 {
    match risk.trim().to_ascii_lowercase().as_str() {
        "none" => 0,
        "low" => 1,
        "high" | "critical" => 3,
        _ => 2,
    }
}

/// Trailer keys this understands, lowercased.
const TRAILER_AREA: &str = "area";
const TRAILER_SURFACE: &str = "surface";
const TRAILER_RISK: &str = "risk";
const TRAILER_REVIEW: &str = "review";
const TRAILER_MODEL: &str = "model";

/// Read `Area:` / `Surface:` / `Risk:` / `Review:` trailers from one commit
/// message.
///
/// Deliberately lenient. These ride a message whose real job is
/// `Problem`/`Decision`/`Rationale`/`Validation`, and a change that fails to
/// parse must not fail a commit -- an unreadable trailer yields no
/// classification, which falls back to the path-derived floor and reviews
/// *more*, not less.
///
/// Trailers are read from anywhere in the body rather than only a trailing
/// block, because the structured sections above them already break git's
/// "last paragraph" trailer convention.
pub fn parse_classification(message: &str) -> CommitClassification {
    let mut parsed = CommitClassification::default();
    for line in message.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        // A key with whitespace is prose containing a colon, not a trailer.
        let key = key.trim().to_ascii_lowercase();
        if key.is_empty() || key.contains(char::is_whitespace) {
            continue;
        }
        let values = || {
            value
                .split(',')
                .map(|item| item.trim().to_ascii_lowercase())
                .filter(|item| !item.is_empty())
        };
        match key.as_str() {
            TRAILER_AREA => parsed.areas.extend(values()),
            TRAILER_SURFACE => parsed.surfaces.extend(values()),
            TRAILER_REVIEW => parsed.requested.extend(values()),
            TRAILER_MODEL => {
                // First declaration wins, the same way it does across commits
                // in `merge`. A message naming two models still has one author
                // and one first claim; letting the last line win would make
                // the answer depend on where in the body a trailer was added.
                if parsed.model.is_none() {
                    parsed.model = values().next();
                }
            }
            TRAILER_RISK => {
                if let Some(risk) = values().next() {
                    parsed.risk = Some(match parsed.risk.take() {
                        Some(current) if risk_rank(&current) >= risk_rank(&risk) => current,
                        _ => risk,
                    });
                }
            }
            _ => {}
        }
    }
    parsed
}

// ---------------------------------------------------------------------------
// What actually happened
// ---------------------------------------------------------------------------

/// A lifecycle transition a rule can fire on.
///
/// `ReplacementCommit` and `AdditionalCommit` are separate because collapsing
/// them into "the head moved" is half of why always-on review over-fires: a
/// force-push that rewrites the same logical change and a commit stacked on top
/// of reviewed work justify different responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewTrigger {
    PullRequestOpened,
    ReadyForReview,
    Reopened,
    /// The head was rewritten -- force-push, amend, rebase.
    ReplacementCommit,
    /// A new commit was added on top of the existing head.
    AdditionalCommit,
    BaseRetargeted,
    ReviewDismissed,
    MergeQueueEntered,
    /// A periodic sweep, for rules that exist to catch what events missed.
    Scheduled,
    /// `broker review request`, which bypasses eligibility entirely.
    Manual,
    /// A provider completed a review that Aethyme did not request.
    ///
    /// This is a ledger fact, not a policy trigger. It is deliberately part
    /// of the shared vocabulary so an unsolicited completion can be recorded
    /// without inventing a request or a backend-specific state.
    Unsolicited,
}

/// Everything a predicate may read about one change.
///
/// Gathered by the caller from git and the provider, never fetched here, so the
/// whole decision is testable without a repository or a network.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeFacts {
    pub trigger: Option<ReviewTrigger>,
    /// Repository-relative paths touched by the change.
    pub paths: Vec<String>,
    /// What the author declared, merged across the change's commits.
    pub classification: CommitClassification,
    /// A pull request from a fork, which is the classic untrusted-input case.
    pub from_fork: bool,
    /// The author has not previously landed a change here.
    pub first_time_contributor: bool,
    /// Model that authored the change, when Aethyme recorded one.
    ///
    /// Carried so a rule can require a reviewer that is not the author: a
    /// reviewer running the model that wrote the code has correlated blind
    /// spots exactly where review is supposed to be independent.
    pub authored_by_model: Option<String>,
}

// ---------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------

/// One `[[review.trigger.rule]]` entry: when this matches, require these types.
///
/// Every field is a condition, and an absent field is not a condition. A rule
/// with no conditions at all matches every change, which is a legitimate way to
/// say "always code-review" and is why there is no guard against it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewTriggerRule {
    /// Human-readable, used in the decision's reason so an operator can tell
    /// which rule spent their quota.
    #[serde(default)]
    pub name: Option<String>,
    /// Review types this rule requires when it matches.
    pub require: Vec<ReviewType>,
    /// Lifecycle transitions this rule fires on. Empty means any.
    #[serde(default)]
    pub on: Vec<ReviewTrigger>,
    /// Glob-ish path patterns; matching any one satisfies the condition.
    #[serde(default)]
    pub paths: Vec<String>,
    /// `Surface:` values that satisfy the condition.
    #[serde(default)]
    pub surfaces: Vec<String>,
    /// `Area:` values that satisfy the condition.
    #[serde(default)]
    pub areas: Vec<String>,
    /// Minimum declared risk, compared by [`risk_rank`].
    #[serde(default)]
    pub min_risk: Option<String>,
    /// Only when the change comes from a fork.
    #[serde(default)]
    pub from_fork: Option<bool>,
    /// Only when the author has not landed here before.
    #[serde(default)]
    pub first_time_contributor: Option<bool>,
    /// Only when a model declared itself the author (`true`), or only when none
    /// did (`false`).
    ///
    /// Reads the `Model:` trailer, which is a declaration rather than a
    /// detection: `true` means "somebody said a model wrote this", and it is
    /// deliberately not the same claim as "a model wrote this". A rule may
    /// therefore only ever *add* a review on the strength of it, which is the
    /// same floor every other condition here obeys.
    #[serde(default)]
    pub authored_by_model: Option<bool>,
    /// Only when the declared model is one of these, compared case-insensitively.
    ///
    /// Narrower than `authored_by_model` and for the one case that needs it: a
    /// reviewer running the model that wrote the code shares its blind spots
    /// exactly where review is supposed to be independent, so a repository that
    /// reviews with one model can name it here and route those changes
    /// elsewhere.
    #[serde(default)]
    pub models: Vec<String>,
}

/// What makes a completed review stale for one dimension.
///
/// One rule for every dimension is the thing this exists to stop. When the base
/// moves, what a *code* diff means changes -- the same lines now sit on top of
/// code the reviewer never saw. A *security* finding surface is mostly the
/// head's own content, and advancing the base does not retroactively introduce
/// a vulnerability into code already reviewed at that exact SHA. Forcing both
/// dimensions onto whichever rule is stricter is what left `Aeptus/mockup`'s
/// PR #619 permanently unmergeable on 2026-09-11: it had a full security review
/// bound to its current head, and the only way to clear the flag was a re-run
/// the provider's exhausted quota could not serve (#172).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ReviewFreshness {
    /// A review stays valid until the head SHA changes.
    ///
    /// The default, and the rule every dimension has followed since the router
    /// existed -- so a repository that does not write this key keeps exactly
    /// today's behaviour.
    #[default]
    Head,
    /// The head SHA changing invalidates, and so does the base commit moving.
    ///
    /// Opt-in, and worth declaring for `code`. The cost is real: every merge
    /// into the base branch invalidates every open pull request's review, which
    /// on a busy trunk can be more re-reviews than the quota affords. That is
    /// the trade the operator is making, and why it is not the default.
    HeadAndBase,
}

/// How much may be spent, and how often.
///
/// This is the half that keeps a rich predicate set from becoming a quota
/// bonfire. Predicates decide what a change deserves; this decides what it gets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewSchedule {
    /// Minimum gap between two reviews of one type on one pull request.
    #[serde(default)]
    pub debounce_seconds: u64,
    /// Cap on reviews of one type per pull request. `0` means no cap.
    ///
    /// The bound that matters on a long-lived branch: without it a pull request
    /// with forty pushes spends forty security reviews and starves every other
    /// pull request in the repository.
    #[serde(default)]
    pub max_per_pull_request: u32,
    /// Re-review when the head moves even if the cap is reached.
    ///
    /// Off by default: the cap exists precisely to survive an active branch.
    #[serde(default)]
    pub always_on_new_head: bool,
    /// What invalidates a completed review of this dimension.
    ///
    /// Per dimension because that is the whole point: an operator who has to
    /// apply one rule to every dimension ends up applying the strictest one,
    /// and then reaches for a blanket override to escape it.
    #[serde(default)]
    pub freshness: ReviewFreshness,
}

impl Default for ReviewSchedule {
    fn default() -> Self {
        // Ten minutes absorbs a burst of pushes without deferring a review
        // anyone is waiting on; eight is generous for one pull request while
        // still bounding a runaway branch.
        Self {
            debounce_seconds: 600,
            max_per_pull_request: 8,
            always_on_new_head: false,
            freshness: ReviewFreshness::Head,
        }
    }
}

/// The `[review.trigger]` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewTriggerPolicy {
    #[serde(default = "default_trigger_schema_version")]
    pub schema_version: u32,
    /// Default off, like every other opt-in broker surface: a repository that
    /// has not asked for this keeps provider behaviour byte-for-byte.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub rule: Vec<ReviewTriggerRule>,
    #[serde(default)]
    pub schedule: BTreeMap<ReviewType, ReviewSchedule>,
    /// Schedule for a type with no entry in `schedule`.
    #[serde(default)]
    pub default_schedule: ReviewSchedule,
    /// Review pull requests whose head lives in a different repository.
    ///
    /// Off by default. A fork's diff is written by someone with no standing in
    /// this repository, and a Chau7 reviewer reads that diff from a checkout on
    /// the operator's machine, as the operator. Text in the diff is therefore
    /// input to an agent -- the classic prompt-injection shape -- and the only
    /// defence that holds whatever the reviewer's sandbox turns out to allow is
    /// not starting it. A repository that wants fork review says so here, and
    /// every other rule, `from_fork` included, then applies unchanged.
    #[serde(default)]
    pub include_forks: bool,
}

fn default_trigger_schema_version() -> u32 {
    REVIEW_TRIGGER_SCHEMA_VERSION
}

impl Default for ReviewTriggerPolicy {
    fn default() -> Self {
        Self {
            schema_version: REVIEW_TRIGGER_SCHEMA_VERSION,
            enabled: false,
            rule: Vec::new(),
            schedule: BTreeMap::new(),
            default_schedule: ReviewSchedule::default(),
            include_forks: false,
        }
    }
}

/// Why a fork's pull request was not reviewed, in the decision's own words.
///
/// Spelled out rather than left as an empty eligibility list: "no review
/// needed" and "a review was needed and deliberately withheld" must not look
/// the same to an operator reading the tick.
pub const FORK_EXCLUDED_WHY: &str = "pull request is from a fork; fork pull requests are not \
     reviewed unless `[review.trigger] include_forks = true`, because their diff is untrusted \
     input to a reviewer running on this machine";

/// Why a policy could not be used. Each names what an operator must change.
#[derive(Debug, thiserror::Error)]
pub enum ReviewTriggerError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid {path}: {source}")]
    Parse {
        path: String,
        source: toml::de::Error,
    },
    #[error(
        "{path}: review.trigger schema_version {found} is newer than this broker understands \
         ({supported}); upgrade aethyme or pin the policy"
    )]
    UnsupportedSchema {
        path: String,
        found: u32,
        supported: u32,
    },
    #[error("{path}: review.trigger rule {index} requires no review types; give it `require`")]
    RuleRequiresNothing { path: String, index: usize },
    #[error(
        "{path}: review.trigger rule {index} waits for `{trigger}`, which no tick can \
         report; remove it from `on` or the rule will never fire"
    )]
    RuleWaitsForever {
        path: String,
        index: usize,
        trigger: &'static str,
    },
}

impl ReviewTriggerPolicy {
    /// Load from `.aethyme/config.toml`, defaulting to disabled when the file or
    /// the table is absent.
    ///
    /// A *newer* schema refuses rather than falling back to the default. The
    /// default is "review nothing", and silently reviewing nothing because a
    /// policy was written for a later broker is the one failure this must not
    /// have.
    pub fn load(root: &Path) -> Result<Self, ReviewTriggerError> {
        let path = root.join(".aethyme/config.toml");
        let display = path.display().to_string();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => {
                return Err(ReviewTriggerError::Read {
                    path: display,
                    source,
                });
            }
        };
        let value: toml::Value = text.parse().map_err(|source| ReviewTriggerError::Parse {
            path: display.clone(),
            source,
        })?;
        let Some(table) = value.get("review").and_then(|review| review.get("trigger")) else {
            return Ok(Self::default());
        };
        let policy: Self =
            table
                .clone()
                .try_into()
                .map_err(|source| ReviewTriggerError::Parse {
                    path: display.clone(),
                    source,
                })?;
        policy.validate(&display)?;
        Ok(policy)
    }

    fn validate(&self, path: &str) -> Result<(), ReviewTriggerError> {
        if self.schema_version > REVIEW_TRIGGER_SCHEMA_VERSION {
            return Err(ReviewTriggerError::UnsupportedSchema {
                path: path.to_string(),
                found: self.schema_version,
                supported: REVIEW_TRIGGER_SCHEMA_VERSION,
            });
        }
        for (index, rule) in self.rule.iter().enumerate() {
            if rule.require.is_empty() {
                return Err(ReviewTriggerError::RuleRequiresNothing {
                    path: path.to_string(),
                    index,
                });
            }
            // A rule may only wait for something a tick can report. The
            // spelling parses either way, so without this the rule sits in the
            // file looking configured and never fires once -- the most
            // expensive kind of wrong, because nothing ever says so.
            for trigger in &rule.on {
                if !trigger.is_observable() {
                    return Err(ReviewTriggerError::RuleWaitsForever {
                        path: path.to_string(),
                        index,
                        trigger: trigger.as_str(),
                    });
                }
            }
        }
        Ok(())
    }

    fn schedule_for(&self, review_type: &str) -> &ReviewSchedule {
        self.schedule
            .get(review_type)
            .unwrap_or(&self.default_schedule)
    }
}

// ---------------------------------------------------------------------------
// Matching
// ---------------------------------------------------------------------------

/// Match one repository-relative path against a pattern.
///
/// A deliberately small glob: `*` spans one segment, `**` spans any number, and
/// everything else is literal. Enough for `crates/*/src/auth/**` and
/// `.github/workflows/**`, and small enough that an operator can predict it
/// without consulting a reference -- which matters more here than expressiveness,
/// because a pattern that silently fails to match quietly removes a review.
fn path_matches(pattern: &str, path: &str) -> bool {
    fn matches(pattern: &[&str], path: &[&str]) -> bool {
        match pattern.split_first() {
            None => path.is_empty(),
            Some((&"**", rest)) => {
                // `**` is allowed to match nothing, so try every split.
                (0..=path.len()).any(|take| matches(rest, &path[take..]))
            }
            Some((&segment, rest)) => match path.split_first() {
                Some((&head, tail)) if segment == "*" || segment == head => matches(rest, tail),
                _ => false,
            },
        }
    }
    matches(
        &pattern
            .split('/')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>(),
        &path
            .split('/')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>(),
    )
}

impl ReviewTriggerRule {
    /// Whether every condition this rule states is satisfied.
    ///
    /// Conditions are AND-ed across kinds and OR-ed within one kind: a rule
    /// naming both `paths` and `surfaces` wants a path match *and* a surface
    /// match. That reading makes a rule narrower as an operator adds to it,
    /// which is the direction people expect when they are trying to stop a rule
    /// firing too often.
    fn matches(&self, facts: &ChangeFacts) -> bool {
        if !self.on.is_empty() && !facts.trigger.is_some_and(|t| self.on.contains(&t)) {
            return false;
        }
        if !self.paths.is_empty()
            && !self
                .paths
                .iter()
                .any(|pattern| facts.paths.iter().any(|path| path_matches(pattern, path)))
        {
            return false;
        }
        if !self.surfaces.is_empty()
            && !self.surfaces.iter().any(|surface| {
                facts
                    .classification
                    .surfaces
                    .contains(&surface.to_ascii_lowercase())
            })
        {
            return false;
        }
        if !self.areas.is_empty()
            && !self.areas.iter().any(|area| {
                facts
                    .classification
                    .areas
                    .contains(&area.to_ascii_lowercase())
            })
        {
            return false;
        }
        if let Some(minimum) = &self.min_risk {
            let declared = facts.classification.risk.as_deref().unwrap_or("none");
            if risk_rank(declared) < risk_rank(minimum) {
                return false;
            }
        }
        if self
            .from_fork
            .is_some_and(|required| required != facts.from_fork)
        {
            return false;
        }
        if self
            .first_time_contributor
            .is_some_and(|required| required != facts.first_time_contributor)
        {
            return false;
        }
        if self
            .authored_by_model
            .is_some_and(|required| required != facts.authored_by_model.is_some())
        {
            return false;
        }
        if !self.models.is_empty() {
            let declared = facts
                .authored_by_model
                .as_deref()
                .map(str::to_ascii_lowercase);
            let Some(declared) = declared else {
                return false;
            };
            if !self
                .models
                .iter()
                .any(|model| model.to_ascii_lowercase() == declared)
            {
                return false;
            }
        }
        true
    }

    fn label(&self, index: usize) -> String {
        self.name.clone().unwrap_or_else(|| format!("rule {index}"))
    }
}

/// One review type a change was found to need, and what asked for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EligibleReview {
    pub review_type: ReviewType,
    /// Rule names, or `author declaration`, in the order they were found.
    pub because: Vec<String>,
    /// Needed, and withheld by policy rather than by cost -- today only a fork
    /// pull request under the default `include_forks = false`. [`schedule`]
    /// turns it into a `Skip` carrying this reason, so the rules that matched
    /// stay visible next to the reason nothing was spent on them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excluded: Option<String>,
}

/// Review types this change needs, before anything about cost is considered.
///
/// A pure function of the facts: same change, same answer, no history and no
/// clock. That is what makes a policy testable against a repository's existing
/// pull requests before it is ever switched on.
///
/// Author declarations are unioned in rather than consulted as an alternative,
/// which is the mechanism behind "escalate, never waive": the path-derived
/// rules set a floor that no declaration can lower.
pub fn eligible_types(policy: &ReviewTriggerPolicy, facts: &ChangeFacts) -> Vec<EligibleReview> {
    let mut found: BTreeMap<ReviewType, Vec<String>> = BTreeMap::new();
    if !policy.enabled {
        return Vec::new();
    }
    for (index, rule) in policy.rule.iter().enumerate() {
        if !rule.matches(facts) {
            continue;
        }
        for review_type in &rule.require {
            found
                .entry(review_type.clone())
                .or_default()
                .push(rule.label(index));
        }
    }
    for requested in &facts.classification.requested {
        found
            .entry(requested.clone())
            .or_default()
            .push("author declaration".to_string());
    }
    // Applied after matching, not instead of it: the rules a fork's change
    // matched are still reported, next to the reason none of them is spent.
    // An author declaration is excluded too -- a fork's author writes its
    // trailers, and escalating into a reviewer is exactly what must not be
    // theirs to ask for.
    let excluded =
        (facts.from_fork && !policy.include_forks).then(|| FORK_EXCLUDED_WHY.to_string());
    found
        .into_iter()
        .map(|(review_type, because)| EligibleReview {
            review_type,
            because,
            excluded: excluded.clone(),
        })
        .collect()
}

/// A declared classification that its own diff does not support.
///
/// Not an error and not a veto -- the union rule already makes a wrong
/// declaration harmless. It is reported because a mismatch is evidence about
/// the author: a change declared `frontend` that edits the broker crate is
/// either a mistake worth fixing or a signal worth looking at, and neither is
/// visible if the declaration is silently overridden.
///
/// Checked against paths rather than by asking a model, so it costs nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassificationConflict {
    pub declared_area: String,
    pub contradicted_by: Vec<String>,
}

/// Report declared areas whose expected paths the change never touches.
///
/// `area_paths` maps an area to the patterns that area is expected to live in;
/// an area absent from the map is simply not checked, so a repository only pays
/// for the areas it has bothered to describe.
pub fn classification_conflicts(
    facts: &ChangeFacts,
    area_paths: &BTreeMap<String, Vec<String>>,
) -> Vec<ClassificationConflict> {
    if facts.classification.is_empty() {
        return Vec::new();
    }
    let mut conflicts = Vec::new();
    for area in &facts.classification.areas {
        let Some(patterns) = area_paths.get(area) else {
            continue;
        };
        let matched = facts
            .paths
            .iter()
            .any(|path| patterns.iter().any(|pattern| path_matches(pattern, path)));
        if !matched {
            // Name the paths that are actually there, capped: an operator needs
            // enough to recognise the change, not the whole diff.
            let contradicted_by = facts.paths.iter().take(5).cloned().collect();
            conflicts.push(ClassificationConflict {
                declared_area: area.clone(),
                contradicted_by,
            });
        }
    }
    conflicts
}

// ---------------------------------------------------------------------------
// Scheduling
// ---------------------------------------------------------------------------

/// What has already been spent on one review type for one pull request.
///
/// Supplied by the caller from the review record rather than read here, so the
/// scheduling decision stays a pure function like the eligibility one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewSpend {
    /// Reviews of this type already requested for this pull request.
    pub requested_count: u32,
    /// When the most recent one was requested.
    pub last_requested_ms: Option<i64>,
    /// The commit the most recent one was bound to.
    pub last_requested_commit: Option<String>,
    /// The base commit the most recent one was requested against.
    ///
    /// `None` for a row written before the ledger recorded one, and read as
    /// "cannot prove the base is unchanged" rather than "unchanged": under
    /// `head_and_base` that costs one re-review and never passes a stale one.
    pub last_requested_base: Option<String>,
}

/// What to do about one eligible review type, now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ReviewTriggerDecision {
    /// Spend a review now.
    Request {
        review_type: ReviewType,
        because: Vec<String>,
    },
    /// Eligible, but not now. Distinct from `Skip` because a deferred review is
    /// one the next tick should reconsider, and a skipped one is settled.
    Defer {
        review_type: ReviewType,
        why: String,
        retry_after_ms: Option<i64>,
    },
    /// Eligible, and deliberately not going to happen for this change.
    Skip {
        review_type: ReviewType,
        why: String,
    },
}

impl ReviewTriggerDecision {
    pub fn review_type(&self) -> &str {
        match self {
            Self::Request { review_type, .. }
            | Self::Defer { review_type, .. }
            | Self::Skip { review_type, .. } => review_type,
        }
    }
}

/// Turn eligibility into decisions, given what has already been spent.
///
/// `head` is the commit under consideration and `base` is the commit its target
/// branch currently points at; `now_ms` is the caller's clock. A manual request
/// bypasses eligibility entirely -- an operator asking for a review by name has
/// already made the decision this function exists to make.
///
/// `base` is read only by a dimension declaring `freshness = "head_and_base"`,
/// so a caller that cannot determine one passes `None` and every dimension
/// routes exactly as it did before the key existed.
pub fn schedule(
    policy: &ReviewTriggerPolicy,
    eligible: &[EligibleReview],
    spend: &BTreeMap<ReviewType, ReviewSpend>,
    head: &str,
    base: Option<&str>,
    now_ms: i64,
) -> Vec<ReviewTriggerDecision> {
    eligible
        .iter()
        .map(|candidate| {
            // First, so no spend, cap or debounce can reword it: a withheld
            // review is settled for this change, not waiting on a clock.
            if let Some(why) = &candidate.excluded {
                return ReviewTriggerDecision::Skip {
                    review_type: candidate.review_type.clone(),
                    why: why.clone(),
                };
            }
            let schedule = policy.schedule_for(&candidate.review_type);
            let spent = spend
                .get(&candidate.review_type)
                .cloned()
                .unwrap_or_default();
            let head_is_new = spent
                .last_requested_commit
                .as_deref()
                .is_none_or(|last| last != head);

            // Under `head_and_base` the base moving invalidates too. A base
            // that cannot be compared -- the caller did not supply one, or the
            // row predates the ledger recording it -- counts as moved: the
            // question is whether the base is *proven* unchanged, and silence
            // does not prove it. That costs one review; the other reading
            // passes a stale one.
            let base_is_new = match schedule.freshness {
                ReviewFreshness::Head => false,
                ReviewFreshness::HeadAndBase => {
                    match (base, spent.last_requested_base.as_deref()) {
                        (Some(now), Some(then)) => now != then,
                        _ => true,
                    }
                }
            };

            // A review already bound to this exact head -- and, where the
            // dimension says so, this exact base -- is the answer we wanted;
            // spending another buys nothing.
            if !head_is_new && !base_is_new {
                return ReviewTriggerDecision::Skip {
                    review_type: candidate.review_type.clone(),
                    why: format!("already requested for {head}"),
                };
            }
            // `always_on_new_head` says what it says. A base that moved is
            // not a new head, so it does not lift the cap: `head_and_base` on
            // a busy trunk would otherwise turn an uncapped dimension into one
            // review per merge into the base branch, forever.
            if schedule.max_per_pull_request > 0
                && spent.requested_count >= schedule.max_per_pull_request
                && !(schedule.always_on_new_head && head_is_new)
            {
                return ReviewTriggerDecision::Skip {
                    review_type: candidate.review_type.clone(),
                    why: format!(
                        "{} of {} reviews already spent on this pull request",
                        spent.requested_count, schedule.max_per_pull_request
                    ),
                };
            }
            if schedule.debounce_seconds > 0 {
                let window = schedule.debounce_seconds as i64 * 1_000;
                if let Some(last) = spent.last_requested_ms {
                    let elapsed = now_ms.saturating_sub(last);
                    if elapsed < window {
                        return ReviewTriggerDecision::Defer {
                            review_type: candidate.review_type.clone(),
                            why: format!(
                                "debounced; {}s of {}s elapsed since the last request",
                                elapsed / 1_000,
                                schedule.debounce_seconds
                            ),
                            retry_after_ms: Some(now_ms + (window - elapsed)),
                        };
                    }
                }
            }
            ReviewTriggerDecision::Request {
                review_type: candidate.review_type.clone(),
                because: candidate.because.clone(),
            }
        })
        .collect()
}

/// Evaluate a change end to end: eligibility, then scheduling.
pub fn decide(
    policy: &ReviewTriggerPolicy,
    facts: &ChangeFacts,
    spend: &BTreeMap<ReviewType, ReviewSpend>,
    head: &str,
    base: Option<&str>,
    now_ms: i64,
) -> Vec<ReviewTriggerDecision> {
    schedule(
        policy,
        &eligible_types(policy, facts),
        spend,
        head,
        base,
        now_ms,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(paths: &[&str]) -> ChangeFacts {
        ChangeFacts {
            trigger: Some(ReviewTrigger::PullRequestOpened),
            paths: paths.iter().map(|p| p.to_string()).collect(),
            classification: CommitClassification::default(),
            from_fork: false,
            first_time_contributor: false,
            authored_by_model: None,
        }
    }

    fn rule(name: &str, require: &[&str]) -> ReviewTriggerRule {
        ReviewTriggerRule {
            name: Some(name.to_string()),
            require: require.iter().map(|t| t.to_string()).collect(),
            on: Vec::new(),
            paths: Vec::new(),
            surfaces: Vec::new(),
            areas: Vec::new(),
            min_risk: None,
            from_fork: None,
            first_time_contributor: None,
            authored_by_model: None,
            models: Vec::new(),
        }
    }

    fn enabled(rules: Vec<ReviewTriggerRule>) -> ReviewTriggerPolicy {
        ReviewTriggerPolicy {
            enabled: true,
            rule: rules,
            ..Default::default()
        }
    }

    // -- trailers ----------------------------------------------------------

    #[test]
    fn the_model_that_wrote_a_change_is_declared_not_guessed() {
        // `Co-Authored-By` is the lookalike, and deliberately not what this
        // reads: it is written by convention, carries a display name rather
        // than an identifier, and appears on commits a model only helped with.
        let parsed = parse_classification(
            "feat(x): y\n\nCo-Authored-By: Claude Opus 5 <noreply@anthropic.com>\n",
        );
        assert_eq!(parsed.model, None);

        let parsed = parse_classification("feat(x): y\n\nModel: claude-opus-5\n");
        assert_eq!(parsed.model.as_deref(), Some("claude-opus-5"));
    }

    #[test]
    fn the_first_commit_to_name_a_model_is_the_one_that_counts() {
        // Commits merge oldest-first, and a later commit that names a
        // different model must not be able to relabel work somebody already
        // declared -- the same direction `Risk:` escalates in.
        let merged = CommitClassification::merge([
            parse_classification("feat(x): first\n\nModel: claude-opus-5\n"),
            parse_classification("fix(x): second\n\nModel: something-else\n"),
        ]);
        assert_eq!(merged.model.as_deref(), Some("claude-opus-5"));

        // Silence in a later commit is not a retraction.
        let merged = CommitClassification::merge([
            parse_classification("feat(x): first\n\nModel: claude-opus-5\n"),
            parse_classification("fix(x): second\n"),
        ]);
        assert_eq!(merged.model.as_deref(), Some("claude-opus-5"));
    }

    #[test]
    fn a_declared_model_is_not_counted_as_a_classification() {
        // `is_empty` gates diff-conflict detection, and nothing in a diff can
        // contradict a claim about who wrote it. Counting `model` would make
        // every model-authored change look classified when it declared nothing
        // a rule can act on.
        let parsed = parse_classification("feat(x): y\n\nModel: claude-opus-5\n");
        assert!(parsed.is_empty());
    }

    #[test]
    fn a_rule_can_require_that_a_model_declared_itself_the_author() {
        let mut needs_independence = rule("model-authored", &["independent"]);
        needs_independence.authored_by_model = Some(true);
        let policy = enabled(vec![needs_independence]);

        let mut authored = facts(&["src/a.rs"]);
        authored.authored_by_model = Some("claude-opus-5".into());
        assert_eq!(eligible_types(&policy, &authored).len(), 1);

        // Silence is not a denial and not a claim: the rule simply does not
        // match, which is why it may only ever add a review.
        assert!(eligible_types(&policy, &facts(&["src/a.rs"])).is_empty());
    }

    #[test]
    fn a_rule_can_name_the_model_it_will_not_let_review_its_own_output() {
        let mut avoid = rule("not-by-itself", &["security"]);
        avoid.models = vec!["Claude-Opus-5".into()];
        let policy = enabled(vec![avoid]);

        let mut same = facts(&["src/a.rs"]);
        same.authored_by_model = Some("claude-opus-5".into());
        assert_eq!(
            eligible_types(&policy, &same).len(),
            1,
            "the trailer's casing is the author's choice, not a condition"
        );

        let mut other = facts(&["src/a.rs"]);
        other.authored_by_model = Some("some-other-model".into());
        assert!(eligible_types(&policy, &other).is_empty());
        assert!(eligible_types(&policy, &facts(&["src/a.rs"])).is_empty());
    }

    #[test]
    fn trailers_are_read_from_anywhere_in_the_body() {
        // This repository's commit convention puts Problem/Decision/Rationale/
        // Validation sections after the subject, which breaks git's "trailers
        // live in the last paragraph" rule. A parser that honoured that rule
        // would read nothing from any commit we actually write.
        let message = "feat(broker): add trigger policy\n\n\
             Area: backend\n\n\
             Problem: reviews fire on every push.\n\n\
             Decision: split eligibility from scheduling.\n\n\
             Surface: auth\n\
             Risk: high\n";
        let parsed = parse_classification(message);
        assert!(parsed.areas.contains("backend"));
        assert!(parsed.surfaces.contains("auth"));
        assert_eq!(parsed.risk.as_deref(), Some("high"));
    }

    #[test]
    fn prose_containing_a_colon_is_not_a_trailer() {
        let parsed = parse_classification("fix: thing\n\nWe decided this: the area is backend.\n");
        assert!(parsed.is_empty(), "prose parsed as {parsed:?}");
    }

    #[test]
    fn a_comma_separated_trailer_yields_several_values() {
        let parsed = parse_classification("feat: x\n\nArea: Backend, Infra\n");
        assert!(parsed.areas.contains("backend") && parsed.areas.contains("infra"));
    }

    #[test]
    fn merging_takes_the_union_and_the_highest_risk() {
        let merged = CommitClassification::merge([
            parse_classification("feat: a\n\nArea: backend\nRisk: low\n"),
            parse_classification("feat: b\n\nArea: frontend\nRisk: high\n"),
        ]);
        assert!(merged.areas.contains("backend") && merged.areas.contains("frontend"));
        assert_eq!(merged.risk.as_deref(), Some("high"));
    }

    #[test]
    fn an_unrecognised_risk_outranks_low_but_not_high() {
        // A typo must escalate, not silently downgrade: `Risk: hgih` should
        // cost a review, never remove one.
        assert!(risk_rank("hgih") > risk_rank("low"));
        assert!(risk_rank("hgih") < risk_rank("high"));
    }

    // -- eligibility -------------------------------------------------------

    #[test]
    fn a_disabled_policy_is_eligible_for_nothing() {
        let mut policy = enabled(vec![rule("all", &["code"])]);
        policy.enabled = false;
        assert!(eligible_types(&policy, &facts(&["src/lib.rs"])).is_empty());
    }

    #[test]
    fn a_rule_with_no_conditions_matches_every_change() {
        let policy = enabled(vec![rule("always", &["code"])]);
        let found = eligible_types(&policy, &facts(&["README.md"]));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].review_type, "code");
        assert_eq!(found[0].because, vec!["always".to_string()]);
    }

    #[test]
    fn conditions_of_different_kinds_must_all_hold() {
        let mut narrow = rule("auth-backend", &["security"]);
        narrow.paths = vec!["crates/**".to_string()];
        narrow.surfaces = vec!["auth".to_string()];
        let policy = enabled(vec![narrow]);

        // Path matches, surface does not.
        assert!(eligible_types(&policy, &facts(&["crates/broker/src/lib.rs"])).is_empty());

        let mut both = facts(&["crates/broker/src/lib.rs"]);
        both.classification.surfaces.insert("auth".to_string());
        assert_eq!(eligible_types(&policy, &both).len(), 1);
    }

    #[test]
    fn a_declaration_escalates_but_cannot_waive() {
        let mut security = rule("touches-auth", &["security"]);
        security.paths = vec!["src/auth/**".to_string()];
        let policy = enabled(vec![security]);

        // Declaring a harmless area does not remove the path-derived review.
        let mut evasive = facts(&["src/auth/login.rs"]);
        evasive.classification.areas.insert("docs".to_string());
        evasive.classification.risk = Some("none".to_string());
        let found = eligible_types(&policy, &evasive);
        assert_eq!(
            found
                .iter()
                .map(|e| e.review_type.as_str())
                .collect::<Vec<_>>(),
            ["security"]
        );

        // Asking for one the rules did not require still gets it.
        let mut asking = facts(&["README.md"]);
        asking
            .classification
            .requested
            .insert("security".to_string());
        let found = eligible_types(&policy, &asking);
        assert_eq!(found[0].because, vec!["author declaration".to_string()]);
    }

    #[test]
    fn several_rules_requiring_one_type_all_appear_in_the_reason() {
        let mut by_path = rule("by-path", &["security"]);
        by_path.paths = vec!["src/auth/**".to_string()];
        let mut by_fork = rule("by-fork", &["security"]);
        by_fork.from_fork = Some(true);
        let mut policy = enabled(vec![by_path, by_fork]);
        policy.include_forks = true;

        let mut change = facts(&["src/auth/login.rs"]);
        change.from_fork = true;
        let found = eligible_types(&policy, &change);
        assert_eq!(found.len(), 1, "one type, not one per rule");
        assert_eq!(
            found[0].because,
            vec!["by-path".to_string(), "by-fork".to_string()]
        );
    }

    fn fork_change() -> ChangeFacts {
        let mut change = facts(&["src/auth/login.rs"]);
        change.from_fork = true;
        change.classification.requested.insert("code".to_string());
        change
    }

    /// M5: a fork's diff is untrusted input to an agent holding the operator's
    /// machine. Matching rules and author declarations are both withheld, and
    /// the decision says why rather than reading as "nothing needed".
    #[test]
    fn a_fork_pull_request_is_excluded_by_default_and_says_why() {
        let mut by_path = rule("by-path", &["security"]);
        by_path.paths = vec!["src/auth/**".to_string()];
        let policy = enabled(vec![by_path]);

        let found = eligible_types(&policy, &fork_change());
        assert_eq!(found.len(), 2, "matching is still reported: {found:?}");
        for eligible in &found {
            assert_eq!(eligible.excluded.as_deref(), Some(FORK_EXCLUDED_WHY));
        }
        let decisions = decide(&policy, &fork_change(), &BTreeMap::new(), "h", None, 0);
        assert_eq!(decisions.len(), 2);
        for decision in &decisions {
            match decision {
                ReviewTriggerDecision::Skip { why, .. } => {
                    assert!(why.contains("include_forks = true"), "{why}");
                    assert!(why.contains("fork"), "{why}");
                }
                other => panic!("a fork must not be reviewed by default: {other:?}"),
            }
        }

        // The same change from a branch in this repository is reviewed.
        let mut local = fork_change();
        local.from_fork = false;
        assert!(
            decide(&policy, &local, &BTreeMap::new(), "h", None, 0)
                .iter()
                .all(|decision| matches!(decision, ReviewTriggerDecision::Request { .. }))
        );
    }

    #[test]
    fn include_forks_reviews_a_fork_like_any_other_change() {
        let mut by_path = rule("by-path", &["security"]);
        by_path.paths = vec!["src/auth/**".to_string()];
        let mut policy = enabled(vec![by_path]);
        policy.include_forks = true;

        let found = eligible_types(&policy, &fork_change());
        assert!(found.iter().all(|eligible| eligible.excluded.is_none()));
        let decisions = decide(&policy, &fork_change(), &BTreeMap::new(), "h", None, 0);
        assert_eq!(decisions.len(), 2);
        assert!(
            decisions
                .iter()
                .all(|decision| matches!(decision, ReviewTriggerDecision::Request { .. })),
            "{decisions:?}"
        );
    }

    #[test]
    fn include_forks_parses_from_the_trigger_table_and_defaults_off() {
        let temp = tempfile::tempdir().unwrap();
        write_config(temp.path(), "[review.trigger]\nenabled = true\n");
        assert!(
            !ReviewTriggerPolicy::load(temp.path())
                .unwrap()
                .include_forks
        );
        write_config(
            temp.path(),
            "[review.trigger]\nenabled = true\ninclude_forks = true\n",
        );
        assert!(
            ReviewTriggerPolicy::load(temp.path())
                .unwrap()
                .include_forks
        );
    }

    #[test]
    fn min_risk_compares_by_rank_not_by_string() {
        let mut risky = rule("risky", &["security"]);
        risky.min_risk = Some("high".to_string());
        let policy = enabled(vec![risky]);

        let mut low = facts(&["src/lib.rs"]);
        low.classification.risk = Some("low".to_string());
        assert!(eligible_types(&policy, &low).is_empty());

        let mut critical = facts(&["src/lib.rs"]);
        critical.classification.risk = Some("critical".to_string());
        assert_eq!(eligible_types(&policy, &critical).len(), 1);
    }

    #[test]
    fn a_trigger_condition_narrows_to_the_named_triggers() {
        let mut on_open = rule("on-open", &["code"]);
        on_open.on = vec![
            ReviewTrigger::PullRequestOpened,
            ReviewTrigger::ReadyForReview,
        ];
        let policy = enabled(vec![on_open]);

        let mut pushed = facts(&["src/lib.rs"]);
        pushed.trigger = Some(ReviewTrigger::AdditionalCommit);
        assert!(eligible_types(&policy, &pushed).is_empty());
        assert_eq!(eligible_types(&policy, &facts(&["src/lib.rs"])).len(), 1);
    }

    // -- globs -------------------------------------------------------------

    #[test]
    fn a_star_spans_one_segment_and_a_double_star_spans_any() {
        assert!(path_matches(
            "crates/*/src/lib.rs",
            "crates/broker/src/lib.rs"
        ));
        assert!(!path_matches("crates/*/lib.rs", "crates/broker/src/lib.rs"));
        assert!(path_matches("crates/**/lib.rs", "crates/broker/src/lib.rs"));
        assert!(path_matches("crates/**", "crates/broker/src/lib.rs"));
        // `**` matching nothing is what makes `a/**/b` cover `a/b`.
        assert!(path_matches("crates/**/lib.rs", "crates/lib.rs"));
        assert!(!path_matches("crates/**", "packages/broker.rs"));
    }

    // -- conflicts ---------------------------------------------------------

    #[test]
    fn a_declared_area_whose_paths_are_absent_is_reported() {
        let mut area_paths = BTreeMap::new();
        area_paths.insert("frontend".to_string(), vec!["web/**".to_string()]);

        let mut change = facts(&["crates/broker/src/lib.rs"]);
        change.classification.areas.insert("frontend".to_string());
        let conflicts = classification_conflicts(&change, &area_paths);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].declared_area, "frontend");

        // An area the repository never described is not second-guessed.
        change.classification.areas.clear();
        change.classification.areas.insert("infra".to_string());
        assert!(classification_conflicts(&change, &area_paths).is_empty());
    }

    // -- scheduling --------------------------------------------------------

    fn one(review_type: &str) -> Vec<EligibleReview> {
        vec![EligibleReview {
            review_type: review_type.to_string(),
            because: vec!["rule".to_string()],
            excluded: None,
        }]
    }

    // -- freshness (#172) --------------------------------------------------

    /// One dimension's settled review, bound to `head` on base `base`.
    fn settled(
        review_type: &str,
        head: &str,
        base: Option<&str>,
    ) -> BTreeMap<ReviewType, ReviewSpend> {
        let mut spend = BTreeMap::new();
        spend.insert(
            review_type.to_string(),
            ReviewSpend {
                requested_count: 1,
                last_requested_ms: Some(0),
                last_requested_commit: Some(head.to_string()),
                last_requested_base: base.map(str::to_string),
            },
        );
        spend
    }

    /// `freshness` on one dimension, everything else left at its default.
    fn with_freshness(review_type: &str, freshness: ReviewFreshness) -> ReviewTriggerPolicy {
        let mut policy = enabled(Vec::new());
        policy.schedule.insert(
            review_type.to_string(),
            ReviewSchedule {
                freshness,
                ..Default::default()
            },
        );
        policy
    }

    /// The default, and the regression that matters most: a repository that
    /// never writes `freshness` must not notice this feature exists.
    #[test]
    fn a_moving_base_does_not_invalidate_by_default() {
        let policy = enabled(Vec::new());
        let spend = settled("code", "head1", Some("base1"));
        let decisions = schedule(
            &policy,
            &one("code"),
            &spend,
            "head1",
            Some("base2"),
            10_000_000,
        );
        assert!(
            matches!(decisions[0], ReviewTriggerDecision::Skip { .. }),
            "{decisions:?}"
        );
    }

    #[test]
    fn a_moving_base_invalidates_a_head_and_base_dimension() {
        let policy = with_freshness("code", ReviewFreshness::HeadAndBase);
        let spend = settled("code", "head1", Some("base1"));
        let decisions = schedule(
            &policy,
            &one("code"),
            &spend,
            "head1",
            Some("base2"),
            10_000_000,
        );
        assert!(
            matches!(decisions[0], ReviewTriggerDecision::Request { .. }),
            "{decisions:?}"
        );
    }

    #[test]
    fn a_still_base_leaves_a_head_and_base_dimension_settled() {
        let policy = with_freshness("code", ReviewFreshness::HeadAndBase);
        let spend = settled("code", "head1", Some("base1"));
        let decisions = schedule(
            &policy,
            &one("code"),
            &spend,
            "head1",
            Some("base1"),
            10_000_000,
        );
        assert!(
            matches!(decisions[0], ReviewTriggerDecision::Skip { .. }),
            "{decisions:?}"
        );
    }

    /// The whole point of #172: the strict rule is declared per dimension, so
    /// `security` stays valid on the exact head it was reviewed at while
    /// `code` re-reviews. One rule for both is what forced the blanket
    /// override that waived security along with code.
    #[test]
    fn freshness_is_declared_per_dimension() {
        let mut policy = with_freshness("code", ReviewFreshness::HeadAndBase);
        policy.schedule.insert(
            "security".to_string(),
            ReviewSchedule {
                freshness: ReviewFreshness::Head,
                ..Default::default()
            },
        );
        let eligible = vec![
            EligibleReview {
                review_type: "code".to_string(),
                because: vec!["rule".to_string()],
                excluded: None,
            },
            EligibleReview {
                review_type: "security".to_string(),
                because: vec!["rule".to_string()],
                excluded: None,
            },
        ];
        let mut spend = settled("code", "head1", Some("base1"));
        spend.extend(settled("security", "head1", Some("base1")));
        let decisions = schedule(
            &policy,
            &eligible,
            &spend,
            "head1",
            Some("base2"),
            10_000_000,
        );
        let by_type: BTreeMap<&str, &ReviewTriggerDecision> = decisions
            .iter()
            .map(|decision| (decision.review_type(), decision))
            .collect();
        assert!(
            matches!(by_type["code"], ReviewTriggerDecision::Request { .. }),
            "{decisions:?}"
        );
        assert!(
            matches!(by_type["security"], ReviewTriggerDecision::Skip { .. }),
            "{decisions:?}"
        );
    }

    /// A base nobody recorded is not a base nobody moved. The ledger predates
    /// `base_commit`, so the row says nothing; re-reviewing costs one review,
    /// and the other reading passes a review that may well be stale.
    #[test]
    fn a_base_that_cannot_be_compared_counts_as_moved() {
        let policy = with_freshness("code", ReviewFreshness::HeadAndBase);
        let unrecorded = settled("code", "head1", None);
        assert!(
            matches!(
                schedule(
                    &policy,
                    &one("code"),
                    &unrecorded,
                    "head1",
                    Some("base1"),
                    10_000_000
                )[0],
                ReviewTriggerDecision::Request { .. }
            ),
            "a row with no recorded base must re-review"
        );
        let uncallable = settled("code", "head1", Some("base1"));
        assert!(
            matches!(
                schedule(
                    &policy,
                    &one("code"),
                    &uncallable,
                    "head1",
                    None,
                    10_000_000
                )[0],
                ReviewTriggerDecision::Request { .. }
            ),
            "a caller that cannot name today's base must re-review"
        );
    }

    /// `always_on_new_head` says new *head*. Letting a moved base lift the cap
    /// would turn a `head_and_base` dimension on a busy trunk into one review
    /// per merge into the base branch, with no ceiling at all.
    #[test]
    fn a_moved_base_does_not_lift_the_per_pull_request_cap() {
        let mut policy = with_freshness("code", ReviewFreshness::HeadAndBase);
        policy.schedule.insert(
            "code".to_string(),
            ReviewSchedule {
                freshness: ReviewFreshness::HeadAndBase,
                max_per_pull_request: 2,
                always_on_new_head: true,
                ..Default::default()
            },
        );
        let mut spend = settled("code", "head1", Some("base1"));
        spend.get_mut("code").unwrap().requested_count = 2;
        let decisions = schedule(
            &policy,
            &one("code"),
            &spend,
            "head1",
            Some("base2"),
            10_000_000,
        );
        assert!(
            matches!(decisions[0], ReviewTriggerDecision::Skip { .. }),
            "{decisions:?}"
        );
        // The same exhausted budget still re-reviews on a genuinely new head,
        // which is what `always_on_new_head` was asked for.
        let on_new_head = schedule(
            &policy,
            &one("code"),
            &spend,
            "head2",
            Some("base2"),
            10_000_000,
        );
        assert!(
            matches!(on_new_head[0], ReviewTriggerDecision::Request { .. }),
            "{on_new_head:?}"
        );
    }

    #[test]
    fn a_first_review_is_requested() {
        let policy = enabled(Vec::new());
        let decisions = schedule(
            &policy,
            &one("code"),
            &BTreeMap::new(),
            "abc123",
            None,
            1_000,
        );
        assert!(matches!(
            decisions[0],
            ReviewTriggerDecision::Request { .. }
        ));
    }

    #[test]
    fn a_review_already_bound_to_this_head_is_skipped_not_deferred() {
        // Settled, not postponed: no later tick on this same commit will make
        // spending another review worthwhile.
        let policy = enabled(Vec::new());
        let mut spend = BTreeMap::new();
        spend.insert(
            "code".to_string(),
            ReviewSpend {
                requested_count: 1,
                last_requested_ms: Some(0),
                last_requested_commit: Some("abc123".to_string()),
                last_requested_base: None,
            },
        );
        let decisions = schedule(&policy, &one("code"), &spend, "abc123", None, 10_000_000);
        assert!(
            matches!(decisions[0], ReviewTriggerDecision::Skip { .. }),
            "{decisions:?}"
        );
    }

    #[test]
    fn a_new_head_inside_the_debounce_window_defers_with_a_retry_time() {
        let policy = enabled(Vec::new());
        let mut spend = BTreeMap::new();
        spend.insert(
            "code".to_string(),
            ReviewSpend {
                requested_count: 1,
                last_requested_ms: Some(1_000),
                last_requested_commit: Some("old".to_string()),
                last_requested_base: None,
            },
        );
        // Default debounce is 600s; 60s have passed.
        let decisions = schedule(&policy, &one("code"), &spend, "new", None, 61_000);
        match &decisions[0] {
            ReviewTriggerDecision::Defer { retry_after_ms, .. } => {
                assert_eq!(*retry_after_ms, Some(601_000));
            }
            other => panic!("expected Defer, got {other:?}"),
        }
    }

    #[test]
    fn past_the_debounce_window_the_same_facts_request() {
        let policy = enabled(Vec::new());
        let mut spend = BTreeMap::new();
        spend.insert(
            "code".to_string(),
            ReviewSpend {
                requested_count: 1,
                last_requested_ms: Some(1_000),
                last_requested_commit: Some("old".to_string()),
                last_requested_base: None,
            },
        );
        let decisions = schedule(&policy, &one("code"), &spend, "new", None, 601_001);
        assert!(
            matches!(decisions[0], ReviewTriggerDecision::Request { .. }),
            "{decisions:?}"
        );
    }

    #[test]
    fn the_per_pull_request_cap_skips_once_it_is_reached() {
        let policy = enabled(Vec::new());
        let mut spend = BTreeMap::new();
        spend.insert(
            "code".to_string(),
            ReviewSpend {
                requested_count: 8,
                last_requested_ms: Some(0),
                last_requested_commit: Some("old".to_string()),
                last_requested_base: None,
            },
        );
        let decisions = schedule(&policy, &one("code"), &spend, "new", None, 10_000_000);
        assert!(
            matches!(decisions[0], ReviewTriggerDecision::Skip { .. }),
            "{decisions:?}"
        );
    }

    #[test]
    fn always_on_new_head_overrides_the_cap_but_not_the_debounce() {
        let mut policy = enabled(Vec::new());
        policy.schedule.insert(
            "code".to_string(),
            ReviewSchedule {
                debounce_seconds: 600,
                max_per_pull_request: 1,
                always_on_new_head: true,
                freshness: ReviewFreshness::Head,
            },
        );
        let mut spend = BTreeMap::new();
        spend.insert(
            "code".to_string(),
            ReviewSpend {
                requested_count: 9,
                last_requested_ms: Some(0),
                last_requested_commit: Some("old".to_string()),
                last_requested_base: None,
            },
        );
        assert!(matches!(
            schedule(&policy, &one("code"), &spend, "new", None, 10_000_000)[0],
            ReviewTriggerDecision::Request { .. }
        ));
        // Still debounced, because the cap and the rate limit bound different
        // things: total spend versus burst.
        assert!(matches!(
            schedule(&policy, &one("code"), &spend, "new", None, 1_000)[0],
            ReviewTriggerDecision::Defer { .. }
        ));
    }

    #[test]
    fn each_type_carries_its_own_schedule() {
        let mut policy = enabled(Vec::new());
        policy.schedule.insert(
            "security".to_string(),
            ReviewSchedule {
                debounce_seconds: 0,
                max_per_pull_request: 0,
                always_on_new_head: false,
                freshness: ReviewFreshness::Head,
            },
        );
        let eligible = vec![
            EligibleReview {
                review_type: "code".to_string(),
                because: vec!["r".to_string()],
                excluded: None,
            },
            EligibleReview {
                review_type: "security".to_string(),
                because: vec!["r".to_string()],
                excluded: None,
            },
        ];
        let mut spend = BTreeMap::new();
        for review_type in ["code", "security"] {
            spend.insert(
                review_type.to_string(),
                ReviewSpend {
                    requested_count: 2,
                    last_requested_ms: Some(0),
                    last_requested_commit: Some("old".to_string()),
                    last_requested_base: None,
                },
            );
        }
        let decisions = schedule(&policy, &eligible, &spend, "new", None, 1_000);
        assert!(
            matches!(decisions[0], ReviewTriggerDecision::Defer { .. }),
            "{decisions:?}"
        );
        assert!(
            matches!(decisions[1], ReviewTriggerDecision::Request { .. }),
            "{decisions:?}"
        );
    }

    // -- policy loading ----------------------------------------------------

    fn write_config(dir: &Path, body: &str) {
        std::fs::create_dir_all(dir.join(".aethyme")).unwrap();
        std::fs::write(dir.join(".aethyme/config.toml"), body).unwrap();
    }

    #[test]
    fn a_missing_file_or_table_loads_the_disabled_default() {
        let temp = tempfile::tempdir().unwrap();
        assert!(!ReviewTriggerPolicy::load(temp.path()).unwrap().enabled);
        write_config(temp.path(), "[review]\nenabled = true\n");
        assert!(!ReviewTriggerPolicy::load(temp.path()).unwrap().enabled);
    }

    #[test]
    fn a_policy_round_trips_from_toml() {
        let temp = tempfile::tempdir().unwrap();
        write_config(
            temp.path(),
            r#"
[review.trigger]
enabled = true

[[review.trigger.rule]]
name = "auth"
require = ["security"]
paths = ["src/auth/**"]
on = ["pull_request_opened", "replacement_commit"]

[review.trigger.schedule.security]
debounce_seconds = 60
max_per_pull_request = 3
always_on_new_head = true
"#,
        );
        let policy = ReviewTriggerPolicy::load(temp.path()).unwrap();
        assert!(policy.enabled);
        assert_eq!(
            policy.rule[0].on,
            vec![
                ReviewTrigger::PullRequestOpened,
                ReviewTrigger::ReplacementCommit
            ]
        );
        assert_eq!(policy.schedule_for("security").debounce_seconds, 60);
        // An unlisted type falls back rather than inheriting security's.
        assert_eq!(policy.schedule_for("code"), &ReviewSchedule::default());
    }

    #[test]
    fn a_newer_schema_refuses_rather_than_reviewing_nothing() {
        let temp = tempfile::tempdir().unwrap();
        write_config(
            temp.path(),
            "[review.trigger]\nschema_version = 99\nenabled = true\n",
        );
        assert!(matches!(
            ReviewTriggerPolicy::load(temp.path()),
            Err(ReviewTriggerError::UnsupportedSchema { found: 99, .. })
        ));
    }

    #[test]
    fn a_rule_that_requires_nothing_is_rejected() {
        // A rule that matches but requires nothing is a rule that silently does
        // nothing -- indistinguishable, once running, from a policy that was
        // never loaded. Refuse it at load, where an operator is still looking.
        let temp = tempfile::tempdir().unwrap();
        write_config(
            temp.path(),
            "[review.trigger]\nenabled = true\n\n\
             [[review.trigger.rule]]\nname = \"empty\"\nrequire = []\n",
        );
        assert!(matches!(
            ReviewTriggerPolicy::load(temp.path()),
            Err(ReviewTriggerError::RuleRequiresNothing { index: 0, .. })
        ));
    }

    #[test]
    fn a_rule_that_omits_require_entirely_is_rejected_too() {
        // Caught by serde rather than by `validate`, because `require` has no
        // default. Same outcome, and worth pinning so a later `#[serde(default)]`
        // cannot quietly turn a missing field into a no-op rule.
        let temp = tempfile::tempdir().unwrap();
        write_config(
            temp.path(),
            "[review.trigger]\nenabled = true\n\n[[review.trigger.rule]]\nname = \"empty\"\n",
        );
        assert!(matches!(
            ReviewTriggerPolicy::load(temp.path()),
            Err(ReviewTriggerError::Parse { .. })
        ));
    }

    #[test]
    fn a_misspelled_field_is_rejected_rather_than_ignored() {
        // `deny_unknown_fields` is the difference between a typo that costs a
        // review and a typo that fails loudly at load.
        let temp = tempfile::tempdir().unwrap();
        write_config(
            temp.path(),
            "[review.trigger]\nenabled = true\n\n[[review.trigger.rule]]\n\
             require = [\"code\"]\npath = [\"src/**\"]\n",
        );
        assert!(matches!(
            ReviewTriggerPolicy::load(temp.path()),
            Err(ReviewTriggerError::Parse { .. })
        ));
    }

    #[test]
    fn a_rule_waiting_for_a_transition_no_tick_reports_is_rejected() {
        // `merge_queue_entered` parses, reads sensibly, and can never fire:
        // nothing the tick can ask the provider distinguishes it. Accepting it
        // would reproduce the exact bug this trigger vocabulary was written to
        // fix -- configuration that looks active and is dead.
        let temp = tempfile::tempdir().unwrap();
        write_config(
            temp.path(),
            r#"[review.trigger]
enabled = true

[[review.trigger.rule]]
name = "queued"
require = ["code"]
on = ["merge_queue_entered"]
"#,
        );
        assert!(matches!(
            ReviewTriggerPolicy::load(temp.path()),
            Err(ReviewTriggerError::RuleWaitsForever {
                index: 0,
                trigger: "merge_queue_entered",
                ..
            })
        ));
    }

    #[test]
    fn every_other_transition_is_still_accepted_in_a_rule() {
        // The guard above rejects by name, so it must not drift into rejecting
        // the nine triggers a tick genuinely derives.
        let temp = tempfile::tempdir().unwrap();
        write_config(
            temp.path(),
            r#"[review.trigger]
enabled = true

[[review.trigger.rule]]
name = "everything"
require = ["code"]
on = [
    "pull_request_opened",
    "additional_commit",
    "replacement_commit",
    "ready_for_review",
    "reopened",
    "base_retargeted",
    "review_dismissed",
    "scheduled",
    "manual",
]
"#,
        );
        let policy = ReviewTriggerPolicy::load(temp.path()).unwrap();
        assert_eq!(policy.rule[0].on.len(), 9);
    }

    #[test]
    fn unsolicited_is_a_ledger_trigger_but_not_a_policy_trigger() {
        assert_eq!(
            ReviewTrigger::parse("unsolicited"),
            Some(ReviewTrigger::Unsolicited)
        );
        assert_eq!(ReviewTrigger::Unsolicited.as_str(), "unsolicited");
        assert!(!ReviewTrigger::Unsolicited.is_observable());
    }
}
