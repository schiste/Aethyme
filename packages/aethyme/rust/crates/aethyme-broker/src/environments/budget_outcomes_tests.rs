//! #1239 breach outcomes: windows, thresholds, recovery and retries.

use std::collections::BTreeMap;

use super::budget_outcomes::{
    ActionKind, BUDGET_OUTCOME_SOURCE, OutcomeSink, OutcomeState, Route, RuleSink, apply, evaluate,
    fingerprint, github_slug, outcome_key, render, route,
};
use super::budgets::MetricReport;
use super::spec::BudgetOutcomesSpec;
use super::store::EnvironmentStore;

fn spec(windows: u32, failure_rate_pct: Option<u32>) -> BudgetOutcomesSpec {
    BudgetOutcomesSpec {
        issue: true,
        windows,
        failure_rate_pct,
        labels: vec!["environments".into()],
    }
}

fn report(metric: &str, status: &'static str, count: usize, failures: usize) -> MetricReport {
    MetricReport {
        metric: metric.into(),
        unit: "seconds",
        budget: Some(10),
        count,
        failures,
        p50: Some(if status == "over" { 20.0 } else { 5.0 }),
        p95: Some(if status == "ok" { 6.0 } else { 25.0 }),
        latest: Some(5.0),
        trend: None,
        status,
    }
}

/// Run one window per entry of `statuses` for metric `m`, moving its head
/// each time; returns the action kinds per window.
fn windows(
    spec: &BudgetOutcomesSpec,
    states: &mut BTreeMap<String, OutcomeState>,
    statuses: &[&'static str],
) -> Vec<Vec<ActionKind>> {
    let mut seen = Vec::new();
    for (i, status) in statuses.iter().enumerate() {
        let heads: BTreeMap<String, i64> = [("m".to_string(), i as i64 + 1)].into();
        let (mut updated, actions) = evaluate(spec, &[report("m", status, 10, 0)], &heads, states);
        seen.push(actions.iter().map(|a| a.kind.clone()).collect());
        apply(
            "repo",
            spec,
            actions,
            &mut updated,
            &mut FakeSink::default(),
        );
        states.extend(updated);
    }
    seen
}

/// #1239 acceptance: over for the configured number of windows opens one
/// issue, every later breached window updates it, and returning to `ok`
/// resolves it. `watch` neither breaches nor recovers.
#[test]
fn a_sustained_breach_opens_one_issue_and_recovery_resolves_it() {
    let spec = spec(3, None);
    let mut states = BTreeMap::new();
    let seen = windows(
        &spec,
        &mut states,
        &["over", "over", "over", "over", "watch", "ok", "ok"],
    );
    assert_eq!(
        seen,
        vec![
            vec![],
            vec![],
            vec![ActionKind::Upsert],
            vec![ActionKind::Upsert],
            vec![],
            vec![ActionKind::Resolve],
            vec![],
        ]
    );
}

/// An `ok` window between breaches restarts the count; `watch` keeps it.
#[test]
fn an_ok_window_restarts_the_count_and_watch_keeps_it() {
    let spec = spec(2, None);
    let mut states = BTreeMap::new();
    let seen = windows(&spec, &mut states, &["over", "ok", "over", "watch", "over"]);
    assert_eq!(
        seen,
        vec![vec![], vec![], vec![], vec![], vec![ActionKind::Upsert]]
    );
}

/// No new runs, no window: re-evaluating the same head changes nothing.
#[test]
fn an_unmoved_head_is_not_a_window() {
    let spec = spec(1, None);
    let heads: BTreeMap<String, i64> = [("m".to_string(), 7)].into();
    let states: BTreeMap<String, OutcomeState> = [(
        "m".to_string(),
        OutcomeState {
            head: 7,
            consecutive: 0,
            issue_open: false,
        },
    )]
    .into();
    let (updated, actions) = evaluate(&spec, &[report("m", "over", 10, 0)], &heads, &states);
    assert!(updated.is_empty());
    assert!(actions.is_empty());
}

/// A failure rate above the threshold is a breach once there are enough
/// runs; a metric with no budget is never acted on.
#[test]
fn a_high_failure_rate_breaches_from_enough_runs() {
    let spec = spec(1, Some(50));
    let heads: BTreeMap<String, i64> = [
        ("m".to_string(), 1),
        ("few".to_string(), 1),
        ("free".to_string(), 1),
    ]
    .into();
    let mut free = report("free", "over", 10, 0);
    free.budget = None;
    let reports = vec![report("m", "ok", 4, 6), report("few", "ok", 1, 3), free];
    let (_, actions) = evaluate(&spec, &reports, &heads, &BTreeMap::new());
    let acted: Vec<(&str, &str)> = actions
        .iter()
        .map(|a| (a.metric.as_str(), a.reason))
        .collect();
    assert_eq!(acted, vec![("m", "failure_rate")]);
}

#[derive(Default)]
struct FakeSink {
    fail: bool,
    calls: Vec<(String, String)>,
}

impl OutcomeSink for FakeSink {
    fn upsert(
        &mut self,
        _metric: &str,
        fingerprint: &str,
        _title: &str,
        _body: &str,
        labels: &[String],
    ) -> Result<String, String> {
        self.calls.push((
            "upsert".into(),
            format!("{fingerprint} {}", labels.join(",")),
        ));
        if self.fail {
            Err("denied".into())
        } else {
            Ok("opened".into())
        }
    }
    fn resolve(&mut self, _metric: &str, fingerprint: &str, _body: &str) -> Result<String, String> {
        self.calls.push(("resolve".into(), fingerprint.into()));
        if self.fail {
            Err("denied".into())
        } else {
            Ok("closed".into())
        }
    }
}

/// A refused or failed write leaves the issue unmarked, so the next window
/// tries again; a successful one marks it open with a stable fingerprint.
#[test]
fn a_failed_write_is_retried_on_the_next_window() {
    let spec = spec(1, None);
    let heads: BTreeMap<String, i64> = [("m".to_string(), 1)].into();
    let (mut states, actions) = evaluate(
        &spec,
        &[report("m", "over", 10, 0)],
        &heads,
        &BTreeMap::new(),
    );
    let mut failing = FakeSink {
        fail: true,
        ..FakeSink::default()
    };
    let outcomes = apply("repo", &spec, actions.clone(), &mut states, &mut failing);
    assert_eq!(outcomes[0].status, "failed");
    assert!(!states["m"].issue_open);

    let mut sink = FakeSink::default();
    let outcomes = apply("repo", &spec, actions, &mut states, &mut sink);
    assert_eq!(outcomes[0].status, "done");
    assert!(states["m"].issue_open);
    assert_eq!(
        sink.calls,
        vec![(
            "upsert".to_string(),
            format!("{} environments", fingerprint("repo", "m"))
        )]
    );
    assert_ne!(fingerprint("repo", "m"), fingerprint("other", "m"));
    assert_ne!(fingerprint("repo", "m"), fingerprint("repo", "n"));
}

/// The body carries the metric's numbers and nothing else.
#[test]
fn the_issue_names_the_metric_and_its_numbers() {
    let spec = spec(1, None);
    let heads: BTreeMap<String, i64> = [("up.cold".to_string(), 1)].into();
    let (_, actions) = evaluate(
        &spec,
        &[report("up.cold", "over", 10, 0)],
        &heads,
        &BTreeMap::new(),
    );
    let (title, body) = render(&actions[0]);
    assert_eq!(title, "Environment budget over: up.cold (p50 20.0s > 10s)");
    assert!(body.contains("| `up.cold` | over | 10s | 10 | 0 | 20.0 | 25.0 | 5.0 | - |"));
}

/// State persists per repository and metric, and heads track the newest
/// timing and sample of each metric.
#[test]
fn outcome_state_and_heads_persist_in_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = EnvironmentStore::open(&dir.path().join("environments.db")).unwrap();
    store
        .record_timing("repo", "park", "env", 1.0, true)
        .unwrap();
    store
        .record_timing("repo", "park", "env", 2.0, false)
        .unwrap();
    store
        .record_timing("other", "park", "env", 1.0, true)
        .unwrap();
    let heads = store.metric_heads("repo").unwrap();
    assert_eq!(heads.keys().collect::<Vec<_>>(), vec!["park"]);
    assert_eq!(heads["park"], 2);

    let state = OutcomeState {
        head: 2,
        consecutive: 3,
        issue_open: true,
    };
    store.set_budget_outcome("repo", "park", &state).unwrap();
    assert_eq!(store.budget_outcomes("repo").unwrap()["park"], state);
    assert!(store.budget_outcomes("other").unwrap().is_empty());
}

/// `[budgets.outcomes]` defaults to three windows and no issue; zero windows,
/// a failure rate outside 1..=100 and an empty label are refused.
#[test]
fn budget_outcomes_are_validated() {
    use super::spec::{EnvironmentsFile, HostEnvironmentsFile};
    use super::{Loaded, parse_loaded};
    let validation = |text: &str| -> Vec<&'static str> {
        let loaded: Loaded<EnvironmentsFile> =
            parse_loaded(std::path::PathBuf::from("t"), "t", text);
        let file = loaded.value.unwrap_or_else(|d| panic!("{}", d.render()));
        super::validate(&file, &HostEnvironmentsFile::default(), "t", "h")
            .into_iter()
            .map(|d| d.code)
            .collect()
    };
    let base = "schema = 1\n[[profiles]]\nname = \"backend\"\n[[profiles.components]]\nname = \"db\"\nruntime = \"hooks\"\nprovision = \"true\"\ndestroy = \"true\"\n";
    let defaults: Loaded<EnvironmentsFile> = parse_loaded(
        std::path::PathBuf::from("t"),
        "t",
        &format!("{base}[budgets.outcomes]\n"),
    );
    let outcomes = defaults.value.unwrap().budgets.outcomes.unwrap();
    assert_eq!((outcomes.issue, outcomes.windows), (false, 3));
    let valid = format!(
        "{base}[budgets.outcomes]\nissue = true\nwindows = 2\nfailure_rate_pct = 50\nlabels = [\"environments\"]\n"
    );
    assert!(validation(&valid).is_empty(), "{:?}", validation(&valid));
    for bad in [
        "windows = 0",
        "failure_rate_pct = 0",
        "failure_rate_pct = 101",
        "labels = [\" \"]",
    ] {
        let text = format!("{base}[budgets.outcomes]\n{bad}\n");
        assert_eq!(validation(&text), vec!["invalid_budget"], "{bad}");
    }
}

#[test]
fn github_slugs_come_from_every_remote_form() {
    for url in [
        "https://github.com/o/n.git",
        "https://github.com/o/n",
        "git@github.com:o/n.git",
        "ssh://git@github.com/o/n.git",
    ] {
        assert_eq!(github_slug(url).as_deref(), Some("o/n"), "{url}");
    }
    for url in [
        "https://gitlab.com/o/n.git",
        "https://github.com/o",
        "/tmp/repo",
        "",
    ] {
        assert_eq!(github_slug(url), None, "{url}");
    }
}

/// Hook metric names are free text; the fingerprint keeps only what an
/// issue marker allows, and stays bounded.
#[test]
fn fingerprints_sanitize_free_text_metric_names() {
    let fp = fingerprint("repo", "my metric/1!");
    assert!(fp.starts_with("env-budget:"), "{fp}");
    assert!(fp.ends_with(":my_metric_1_"), "{fp}");
    assert!(fingerprint("repo", &"x".repeat(500)).len() <= 128);
}

/// Answers every issue request with a fixed action and records it.
struct FakeIssues {
    requests: Vec<crate::OutcomeIssueRequest>,
    answer: crate::OutcomeIssueAction,
}

impl crate::OutcomeIssueSink for FakeIssues {
    fn upsert(
        &mut self,
        _broker: &mut crate::Broker,
        request: &crate::OutcomeIssueRequest,
    ) -> Result<crate::OutcomeIssueResult, String> {
        self.requests.push(request.clone());
        let refused = self.answer == crate::OutcomeIssueAction::Refused;
        Ok(crate::OutcomeIssueResult {
            schema: crate::OUTCOME_ISSUE_SCHEMA,
            repository: request.repository.clone(),
            fingerprint: request.fingerprint.clone(),
            action: self.answer,
            issue_number: (!refused).then_some(9),
            issue_url: (!refused).then(|| "https://github.com/o/n/issues/9".to_string()),
            code: refused.then_some("automation_issues_denied"),
            message: "ok".into(),
            operation_ids: Vec::new(),
        })
    }
}

fn budget_rule() -> crate::OutcomeRule {
    crate::OutcomeRule {
        source: BUDGET_OUTCOME_SOURCE.into(),
        issue: true,
        labels: vec!["env-budget".into()],
        notify_owners: false,
        paths: Vec::new(),
    }
}

/// A broker on a throwaway repository, isolated from host state.
fn broker(temp: &std::path::Path) -> crate::Broker {
    let repo = temp.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    git(&["add", ".gitignore"]);
    git(&["commit", "-qm", "initial"]);
    let host_state = temp.join("host-state");
    let options = crate::BrokerOpenOptions::default()
        .with_host_state_dir(&host_state)
        .with_host_cache_dir(temp.join("host-cache"))
        .with_worktree_root(temp.join("worktrees"))
        .with_collaboration_state(host_state.clone())
        .with_host_operation_database(host_state.join("host-operations.db"));
    crate::Broker::open_with_options(&repo, options).unwrap()
}

fn rule_sink<'a>(
    rules: &'a [crate::OutcomeRule],
    issues: &'a mut FakeIssues,
    broker: crate::Broker,
) -> RuleSink<'a> {
    let mut slot = Some(broker);
    RuleSink {
        rules,
        repository: Some("o/n".into()),
        issues,
        open_broker: Box::new(move || slot.take().ok_or_else(|| "opened twice".to_string())),
        broker: None,
        now_ms: 1_000,
    }
}

/// With a declared rule, a sustained breach is reported through the
/// outcome hook: one issue per metric under the outcome fingerprint, the
/// rule's labels rather than `[budgets.outcomes] labels`, a recorded event,
/// and a resolve once the metric recovers.
#[test]
fn a_declared_rule_carries_each_breach_and_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let rules = [budget_rule()];
    let mut issues = FakeIssues {
        requests: Vec::new(),
        answer: crate::OutcomeIssueAction::Created,
    };
    let mut resolving = FakeIssues {
        requests: Vec::new(),
        answer: crate::OutcomeIssueAction::Resolved,
    };
    let mut sink = rule_sink(&rules, &mut issues, broker(temp.path()));
    let spec = spec(1, None);
    let metric = "p95 provision";

    let heads: BTreeMap<String, i64> = [(metric.to_string(), 1)].into();
    let (mut states, actions) = evaluate(
        &spec,
        &[report(metric, "over", 10, 0)],
        &heads,
        &BTreeMap::new(),
    );
    let outcomes = apply("repo", &spec, actions, &mut states, &mut sink);
    let expected = format!("outcome:budget:environments:{}", outcome_key(metric));
    assert_eq!(outcomes[0].status, "done", "{}", outcomes[0].detail);
    assert_eq!(outcomes[0].fingerprint, expected);
    assert!(states[metric].issue_open);

    let heads: BTreeMap<String, i64> = [(metric.to_string(), 2)].into();
    let (mut recovered, actions) = evaluate(&spec, &[report(metric, "ok", 10, 0)], &heads, &states);
    sink.issues = &mut resolving;
    let outcomes = apply("repo", &spec, actions, &mut recovered, &mut sink);
    assert_eq!(outcomes[0].status, "done", "{}", outcomes[0].detail);
    assert!(!recovered[metric].issue_open);
    let events = sink
        .broker
        .as_mut()
        .unwrap()
        .store()
        .events_after_filtered(0, 10, Some("automation.outcome.reported"))
        .unwrap();
    assert_eq!(events.len(), 2);
    drop(sink);

    let opened = &issues.requests[0];
    assert!(!opened.resolve);
    assert_eq!(opened.fingerprint, expected);
    assert_eq!(opened.labels, vec!["env-budget".to_string()]);
    assert!(opened.title.contains(metric), "{}", opened.title);
    assert_eq!(resolving.requests.len(), 1);
    assert!(resolving.requests[0].resolve);
    assert_eq!(resolving.requests[0].fingerprint, expected);
}

/// A refused issue is not an open one: the window is retried.
#[test]
fn a_refused_rule_report_is_retried_on_the_next_window() {
    let temp = tempfile::tempdir().unwrap();
    let rules = [budget_rule()];
    let mut issues = FakeIssues {
        requests: Vec::new(),
        answer: crate::OutcomeIssueAction::Refused,
    };
    let mut sink = rule_sink(&rules, &mut issues, broker(temp.path()));
    let spec = spec(1, None);
    let heads: BTreeMap<String, i64> = [("m".to_string(), 1)].into();
    let (mut states, actions) = evaluate(
        &spec,
        &[report("m", "over", 10, 0)],
        &heads,
        &BTreeMap::new(),
    );
    let outcomes = apply("repo", &spec, actions, &mut states, &mut sink);
    assert_eq!(outcomes[0].status, "failed");
    assert!(
        outcomes[0].detail.contains("automation_issues_denied"),
        "{}",
        outcomes[0].detail
    );
    assert!(!states["m"].issue_open);
}

/// A window with nothing to write never opens the broker.
#[test]
fn nothing_to_write_leaves_the_broker_closed() {
    let rules = [budget_rule()];
    let mut issues = FakeIssues {
        requests: Vec::new(),
        answer: crate::OutcomeIssueAction::Created,
    };
    let mut sink = RuleSink {
        rules: &rules,
        repository: Some("o/n".into()),
        issues: &mut issues,
        open_broker: Box::new(|| panic!("the broker must not be opened")),
        broker: None,
        now_ms: 1_000,
    };
    let spec = spec(3, None);
    let heads: BTreeMap<String, i64> = [("m".to_string(), 1)].into();
    let (mut states, actions) = evaluate(
        &spec,
        &[report("m", "over", 10, 0)],
        &heads,
        &BTreeMap::new(),
    );
    assert!(actions.is_empty());
    assert!(apply("repo", &spec, actions, &mut states, &mut sink).is_empty());
    assert!(sink.broker.is_none());
}

/// A declared rule wins over `issue = true`; without one, `issue = true`
/// keeps the direct path; with neither, nothing is evaluated.
#[test]
fn a_declared_rule_routes_budget_issues() {
    let direct = spec(3, None);
    let silent = BudgetOutcomesSpec {
        issue: false,
        ..spec(3, None)
    };
    let other = crate::OutcomeRule {
        source: "prebuild:db-template".into(),
        ..budget_rule()
    };
    assert_eq!(route(&direct, &[budget_rule()]), Some(Route::Rule));
    assert_eq!(route(&silent, &[budget_rule()]), Some(Route::Rule));
    assert_eq!(
        route(&direct, std::slice::from_ref(&other)),
        Some(Route::Direct)
    );
    assert_eq!(route(&silent, &[other]), None);
    assert_eq!(route(&silent, &[]), None);
}

/// A metric name that already is a key is kept; a free-text one becomes a
/// bounded key that still tells two names apart.
#[test]
fn outcome_keys_are_valid_and_distinct() {
    assert_eq!(outcome_key("provision.api"), "provision.api");
    let spaced = outcome_key("a b");
    assert!(spaced.starts_with("a_b-"), "{spaced}");
    assert_ne!(spaced, outcome_key("a_b"));
    assert_ne!(spaced, outcome_key("a/b"));
    let long = outcome_key(&"x".repeat(200));
    assert!(long.len() <= 64, "{long}");
    assert_ne!(long, outcome_key(&"x".repeat(201)));
    for key in [spaced, long, outcome_key(""), outcome_key("é")] {
        assert!(
            (1..=64).contains(&key.len())
                && key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')),
            "{key:?}"
        );
    }
}
