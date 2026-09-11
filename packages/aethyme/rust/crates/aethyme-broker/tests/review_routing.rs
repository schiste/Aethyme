//! The three review policies composed, on one repository configuration.
//!
//! Each module is unit-tested in isolation, which is where the judgement lives.
//! What no unit test covers is the seam: eligibility feeding scheduling feeding
//! routing feeding projection, against a single `.aethyme/config.toml` written
//! the way an operator would write it. A change to any one module's shape that
//! silently stops the next one receiving anything shows up here and nowhere
//! else.

use std::collections::BTreeMap;
use std::path::Path;

use aethyme_broker::{
    ChangeFacts, Chau7Tab, CommitClassification, InFlightReview, PrProjectionAction,
    PrProjectionFacts, PrProjectionPolicy, ProjectedReview, ProjectedReviewState, ReviewBackend,
    ReviewDispatchAction, ReviewProjection, ReviewRoutingPolicy, ReviewTrigger,
    ReviewTriggerDecision, ReviewTriggerPolicy, dispatch_review, eligible_types,
    parse_classification, project, schedule,
};

/// A configuration in the shape `.aethyme/config.toml` documents: always
/// code-review, security on sensitive paths, security to a Chau7 agent, code to
/// a provider bot, and projection on.
const CONFIG: &str = r#"
[review.trigger]
enabled = true

[[review.trigger.rule]]
name = "always-code-review"
require = ["code"]

[[review.trigger.rule]]
name = "security-sensitive-paths"
require = ["security"]
paths = ["crates/*/src/operations.rs", ".github/workflows/**"]

[review.trigger.schedule.security]
debounce_seconds = 0
max_per_pull_request = 0

[review.routing]
enabled = true
workspace_root = ".aethyme/reviews"

[review.routing.default_route]
backend = "record"

[review.routing.route.security]
backend = "chau7"
max_concurrent = 1

[review.routing.route.code]
backend = "provider_comment"
mention = "codex"

[review.projection]
enabled = true
label_prefix = "aethyme/"
reserved = ["skip-review"]
"#;

struct Policies {
    trigger: ReviewTriggerPolicy,
    routing: ReviewRoutingPolicy,
    projection: PrProjectionPolicy,
    root: tempfile::TempDir,
}

fn policies() -> Policies {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".aethyme")).unwrap();
    std::fs::write(root.path().join(".aethyme/config.toml"), CONFIG).unwrap();
    Policies {
        trigger: ReviewTriggerPolicy::load(root.path()).unwrap(),
        routing: ReviewRoutingPolicy::load(root.path()).unwrap(),
        projection: PrProjectionPolicy::load(root.path()).unwrap(),
        root,
    }
}

fn facts(paths: &[&str], message: &str) -> ChangeFacts {
    ChangeFacts {
        trigger: Some(ReviewTrigger::PullRequestOpened),
        paths: paths.iter().map(|p| p.to_string()).collect(),
        classification: parse_classification(message),
        from_fork: false,
        first_time_contributor: false,
        authored_by_model: None,
    }
}

/// Run the whole chain the way a dispatcher would.
fn plan(
    policies: &Policies,
    facts: &ChangeFacts,
    head: &str,
    tabs: &[Chau7Tab],
    in_flight: &[InFlightReview],
) -> (
    Vec<ReviewTriggerDecision>,
    Vec<ReviewDispatchAction>,
    Vec<PrProjectionAction>,
) {
    let eligible = eligible_types(&policies.trigger, facts);
    let decisions = schedule(
        &policies.trigger,
        &eligible,
        &BTreeMap::new(),
        head,
        1_000_000_000_000,
    );

    let dispatch: Vec<ReviewDispatchAction> = decisions
        .iter()
        .filter_map(|decision| match decision {
            ReviewTriggerDecision::Request { review_type, .. } => Some(dispatch_review(
                &policies.routing,
                policies.root.path(),
                review_type,
                77,
                head,
                tabs,
                in_flight,
            )),
            _ => None,
        })
        .collect();

    let projection = ReviewProjection {
        head: Some(head.to_string()),
        reviews: decisions
            .iter()
            .map(|decision| ProjectedReview {
                review_type: decision.review_type().to_string(),
                state: match decision {
                    ReviewTriggerDecision::Request { .. } => ProjectedReviewState::Requested,
                    ReviewTriggerDecision::Defer { .. } => ProjectedReviewState::Deferred,
                    ReviewTriggerDecision::Skip { .. } => ProjectedReviewState::Skipped,
                },
                detail: None,
            })
            .collect(),
        classification: facts.classification.clone(),
        conflicts: Vec::new(),
    };
    let actions = project(
        &policies.projection,
        &projection,
        &PrProjectionFacts {
            pull_request: 77,
            ..Default::default()
        },
    );
    (decisions, dispatch, actions)
}

fn tab(cwd: &str) -> Chau7Tab {
    Chau7Tab {
        tab_id: "tab_1".into(),
        cwd: Some(cwd.into()),
        repo_root: Some(cwd.into()),
        git_branch: None,
        ai_provider: Some("claude".into()),
        status: Some("idle".into()),
        is_mcp_controlled: Some(true),
    }
}

#[test]
fn a_sensitive_change_reaches_both_backends_and_the_pull_request() {
    let policies = policies();
    let change = facts(
        &["crates/aethyme-broker/src/operations.rs"],
        "feat(broker): coordinated write\n\nArea: backend\nSurface: auth\nRisk: high\n",
    );
    let (decisions, dispatch, actions) = plan(&policies, &change, "abc123", &[], &[]);

    // Both dimensions fire: `code` from the unconditional rule, `security`
    // from the path rule.
    let types: Vec<&str> = decisions
        .iter()
        .map(ReviewTriggerDecision::review_type)
        .collect();
    assert_eq!(types, ["code", "security"]);
    assert!(
        decisions
            .iter()
            .all(|d| matches!(d, ReviewTriggerDecision::Request { .. }))
    );

    // Each dimension goes to the backend its route names -- the point of the
    // router being per-type rather than per-repository.
    match &dispatch[0] {
        ReviewDispatchAction::MentionOnPullRequest { body, .. } => assert!(body.contains("@codex")),
        other => panic!("code should reach the provider bot, got {other:?}"),
    }
    match &dispatch[1] {
        ReviewDispatchAction::SpawnChau7Review { workspace, .. } => {
            assert!(
                workspace.ends_with(".aethyme/reviews/pr-77/security"),
                "{workspace}"
            );
        }
        other => panic!("security should spawn an agent, got {other:?}"),
    }

    // And the record becomes visible on the pull request without anyone
    // querying Aethyme.
    let labels: Vec<String> = actions
        .iter()
        .filter_map(|action| match action {
            PrProjectionAction::AddLabels { names } => Some(names.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(
        labels,
        [
            "aethyme/area:backend",
            "aethyme/review:code",
            "aethyme/review:security",
            "aethyme/risk:high",
            "aethyme/surface:auth",
        ]
    );
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, PrProjectionAction::CreateComment { .. }))
    );
}

#[test]
fn an_ordinary_change_gets_only_the_unconditional_review() {
    let policies = policies();
    let change = facts(&["README.md"], "docs: fix a typo\n");
    let (decisions, dispatch, _) = plan(&policies, &change, "abc123", &[], &[]);
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].review_type(), "code");
    assert!(matches!(
        dispatch[0],
        ReviewDispatchAction::MentionOnPullRequest { .. }
    ));
}

#[test]
fn a_declaration_cannot_talk_a_sensitive_path_out_of_its_review() {
    // The escalate-never-waive rule, end to end: a change to a path the policy
    // guards gets the security review whatever the author declared.
    let policies = policies();
    let change = facts(
        &["crates/aethyme-broker/src/operations.rs"],
        "chore: tidy\n\nArea: docs\nRisk: none\n",
    );
    let (decisions, _, _) = plan(&policies, &change, "abc123", &[], &[]);
    assert!(decisions.iter().any(|d| d.review_type() == "security"));
}

#[test]
fn a_declaration_can_ask_for_a_dimension_no_rule_required() {
    let policies = policies();
    let change = facts(
        &["README.md"],
        "docs: rewrite the auth guide\n\nReview: security\n",
    );
    let (decisions, dispatch, _) = plan(&policies, &change, "abc123", &[], &[]);
    assert!(decisions.iter().any(|d| d.review_type() == "security"));
    assert!(
        dispatch
            .iter()
            .any(|action| matches!(action, ReviewDispatchAction::SpawnChau7Review { .. }))
    );
}

#[test]
fn a_review_already_running_is_not_started_twice() {
    let policies = policies();
    let change = facts(&["crates/aethyme-broker/src/operations.rs"], "fix: x\n");
    let workspace = policies
        .routing
        .workspace(policies.root.path(), 77, "security");
    let (_, dispatch, _) = plan(&policies, &change, "abc123", &[tab(&workspace)], &[]);
    let security = dispatch
        .iter()
        .find(|a| a.review_type() == "security")
        .unwrap();
    assert!(
        matches!(security, ReviewDispatchAction::Defer { .. }),
        "{security:?}"
    );
}

#[test]
fn the_slot_count_holds_a_second_repositorys_review_back() {
    let policies = policies();
    let change = facts(&["crates/aethyme-broker/src/operations.rs"], "fix: x\n");
    let in_flight = vec![InFlightReview {
        review_type: "security".into(),
        pull_request: 1,
    }];
    let (_, dispatch, _) = plan(&policies, &change, "abc123", &[], &in_flight);
    let security = dispatch
        .iter()
        .find(|a| a.review_type() == "security")
        .unwrap();
    assert!(
        matches!(security, ReviewDispatchAction::Defer { .. }),
        "{security:?}"
    );

    // The other dimension is unaffected: separate budgets are what keep one
    // backlogged dimension from silencing the others.
    assert!(
        dispatch
            .iter()
            .any(|a| matches!(a, ReviewDispatchAction::MentionOnPullRequest { .. }))
    );
}

#[test]
fn an_unrouted_dimension_is_recorded_rather_than_dropped() {
    // `performance` has no route, so it falls to `record`. The record still
    // exists and the label still appears -- CI carries the check.
    let policies = policies();
    let change = facts(&["README.md"], "feat: x\n\nReview: performance\n");
    let (decisions, dispatch, actions) = plan(&policies, &change, "abc123", &[], &[]);
    assert!(decisions.iter().any(|d| d.review_type() == "performance"));
    assert!(dispatch.iter().any(|a| a.review_type() == "performance"
        && matches!(a, ReviewDispatchAction::RecordOnly { .. })));
    assert!(actions.iter().any(|action| match action {
        PrProjectionAction::AddLabels { names } =>
            names.iter().any(|n| n == "aethyme/review:performance"),
        _ => false,
    }));
}

#[test]
fn every_backend_config_the_scaffold_documents_actually_parses() {
    // The commented block in `.aethyme/config.toml` is the only documentation
    // an operator reads before switching this on. A key renamed here without
    // renaming it there is a config that fails to load in their hands.
    let policies = policies();
    assert_eq!(
        policies.routing.route_for("security").backend,
        ReviewBackend::Chau7
    );
    assert_eq!(
        policies.routing.route_for("code").backend,
        ReviewBackend::ProviderComment
    );
    assert_eq!(
        policies.routing.route_for("anything-else").backend,
        ReviewBackend::Record
    );
    assert!(policies.projection.reserved.contains("skip-review"));
    assert_eq!(policies.trigger.rule.len(), 2);
}

#[test]
fn the_repositorys_own_scaffold_is_a_valid_configuration_when_uncommented() {
    // The commented scaffold in this repository's `.aethyme/config.toml` is the
    // only documentation an operator reads before switching any of this on. A
    // field renamed in the code and not there produces a configuration that
    // fails to load in their hands, and nothing else catches it.
    //
    // The scaffold marks its configuration lines `#>` and its prose `#`, so
    // recovering it is a prefix strip rather than a guess at which comment is
    // TOML.
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../../.aethyme/config.toml");
    let text =
        std::fs::read_to_string(&source).unwrap_or_else(|e| panic!("{}: {e}", source.display()));

    let scaffold: String = text
        .lines()
        .filter_map(|line| line.strip_prefix("#>"))
        .map(|rest| rest.strip_prefix(' ').unwrap_or(rest))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        scaffold.contains("[review.trigger]")
            && scaffold.contains("[review.routing]")
            && scaffold.contains("[review.projection]"),
        "the scaffold lost a table:\n{scaffold}"
    );

    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join(".aethyme")).unwrap();
    std::fs::write(root.path().join(".aethyme/config.toml"), &scaffold).unwrap();

    let trigger = ReviewTriggerPolicy::load(root.path())
        .unwrap_or_else(|e| panic!("[review.trigger] does not load: {e}\n{scaffold}"));
    let routing = ReviewRoutingPolicy::load(root.path())
        .unwrap_or_else(|e| panic!("[review.routing] does not load: {e}\n{scaffold}"));
    let projection = PrProjectionPolicy::load(root.path())
        .unwrap_or_else(|e| panic!("[review.projection] does not load: {e}\n{scaffold}"));

    // Loading is not enough: uncommenting has to yield the behaviour the prose
    // around it promises.
    assert!(trigger.enabled && routing.enabled && projection.enabled);
    assert_eq!(routing.route_for("security").backend, ReviewBackend::Chau7);
    assert_eq!(
        routing.route_for("code").backend,
        ReviewBackend::ProviderComment
    );
    assert_eq!(routing.route_for("unrouted").backend, ReviewBackend::Record);
}

#[test]
fn an_empty_classification_projects_nothing_about_the_author() {
    let policies = policies();
    let change = facts(&["README.md"], "docs: x\n");
    assert!(change.classification == CommitClassification::default());
    let (_, _, actions) = plan(&policies, &change, "abc123", &[], &[]);
    let labels: Vec<String> = actions
        .iter()
        .filter_map(|action| match action {
            PrProjectionAction::AddLabels { names } => Some(names.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(labels, ["aethyme/review:code"]);
}
