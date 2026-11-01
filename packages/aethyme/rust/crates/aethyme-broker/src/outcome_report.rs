//! Outcome hooks (#1238, fourth primitive): one report from an automation
//! sets in motion what its `[[automation.outcomes]]` rule declares.
//!
//! ```toml
//! [[automation.outcomes]]
//! source = "prebuild:db-template"
//! issue = true                       # default true
//! labels = ["migrations"]            # must also be in [automation] issue_labels
//! notify_owners = true               # default false
//! paths = ["**/migrations/**"]       # narrow the owners
//! ```
//!
//! On a failure it resolves the owners of the reported commits, upserts the
//! outcome's issue (naming every owner's branch and pull request, live or
//! not), and sends each live owner an automation note linking the issue. On a
//! recovery it resolves the issue. Each action is recorded with its own
//! status: a refused or failed action is reported beside the others and never
//! hides the failure being reported. The rules are read, like the issue
//! policy, from the default branch's remote-tracking commit.

use serde::Serialize;

use crate::outcome_issues::{
    OutcomeIssueAction, OutcomeIssueRequest, OutcomeIssueResult, OutcomeRule,
};
use crate::{Broker, CommitOwner};

pub const OUTCOME_REPORT_SCHEMA: &str = "aethyme.outcome-report/v1";

/// What an automation reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutcomeReportRequest {
    pub source: String,
    /// `true` for a failure, `false` for a recovery.
    pub failed: bool,
    /// Distinguishes outcomes of one source, for example a template digest.
    pub key: Option<String>,
    /// The landed range the failure is about, for owner resolution.
    pub commits: Option<String>,
    /// Required with a failure.
    pub title: Option<String>,
    pub body: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutcomeActionRecord {
    /// `owners`, `issue`, `note` or `resolve`.
    pub kind: &'static str,
    /// `done`, `deduplicated`, `skipped`, `refused` or `failed`.
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OutcomeReport {
    pub schema: &'static str,
    pub source: String,
    /// `failed` or `recovered`.
    pub state: &'static str,
    pub fingerprint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<OutcomeIssueResult>,
    pub owners: Vec<CommitOwner>,
    pub actions: Vec<OutcomeActionRecord>,
}

impl OutcomeReport {
    /// Whether any action was refused or failed.
    pub fn incomplete(&self) -> bool {
        self.actions
            .iter()
            .any(|action| matches!(action.status, "refused" | "failed"))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OutcomeReportError {
    #[error(
        "no [[automation.outcomes]] rule for source {0:?} on the default branch; declare one in .aethyme/config.toml"
    )]
    NoRule(String),
    #[error("{0}")]
    InvalidRequest(String),
}

impl OutcomeReportError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoRule(_) => "no_outcome_rule",
            Self::InvalidRequest(_) => "invalid_request",
        }
    }
}

/// The issue step, so the flow can be exercised without GitHub.
pub trait OutcomeIssueSink {
    fn upsert(
        &mut self,
        broker: &mut Broker,
        request: &OutcomeIssueRequest,
    ) -> Result<OutcomeIssueResult, String>;
}

/// The automation note sender for a source: `prebuild:db-template` sends as
/// `automation:prebuild.db-template`.
pub fn outcome_note_sender(source: &str) -> String {
    format!(
        "{}{}",
        crate::broker::lifecycle::AUTOMATION_SENDER_PREFIX,
        source.replace(':', ".")
    )
}

/// `outcome:<source>[:<key>]`.
pub fn outcome_fingerprint(source: &str, key: Option<&str>) -> String {
    match key {
        Some(key) => format!("outcome:{source}:{key}"),
        None => format!("outcome:{source}"),
    }
}

fn owners_section(owners: &[CommitOwner], unowned: usize) -> String {
    if owners.is_empty() && unowned == 0 {
        return String::new();
    }
    let mut section = String::from("\n\n### Who to ask\n");
    for owner in owners {
        let pull_request = owner
            .pull_request
            .map(|number| format!(", #{number}"))
            .unwrap_or_default();
        let state = if owner.live {
            "live, notified"
        } else {
            "ended"
        };
        section.push_str(&format!(
            "- session {} (`{}`{pull_request}): {state}\n",
            owner.session_id, owner.branch
        ));
    }
    if unowned > 0 {
        section.push_str(&format!("- {unowned} commit(s) with no recorded session\n"));
    }
    section
}

/// Cut `text` to at most `max` bytes on a character boundary.
fn cut(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn record(
    kind: &'static str,
    status: &'static str,
    target: Option<String>,
    code: Option<String>,
    message: Option<String>,
) -> OutcomeActionRecord {
    OutcomeActionRecord {
        kind,
        status,
        target,
        code,
        message,
    }
}

fn issue_record(
    kind: &'static str,
    result: &Result<OutcomeIssueResult, String>,
) -> OutcomeActionRecord {
    match result {
        Ok(result) => {
            let status = match result.action {
                OutcomeIssueAction::Created
                | OutcomeIssueAction::Commented
                | OutcomeIssueAction::Resolved => "done",
                OutcomeIssueAction::RateLimited | OutcomeIssueAction::NothingToResolve => "skipped",
                OutcomeIssueAction::Refused => "refused",
            };
            record(
                kind,
                status,
                result.issue_url.clone(),
                result.code.map(str::to_string),
                Some(result.message.clone()),
            )
        }
        Err(error) => record(kind, "failed", None, None, Some(error.clone())),
    }
}

/// Carry out what `rules` declare for one report. `repository` is the
/// origin's GitHub `owner/name`; without one the issue steps are skipped.
pub fn report_outcome(
    broker: &mut Broker,
    rules: &[OutcomeRule],
    repository: Option<&str>,
    request: &OutcomeReportRequest,
    issues: &mut dyn OutcomeIssueSink,
    now_ms: i64,
) -> Result<OutcomeReport, OutcomeReportError> {
    let rule = rules
        .iter()
        .find(|rule| rule.source == request.source)
        .ok_or_else(|| OutcomeReportError::NoRule(request.source.clone()))?;
    if let Some(key) = request.key.as_deref()
        && (key.is_empty()
            || key.len() > 64
            || !key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
    {
        return Err(OutcomeReportError::InvalidRequest(format!(
            "--key {key:?} must be 1-64 of A-Z a-z 0-9 . _ -"
        )));
    }
    let fingerprint = outcome_fingerprint(&rule.source, request.key.as_deref());
    let mut actions = Vec::new();
    let mut owners = Vec::new();
    let mut issue = None;

    if !request.failed {
        if rule.issue {
            match repository {
                Some(repository) => {
                    let result = issues.upsert(
                        broker,
                        &OutcomeIssueRequest {
                            repository: repository.to_string(),
                            fingerprint: fingerprint.clone(),
                            title: String::new(),
                            body: request.body.clone(),
                            labels: Vec::new(),
                            resolve: true,
                        },
                    );
                    actions.push(issue_record("resolve", &result));
                    issue = result.ok();
                }
                None => actions.push(record(
                    "resolve",
                    "skipped",
                    None,
                    Some("no_github_origin".into()),
                    None,
                )),
            }
        }
    } else {
        let title = request
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .ok_or_else(|| {
                OutcomeReportError::InvalidRequest("a failure requires --title".into())
            })?;
        let mut unowned = 0;
        if rule.notify_owners {
            match request.commits.as_deref() {
                Some(range) => match broker.owners(range, &rule.paths, now_ms) {
                    Ok(report) => {
                        unowned = report.unowned.len();
                        actions.push(record(
                            "owners",
                            "done",
                            Some(range.to_string()),
                            None,
                            Some(format!(
                                "{} owner(s), {} live",
                                report.owners.len(),
                                report.owners.iter().filter(|owner| owner.live).count()
                            )),
                        ));
                        owners = report.owners;
                    }
                    Err(error) => actions.push(record(
                        "owners",
                        "failed",
                        Some(range.to_string()),
                        None,
                        Some(error.to_string()),
                    )),
                },
                None => actions.push(record(
                    "owners",
                    "skipped",
                    None,
                    Some("no_commits".into()),
                    Some("the rule notifies owners but no --commits range was reported".into()),
                )),
            }
        }
        if rule.issue {
            match repository {
                Some(repository) => {
                    let body = format!(
                        "{}{}",
                        request.body.as_deref().unwrap_or(title),
                        owners_section(&owners, unowned)
                    );
                    let result = issues.upsert(
                        broker,
                        &OutcomeIssueRequest {
                            repository: repository.to_string(),
                            fingerprint: fingerprint.clone(),
                            title: title.to_string(),
                            body: Some(body),
                            labels: rule.labels.clone(),
                            resolve: false,
                        },
                    );
                    actions.push(issue_record("issue", &result));
                    issue = result.ok();
                }
                None => actions.push(record(
                    "issue",
                    "skipped",
                    None,
                    Some("no_github_origin".into()),
                    None,
                )),
            }
        }
        let tracked = match issue.as_ref() {
            Some(result) if result.issue_url.is_some() => {
                format!(
                    "tracked in {}",
                    result.issue_url.as_deref().unwrap_or_default()
                )
            }
            Some(result) => format!("no issue filed ({})", result.code.unwrap_or("not written")),
            None if rule.issue => "no issue filed".to_string(),
            None => "no issue kept for this outcome".to_string(),
        };
        let sender = outcome_note_sender(&rule.source);
        let suffix = format!(" — {tracked}");
        let budget = crate::SESSION_NOTE_MAX_BYTES.saturating_sub(suffix.len());
        let message = format!("{}{suffix}", cut(title, budget));
        for owner in owners.iter().filter(|owner| owner.live) {
            let target = Some(format!("session {}", owner.session_id));
            match broker.send_automation_note(
                &sender,
                owner.session_id,
                &message,
                Some(&fingerprint),
            ) {
                Ok(sent) => actions.push(record(
                    "note",
                    if sent.deduplicated {
                        "deduplicated"
                    } else {
                        "done"
                    },
                    target,
                    None,
                    None,
                )),
                Err(error) => actions.push(record(
                    "note",
                    "failed",
                    target,
                    None,
                    Some(error.to_string()),
                )),
            }
        }
    }

    let report = OutcomeReport {
        schema: OUTCOME_REPORT_SCHEMA,
        source: rule.source.clone(),
        state: if request.failed {
            "failed"
        } else {
            "recovered"
        },
        fingerprint,
        issue,
        owners,
        actions,
    };
    let payload = serde_json::json!({
        "source": report.source,
        "state": report.state,
        "fingerprint": report.fingerprint,
        "actions": report
            .actions
            .iter()
            .map(|action| serde_json::json!({
                "kind": action.kind,
                "status": action.status,
                "code": action.code,
            }))
            .collect::<Vec<_>>(),
    })
    .to_string();
    crate::warn_unrecorded(
        "record an automation outcome report",
        broker
            .store()
            .append_event("automation.outcome.reported", None, Some(&payload)),
    );
    Ok(report)
}

/// The real issue step: the trusted policy and journal of `repo`, writing
/// through the coordinated layer when a session is given.
pub struct RepositoryIssueSink<'a> {
    pub repo: &'a crate::GitRepo,
    pub session: Option<i64>,
}

impl OutcomeIssueSink for RepositoryIssueSink<'_> {
    fn upsert(
        &mut self,
        broker: &mut Broker,
        request: &OutcomeIssueRequest,
    ) -> Result<OutcomeIssueResult, String> {
        let coordinated = self.session.map(|session| (broker, session));
        crate::upsert_outcome_issue_for_repository(self.repo, request, coordinated)
            .map_err(|error| format!("{}: {error}", error.code()))
    }
}

#[cfg(test)]
#[path = "outcome_report_tests.rs"]
mod tests;
