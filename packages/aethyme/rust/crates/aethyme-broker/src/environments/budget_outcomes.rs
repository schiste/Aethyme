//! What a sustained budget breach does (#1239, through #1238).
//!
//! `[budgets.outcomes]` turns a metric that stays over budget into one
//! tracked issue. Each evaluation is a *window* for every metric that has new
//! runs since the last one (its newest record moved). A window is a breach
//! when the metric is `over`, or when more than `failure_rate_pct` of its
//! runs failed. After `windows` consecutive breaches the metric's issue is
//! upserted (opened, or commented on if already open); once the metric is
//! back to `ok` it is resolved. `watch` holds: neither a breach nor a
//! recovery.
//!
//! The decision is pure ([`evaluate`]); writing the issue goes through an
//! [`OutcomeSink`], so a failed or refused write leaves the issue unmarked
//! and the next window retries it.

use std::collections::BTreeMap;

use serde::Serialize;

use super::budgets::MetricReport;
use super::spec::BudgetOutcomesSpec;

/// Fewest runs (successful plus failed) a failure rate is judged from.
pub const MIN_FAILURE_RATE_RUNS: usize = 5;

/// What the last evaluation left for one metric.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutcomeState {
    /// The newest record seen: a window needs this to move.
    pub head: i64,
    /// Consecutive breached windows.
    pub consecutive: u32,
    /// An issue was opened and not yet resolved.
    pub issue_open: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Upsert,
    Resolve,
}

/// One issue write an evaluation asks for.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OutcomeAction {
    pub kind: ActionKind,
    pub metric: String,
    /// Why: `over`, `failure_rate` or `recovered`.
    pub reason: &'static str,
    pub consecutive: u32,
    pub report: MetricReport,
}

/// Writes the issue for one metric. `fingerprint` is stable per repository
/// and metric, so one metric never has two issues.
pub trait OutcomeSink {
    fn upsert(
        &mut self,
        fingerprint: &str,
        title: &str,
        body: &str,
        labels: &[String],
    ) -> Result<String, String>;
    fn resolve(&mut self, fingerprint: &str, body: &str) -> Result<String, String>;
}

/// The result of carrying out one action.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ActionOutcome {
    #[serde(flatten)]
    pub action: OutcomeAction,
    pub fingerprint: String,
    /// `done` or `failed`.
    pub status: &'static str,
    pub detail: String,
}

fn breach(spec: &BudgetOutcomesSpec, report: &MetricReport) -> Option<&'static str> {
    if report.status == "over" {
        return Some("over");
    }
    let runs = report.count + report.failures;
    let pct = spec.failure_rate_pct?;
    (runs >= MIN_FAILURE_RATE_RUNS && report.failures * 100 > pct as usize * runs)
        .then_some("failure_rate")
}

/// Advance every budgeted metric that has a new window, returning the new
/// states and the issue writes they call for. A metric whose head did not
/// move is left as it was.
pub fn evaluate(
    spec: &BudgetOutcomesSpec,
    reports: &[MetricReport],
    heads: &BTreeMap<String, i64>,
    states: &BTreeMap<String, OutcomeState>,
) -> (BTreeMap<String, OutcomeState>, Vec<OutcomeAction>) {
    let mut updated = BTreeMap::new();
    let mut actions = Vec::new();
    for report in reports.iter().filter(|r| r.budget.is_some()) {
        let Some(&head) = heads.get(&report.metric) else {
            continue;
        };
        let previous = states.get(&report.metric).cloned().unwrap_or_default();
        if head == previous.head {
            continue;
        }
        let mut state = OutcomeState {
            head,
            ..previous.clone()
        };
        match breach(spec, report) {
            Some(reason) => {
                state.consecutive = previous.consecutive.saturating_add(1);
                if state.consecutive >= spec.windows.max(1) {
                    actions.push(OutcomeAction {
                        kind: ActionKind::Upsert,
                        metric: report.metric.clone(),
                        reason,
                        consecutive: state.consecutive,
                        report: report.clone(),
                    });
                }
            }
            None => {
                if report.status != "watch" {
                    state.consecutive = 0;
                }
                if report.status == "ok" && previous.issue_open {
                    actions.push(OutcomeAction {
                        kind: ActionKind::Resolve,
                        metric: report.metric.clone(),
                        reason: "recovered",
                        consecutive: 0,
                        report: report.clone(),
                    });
                }
            }
        }
        updated.insert(report.metric.clone(), state);
    }
    (updated, actions)
}

/// `env-budget:<repository>:<metric>`, with the repository fingerprint
/// shortened: stable for the pair, short enough for an issue marker.
pub fn fingerprint(repository: &str, metric: &str) -> String {
    use sha2::Digest;
    let digest = format!("{:x}", sha2::Sha256::digest(repository.as_bytes()));
    // Hook metric names are free text; an issue fingerprint is not.
    let metric: String = metric
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(96)
        .collect();
    format!("env-budget:{}:{metric}", &digest[..12])
}

fn number(value: Option<f64>) -> String {
    value.map_or_else(|| "-".to_string(), |v| format!("{v:.1}"))
}

fn unit(report: &MetricReport) -> &'static str {
    if report.unit == "seconds" { "s" } else { "MiB" }
}

/// The issue title and body for an action: the metric's numbers, and how to
/// look again. Bounded and free of anything but the report.
pub fn render(action: &OutcomeAction) -> (String, String) {
    let r = &action.report;
    let title = match action.kind {
        ActionKind::Upsert if action.reason == "failure_rate" => {
            format!("Environment budget: {} fails too often", r.metric)
        }
        _ => format!(
            "Environment budget over: {} (p50 {}{} > {}{})",
            r.metric,
            number(r.p50),
            unit(r),
            r.budget.unwrap_or_default(),
            unit(r)
        ),
    };
    let trend = r
        .trend
        .as_ref()
        .map(|t| format!("{:+.1}% ({} → {})", t.change_pct, t.older_p50, t.newer_p50))
        .unwrap_or_else(|| "-".into());
    let lead = match action.kind {
        ActionKind::Upsert => format!(
            "`{}` breached its declared budget ({}) for {} consecutive evaluation window(s).",
            r.metric, action.reason, action.consecutive
        ),
        ActionKind::Resolve => format!("`{}` is back within its declared budget.", r.metric),
    };
    let body = format!(
        "{lead}\n\n| metric | status | budget | count | failures | p50 | p95 | latest | trend |\n|---|---|---|---|---|---|---|---|---|\n| `{}` | {} | {}{} | {} | {} | {} | {} | {} | {} |\n\nRecorded by Aethyme on the host that runs these environments. Look again with `aethyme env budgets`; propose budget changes with `aethyme env calibrate`.\n",
        r.metric,
        r.status,
        r.budget.unwrap_or_default(),
        unit(r),
        r.count,
        r.failures,
        number(r.p50),
        number(r.p95),
        number(r.latest),
        trend,
    );
    (title, body)
}

/// Carry out `actions` through `sink`. A state is marked open only after a
/// successful upsert and closed only after a successful resolve, so a
/// failure is retried on the next window.
pub fn apply(
    repository: &str,
    spec: &BudgetOutcomesSpec,
    actions: Vec<OutcomeAction>,
    states: &mut BTreeMap<String, OutcomeState>,
    sink: &mut dyn OutcomeSink,
) -> Vec<ActionOutcome> {
    let mut outcomes = Vec::new();
    for action in actions {
        let fingerprint = fingerprint(repository, &action.metric);
        let (title, body) = render(&action);
        let result = match action.kind {
            ActionKind::Upsert => sink.upsert(&fingerprint, &title, &body, &spec.labels),
            ActionKind::Resolve => sink.resolve(&fingerprint, &body),
        };
        if result.is_ok()
            && let Some(state) = states.get_mut(&action.metric)
        {
            state.issue_open = action.kind == ActionKind::Upsert;
        }
        let (status, detail) = match result {
            Ok(detail) => ("done", detail),
            Err(detail) => ("failed", detail),
        };
        outcomes.push(ActionOutcome {
            action,
            fingerprint,
            status,
            detail,
        });
    }
    outcomes
}

/// How often the supervisor evaluates budget outcomes.
pub const SUPERVISOR_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// One repository's evaluation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RepositoryOutcomes {
    pub repository: String,
    /// Metrics that had a new window.
    pub windows: usize,
    pub actions: Vec<ActionOutcome>,
}

/// Writes through the #1238 issue upsert, under the repository's trusted
/// `[automation]` policy and journal.
struct RepositorySink<'a> {
    repo: &'a crate::GitRepo,
    slug: String,
}

impl RepositorySink<'_> {
    fn send(
        &mut self,
        fingerprint: &str,
        title: &str,
        body: &str,
        labels: &[String],
        resolve: bool,
    ) -> Result<String, String> {
        use crate::outcome_issues::{
            OutcomeIssueAction, OutcomeIssueRequest, upsert_outcome_issue_for_repository,
        };
        let request = OutcomeIssueRequest {
            repository: self.slug.clone(),
            fingerprint: fingerprint.to_string(),
            title: title.to_string(),
            body: Some(body.to_string()),
            labels: labels.to_vec(),
            resolve,
        };
        let result = upsert_outcome_issue_for_repository(self.repo, &request, None)
            .map_err(|error| format!("{}: {error}", error.code()))?;
        let action = serde_json::to_value(result.action)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        match result.action {
            OutcomeIssueAction::Refused => Err(format!(
                "{}: {}",
                result.code.unwrap_or("refused"),
                result.message
            )),
            _ => Ok(match result.issue_url {
                Some(url) => format!("{action} {url}"),
                None => action,
            }),
        }
    }
}

impl OutcomeSink for RepositorySink<'_> {
    fn upsert(
        &mut self,
        fingerprint: &str,
        title: &str,
        body: &str,
        labels: &[String],
    ) -> Result<String, String> {
        self.send(fingerprint, title, body, labels, false)
    }
    fn resolve(&mut self, fingerprint: &str, body: &str) -> Result<String, String> {
        self.send(fingerprint, "", body, &[], true)
    }
}

/// `owner/name` from a GitHub remote URL (`https://github.com/o/n(.git)`,
/// `git@github.com:o/n(.git)` or `ssh://git@github.com/o/n(.git)`).
pub fn github_slug(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("git@github.com:"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))?;
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let mut parts = rest.split('/');
    let (owner, name) = (parts.next()?, parts.next()?);
    (parts.next().is_none() && !owner.is_empty() && !name.is_empty())
        .then(|| format!("{owner}/{name}"))
}

/// Evaluate one repository's budgets and carry out what a sustained breach
/// or a recovery calls for. `Ok(None)` when the repository does not opt in
/// with `[budgets.outcomes] issue = true`.
pub fn act_for_repository(
    repo_root: &std::path::Path,
    store: &mut super::store::EnvironmentStore,
) -> Result<Option<RepositoryOutcomes>, String> {
    let Some(loaded) = super::load_repository_file(repo_root).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    let file = loaded.value.map_err(|d| d.render())?;
    let Some(spec) = file.budgets.outcomes.clone().filter(|o| o.issue) else {
        return Ok(None);
    };
    let repo = crate::GitRepo::discover(repo_root).map_err(|e| e.to_string())?;
    let repository = crate::gates::git_origin_fingerprint(&repo);
    let origin = repo.remote_url("origin").unwrap_or_default();
    let slug = github_slug(origin.trim()).ok_or_else(|| {
        format!("origin {origin:?} is not a GitHub repository; budget issues need one")
    })?;
    let timings = store.timings(&repository).map_err(|e| e.to_string())?;
    let footprints = store.footprints(&repository).map_err(|e| e.to_string())?;
    let reports = super::budgets::report(&file.budgets, &timings, &footprints);
    let heads = store.metric_heads(&repository).map_err(|e| e.to_string())?;
    let states = store
        .budget_outcomes(&repository)
        .map_err(|e| e.to_string())?;
    let (mut updated, actions) = evaluate(&spec, &reports, &heads, &states);
    let mut sink = RepositorySink { repo: &repo, slug };
    let actions = apply(&repository, &spec, actions, &mut updated, &mut sink);
    for (metric, state) in &updated {
        store
            .set_budget_outcome(&repository, metric, state)
            .map_err(|e| e.to_string())?;
    }
    Ok(Some(RepositoryOutcomes {
        repository,
        windows: updated.len(),
        actions,
    }))
}
