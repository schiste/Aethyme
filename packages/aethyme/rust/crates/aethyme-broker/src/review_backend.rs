//! Routing a requested review to whatever will actually perform it.
//!
//! [`crate::review_trigger`] decides that a review is owed. It says nothing
//! about who does it, and that is the part a repository most wants to choose:
//! the same `security` dimension might be a Chau7 agent here, a mention of a
//! provider bot there, and on a repository with a thorough CI pipeline nothing
//! at all -- recorded, and left to the checks that already run.
//!
//! Three backends, one router:
//!
//! - [`ReviewBackend::Chau7`] spawns a terminal agent in a workspace of its own.
//! - [`ReviewBackend::ProviderComment`] mentions a bot on the pull request.
//! - [`ReviewBackend::Record`] writes the record and stops.
//!
//! `Record` is not a null option. A missed review is survivable precisely
//! because CI still runs, so a repository that wants the record and the labels
//! without paying for a second reviewer can have exactly that, and every other
//! part of this system behaves identically.
//!
//! **Concurrency is bounded here rather than by the backend.** A pull request
//! that opens with four eligible dimensions would otherwise spawn four agents
//! at once, and a busy afternoon would spawn as many as the triggers produced.
//! [`ReviewRoute::max_concurrent`] is a slot count, checked against observed
//! state rather than a counter, so a crashed dispatcher cannot leak slots: the
//! tabs either exist or they do not.
//!
//! As elsewhere in this area, nothing here performs anything. The output is a
//! [`ReviewDispatchAction`] an adapter executes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::chau7_tabs::{Chau7Tab, workspace_tab_ids};
use crate::review_execution::Chau7Teardown;
use crate::review_ledger::{RefusalClass, ReviewRequest};
use crate::review_report::ReviewReportingPolicy;
use crate::review_trigger::ReviewType;

/// Current shape of the `[review.routing]` table.
pub const REVIEW_ROUTING_SCHEMA_VERSION: u32 = 1;

/// Who performs a review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewBackend {
    /// Spawn a Chau7 terminal agent in its own workspace.
    Chau7,
    /// Mention a review bot on the pull request and let it answer there.
    ProviderComment,
    /// Record the request and perform nothing.
    Record,
}

/// How one review type is performed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewRoute {
    pub backend: ReviewBackend,
    /// Who to mention, for [`ReviewBackend::ProviderComment`]. Stored without
    /// the `@` so a repository can name a bot the same way `gh` prints it.
    #[serde(default)]
    pub mention: Option<String>,
    /// Extra instruction appended to the generated prompt. The generated part
    /// is not replaceable: a prompt that omitted the pull request number would
    /// send an agent to review nothing, which is a failure no operator would
    /// see until they read the transcript.
    #[serde(default)]
    pub instructions: Option<String>,
    /// Reviews of this type that may be in flight at once across the
    /// repository. `0` means unbounded.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: u32,
    /// Where this review goes when its own backend refuses, keyed by why.
    ///
    /// Empty by default, and that is the whole safety story: a repository that
    /// declares nothing keeps exactly today's behaviour. An edge here is an
    /// operator saying "when the provider is spent, this dimension is worth
    /// paying a local agent for" -- a sentence only they can say, because only
    /// they know what that compute costs them (#175).
    ///
    /// Consulted only for a *classified* refusal. An `unknown` refusal is a
    /// scrape the classifier could not read, and spending an agent slot on a
    /// guess is the one outcome worse than leaving the gate stated-but-unclear.
    #[serde(default)]
    pub on_refusal: BTreeMap<RefusalClass, ReviewFallback>,
    /// How long an unfinished review of this type may hold a slot before the
    /// router gives up on it and asks again. `0` means never.
    ///
    /// This is the companion to `max_concurrent` and exists because of it. A
    /// slot is released by whoever reports the outcome, and nothing guarantees
    /// anyone does: a Chau7 tab can be closed, an adapter can crash, a review
    /// bot can be uninstalled mid-review. Without a window those slots are
    /// gone for good, and the repository dispatches `max_concurrent` reviews of
    /// this type and then silently stops dispatching any.
    ///
    /// Long enough that a slow review is not interrupted, short enough that a
    /// dead one is not waited on for a working day.
    #[serde(default = "default_stale_after_minutes")]
    pub stale_after_minutes: u32,
}

fn default_max_concurrent() -> u32 {
    2
}

fn default_stale_after_minutes() -> u32 {
    6 * 60
}

impl Default for ReviewRoute {
    fn default() -> Self {
        // Recording is the only backend that cannot surprise an operator with a
        // process or a comment, so it is what an unconfigured type gets.
        Self {
            backend: ReviewBackend::Record,
            mention: None,
            instructions: None,
            on_refusal: BTreeMap::new(),
            max_concurrent: default_max_concurrent(),
            stale_after_minutes: default_stale_after_minutes(),
        }
    }
}

/// Where a review goes when its declared backend refuses.
///
/// Deliberately not a [`ReviewRoute`]: a fallback has no fallback of its own.
/// Making that a property of the type rather than a depth check means no
/// configuration can describe a chain, so nothing has to decide at runtime
/// where to cut one off. One hop is also all the problem needs -- the point is
/// an escape hatch from a spent provider, not a cascade.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewFallback {
    pub backend: ReviewBackend,
    /// Who to mention, for [`ReviewBackend::ProviderComment`].
    #[serde(default)]
    pub mention: Option<String>,
    #[serde(default)]
    pub instructions: Option<String>,
    /// Slot count for the fallback, independent of the route's own.
    ///
    /// Separate because the backends are not interchangeable in cost: two
    /// concurrent provider mentions are two comments, and two concurrent
    /// Chau7 reviews are two agents on the operator's machine. A fallback that
    /// inherited the provider's slot count would inherit a number chosen for
    /// the cheap case.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: u32,
    #[serde(default = "default_stale_after_minutes")]
    pub stale_after_minutes: u32,
}

impl ReviewFallback {
    /// The route this fallback stands in for.
    ///
    /// `on_refusal` is empty by construction, which is what stops a chain.
    fn as_route(&self) -> ReviewRoute {
        ReviewRoute {
            backend: self.backend,
            mention: self.mention.clone(),
            instructions: self.instructions.clone(),
            on_refusal: BTreeMap::new(),
            max_concurrent: self.max_concurrent,
            stale_after_minutes: self.stale_after_minutes,
        }
    }
}

/// The `[review.routing]` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewRoutingPolicy {
    #[serde(default = "default_routing_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub enabled: bool,
    /// Directory under which each review's workspace is created. Relative
    /// paths resolve against the repository root.
    #[serde(default = "default_workspace_root")]
    pub workspace_root: String,
    /// Route for a type with no entry in `route`.
    #[serde(default)]
    pub default_route: ReviewRoute,
    #[serde(default)]
    pub route: BTreeMap<ReviewType, ReviewRoute>,
}

fn default_routing_schema_version() -> u32 {
    REVIEW_ROUTING_SCHEMA_VERSION
}

fn default_workspace_root() -> String {
    ".aethyme/reviews".to_string()
}

impl Default for ReviewRoutingPolicy {
    fn default() -> Self {
        Self {
            schema_version: REVIEW_ROUTING_SCHEMA_VERSION,
            enabled: false,
            workspace_root: default_workspace_root(),
            default_route: ReviewRoute::default(),
            route: BTreeMap::new(),
        }
    }
}

/// Why a routing policy could not be used.
#[derive(Debug, thiserror::Error)]
pub enum ReviewRoutingError {
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
        "{path}: review.routing schema_version {found} is newer than this broker understands \
         ({supported}); upgrade aethyme or pin the policy"
    )]
    UnsupportedSchema {
        path: String,
        found: u32,
        supported: u32,
    },
    #[error("{path}: review.routing route {review_type:?} uses provider_comment without `mention`")]
    MentionRequired { path: String, review_type: String },
    #[error(
        "{path}: review.routing route {review_type:?} falls back to the same backend it is          escaping on {class:?}; a fallback must differ from the route that refused"
    )]
    FallbackIsNoOp {
        path: String,
        review_type: String,
        class: String,
    },
}

impl ReviewRoutingPolicy {
    /// Load from `.aethyme/config.toml`, defaulting to disabled when the file or
    /// the table is absent.
    pub fn load(root: &Path) -> Result<Self, ReviewRoutingError> {
        let path = root.join(".aethyme/config.toml");
        let display = path.display().to_string();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => {
                return Err(ReviewRoutingError::Read {
                    path: display,
                    source,
                });
            }
        };
        let value: toml::Value = text.parse().map_err(|source| ReviewRoutingError::Parse {
            path: display.clone(),
            source,
        })?;
        let Some(table) = value.get("review").and_then(|review| review.get("routing")) else {
            return Ok(Self::default());
        };
        let policy: Self =
            table
                .clone()
                .try_into()
                .map_err(|source| ReviewRoutingError::Parse {
                    path: display.clone(),
                    source,
                })?;
        policy.validate(&display)?;
        Ok(policy)
    }

    fn validate(&self, path: &str) -> Result<(), ReviewRoutingError> {
        if self.schema_version > REVIEW_ROUTING_SCHEMA_VERSION {
            return Err(ReviewRoutingError::UnsupportedSchema {
                path: path.to_string(),
                found: self.schema_version,
                supported: REVIEW_ROUTING_SCHEMA_VERSION,
            });
        }
        // A mention-less provider route would post a comment nobody is
        // listening for, and look from the outside exactly like a review that
        // was requested and never answered.
        let default_name = "default".to_string();
        let routes = std::iter::once((&default_name, &self.default_route)).chain(self.route.iter());
        for (review_type, route) in routes {
            if route.backend == ReviewBackend::ProviderComment && route.mention.is_none() {
                return Err(ReviewRoutingError::MentionRequired {
                    path: path.to_string(),
                    review_type: review_type.clone(),
                });
            }
            for (class, fallback) in &route.on_refusal {
                if fallback.backend == ReviewBackend::ProviderComment && fallback.mention.is_none()
                {
                    return Err(ReviewRoutingError::MentionRequired {
                        path: path.to_string(),
                        review_type: review_type.clone(),
                    });
                }
                // Escaping to the thing that just refused is not an escape.
                // It reads as a configured recovery and behaves as a second
                // refusal, which is worse than declaring nothing: the operator
                // believes the dimension has an exit.
                //
                // The mention is part of the identity, so provider A falling
                // back to provider B is a real edge and is allowed.
                if fallback.backend == route.backend && fallback.mention == route.mention {
                    return Err(ReviewRoutingError::FallbackIsNoOp {
                        path: path.to_string(),
                        review_type: review_type.clone(),
                        class: class.label().to_string(),
                    });
                }
            }
        }
        Ok(())
    }

    pub fn route_for(&self, review_type: &str) -> &ReviewRoute {
        self.route.get(review_type).unwrap_or(&self.default_route)
    }

    /// The declared escape for `review_type` when its backend refused with
    /// `class`, if the repository declared one.
    pub fn fallback_for(&self, review_type: &str, class: RefusalClass) -> Option<&ReviewFallback> {
        self.route_for(review_type).on_refusal.get(&class)
    }

    /// The route that should actually perform this review.
    ///
    /// `refused` is what the *previous* attempt at this exact dimension came
    /// back with, read from the ledger rather than remembered: a refusal is
    /// already a durable row, so a dispatcher that restarts between the
    /// refusal and the retry still takes the same edge (#175).
    ///
    /// Owned rather than borrowed because a fallback is not a `ReviewRoute` in
    /// the policy -- there is no `&ReviewRoute` to hand back.
    pub fn effective_route(&self, review_type: &str, refused: Option<RefusalClass>) -> ReviewRoute {
        let route = self.route_for(review_type);
        match refused.and_then(|class| route.on_refusal.get(&class)) {
            Some(fallback) => fallback.as_route(),
            None => route.clone(),
        }
    }

    /// Where a Chau7 review of `review_type` on `pull_request` runs.
    ///
    /// Derived, never stored. The workspace path *is* the identity of an
    /// in-flight review: a tab sitting in it means that review is already
    /// running, so nothing has to be persisted for the check to be correct
    /// after a restart.
    pub fn workspace(&self, repo_root: &Path, pull_request: i64, review_type: &str) -> String {
        let root = Path::new(&self.workspace_root);
        let base = if root.is_absolute() {
            root.to_path_buf()
        } else {
            repo_root.join(&self.workspace_root)
        };
        base.join(format!("pr-{pull_request}"))
            .join(sanitize_segment(review_type))
            .display()
            .to_string()
    }
}

/// Keep a review type usable as one path segment.
///
/// A type is free-form because the dimension set is repository policy, and a
/// repository that writes `security/authn` must not thereby get a nested
/// directory that no longer matches what the identity check looks for.
fn sanitize_segment(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Decisions
// ---------------------------------------------------------------------------

/// One review already in flight, as observed rather than as recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InFlightReview {
    pub review_type: ReviewType,
    pub pull_request: i64,
}

/// What to do about one requested review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ReviewDispatchAction {
    /// Start a Chau7 tab in `workspace` and send it `prompt`.
    ///
    /// The caller is responsible for `workspace` existing and holding a
    /// checkout of the pull request's head; this decides only that it should.
    SpawnChau7Review {
        review_type: ReviewType,
        pull_request: i64,
        workspace: String,
        prompt: String,
    },
    /// Ask a bot for the review by mentioning it on the pull request.
    MentionOnPullRequest {
        review_type: ReviewType,
        pull_request: i64,
        body: String,
    },
    /// Record the request; perform nothing.
    RecordOnly {
        review_type: ReviewType,
        why: String,
    },
    /// Cannot start now. The next tick reconsiders.
    Defer {
        review_type: ReviewType,
        why: String,
    },
}

impl ReviewDispatchAction {
    pub fn review_type(&self) -> &str {
        match self {
            Self::SpawnChau7Review { review_type, .. }
            | Self::MentionOnPullRequest { review_type, .. }
            | Self::RecordOnly { review_type, .. }
            | Self::Defer { review_type, .. } => review_type,
        }
    }

    /// Arguments for `aethyme broker gh --repo <owner/name> -- <these>`, for
    /// the one variant that talks to GitHub.
    pub fn gh_args(&self) -> Option<Vec<String>> {
        match self {
            Self::MentionOnPullRequest {
                pull_request, body, ..
            } => Some(vec![
                "pr".into(),
                "comment".into(),
                pull_request.to_string(),
                "--body".into(),
                body.clone(),
            ]),
            _ => None,
        }
    }
}

/// Route one requested review.
///
/// `tabs` is the live Chau7 snapshot and `in_flight` the reviews already
/// running, both observed. Deriving the slot count from observation rather than
/// from a stored counter is what makes this safe to run after a crash: a
/// dispatcher that died between deciding and spawning leaves no phantom slot.
///
/// `refused` is how the previous attempt at this dimension ended, and selects
/// a declared `on_refusal` edge when there is one. `None` is both "first
/// attempt" and "the last one did not refuse", which route identically -- the
/// edge exists to escape a refusal, so nothing else may take it.
pub fn dispatch_review(
    policy: &ReviewRoutingPolicy,
    reporting: &ReviewReportingPolicy,
    repo_root: &Path,
    repository: &str,
    review_type: &str,
    pull_request: i64,
    head: &str,
    tabs: &[Chau7Tab],
    in_flight: &[InFlightReview],
    refused: Option<RefusalClass>,
) -> ReviewDispatchAction {
    if !policy.enabled {
        return ReviewDispatchAction::RecordOnly {
            review_type: review_type.to_string(),
            why: "review routing is not enabled for this repository".into(),
        };
    }
    let route = &policy.effective_route(review_type, refused);
    match route.backend {
        ReviewBackend::Record => ReviewDispatchAction::RecordOnly {
            review_type: review_type.to_string(),
            why: "routed to record only; CI carries the check".into(),
        },
        ReviewBackend::ProviderComment => {
            let mention = route.mention.as_deref().unwrap_or_default();
            ReviewDispatchAction::MentionOnPullRequest {
                review_type: review_type.to_string(),
                pull_request,
                body: format!(
                    "@{mention} please review this pull request for **{review_type}**.\n\n\
                     Requested by Aethyme for commit `{head}`."
                ),
            }
        }
        ReviewBackend::Chau7 => {
            let workspace = policy.workspace(repo_root, pull_request, review_type);

            // The workspace is occupied. Checked before the slot count, so a
            // repeated tick reports the truth rather than "no slots" -- an
            // operator reading the second would go looking for a capacity
            // problem.
            //
            // A tab is evidence of occupancy and nothing more. It is not
            // evidence that a review is in progress: the reviewer's shell is
            // interactive, so a finished reviewer sits at its prompt looking
            // exactly like a working one, and this message said "a review is
            // already running" about both until 2026-09-12. The ledger is what
            // knows which -- and [`finished_workspaces`] is what turns the
            // finished case into a closed tab instead of a defer that never
            // ends.
            if !workspace_tab_ids(tabs, &workspace).is_empty() {
                return ReviewDispatchAction::Defer {
                    review_type: review_type.to_string(),
                    why: format!(
                        "a tab still occupies {workspace}; the next tick reclaims it if its \
                         review is settled"
                    ),
                };
            }

            if route.max_concurrent > 0 {
                let running = in_flight
                    .iter()
                    .filter(|review| review.review_type == review_type)
                    .count();
                if running >= route.max_concurrent as usize {
                    return ReviewDispatchAction::Defer {
                        review_type: review_type.to_string(),
                        why: format!(
                            "{running} of {} {review_type} reviews already in flight",
                            route.max_concurrent
                        ),
                    };
                }
            }

            ReviewDispatchAction::SpawnChau7Review {
                review_type: review_type.to_string(),
                pull_request,
                prompt: review_prompt(
                    review_type,
                    repository,
                    pull_request,
                    head,
                    reporting,
                    route.instructions.as_deref(),
                ),
                workspace,
            }
        }
    }
}

/// Reviewer workspaces this tick should reclaim.
///
/// The counterpart to [`dispatch_review`], and the answer to the question that
/// function deliberately does not ask: a tab occupies a workspace, but is
/// anyone still using it?
///
/// The signal is the ledger, not the tab. Tab status cannot answer this --
/// `running` is what Chau7 reports for a shell that is thinking and for one
/// sitting at its prompt with the review posted an hour ago, and the reviewer
/// shell is interactive, so it never exits to report anything else. So a
/// dimension is finished exactly when no row for it still
/// [`ReviewRequestState::occupies_a_slot`], which covers every way a review
/// can end: reported `satisfied` by the reviewer, `failed` by a reviewer that
/// could not conclude, or `abandoned` by the staleness sweep for one that
/// never came back.
///
/// Keyed on the dimension rather than on one row, because the workspace is
/// per pull request and per dimension and outlives any single head. A reviewer
/// working on the current head holds a live row, so its tab is never in this
/// list even though older settled rows for the same dimension are.
///
/// `rows` must already carry this tick's expiries. Reading them straight from
/// the database would make a dry run plan a teardown a real run would not, and
/// the point of the dry run is that it prints what would happen.
pub fn finished_workspaces(
    policy: &ReviewRoutingPolicy,
    repo_root: &Path,
    pull_request: i64,
    rows: &[ReviewRequest],
    tabs: &[Chau7Tab],
) -> Vec<Chau7Teardown> {
    let mut live: BTreeSet<&str> = BTreeSet::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for row in rows {
        // Only this pull request's own rows. The caller passes them already
        // scoped, but a workspace path is derived from `pull_request` below
        // and reclaiming a neighbour's tab on a mismatch would be silent.
        if row.pr_number != pull_request {
            continue;
        }
        seen.insert(row.review_type.as_str());
        if row.state.occupies_a_slot() {
            live.insert(row.review_type.as_str());
        }
    }

    let mut teardown = Vec::new();
    for review_type in seen.difference(&live) {
        // Only Chau7 routes have a workspace. A dimension re-routed to
        // `record` since its tab was spawned still has one on disk, which is
        // why the tab list decides this and the current policy does not.
        let workspace = policy.workspace(repo_root, pull_request, review_type);
        let tab_ids = workspace_tab_ids(tabs, &workspace);
        if tab_ids.is_empty() {
            continue;
        }
        teardown.push(Chau7Teardown {
            review_type: (*review_type).to_string(),
            pull_request,
            workspace,
            tab_ids,
            why: format!("every {review_type} review of #{pull_request} has settled"),
        });
    }
    teardown
}

/// The prompt a spawned reviewer receives.
///
/// Bounded on purpose. An agent told to "review this" reads the whole
/// repository; an agent told which pull request, which dimension, and to report
/// on the pull request reads the diff. The instruction not to push is not
/// decoration -- a reviewer with a checkout can commit, and a review that
/// edited the code under review is no longer a review.
///
/// Three parts, in descending order of how much a repository may change them.
/// The task is generated and fixed. The reporting half comes from
/// [`ReviewReportingPolicy`], so a repository chooses its severity ladder and
/// its posting lane but never whether findings are tagged and located at all.
/// `instructions` is free text and last, for what only this repository knows.
pub fn review_prompt(
    review_type: &str,
    repository: &str,
    pull_request: i64,
    head: &str,
    reporting: &ReviewReportingPolicy,
    instructions: Option<&str>,
) -> String {
    let mut prompt = format!(
        "Review pull request #{pull_request} in {repository} (head `{head}`) for \
         **{review_type}**.\n\n\
         - Read the diff with `gh pr diff {pull_request}`; the checkout in this \
           directory is at that head.\n\
         - Limit the review to {review_type}. Another reviewer covers the rest.\n\
         - Do not commit, push, or edit the branch under review.\n\n"
    );
    prompt.push_str(&reporting.instructions(review_type, repository, pull_request, head));
    if let Some(extra) = instructions {
        prompt.push('\n');
        prompt.push_str(extra.trim_end());
        prompt.push('\n');
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review_ledger::ReviewRequestState;

    fn tab(id: &str, cwd: &str) -> Chau7Tab {
        Chau7Tab {
            tab_id: id.into(),
            cwd: Some(cwd.into()),
            repo_root: Some(cwd.into()),
            git_branch: None,
            ai_provider: Some("claude".into()),
            status: Some("idle".into()),
            is_mcp_controlled: Some(true),
        }
    }

    fn chau7_policy() -> ReviewRoutingPolicy {
        ReviewRoutingPolicy {
            enabled: true,
            default_route: ReviewRoute {
                backend: ReviewBackend::Chau7,
                ..ReviewRoute::default()
            },
            ..Default::default()
        }
    }

    fn dispatch(
        policy: &ReviewRoutingPolicy,
        tabs: &[Chau7Tab],
        in_flight: &[InFlightReview],
    ) -> ReviewDispatchAction {
        dispatch_after(policy, tabs, in_flight, None)
    }

    fn dispatch_after(
        policy: &ReviewRoutingPolicy,
        tabs: &[Chau7Tab],
        in_flight: &[InFlightReview],
        refused: Option<RefusalClass>,
    ) -> ReviewDispatchAction {
        dispatch_review(
            policy,
            &ReviewReportingPolicy::default(),
            Path::new("/repo"),
            "o/r",
            "security",
            42,
            "abc123",
            tabs,
            in_flight,
            refused,
        )
    }

    // -- opt-in ------------------------------------------------------------

    #[test]
    fn a_disabled_policy_records_rather_than_doing_nothing() {
        // The distinction that makes disabling safe: the record still exists,
        // so turning routing on later does not reveal a gap in the history.
        let mut off = chau7_policy();
        off.enabled = false;
        assert!(matches!(
            dispatch(&off, &[], &[]),
            ReviewDispatchAction::RecordOnly { .. }
        ));
    }

    #[test]
    fn an_unconfigured_type_records_rather_than_spawning() {
        let policy = ReviewRoutingPolicy {
            enabled: true,
            ..Default::default()
        };
        assert!(matches!(
            dispatch(&policy, &[], &[]),
            ReviewDispatchAction::RecordOnly { .. }
        ));
    }

    // -- chau7 -------------------------------------------------------------

    #[test]
    fn a_chau7_route_spawns_into_a_workspace_derived_from_the_pull_request() {
        match dispatch(&chau7_policy(), &[], &[]) {
            ReviewDispatchAction::SpawnChau7Review {
                workspace, prompt, ..
            } => {
                assert_eq!(workspace, "/repo/.aethyme/reviews/pr-42/security");
                assert!(prompt.contains("#42") && prompt.contains("security"));
                // A reviewer with a checkout can commit, and a review that
                // edited the branch is no longer a review.
                assert!(prompt.contains("Do not commit"));
            }
            other => panic!("expected a spawn, got {other:?}"),
        }
    }

    #[test]
    fn a_tab_already_in_the_workspace_defers_instead_of_spawning_a_second() {
        // The workspace path is the identity of an in-flight review, so this
        // stays correct across a dispatcher restart with nothing persisted.
        let tabs = vec![tab("tab_9", "/repo/.aethyme/reviews/pr-42/security")];
        match dispatch(&chau7_policy(), &tabs, &[]) {
            ReviewDispatchAction::Defer { why, .. } => {
                // The claim is occupancy, not activity. A tab whose reviewer
                // posted an hour ago is indistinguishable from a working one
                // here, and saying "a review is already running" about the
                // first sends an operator looking for a reviewer that is not
                // there.
                assert!(why.contains("occupies"), "{why}");
                assert!(
                    !why.contains("already running"),
                    "the tab list cannot support that claim: {why}"
                );
            }
            other => panic!("expected a defer, got {other:?}"),
        }
    }

    // -- reclaiming a finished reviewer's workspace -------------------------

    fn row(id: i64, review_type: &str, head: &str, state: ReviewRequestState) -> ReviewRequest {
        ReviewRequest {
            id,
            repository: "o/r".into(),
            pr_number: 42,
            review_type: review_type.into(),
            head_commit: head.into(),
            base_commit: None,
            backend: "chau7".into(),
            state,
            detail: None,
            requested_at: 0,
            updated_at: 0,
        }
    }

    fn reclaim(rows: &[ReviewRequest], tabs: &[Chau7Tab]) -> Vec<Chau7Teardown> {
        finished_workspaces(&chau7_policy(), Path::new("/repo"), 42, rows, tabs)
    }

    /// The defect this whole path exists for. A reviewer that posted and
    /// reported `satisfied` left its shell sitting at a prompt, the workspace
    /// stayed occupied, and every later security review of #42 deferred --
    /// until `stale_after_minutes` filed the finished review as `abandoned`.
    #[test]
    fn a_settled_review_whose_tab_is_still_open_is_reclaimed() {
        let tabs = vec![tab("tab_9", "/repo/.aethyme/reviews/pr-42/security")];
        let teardown = reclaim(
            &[row(1, "security", "abc123", ReviewRequestState::Satisfied)],
            &tabs,
        );
        assert_eq!(teardown.len(), 1);
        assert_eq!(teardown[0].review_type, "security");
        assert_eq!(teardown[0].tab_ids, vec!["tab_9".to_string()]);
        assert_eq!(
            teardown[0].workspace,
            "/repo/.aethyme/reviews/pr-42/security"
        );
    }

    /// Every way a review can end, because the question is "is anyone still
    /// using this workspace" and all three answer it the same way. `Failed`
    /// and `Abandoned` especially: those are the tabs most likely to be
    /// stranded, since nothing about them was ever tidy.
    #[test]
    fn every_settled_state_releases_the_workspace() {
        for state in [
            ReviewRequestState::Satisfied,
            ReviewRequestState::Failed,
            ReviewRequestState::Recorded,
            ReviewRequestState::Abandoned,
        ] {
            let tabs = vec![tab("tab_9", "/repo/.aethyme/reviews/pr-42/security")];
            assert_eq!(
                reclaim(&[row(1, "security", "abc123", state)], &tabs).len(),
                1,
                "{} should release the workspace",
                state.label()
            );
        }
    }

    /// The check that keeps this from being a bug worse than the one it fixes:
    /// closing the tab of a reviewer that is still reading is a review
    /// destroyed with no trace, and the ledger row would still say `running`.
    #[test]
    fn a_live_review_is_never_reclaimed() {
        for state in [ReviewRequestState::Requested, ReviewRequestState::Running] {
            let tabs = vec![tab("tab_9", "/repo/.aethyme/reviews/pr-42/security")];
            assert!(
                reclaim(&[row(1, "security", "abc123", state)], &tabs).is_empty(),
                "{} must hold its workspace",
                state.label()
            );
        }
    }

    /// A workspace outlives any one head, so the question has to be asked of
    /// the dimension. Three settled rows from earlier pushes do not mean the
    /// reviewer currently reading the fourth head may be shut down.
    #[test]
    fn an_older_heads_settled_row_does_not_reclaim_a_live_reviewer() {
        let tabs = vec![tab("tab_9", "/repo/.aethyme/reviews/pr-42/security")];
        let rows = vec![
            row(1, "security", "aaa", ReviewRequestState::Satisfied),
            row(2, "security", "bbb", ReviewRequestState::Abandoned),
            row(3, "security", "ccc", ReviewRequestState::Running),
        ];
        assert!(
            reclaim(&rows, &tabs).is_empty(),
            "the running reviewer on head ccc is still using that workspace"
        );
    }

    /// Each dimension has its own workspace, so settling one says nothing
    /// about the other. Getting this wrong would close the code reviewer's tab
    /// the moment the security reviewer finished.
    #[test]
    fn one_dimension_settling_does_not_reclaim_another() {
        let tabs = vec![
            tab("tab_9", "/repo/.aethyme/reviews/pr-42/security"),
            tab("tab_10", "/repo/.aethyme/reviews/pr-42/code"),
        ];
        let rows = vec![
            row(1, "security", "abc", ReviewRequestState::Satisfied),
            row(2, "code", "abc", ReviewRequestState::Running),
        ];
        let teardown = reclaim(&rows, &tabs);
        assert_eq!(teardown.len(), 1);
        assert_eq!(teardown[0].review_type, "security");
        assert_eq!(teardown[0].tab_ids, vec!["tab_9".to_string()]);
    }

    /// Nothing to close is not a teardown. Emitting one would make every tick
    /// after a review hand the adapter a `tab_close` for a tab that is not
    /// there, and an adapter that treats that as an error would then fail
    /// forever on a pull request nobody is reviewing.
    #[test]
    fn a_settled_review_with_no_tab_plans_nothing() {
        assert!(
            reclaim(
                &[row(1, "security", "abc123", ReviewRequestState::Satisfied)],
                &[]
            )
            .is_empty()
        );
    }

    /// Two tabs in one workspace is not the expected shape, but closing only
    /// the first would leave it occupied and the defer permanent -- which is
    /// the exact failure this function exists to end.
    #[test]
    fn every_tab_in_a_reclaimed_workspace_is_closed() {
        let tabs = vec![
            tab("tab_9", "/repo/.aethyme/reviews/pr-42/security"),
            tab("tab_11", "/repo/.aethyme/reviews/pr-42/security"),
        ];
        let teardown = reclaim(
            &[row(1, "security", "abc", ReviewRequestState::Satisfied)],
            &tabs,
        );
        assert_eq!(teardown.len(), 1);
        assert_eq!(
            teardown[0].tab_ids,
            vec!["tab_9".to_string(), "tab_11".to_string()]
        );
    }

    /// A pull request's rows decide only that pull request's workspaces. The
    /// path is derived from the number passed in, so a row that belongs to a
    /// neighbour must not be allowed to name a workspace it does not own.
    #[test]
    fn another_pull_requests_row_reclaims_nothing_here() {
        let tabs = vec![tab("tab_9", "/repo/.aethyme/reviews/pr-42/security")];
        let mut stranger = row(1, "security", "abc", ReviewRequestState::Satisfied);
        stranger.pr_number = 41;
        assert!(reclaim(&[stranger], &tabs).is_empty());
    }

    #[test]
    fn a_tab_in_another_reviews_workspace_does_not_block_this_one() {
        let tabs = vec![
            tab("tab_9", "/repo/.aethyme/reviews/pr-41/security"),
            tab("tab_10", "/repo/.aethyme/reviews/pr-42/code"),
        ];
        assert!(matches!(
            dispatch(&chau7_policy(), &tabs, &[]),
            ReviewDispatchAction::SpawnChau7Review { .. }
        ));
    }

    #[test]
    fn the_slot_count_bounds_concurrent_reviews_of_one_type() {
        let in_flight = vec![
            InFlightReview {
                review_type: "security".into(),
                pull_request: 1,
            },
            InFlightReview {
                review_type: "security".into(),
                pull_request: 2,
            },
        ];
        match dispatch(&chau7_policy(), &[], &in_flight) {
            ReviewDispatchAction::Defer { why, .. } => assert!(why.contains("in flight"), "{why}"),
            other => panic!("expected a defer, got {other:?}"),
        }
    }

    #[test]
    fn another_types_reviews_do_not_consume_this_types_slots() {
        // Separate budgets, so a burst of code reviews cannot silently stop
        // security reviews from happening -- the failure #171 was about.
        let in_flight = vec![
            InFlightReview {
                review_type: "code".into(),
                pull_request: 1,
            },
            InFlightReview {
                review_type: "code".into(),
                pull_request: 2,
            },
            InFlightReview {
                review_type: "code".into(),
                pull_request: 3,
            },
        ];
        assert!(matches!(
            dispatch(&chau7_policy(), &[], &in_flight),
            ReviewDispatchAction::SpawnChau7Review { .. }
        ));
    }

    #[test]
    fn an_occupied_workspace_is_reported_as_such_and_not_as_a_capacity_problem() {
        // Both conditions hold at once. Reporting "no slots" would send an
        // operator looking for capacity they already have.
        let tabs = vec![tab("tab_9", "/repo/.aethyme/reviews/pr-42/security")];
        let in_flight = vec![
            InFlightReview {
                review_type: "security".into(),
                pull_request: 1,
            },
            InFlightReview {
                review_type: "security".into(),
                pull_request: 2,
            },
        ];
        match dispatch(&chau7_policy(), &tabs, &in_flight) {
            ReviewDispatchAction::Defer { why, .. } => {
                assert!(why.contains("occupies"), "{why}");
                assert!(!why.contains("in flight"), "{why}");
            }
            other => panic!("expected a defer, got {other:?}"),
        }
    }

    #[test]
    fn max_concurrent_zero_is_unbounded() {
        let mut policy = chau7_policy();
        policy.default_route.max_concurrent = 0;
        let in_flight: Vec<InFlightReview> = (0..50)
            .map(|n| InFlightReview {
                review_type: "security".into(),
                pull_request: n,
            })
            .collect();
        assert!(matches!(
            dispatch(&policy, &[], &in_flight),
            ReviewDispatchAction::SpawnChau7Review { .. }
        ));
    }

    #[test]
    fn a_review_type_with_a_slash_stays_one_path_segment() {
        // Types are free-form, and a nested directory would no longer match the
        // path the in-flight check looks for.
        let policy = chau7_policy();
        assert_eq!(
            policy.workspace(Path::new("/repo"), 42, "security/authn"),
            "/repo/.aethyme/reviews/pr-42/security-authn"
        );
    }

    #[test]
    fn an_absolute_workspace_root_is_not_joined_to_the_repository() {
        let mut policy = chau7_policy();
        policy.workspace_root = "/var/reviews".into();
        assert_eq!(
            policy.workspace(Path::new("/repo"), 42, "security"),
            "/var/reviews/pr-42/security"
        );
    }

    #[test]
    fn route_instructions_are_appended_and_never_replace_the_generated_prompt() {
        let mut policy = chau7_policy();
        policy.default_route.instructions = Some("Focus on the token cache.".into());
        match dispatch(&policy, &[], &[]) {
            ReviewDispatchAction::SpawnChau7Review { prompt, .. } => {
                assert!(prompt.contains("#42"), "generated part survived");
                assert!(prompt.contains("Focus on the token cache."));
            }
            other => panic!("expected a spawn, got {other:?}"),
        }
    }

    // -- provider comment --------------------------------------------------

    #[test]
    fn a_provider_route_mentions_the_bot_and_names_the_commit() {
        let mut policy = chau7_policy();
        policy.default_route = ReviewRoute {
            backend: ReviewBackend::ProviderComment,
            mention: Some("codex".into()),
            ..ReviewRoute::default()
        };
        match dispatch(&policy, &[], &[]) {
            ReviewDispatchAction::MentionOnPullRequest { ref body, .. } => {
                assert!(body.starts_with("@codex"));
                assert!(body.contains("abc123"));
            }
            other => panic!("expected a mention, got {other:?}"),
        }
        let args = dispatch(&policy, &[], &[]).gh_args().expect("a gh command");
        assert_eq!(
            args[0..3],
            ["pr".to_string(), "comment".to_string(), "42".to_string()]
        );
        assert!(!args.iter().any(|arg| arg == "--repo" || arg == "-R"));
    }

    #[test]
    fn only_the_provider_route_produces_a_gh_command() {
        assert!(dispatch(&chau7_policy(), &[], &[]).gh_args().is_none());
    }

    // -- policy loading ----------------------------------------------------

    fn write_config(dir: &Path, body: &str) {
        std::fs::create_dir_all(dir.join(".aethyme")).unwrap();
        std::fs::write(dir.join(".aethyme/config.toml"), body).unwrap();
    }

    #[test]
    fn a_missing_table_loads_the_disabled_default() {
        let temp = tempfile::tempdir().unwrap();
        assert!(!ReviewRoutingPolicy::load(temp.path()).unwrap().enabled);
    }

    #[test]
    fn a_policy_round_trips_from_toml() {
        let temp = tempfile::tempdir().unwrap();
        write_config(
            temp.path(),
            r#"
[review.routing]
enabled = true

[review.routing.default_route]
backend = "record"

[review.routing.route.security]
backend = "chau7"
max_concurrent = 1

[review.routing.route.code]
backend = "provider_comment"
mention = "codex"
"#,
        );
        let policy = ReviewRoutingPolicy::load(temp.path()).unwrap();
        assert_eq!(policy.route_for("security").backend, ReviewBackend::Chau7);
        assert_eq!(policy.route_for("security").max_concurrent, 1);
        assert_eq!(policy.route_for("code").mention.as_deref(), Some("codex"));
        // Types the repository never mentioned fall back rather than inheriting.
        assert_eq!(
            policy.route_for("performance").backend,
            ReviewBackend::Record
        );
    }

    #[test]
    fn a_provider_route_without_a_mention_is_refused() {
        // It would post a comment nobody is listening for, and look from
        // outside exactly like a review that was requested and never answered.
        let temp = tempfile::tempdir().unwrap();
        write_config(
            temp.path(),
            "[review.routing]\nenabled = true\n\n\
             [review.routing.route.code]\nbackend = \"provider_comment\"\n",
        );
        assert!(matches!(
            ReviewRoutingPolicy::load(temp.path()),
            Err(ReviewRoutingError::MentionRequired { ref review_type, .. }) if review_type == "code"
        ));
    }

    #[test]
    fn a_newer_schema_refuses() {
        let temp = tempfile::tempdir().unwrap();
        write_config(
            temp.path(),
            "[review.routing]\nschema_version = 99\nenabled = true\n",
        );
        assert!(matches!(
            ReviewRoutingPolicy::load(temp.path()),
            Err(ReviewRoutingError::UnsupportedSchema { found: 99, .. })
        ));
    }

    /// The shape an operator writes for #175: routine review goes to the
    /// provider, and a spent budget is the one condition that buys an agent.
    #[test]
    fn a_declared_refusal_edge_parses_and_routes() {
        let temp = tempfile::tempdir().unwrap();
        write_config(
            temp.path(),
            "[review.routing]\nenabled = true\n\n\
             [review.routing.route.code]\nbackend = \"provider_comment\"\nmention = \"codex\"\n\n\
             [review.routing.route.code.on_refusal.quota_exhausted]\nbackend = \"chau7\"\n",
        );
        let policy = ReviewRoutingPolicy::load(temp.path()).unwrap();

        // The declared backend is untouched for a first attempt.
        assert_eq!(
            policy.route_for("code").backend,
            ReviewBackend::ProviderComment
        );
        assert_eq!(
            policy.effective_route("code", None).backend,
            ReviewBackend::ProviderComment
        );
        // And the edge is taken for the class that declared it.
        assert_eq!(
            policy
                .effective_route("code", Some(RefusalClass::QuotaExhausted))
                .backend,
            ReviewBackend::Chau7
        );
    }

    /// An edge is per class. A rate limit clears itself by waiting, so a
    /// policy that bought an agent only for a spent budget must not spend one
    /// on a refusal that a retry would have fixed for free.
    #[test]
    fn an_undeclared_refusal_class_keeps_the_declared_backend() {
        let temp = tempfile::tempdir().unwrap();
        write_config(
            temp.path(),
            "[review.routing]\nenabled = true\n\n\
             [review.routing.route.code]\nbackend = \"provider_comment\"\nmention = \"codex\"\n\n\
             [review.routing.route.code.on_refusal.quota_exhausted]\nbackend = \"chau7\"\n",
        );
        let policy = ReviewRoutingPolicy::load(temp.path()).unwrap();
        for class in [
            RefusalClass::RateLimited,
            RefusalClass::ProviderError,
            RefusalClass::Unknown,
        ] {
            assert_eq!(
                policy.effective_route("code", Some(class)).backend,
                ReviewBackend::ProviderComment,
                "{class:?} was not declared and must not reroute"
            );
        }
    }

    /// Declaring nothing must change nothing. This is the whole safety
    /// argument for shipping #175 on by default: an existing repository has no
    /// `on_refusal` table, so no refusal can ever reroute it.
    #[test]
    fn a_policy_with_no_edges_routes_identically_however_it_refused() {
        let temp = tempfile::tempdir().unwrap();
        write_config(
            temp.path(),
            "[review.routing]\nenabled = true\n\n\
             [review.routing.route.code]\nbackend = \"provider_comment\"\nmention = \"codex\"\n",
        );
        let policy = ReviewRoutingPolicy::load(temp.path()).unwrap();
        for class in [
            RefusalClass::QuotaExhausted,
            RefusalClass::RateLimited,
            RefusalClass::ProviderError,
            RefusalClass::Unknown,
        ] {
            assert_eq!(
                &policy.effective_route("code", Some(class)),
                policy.route_for("code")
            );
        }
    }

    /// Escaping to the backend that just refused is not an escape. Refused at
    /// load, because the failure is otherwise invisible: the operator sees a
    /// configured recovery and gets a second refusal.
    #[test]
    fn a_fallback_to_the_refusing_backend_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        write_config(
            temp.path(),
            "[review.routing]\nenabled = true\n\n\
             [review.routing.route.code]\nbackend = \"provider_comment\"\nmention = \"codex\"\n\n\
             [review.routing.route.code.on_refusal.quota_exhausted]\n\
             backend = \"provider_comment\"\nmention = \"codex\"\n",
        );
        assert!(matches!(
            ReviewRoutingPolicy::load(temp.path()),
            Err(ReviewRoutingError::FallbackIsNoOp { .. })
        ));
    }

    /// The same backend with a *different* mention is a real edge: one bot is
    /// spent, another is not. Identity is backend plus mention, not backend.
    #[test]
    fn a_fallback_to_a_different_bot_is_allowed() {
        let temp = tempfile::tempdir().unwrap();
        write_config(
            temp.path(),
            "[review.routing]\nenabled = true\n\n\
             [review.routing.route.code]\nbackend = \"provider_comment\"\nmention = \"codex\"\n\n\
             [review.routing.route.code.on_refusal.quota_exhausted]\n\
             backend = \"provider_comment\"\nmention = \"claude\"\n",
        );
        let policy = ReviewRoutingPolicy::load(temp.path()).unwrap();
        assert_eq!(
            policy
                .effective_route("code", Some(RefusalClass::QuotaExhausted))
                .mention
                .as_deref(),
            Some("claude")
        );
    }

    /// A mention-less provider fallback posts a comment nobody is listening
    /// for -- the same defect `MentionRequired` already refuses on a route,
    /// which an edge must not be able to sneak past.
    #[test]
    fn a_mention_less_provider_fallback_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        write_config(
            temp.path(),
            "[review.routing]\nenabled = true\n\n\
             [review.routing.route.code]\nbackend = \"chau7\"\n\n\
             [review.routing.route.code.on_refusal.quota_exhausted]\n\
             backend = \"provider_comment\"\n",
        );
        assert!(matches!(
            ReviewRoutingPolicy::load(temp.path()),
            Err(ReviewRoutingError::MentionRequired { .. })
        ));
    }

    /// A fallback has no fallback: `ReviewFallback` has no `on_refusal` field,
    /// and `deny_unknown_fields` turns an attempted chain into a parse error
    /// rather than a silently ignored key.
    #[test]
    fn a_chain_of_fallbacks_cannot_be_configured() {
        let temp = tempfile::tempdir().unwrap();
        write_config(
            temp.path(),
            "[review.routing]\nenabled = true\n\n\
             [review.routing.route.code]\nbackend = \"provider_comment\"\nmention = \"codex\"\n\n\
             [review.routing.route.code.on_refusal.quota_exhausted]\nbackend = \"chau7\"\n\n\
             [review.routing.route.code.on_refusal.quota_exhausted.on_refusal.quota_exhausted]\n\
             backend = \"record\"\n",
        );
        assert!(matches!(
            ReviewRoutingPolicy::load(temp.path()),
            Err(ReviewRoutingError::Parse { .. })
        ));
    }

    /// End of the chain: the edge must actually change what gets dispatched,
    /// not merely what `effective_route` reports.
    #[test]
    fn a_quota_refusal_dispatches_the_fallback_backend() {
        let policy = ReviewRoutingPolicy {
            enabled: true,
            default_route: ReviewRoute {
                backend: ReviewBackend::ProviderComment,
                mention: Some("codex".into()),
                on_refusal: BTreeMap::from([(
                    RefusalClass::QuotaExhausted,
                    ReviewFallback {
                        backend: ReviewBackend::Chau7,
                        mention: None,
                        instructions: None,
                        max_concurrent: 1,
                        stale_after_minutes: 60,
                    },
                )]),
                ..ReviewRoute::default()
            },
            ..Default::default()
        };

        assert!(matches!(
            dispatch(&policy, &[], &[]),
            ReviewDispatchAction::MentionOnPullRequest { .. }
        ));
        assert!(matches!(
            dispatch_after(&policy, &[], &[], Some(RefusalClass::QuotaExhausted)),
            ReviewDispatchAction::SpawnChau7Review { .. }
        ));
    }

    /// The fallback's own slot count governs once the edge is taken. A route
    /// that allows two cheap provider mentions must not thereby allow two
    /// concurrent agents on the operator's machine.
    #[test]
    fn the_fallback_bounds_concurrency_with_its_own_slot_count() {
        let policy = ReviewRoutingPolicy {
            enabled: true,
            default_route: ReviewRoute {
                backend: ReviewBackend::ProviderComment,
                mention: Some("codex".into()),
                max_concurrent: 8,
                on_refusal: BTreeMap::from([(
                    RefusalClass::QuotaExhausted,
                    ReviewFallback {
                        backend: ReviewBackend::Chau7,
                        mention: None,
                        instructions: None,
                        max_concurrent: 1,
                        stale_after_minutes: 60,
                    },
                )]),
                ..ReviewRoute::default()
            },
            ..Default::default()
        };
        let running = [InFlightReview {
            review_type: "security".into(),
            pull_request: 41,
        }];
        assert!(matches!(
            dispatch_after(&policy, &[], &running, Some(RefusalClass::QuotaExhausted)),
            ReviewDispatchAction::Defer { .. }
        ));
    }

    #[test]
    fn an_unknown_backend_is_refused_rather_than_defaulted() {
        let temp = tempfile::tempdir().unwrap();
        write_config(
            temp.path(),
            "[review.routing]\nenabled = true\n\n\
             [review.routing.route.code]\nbackend = \"chau8\"\n",
        );
        assert!(matches!(
            ReviewRoutingPolicy::load(temp.path()),
            Err(ReviewRoutingError::Parse { .. })
        ));
    }
}
