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
    ReviewDispatchAction, ReviewProjection, ReviewReportingPolicy, ReviewRequest,
    ReviewRequestState, ReviewRoutingPolicy, ReviewTrigger, ReviewTriggerDecision,
    ReviewTriggerPolicy, dispatch_review, eligible_types, finished_workspaces,
    parse_classification, plan_execution, project, schedule,
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

[review.reporting]
request_changes_at = "blocker"
max_findings = 4

[[review.reporting.severity]]
label = "blocker"
means = "do not merge"

[[review.reporting.severity]]
label = "nit"
means = "taste"

[review.projection]
enabled = true
label_prefix = "aethyme/"
reserved = ["skip-review"]
"#;

struct Policies {
    trigger: ReviewTriggerPolicy,
    routing: ReviewRoutingPolicy,
    reporting: ReviewReportingPolicy,
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
        reporting: ReviewReportingPolicy::load(root.path()).unwrap(),
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
                &policies.reporting,
                policies.root.path(),
                "o/r",
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
fn this_repositorys_own_review_configuration_loads_and_routes_as_written() {
    // `.aethyme/config.toml` stopped being a commented scaffold on 2026-09-12
    // and became the configuration this repository actually reviews itself
    // with. While it was commented, the risk was drift: a field renamed in the
    // code and not there produced a configuration that failed to load in an
    // operator's hands, and nothing else caught it.
    //
    // Live, the risk is worse and this test is the same shape. A file that
    // fails to load does not fall back to the documented behaviour -- it
    // routes nothing, and a repository that has stopped reviewing its own pull
    // requests looks exactly like one where no pull request happened to be
    // eligible. Nothing fails anywhere. So load the real file, from the real
    // path `review run` reads it from, and assert the routing it is supposed
    // to express.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../..");
    let source = root.join(".aethyme/config.toml");
    assert!(source.is_file(), "{} is missing", source.display());

    let trigger = ReviewTriggerPolicy::load(&root)
        .unwrap_or_else(|e| panic!("[review.trigger] does not load: {e}"));
    let routing = ReviewRoutingPolicy::load(&root)
        .unwrap_or_else(|e| panic!("[review.routing] does not load: {e}"));
    let projection = PrProjectionPolicy::load(&root)
        .unwrap_or_else(|e| panic!("[review.projection] does not load: {e}"));

    // Loading is not enough: all three have an `enabled` flag that defaults
    // false, so a table that parsed but was never switched on reads as a
    // perfectly healthy configuration that performs nothing.
    assert!(trigger.enabled, "[review.trigger] parsed but is disabled");
    assert!(routing.enabled, "[review.routing] parsed but is disabled");
    assert!(
        projection.enabled,
        "[review.projection] parsed but is disabled"
    );

    // Both live dimensions are performed by an agent in its own workspace.
    // `record` here would be the quiet failure above wearing a valid config:
    // rows accumulate, labels appear, and nobody ever reads the diff.
    assert_eq!(
        routing.route_for("code").backend,
        ReviewBackend::Chau7,
        "code review must be performed, not merely recorded"
    );
    assert_eq!(routing.route_for("security").backend, ReviewBackend::Chau7);
    assert_eq!(routing.route_for("unrouted").backend, ReviewBackend::Record);

    // Every policy under `[review]`, including the one this file is not about.
    // `ReviewPolicy` is the review *lifecycle* -- a fifth reader of the same
    // namespace -- and it is loaded on the submission path, so a config it
    // rejects does not degrade review routing, it stops `broker submit`
    // outright. That is what happened when routing went live on 2026-09-12,
    // and nothing in this file noticed because nothing here loaded it.
    aethyme_broker::ReviewPolicy::load(&root)
        .unwrap_or_else(|e| panic!("this repository's config breaks the submission path: {e}"));

    let reporting = ReviewReportingPolicy::load(&root)
        .unwrap_or_else(|e| panic!("[review.reporting] does not load: {e}"));

    // Assert on the assembled prompt rather than on any one table. Where these
    // sentences come from has already moved once -- the coordination clause
    // was copied into each route's `instructions` before `[review.reporting]`
    // existed -- and the reviewer only ever sees the total. A test pinned to
    // the layer would have had to be rewritten to keep passing; this one had
    // to be rewritten only because the wording it checks is worth checking.
    for dimension in ["code", "security"] {
        let prompt = aethyme_broker::review_prompt(
            dimension,
            "schiste/Aethyme",
            179,
            "abc123",
            &reporting,
            routing.route_for(dimension).instructions.as_deref(),
        );
        // A reviewer is a shell with credentials; posting with bare `gh` puts
        // a shared-state write outside the operations journal.
        assert!(
            prompt.contains("aethyme broker gh"),
            "the {dimension} reviewer is no longer told to post through the \
             coordinated lane:\n{prompt}"
        );
        // Without a ladder and an anchor, two reviewers of one pull request
        // produce two documents that cannot be read side by side -- which is
        // the state this repository was in until #179.
        assert!(prompt.contains("### [LABEL]"), "{prompt}");
        assert!(prompt.contains("P0"), "{prompt}");
        assert!(prompt.contains("`path:line`"), "{prompt}");
        // The review IS the record. A reviewer that reports "nothing found"
        // only in its terminal leaves a ledger row nobody can interpret.
        assert!(
            prompt.contains(&format!("No {dimension} findings.")),
            "{prompt}"
        );
    }

    // Requesting changes is refused by GitHub on your own pull request, and
    // these reviewers run under the owner's credentials. A threshold here
    // would produce reviews that are written and then refused at the post.
    assert!(
        reporting.blocking_labels().is_empty(),
        "request_changes_at is set, but the reviewers share the author's account"
    );

    // A cap of zero is "unbounded", so a route that lost its budget does not
    // fail -- it opens an agent session per eligible pull request.
    assert!(
        routing.route_for("code").max_concurrent > 0,
        "the code route is unbounded; a busy afternoon would open one Codex \
         shell per pull request"
    );
    assert!(routing.route_for("security").max_concurrent > 0);

    // `always-code-review` plus `security-sensitive-paths`. A rule list that
    // silently shrank to nothing still loads and still routes -- it just finds
    // every change ineligible.
    assert!(
        trigger.rule.len() >= 2,
        "expected the always-code-review and security-sensitive-paths rules, \
         found {}",
        trigger.rule.len()
    );
    assert!(projection.reserved.contains("skip-review"));
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

/// The last link of the chain: decisions become a ledger, a set of GitHub
/// writes, and a set of Chau7 spawns, in that order.
///
/// This is the seam `review run` performs. What it pins is the ordering rule --
/// every review is recorded before anyone is asked to do it -- and the routing
/// of each backend into the right effect. A change that let a Chau7 review be
/// handed out without a ledger row would pass every unit test above and put two
/// reviewers on one pull request the first time an executor crashed.
#[test]
fn execution_records_every_review_before_it_asks_for_one() {
    let policies = policies();
    let change = facts(
        &["crates/aethyme-broker/src/operations.rs"],
        "feat(broker): coordinated write\n\nArea: backend\nSurface: auth\nRisk: high\n",
    );
    let workspace = policies
        .root
        .path()
        .join(".aethyme/reviews")
        .display()
        .to_string();
    let (_, dispatch, actions) = plan(&policies, &change, "abc123", &[tab(&workspace)], &[]);
    let plan = plan_execution(&dispatch, &actions, &[], 77);

    // security -> chau7, code -> the provider bot. Both are recorded as
    // requested; neither is closed by the act of planning.
    let recorded: Vec<(&str, &str, ReviewRequestState)> = plan
        .ledger
        .iter()
        .map(|write| (write.review_type.as_str(), write.backend, write.state))
        .collect();
    assert!(recorded.contains(&("security", "chau7", ReviewRequestState::Requested)));
    assert!(recorded.contains(&("code", "provider_comment", ReviewRequestState::Requested)));

    assert_eq!(plan.chau7.len(), 1, "one Chau7 spawn, for security");
    assert_eq!(plan.chau7[0].review_type, "security");
    assert_eq!(plan.chau7[0].pull_request, 77);

    // The mention comes before the projection: the comment the projection
    // writes describes reviews that have already been asked for.
    assert!(
        plan.gh.len() >= 2,
        "a mention and at least one projection write"
    );
    assert!(
        plan.gh[0].args.iter().any(|arg| arg.contains("codex")),
        "the provider mention is the first GitHub write: {:?}",
        plan.gh[0].args
    );
}

/// A review the router declines to spend now must leave no ledger row.
///
/// A deferred review is one the router intends to ask for later. Recording it
/// would make the unique index refuse that later request, which is the one way
/// a deferral could silently become a cancellation.
#[test]
fn a_deferred_review_is_not_recorded_as_spent() {
    let policies = policies();
    let change = facts(&["README.md"], "docs: tidy\n");
    let workspace = policies
        .root
        .path()
        .join(".aethyme/reviews")
        .display()
        .to_string();
    let (_, dispatch, actions) = plan(&policies, &change, "abc123", &[tab(&workspace)], &[]);
    let plan = plan_execution(&dispatch, &actions, &[], 77);
    for deferred in &plan.deferred {
        assert!(
            !plan
                .ledger
                .iter()
                .any(|write| write.review_type == deferred.review_type),
            "{} was deferred and recorded",
            deferred.review_type
        );
    }
}

/// The reviewer lifecycle end to end: dispatched, occupying, settled,
/// reclaimed, and dispatchable again.
///
/// Every step of this held in isolation before 2026-09-12 and the sequence
/// still did not terminate, because the last two steps did not exist. A
/// reviewer's shell is interactive and never exits, so the tab it opened
/// outlived the review; `dispatch_review` refuses to spawn into an occupied
/// workspace; and nothing closed a tab. One finished security review therefore
/// blocked every later security review of that pull request until
/// `stale_after_minutes` expired the row -- recording a review that ran and
/// posted as `abandoned`, meaning nobody ever looked.
///
/// This asserts the whole cycle rather than the teardown alone, because the
/// property that matters is that it closes. A teardown test that never
/// re-dispatched would pass just as happily against a router that deferred
/// forever afterwards.
#[test]
fn a_finished_reviewers_workspace_is_reclaimed_and_becomes_dispatchable_again() {
    let policies = policies();
    let change = facts(
        &["crates/aethyme-broker/src/operations.rs"],
        "feat(broker): coordinated write\n\nArea: backend\nSurface: auth\nRisk: high\n",
    );
    let workspace = policies
        .routing
        .workspace(policies.root.path(), 77, "security");

    // 1. Nothing is standing in the workspace, so the review is dispatched.
    let (_, dispatch, actions) = plan(&policies, &change, "aaa111", &[], &[]);
    let first = plan_execution(&dispatch, &actions, &[], 77);
    assert_eq!(first.chau7.len(), 1, "the security review is handed out");
    assert_eq!(first.chau7[0].workspace, workspace);
    assert!(first.chau7_close.is_empty(), "nothing has finished yet");

    // 2. The reviewer's tab now occupies it. A second dispatch must defer --
    //    two agents in one checkout read each other's edits.
    let occupied = vec![tab(&workspace)];
    let row = |state| ReviewRequest {
        id: 1,
        repository: "o/r".into(),
        pr_number: 77,
        review_type: "security".into(),
        head_commit: "aaa111".into(),
        backend: "chau7".into(),
        state,
        detail: None,
        requested_at: 0,
        updated_at: 0,
    };
    let live = [row(ReviewRequestState::Running)];
    assert!(
        finished_workspaces(
            &policies.routing,
            policies.root.path(),
            77,
            &live,
            &occupied
        )
        .is_empty(),
        "a reviewer that is still working keeps its tab"
    );
    let (_, dispatch, _) = plan(&policies, &change, "aaa111", &occupied, &[]);
    assert!(
        dispatch
            .iter()
            .any(|action| matches!(action, ReviewDispatchAction::Defer { .. })),
        "an occupied workspace defers: {dispatch:?}"
    );

    // 3. The reviewer posts and reports. Now the tab is the only thing left,
    //    and reclaiming it is what the tick plans.
    let settled = [row(ReviewRequestState::Satisfied)];
    let teardown = finished_workspaces(
        &policies.routing,
        policies.root.path(),
        77,
        &settled,
        &occupied,
    );
    assert_eq!(teardown.len(), 1, "the settled workspace is reclaimed");
    assert_eq!(teardown[0].workspace, workspace);
    assert_eq!(teardown[0].tab_ids.len(), 1);

    // The plan carries it, and carries it as its own step: an adapter that
    // only read `chau7` would start reviews and never release one.
    let (_, dispatch, actions) = plan(&policies, &change, "aaa111", &occupied, &[]);
    let reclaiming = plan_execution(&dispatch, &actions, &teardown, 77);
    assert_eq!(reclaiming.chau7_close.len(), 1);
    assert_eq!(reclaiming.chau7_close[0].workspace, workspace);

    // 4. With the tab gone, the next head is dispatchable. This is the
    //    assertion the old behaviour could not satisfy at any point.
    let (_, dispatch, _) = plan(&policies, &change, "bbb222", &[], &[]);
    assert!(
        dispatch
            .iter()
            .any(|action| matches!(action, ReviewDispatchAction::SpawnChau7Review { .. })),
        "the reclaimed workspace accepts the next review: {dispatch:?}"
    );
}
