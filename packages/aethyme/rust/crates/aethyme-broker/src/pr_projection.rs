//! Making Aethyme's review record visible on the pull request itself.
//!
//! A reviewer looking at a pull request should not have to ask Aethyme what
//! Aethyme thinks. The record already exists; this module turns it into the two
//! things GitHub renders natively -- one comment and some labels -- so the
//! information arrives where the decision is being made.
//!
//! Two rules keep that from becoming vandalism of someone else's pull request.
//!
//! **The broker may project what it records; it may not write what it does not
//! own.** A *recomputable* fact can be written, deleted, and rederived with no
//! loss: `area:backend` comes from a trailer that is still in the commit. A
//! *decision* has no source to re-derive from -- once written it **is** the
//! source, and a broker that rewrites it destroys the only copy. So a label a
//! human sets to mean something is [`PrProjectionPolicy::reserved`]: read,
//! never written, never removed.
//!
//! **One owned comment, edited in place.** Identified by [`COMMENT_MARKER`], an
//! HTML comment GitHub renders as nothing. Idempotent by construction: the body
//! is a pure function of the record, so a crash between deciding and writing
//! costs at most a repeated edit to identical content, which GitHub treats as a
//! no-op. Appending instead would make every crash permanently visible.
//!
//! Like [`crate::review_trigger`], nothing here performs an action. The output
//! is a list of [`PrProjectionAction`] values carrying the `gh` arguments a
//! caller passes to `broker gh`; the judgement is testable without a network.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::review_trigger::{ClassificationConflict, CommitClassification, ReviewType};

/// Current shape of the `[review.projection]` table.
pub const PR_PROJECTION_SCHEMA_VERSION: u32 = 1;

/// Marks the one comment on a pull request that Aethyme owns.
///
/// Rendered as nothing by GitHub, and stable across versions: changing it would
/// orphan every comment already written and start a second one beside it.
pub const COMMENT_MARKER: &str = "<!-- aethyme:review -->";

// ---------------------------------------------------------------------------
// What Aethyme has to say
// ---------------------------------------------------------------------------

/// Where one review dimension currently stands.
///
/// Deliberately excludes any verdict. Whether a change is *good* is the
/// reviewer's to say and is recorded by the provider; what Aethyme knows is
/// whether it asked, and what came back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectedReviewState {
    /// Requested, no answer yet.
    Requested,
    /// A reviewer is working on it.
    Running,
    /// Evidence arrived that satisfies the policy.
    Satisfied,
    /// Eligible, held back by the schedule. Will be reconsidered.
    Deferred,
    /// Eligible, deliberately not happening for this change.
    Skipped,
    /// Asked for, and the attempt failed. Distinct from `Skipped` because
    /// nobody chose this one, and a reader needs to tell those apart.
    Failed,
}

impl ProjectedReviewState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Running => "running",
            Self::Satisfied => "satisfied",
            Self::Deferred => "deferred",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }

    /// Whether this state still owes the pull request an answer.
    fn outstanding(self) -> bool {
        matches!(self, Self::Requested | Self::Running | Self::Deferred)
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Requested => "•",
            Self::Running => "…",
            Self::Satisfied => "✓",
            Self::Deferred => "⏸",
            Self::Skipped => "–",
            Self::Failed => "✗",
        }
    }
}

/// One dimension's standing, as Aethyme recorded it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectedReview {
    pub review_type: ReviewType,
    pub state: ProjectedReviewState,
    /// Why this state, in the record's own words -- a rule name, a debounce
    /// window, a provider error.
    #[serde(default)]
    pub detail: Option<String>,
}

/// Everything to be projected onto one pull request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewProjection {
    /// Head commit these reviews are bound to. Shown so a reader can see at a
    /// glance whether the summary is about the code currently on the branch.
    #[serde(default)]
    pub head: Option<String>,
    #[serde(default)]
    pub reviews: Vec<ProjectedReview>,
    #[serde(default)]
    pub classification: CommitClassification,
    #[serde(default)]
    pub conflicts: Vec<ClassificationConflict>,
}

// ---------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------

/// The `[review.projection]` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrProjectionPolicy {
    #[serde(default = "default_projection_schema_version")]
    pub schema_version: u32,
    /// Default off. A repository that has not asked for this gets a pull
    /// request byte-for-byte identical to one from a broker without it.
    #[serde(default)]
    pub enabled: bool,
    /// Maintain the owned summary comment.
    #[serde(default = "default_true")]
    pub comment: bool,
    /// Namespace every label the broker writes.
    ///
    /// This prefix is the entire safety mechanism for labels: a label outside it
    /// is somebody else's and is never touched, so the broker cannot get into a
    /// flap with CI or with a human over the same name.
    #[serde(default = "default_label_prefix")]
    pub label_prefix: String,
    /// Label declared areas (`aethyme/area:backend`).
    #[serde(default = "default_true")]
    pub label_areas: bool,
    /// Label declared surfaces (`aethyme/surface:auth`).
    #[serde(default = "default_true")]
    pub label_surfaces: bool,
    /// Label declared risk (`aethyme/risk:high`).
    #[serde(default = "default_true")]
    pub label_risk: bool,
    /// Label review dimensions still owed an answer (`aethyme/review:security`).
    #[serde(default = "default_true")]
    pub label_reviews: bool,
    /// Label suffixes under `label_prefix` that the broker reads and never
    /// writes or removes.
    ///
    /// This is where a human parks a decision inside the broker's namespace --
    /// `aethyme/skip-review` on a pull request that has been judged by hand.
    /// Without this list, reconciliation would see an unrecognised label under
    /// its own prefix and helpfully delete the only record of that judgement.
    #[serde(default)]
    pub reserved: BTreeSet<String>,
    /// Create a label that does not exist in the repository yet.
    ///
    /// On by default because `gh pr edit --add-label` fails outright on an
    /// unknown label, so off means an operator must pre-create every label the
    /// classification vocabulary can produce.
    #[serde(default = "default_true")]
    pub create_missing_labels: bool,
}

fn default_projection_schema_version() -> u32 {
    PR_PROJECTION_SCHEMA_VERSION
}

fn default_true() -> bool {
    true
}

fn default_label_prefix() -> String {
    "aethyme/".to_string()
}

impl Default for PrProjectionPolicy {
    fn default() -> Self {
        Self {
            schema_version: PR_PROJECTION_SCHEMA_VERSION,
            enabled: false,
            comment: true,
            label_prefix: default_label_prefix(),
            label_areas: true,
            label_surfaces: true,
            label_risk: true,
            label_reviews: true,
            reserved: BTreeSet::new(),
            create_missing_labels: true,
        }
    }
}

/// Why a projection policy could not be used.
#[derive(Debug, thiserror::Error)]
pub enum PrProjectionError {
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
        "{path}: review.projection schema_version {found} is newer than this broker understands \
         ({supported}); upgrade aethyme or pin the policy"
    )]
    UnsupportedSchema {
        path: String,
        found: u32,
        supported: u32,
    },
    #[error("{path}: review.projection label_prefix must not be empty")]
    EmptyLabelPrefix { path: String },
}

impl PrProjectionPolicy {
    /// Load from `.aethyme/config.toml`, defaulting to disabled when the file or
    /// the table is absent.
    pub fn load(root: &Path) -> Result<Self, PrProjectionError> {
        let path = root.join(".aethyme/config.toml");
        let display = path.display().to_string();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => {
                return Err(PrProjectionError::Read {
                    path: display,
                    source,
                });
            }
        };
        let value: toml::Value = text.parse().map_err(|source| PrProjectionError::Parse {
            path: display.clone(),
            source,
        })?;
        let Some(table) = value
            .get("review")
            .and_then(|review| review.get("projection"))
        else {
            return Ok(Self::default());
        };
        let policy: Self = table
            .clone()
            .try_into()
            .map_err(|source| PrProjectionError::Parse {
                path: display.clone(),
                source,
            })?;
        policy.validate(&display)?;
        Ok(policy)
    }

    fn validate(&self, path: &str) -> Result<(), PrProjectionError> {
        if self.schema_version > PR_PROJECTION_SCHEMA_VERSION {
            return Err(PrProjectionError::UnsupportedSchema {
                path: path.to_string(),
                found: self.schema_version,
                supported: PR_PROJECTION_SCHEMA_VERSION,
            });
        }
        // An empty prefix would make every label in the repository look like
        // one of ours, and reconciliation would remove all of them.
        if self.label_prefix.trim().is_empty() {
            return Err(PrProjectionError::EmptyLabelPrefix {
                path: path.to_string(),
            });
        }
        Ok(())
    }

    fn owns(&self, label: &str) -> bool {
        label.starts_with(&self.label_prefix)
            && !self
                .reserved
                .contains(label.trim_start_matches(&self.label_prefix as &str))
    }

    fn label(&self, kind: &str, value: &str) -> String {
        format!("{}{kind}:{value}", self.label_prefix)
    }
}

// ---------------------------------------------------------------------------
// What GitHub currently shows
// ---------------------------------------------------------------------------

/// The comment on a pull request that carries [`COMMENT_MARKER`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedComment {
    pub id: i64,
    pub body: String,
}

/// Observed pull request state, supplied by the caller from `gh`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrProjectionFacts {
    pub pull_request: i64,
    /// Labels currently on the pull request.
    pub current_labels: Vec<String>,
    /// Labels that exist in the repository at all. A desired label absent here
    /// has to be created before it can be applied.
    pub repository_labels: BTreeSet<String>,
    pub owned_comment: Option<OwnedComment>,
}

/// Find the comment Aethyme owns among a pull request's comments.
///
/// Takes `(id, body)` pairs so the caller keeps its own `gh` JSON shape. The
/// *first* match wins: if a duplicate was ever created, editing the older one
/// consistently is better than alternating between them.
pub fn find_owned_comment<'a>(
    comments: impl IntoIterator<Item = (i64, &'a str)>,
) -> Option<OwnedComment> {
    comments
        .into_iter()
        .find(|(_, body)| body.contains(COMMENT_MARKER))
        .map(|(id, body)| OwnedComment {
            id,
            body: body.to_string(),
        })
}

// ---------------------------------------------------------------------------
// Decisions
// ---------------------------------------------------------------------------

/// One mutation to make on a pull request.
///
/// A value, not an effect. [`Self::gh_args`] renders the arguments to pass
/// after `--` to `aethyme broker gh`, which is the only path allowed to perform
/// them; [`Self::reason`] renders that command's required authorization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum PrProjectionAction {
    /// No owned comment exists yet.
    CreateComment {
        body: String,
    },
    /// One exists and its content has drifted from the record.
    UpdateComment {
        comment_id: i64,
        body: String,
    },
    /// A desired label the repository has never heard of.
    CreateLabel {
        name: String,
        color: String,
        description: String,
    },
    AddLabels {
        names: Vec<String>,
    },
    /// Ours, under our prefix, no longer derivable from the record.
    RemoveLabels {
        names: Vec<String>,
    },
}

impl PrProjectionAction {
    /// Arguments for `aethyme broker gh --repo <owner/name> -- <these>`.
    ///
    /// The repository is never named here. It is asserted once, on the outer
    /// broker command, and a second target inside the arguments is refused --
    /// so `gh api` endpoints use the `{owner}`/`{repo}` placeholders the broker
    /// resolves.
    pub fn gh_args(&self, pull_request: i64) -> Vec<String> {
        let pr = pull_request.to_string();
        match self {
            Self::CreateComment { body } => {
                vec![
                    "pr".into(),
                    "comment".into(),
                    pr,
                    "--body".into(),
                    body.clone(),
                ]
            }
            Self::UpdateComment { comment_id, body } => vec![
                "api".into(),
                "--method".into(),
                "PATCH".into(),
                format!("repos/{{owner}}/{{repo}}/issues/comments/{comment_id}"),
                "-f".into(),
                format!("body={body}"),
            ],
            Self::CreateLabel {
                name,
                color,
                description,
            } => vec![
                "label".into(),
                "create".into(),
                name.clone(),
                "--color".into(),
                color.clone(),
                "--description".into(),
                description.clone(),
            ],
            // One `gh` call for the whole set: each call is a coordinated
            // operation that takes the repository write lock, and a pull request
            // with six labels should not queue six times.
            Self::AddLabels { names } => {
                vec![
                    "pr".into(),
                    "edit".into(),
                    pr,
                    "--add-label".into(),
                    names.join(","),
                ]
            }
            Self::RemoveLabels { names } => {
                vec![
                    "pr".into(),
                    "edit".into(),
                    pr,
                    "--remove-label".into(),
                    names.join(","),
                ]
            }
        }
    }

    /// The `--reason` for the coordinated write.
    pub fn reason(&self, pull_request: i64) -> String {
        match self {
            Self::CreateComment { .. } | Self::UpdateComment { .. } => {
                format!("project aethyme review record onto pull request #{pull_request}")
            }
            Self::CreateLabel { name, .. } => {
                format!("create namespaced review label {name}")
            }
            Self::AddLabels { .. } | Self::RemoveLabels { .. } => {
                format!("reconcile aethyme review labels on pull request #{pull_request}")
            }
        }
    }
}

/// Colour for a label kind. Fixed per kind rather than hashed, so the palette
/// stays legible and a label's colour never changes under an operator.
fn label_color(kind: &str, value: &str) -> &'static str {
    match (kind, value) {
        ("risk", "critical" | "high") => "b60205",
        ("risk", "low" | "none") => "c2e0c6",
        ("risk", _) => "fbca04",
        ("area", _) => "0e8a16",
        ("surface", _) => "1d76db",
        ("review", _) => "5319e7",
        _ => "ededed",
    }
}

fn label_description(kind: &str, value: &str) -> String {
    match kind {
        "area" => format!("Declared area: {value}"),
        "surface" => format!("Declared surface: {value}"),
        "risk" => format!("Declared risk: {value}"),
        "review" => format!("Review outstanding: {value}"),
        _ => format!("{kind}: {value}"),
    }
}

/// Labels the record implies, each with the kind it came from.
fn desired_labels(
    policy: &PrProjectionPolicy,
    projection: &ReviewProjection,
) -> BTreeMap<String, (&'static str, String)> {
    let mut desired = BTreeMap::new();
    let mut add = |kind: &'static str, value: &str| {
        desired.insert(policy.label(kind, value), (kind, value.to_string()));
    };
    if policy.label_areas {
        for area in &projection.classification.areas {
            add("area", area);
        }
    }
    if policy.label_surfaces {
        for surface in &projection.classification.surfaces {
            add("surface", surface);
        }
    }
    if policy.label_risk
        && let Some(risk) = &projection.classification.risk
    {
        add("risk", risk);
    }
    if policy.label_reviews {
        for review in &projection.reviews {
            if review.state.outstanding() {
                add("review", &review.review_type);
            }
        }
    }
    desired
}

/// Everything to do to bring one pull request in line with the record.
///
/// Empty when there is nothing to change, which is the steady state: running
/// this on every tick of an unchanged pull request must cost zero writes.
pub fn project(
    policy: &PrProjectionPolicy,
    projection: &ReviewProjection,
    facts: &PrProjectionFacts,
) -> Vec<PrProjectionAction> {
    if !policy.enabled {
        return Vec::new();
    }
    let mut actions = Vec::new();

    if policy.comment {
        let body = render_comment(projection);
        match &facts.owned_comment {
            // Byte-identical is the common case and must not produce a write:
            // an edit posts a "edited" event that people receive as activity.
            Some(existing) if existing.body.trim() == body.trim() => {}
            Some(existing) => actions.push(PrProjectionAction::UpdateComment {
                comment_id: existing.id,
                body,
            }),
            None => actions.push(PrProjectionAction::CreateComment { body }),
        }
    }

    let desired = desired_labels(policy, projection);
    let current: BTreeSet<&str> = facts.current_labels.iter().map(String::as_str).collect();

    if policy.create_missing_labels {
        for (name, (kind, value)) in &desired {
            if !facts.repository_labels.contains(name) {
                actions.push(PrProjectionAction::CreateLabel {
                    name: name.clone(),
                    color: label_color(kind, value).to_string(),
                    description: label_description(kind, value),
                });
            }
        }
    }

    let to_add: Vec<String> = desired
        .keys()
        .filter(|name| !current.contains(name.as_str()))
        // A label we cannot create and the repository does not have would make
        // `gh pr edit` fail for the whole set, taking the applicable labels with
        // it. Drop it instead and let the rest land.
        .filter(|name| policy.create_missing_labels || facts.repository_labels.contains(*name))
        .cloned()
        .collect();
    if !to_add.is_empty() {
        actions.push(PrProjectionAction::AddLabels { names: to_add });
    }

    // Only ours, only under our prefix, never a reserved one.
    let to_remove: Vec<String> = facts
        .current_labels
        .iter()
        .filter(|name| policy.owns(name))
        .filter(|name| !desired.contains_key(*name))
        .cloned()
        .collect();
    if !to_remove.is_empty() {
        actions.push(PrProjectionAction::RemoveLabels { names: to_remove });
    }

    actions
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// The body of the owned comment.
///
/// A pure function of the record, with everything ordered deterministically, so
/// that re-rendering an unchanged record produces an identical string and
/// [`project`] emits no write. Sorting is not cosmetic here -- it is what makes
/// the idempotence claim true.
pub fn render_comment(projection: &ReviewProjection) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{COMMENT_MARKER}");
    let _ = writeln!(out, "### Aethyme review");
    let _ = writeln!(out);

    if projection.reviews.is_empty() {
        let _ = writeln!(out, "No review dimensions apply to this change.");
    } else {
        for review in &projection.reviews {
            let _ = write!(
                out,
                "- {} **{}** — {}",
                review.state.icon(),
                review.review_type,
                review.state.as_str()
            );
            match &review.detail {
                Some(detail) => {
                    let _ = writeln!(out, " ({detail})");
                }
                None => {
                    let _ = writeln!(out);
                }
            }
        }
    }

    let classification = &projection.classification;
    if !classification.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "**Declared by the author**");
        for (label, values) in [
            ("Area", &classification.areas),
            ("Surface", &classification.surfaces),
        ] {
            if !values.is_empty() {
                let joined = values.iter().cloned().collect::<Vec<_>>().join(", ");
                let _ = writeln!(out, "- {label}: {joined}");
            }
        }
        if let Some(risk) = &classification.risk {
            let _ = writeln!(out, "- Risk: {risk}");
        }
    }

    if !projection.conflicts.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "**Declaration does not match the diff**");
        for conflict in &projection.conflicts {
            let _ = writeln!(
                out,
                "- `{}` declared, but the change touches {}",
                conflict.declared_area,
                conflict
                    .contradicted_by
                    .iter()
                    .map(|path| format!("`{path}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        // Stated explicitly because the natural reading of a mismatch is that
        // the review was skipped. It never is -- the union rule guarantees it.
        let _ = writeln!(out);
        let _ = writeln!(out, "A declaration can add a review; it never removes one.");
    }

    if let Some(head) = &projection.head {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "<sub>For commit `{head}`. Maintained by Aethyme.</sub>"
        );
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> PrProjectionPolicy {
        PrProjectionPolicy {
            enabled: true,
            ..Default::default()
        }
    }

    fn review(review_type: &str, state: ProjectedReviewState) -> ProjectedReview {
        ProjectedReview {
            review_type: review_type.to_string(),
            state,
            detail: None,
        }
    }

    fn projection() -> ReviewProjection {
        ReviewProjection {
            head: Some("abc1234".to_string()),
            reviews: vec![review("security", ProjectedReviewState::Requested)],
            classification: crate::review_trigger::parse_classification(
                "feat: x\n\nArea: backend\nSurface: auth\nRisk: high\n",
            ),
            conflicts: Vec::new(),
        }
    }

    fn facts() -> PrProjectionFacts {
        PrProjectionFacts {
            pull_request: 42,
            ..Default::default()
        }
    }

    fn names(action: &PrProjectionAction) -> Vec<String> {
        match action {
            PrProjectionAction::AddLabels { names }
            | PrProjectionAction::RemoveLabels { names } => names.clone(),
            other => panic!("not a label action: {other:?}"),
        }
    }

    // -- opt-in ------------------------------------------------------------

    #[test]
    fn a_disabled_policy_touches_nothing() {
        let mut off = policy();
        off.enabled = false;
        assert!(project(&off, &projection(), &facts()).is_empty());
    }

    // -- the owned comment -------------------------------------------------

    #[test]
    fn a_first_run_creates_the_comment_and_a_second_run_changes_nothing() {
        let policy = policy();
        let record = projection();
        let actions = project(&policy, &record, &facts());
        let body = match &actions[0] {
            PrProjectionAction::CreateComment { body } => body.clone(),
            other => panic!("expected CreateComment, got {other:?}"),
        };

        // Feed the created comment back as observed state. The steady state has
        // to cost zero writes, or every scheduler tick posts an "edited" event
        // to everyone subscribed to the pull request.
        let mut settled = facts();
        settled.owned_comment = Some(OwnedComment { id: 7, body });
        settled.current_labels = vec![
            "aethyme/area:backend".into(),
            "aethyme/surface:auth".into(),
            "aethyme/risk:high".into(),
            "aethyme/review:security".into(),
        ];
        settled.repository_labels = settled.current_labels.iter().cloned().collect();
        assert!(
            project(&policy, &record, &settled).is_empty(),
            "{:?}",
            project(&policy, &record, &settled)
        );
    }

    #[test]
    fn a_changed_record_edits_the_existing_comment_rather_than_adding_one() {
        let mut stale = facts();
        stale.owned_comment = Some(OwnedComment {
            id: 7,
            body: format!("{COMMENT_MARKER}\nold\n"),
        });
        let actions = project(&policy(), &projection(), &stale);
        assert!(matches!(
            actions[0],
            PrProjectionAction::UpdateComment { comment_id: 7, .. }
        ));
    }

    #[test]
    fn the_owned_comment_is_found_by_its_marker_and_the_first_wins() {
        let found = find_owned_comment([
            (1, "unrelated review comment"),
            (2, &format!("{COMMENT_MARKER}\nfirst")),
            (3, &format!("{COMMENT_MARKER}\nduplicate")),
        ]);
        // A duplicate is possible after a crash; editing the older one
        // consistently beats alternating between the two.
        assert_eq!(found.unwrap().id, 2);
        assert!(find_owned_comment([(1, "nothing of ours")]).is_none());
    }

    #[test]
    fn the_rendered_body_is_stable_across_renders() {
        // Sorting is load-bearing: the no-op case above is only true because
        // the same record always renders byte-identically.
        assert_eq!(render_comment(&projection()), render_comment(&projection()));
        assert!(render_comment(&projection()).starts_with(COMMENT_MARKER));
    }

    #[test]
    fn a_mismatch_is_shown_together_with_the_fact_that_it_waived_nothing() {
        let mut record = projection();
        record.conflicts = vec![ClassificationConflict {
            declared_area: "frontend".to_string(),
            contradicted_by: vec!["crates/aethyme-broker/src/lib.rs".to_string()],
        }];
        let body = render_comment(&record);
        assert!(body.contains("frontend"));
        assert!(body.contains("never removes one"), "{body}");
    }

    // -- labels ------------------------------------------------------------

    #[test]
    fn labels_are_derived_from_the_record_and_namespaced() {
        let mut known = facts();
        known.repository_labels = [
            "aethyme/area:backend",
            "aethyme/surface:auth",
            "aethyme/risk:high",
            "aethyme/review:security",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let actions = project(&policy(), &projection(), &known);
        let add = actions
            .iter()
            .find(|a| matches!(a, PrProjectionAction::AddLabels { .. }))
            .expect("labels to add");
        assert_eq!(
            names(add),
            vec![
                "aethyme/area:backend",
                "aethyme/review:security",
                "aethyme/risk:high",
                "aethyme/surface:auth",
            ]
        );
    }

    #[test]
    fn a_foreign_label_is_never_removed() {
        let mut record = projection();
        record.classification = CommitClassification::default();
        record.reviews.clear();

        let mut current = facts();
        current.current_labels = vec![
            "bug".into(),
            "needs-triage".into(),
            "codex-review".into(),
            "aethyme/area:backend".into(),
        ];
        let actions = project(&policy(), &record, &current);
        let remove = actions
            .iter()
            .find(|a| matches!(a, PrProjectionAction::RemoveLabels { .. }))
            .expect("a stale label of ours");
        // Only ours. A broker that removed `codex-review` would be in a flap
        // with whatever put it there by the next tick.
        assert_eq!(names(remove), vec!["aethyme/area:backend"]);
    }

    #[test]
    fn a_reserved_label_is_read_but_never_removed() {
        // The recomputable/decided line. `aethyme/skip-review` is a human
        // judgement with no source to re-derive it from, so the broker removing
        // it would destroy the only copy.
        let mut with_reserved = policy();
        with_reserved.reserved.insert("skip-review".to_string());

        let mut record = projection();
        record.classification = CommitClassification::default();
        record.reviews.clear();

        let mut current = facts();
        current.current_labels = vec!["aethyme/skip-review".into(), "aethyme/area:backend".into()];
        let actions = project(&with_reserved, &record, &current);
        let remove = actions
            .iter()
            .find(|a| matches!(a, PrProjectionAction::RemoveLabels { .. }))
            .expect("a stale label of ours");
        assert_eq!(names(remove), vec!["aethyme/area:backend"]);
    }

    #[test]
    fn a_label_the_repository_lacks_is_created_first() {
        let actions = project(&policy(), &projection(), &facts());
        let created: Vec<&str> = actions
            .iter()
            .filter_map(|a| match a {
                PrProjectionAction::CreateLabel { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(created.len(), 4, "{actions:?}");
        // Creation must precede the edit that applies them.
        let create_at = actions
            .iter()
            .position(|a| matches!(a, PrProjectionAction::CreateLabel { .. }))
            .unwrap();
        let add_at = actions
            .iter()
            .position(|a| matches!(a, PrProjectionAction::AddLabels { .. }))
            .unwrap();
        assert!(create_at < add_at);
    }

    #[test]
    fn without_creation_an_unknown_label_is_dropped_rather_than_failing_the_set() {
        // `gh pr edit --add-label` rejects the whole call on one unknown label,
        // so including it would lose the applicable ones too.
        let mut no_create = policy();
        no_create.create_missing_labels = false;
        let mut known = facts();
        known.repository_labels = ["aethyme/risk:high"]
            .into_iter()
            .map(String::from)
            .collect();
        let actions = project(&no_create, &projection(), &known);
        let add = actions
            .iter()
            .find(|a| matches!(a, PrProjectionAction::AddLabels { .. }))
            .expect("the one known label");
        assert_eq!(names(add), vec!["aethyme/risk:high"]);
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, PrProjectionAction::CreateLabel { .. }))
        );
    }

    #[test]
    fn a_satisfied_review_drops_its_outstanding_label() {
        let mut record = projection();
        record.reviews = vec![review("security", ProjectedReviewState::Satisfied)];
        let mut current = facts();
        current.current_labels = vec!["aethyme/review:security".into()];
        let actions = project(&policy(), &record, &current);
        let remove = actions
            .iter()
            .find(|a| matches!(a, PrProjectionAction::RemoveLabels { .. }))
            .expect("the outstanding marker to go");
        assert_eq!(names(remove), vec!["aethyme/review:security"]);
    }

    // -- transport ---------------------------------------------------------

    #[test]
    fn gh_arguments_never_name_the_repository() {
        // `broker gh --repo owner/name` asserts it once and refuses a second
        // target inside the arguments; `gh api` uses placeholders it resolves.
        let actions = [
            PrProjectionAction::CreateComment { body: "b".into() },
            PrProjectionAction::UpdateComment {
                comment_id: 9,
                body: "b".into(),
            },
            PrProjectionAction::AddLabels {
                names: vec!["aethyme/area:backend".into()],
            },
            PrProjectionAction::RemoveLabels {
                names: vec!["aethyme/area:backend".into()],
            },
            PrProjectionAction::CreateLabel {
                name: "aethyme/area:backend".into(),
                color: "0e8a16".into(),
                description: "d".into(),
            },
        ];
        for action in &actions {
            let args = action.gh_args(42);
            assert!(
                !args.iter().any(|arg| arg == "--repo" || arg == "-R"),
                "{action:?} names a repository: {args:?}"
            );
            assert!(!action.reason(42).is_empty());
        }
        assert_eq!(
            actions[1].gh_args(42)[3],
            "repos/{owner}/{repo}/issues/comments/9"
        );
    }

    #[test]
    fn a_label_set_is_one_call_not_one_call_per_label() {
        // Each call takes the repository write lock; six labels must not queue
        // six times.
        let action = PrProjectionAction::AddLabels {
            names: vec!["aethyme/area:backend".into(), "aethyme/surface:auth".into()],
        };
        let args = action.gh_args(42);
        assert_eq!(
            args,
            vec![
                "pr",
                "edit",
                "42",
                "--add-label",
                "aethyme/area:backend,aethyme/surface:auth"
            ]
        );
    }

    // -- policy loading ----------------------------------------------------

    fn write_config(dir: &Path, body: &str) {
        std::fs::create_dir_all(dir.join(".aethyme")).unwrap();
        std::fs::write(dir.join(".aethyme/config.toml"), body).unwrap();
    }

    #[test]
    fn a_missing_table_loads_the_disabled_default() {
        let temp = tempfile::tempdir().unwrap();
        assert!(!PrProjectionPolicy::load(temp.path()).unwrap().enabled);
        write_config(temp.path(), "[review]\nenabled = true\n");
        assert!(!PrProjectionPolicy::load(temp.path()).unwrap().enabled);
    }

    #[test]
    fn a_policy_round_trips_from_toml() {
        let temp = tempfile::tempdir().unwrap();
        write_config(
            temp.path(),
            "[review.projection]\nenabled = true\nlabel_prefix = \"review/\"\n\
             label_risk = false\nreserved = [\"skip-review\"]\n",
        );
        let policy = PrProjectionPolicy::load(temp.path()).unwrap();
        assert!(policy.enabled && !policy.label_risk && policy.comment);
        assert_eq!(policy.label("area", "backend"), "review/area:backend");
        assert!(policy.reserved.contains("skip-review"));
    }

    #[test]
    fn an_empty_label_prefix_is_refused() {
        // Every label in the repository would look like ours, and reconciliation
        // would remove all of them.
        let temp = tempfile::tempdir().unwrap();
        write_config(
            temp.path(),
            "[review.projection]\nenabled = true\nlabel_prefix = \"\"\n",
        );
        assert!(matches!(
            PrProjectionPolicy::load(temp.path()),
            Err(PrProjectionError::EmptyLabelPrefix { .. })
        ));
    }

    #[test]
    fn a_newer_schema_refuses() {
        let temp = tempfile::tempdir().unwrap();
        write_config(
            temp.path(),
            "[review.projection]\nschema_version = 99\nenabled = true\n",
        );
        assert!(matches!(
            PrProjectionPolicy::load(temp.path()),
            Err(PrProjectionError::UnsupportedSchema { found: 99, .. })
        ));
    }
}
