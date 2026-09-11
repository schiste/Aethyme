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

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::chau7_tabs::{Chau7Tab, workspace_tab_ids};
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
            max_concurrent: default_max_concurrent(),
            stale_after_minutes: default_stale_after_minutes(),
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
        }
        Ok(())
    }

    pub fn route_for(&self, review_type: &str) -> &ReviewRoute {
        self.route.get(review_type).unwrap_or(&self.default_route)
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
pub fn dispatch_review(
    policy: &ReviewRoutingPolicy,
    repo_root: &Path,
    review_type: &str,
    pull_request: i64,
    head: &str,
    tabs: &[Chau7Tab],
    in_flight: &[InFlightReview],
) -> ReviewDispatchAction {
    if !policy.enabled {
        return ReviewDispatchAction::RecordOnly {
            review_type: review_type.to_string(),
            why: "review routing is not enabled for this repository".into(),
        };
    }
    let route = policy.route_for(review_type);
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

            // Already running. Checked before the slot count, so a repeated
            // tick reports the truth rather than "no slots" -- an operator
            // reading the second would go looking for a capacity problem.
            if !workspace_tab_ids(tabs, &workspace).is_empty() {
                return ReviewDispatchAction::Defer {
                    review_type: review_type.to_string(),
                    why: format!("a review is already running in {workspace}"),
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
                    pull_request,
                    head,
                    route.instructions.as_deref(),
                ),
                workspace,
            }
        }
    }
}

/// The prompt a spawned reviewer receives.
///
/// Bounded on purpose. An agent told to "review this" reads the whole
/// repository; an agent told which pull request, which dimension, and to report
/// on the pull request reads the diff. The instruction not to push is not
/// decoration -- a reviewer with a checkout can commit, and a review that
/// edited the code under review is no longer a review.
pub fn review_prompt(
    review_type: &str,
    pull_request: i64,
    head: &str,
    instructions: Option<&str>,
) -> String {
    let mut prompt = format!(
        "Review pull request #{pull_request} (head `{head}`) for **{review_type}**.\n\n\
         - Read the diff with `gh pr diff {pull_request}`; the checkout in this \
           directory is at that head.\n\
         - Limit the review to {review_type}. Another reviewer covers the rest.\n\
         - Report findings as a review on the pull request, most severe first. \
           Say so explicitly if you find nothing.\n\
         - Do not commit, push, or edit the branch under review.\n"
    );
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
        dispatch_review(
            policy,
            Path::new("/repo"),
            "security",
            42,
            "abc123",
            tabs,
            in_flight,
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
            ReviewDispatchAction::Defer { why, .. } => assert!(why.contains("already running")),
            other => panic!("expected a defer, got {other:?}"),
        }
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
    fn an_already_running_review_is_reported_as_such_and_not_as_a_capacity_problem() {
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
            ReviewDispatchAction::Defer { why, .. } => assert!(why.contains("already running")),
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
