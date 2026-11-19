//! The routine worktree transition, using the same policy and journal as push.

use super::{Parsed, UsageError, open_broker};
use crate::cli_output::out;
use crate::exit_status as exit;
use crate::{Broker, BrokerOpError, GitRepo, PreparationState};
use serde::Serialize;
use std::path::Path;

#[derive(Serialize)]
struct Advance {
    schema: &'static str,
    session_id: i64,
    preparation: Option<PreparationState>,
    pushed_head: Option<String>,
    pull_request: Option<crate::SessionPullRequest>,
    state: &'static str,
    stage: &'static str,
    reason: String,
    next_action: String,
    operation_id: Option<i64>,
    review_status: Option<String>,
    #[serde(skip)]
    code: u8,
}

impl Advance {
    fn new(session_id: i64) -> Self {
        Self {
            schema: "aethyme.broker-advance/v1",
            session_id,
            preparation: None,
            pushed_head: None,
            pull_request: None,
            state: "ready",
            stage: "state",
            reason: String::new(),
            next_action: format!("aethyme broker status --session {session_id}"),
            operation_id: None,
            review_status: None,
            code: exit::SUCCESS,
        }
    }

    fn stop(&mut self, code: u8, reason: impl Into<String>, next: String) {
        self.state = match code {
            exit::OUTCOME_UNKNOWN => "outcome_unknown",
            exit::REFUSED => "blocked",
            _ => "failed",
        };
        self.code = code;
        self.reason = reason.into();
        self.next_action = next;
    }

    fn error(&mut self, error: BrokerOpError) {
        let id = self.session_id;
        let diagnostic = match self.stage {
            "prepare" => format!("aethyme broker submit prepare status --session {id} --json"),
            "publish" => format!("aethyme broker push --session {id} --plan --json"),
            _ => format!("aethyme broker status --session {id} --json"),
        };
        let mut code = exit::for_broker_error(&error);
        let diagnostic = match &error {
            BrokerOpError::CoordinatedOperationBlocked { operation_id, .. }
            | BrokerOpError::CoordinatedOperationTimedOut { operation_id, .. } => {
                code = exit::OUTCOME_UNKNOWN;
                self.operation_id = Some(*operation_id);
                format!("aethyme broker advanced operations list --session {id} --json")
            }
            _ => diagnostic,
        };
        self.stop(code, error.to_string(), diagnostic);
    }
}

pub(super) fn run_advance(parsed: Parsed) -> Result<(), UsageError> {
    let id = parsed.session.ok_or_else(|| {
        UsageError::Message("advance requires a registered worktree or --session <id>".into())
    })?;
    let mut report = Advance::new(id);
    let broker = {
        let _span = crate::phase_telemetry::span("advance_broker_open");
        open_broker(false)?
    };
    if let Err(error) = advance(broker, parsed.open_pr, &mut report) {
        report.error(error);
    }
    if parsed.json {
        out!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        out!("Session {id}: {} ({})", report.state, report.reason);
        if let Some(pr) = &report.pull_request {
            out!("Pull request: #{} {}", pr.number, pr.url);
        }
        out!("Next: {}", report.next_action);
    }
    if report.code == exit::SUCCESS {
        Ok(())
    } else {
        Err(UsageError::SilentExit(report.code))
    }
}

fn advance(mut broker: Broker, open_pr: bool, report: &mut Advance) -> Result<(), BrokerOpError> {
    let id = report.session_id;
    let session = broker.store_ref().session(id)?;
    let state_validation = crate::phase_telemetry::span("advance_state_validation");
    let checkout = GitRepo::discover(Path::new(&session.worktree_path))?;
    let diagnostic = format!("aethyme broker status --session {id} --json");
    if checkout.root() != Path::new(&session.worktree_path)
        || checkout.main_root()? != broker.main_root()
        || checkout.current_branch()? != session.branch
    {
        report.stop(
            exit::REFUSED,
            "the checkout no longer matches the registered session",
            diagnostic,
        );
        return Ok(());
    }
    if let Some(operation) = checkout.in_progress_operation()? {
        report.stop(
            exit::REFUSED,
            format!("{operation} is in progress"),
            diagnostic,
        );
        return Ok(());
    }
    if let Some(blocker) = broker.session_action_required_blocker(id)? {
        if blocker.scope == crate::BlockerScope::Session {
            report.stop(exit::REFUSED, blocker.cause, blocker.clear);
        } else {
            report.stop(
                exit::REFUSED,
                "read the worktree's recovery instructions before continuing",
                format!(
                    "cat '{}'/.aethyme/broker-action-required.md",
                    session.worktree_path.replace('\'', "'\\''")
                ),
            );
        }
        return Ok(());
    }
    drop(state_validation);

    report.stage = "prepare";
    let prepare = {
        let _span = crate::phase_telemetry::span("advance_preparation_status");
        broker.preparation_status(id)?
    };
    report.preparation = Some(prepare.state);
    match prepare.state {
        PreparationState::Current | PreparationState::NotConfigured => {}
        PreparationState::Required | PreparationState::Stale if prepare.authorized_by_policy => {
            let _span = crate::phase_telemetry::span("advance_prepare");
            let prepared = broker.prepare_session(
                id,
                false,
                super::session::DEFAULT_PREPARE_SHARED_CACHE_WAIT,
            )?;
            report.preparation = Some(prepared.state);
            super::telemetry::set_command_metric_details(prepared.metric_details());
            if prepared.state != PreparationState::Current {
                report.stop(
                    exit::REFUSED,
                    "preparation did not reach current state",
                    prepared.next_action,
                );
                return Ok(());
            }
        }
        _ => {
            let next = if prepare.authorized_by_policy {
                prepare.next_action.unwrap_or(diagnostic)
            } else {
                format!("aethyme broker submit prepare status --session {id} --json")
            };
            report.stop(exit::REFUSED, prepare.reason, next);
            return Ok(());
        }
    }

    report.stage = "state";
    let validation = crate::phase_telemetry::span("advance_commit_state");
    // This decision needs the two tips only. Push checks the live state and
    // policy again before any remote write.
    let repo = broker.repo_handle();
    let default = crate::session_push::tracked_default(repo);
    let (head, remote_oid) = broker.session_push_tips(&session, default.as_ref())?;
    let worktree_next = format!(
        "git -C '{}' status --short",
        session.worktree_path.replace('\'', "'\\''"),
    );
    if !checkout.uncommitted_summary()?.is_empty() {
        report.stop(
            exit::REFUSED,
            "the session has uncommitted changes",
            worktree_next,
        );
        return Ok(());
    }
    if session.diff_base.as_deref() == Some(head.as_str()) && remote_oid.is_none() {
        report.reason = "the worktree is ready for edits".into();
        report.next_action = worktree_next;
        return Ok(());
    }
    let push_enabled = default.as_ref().is_some_and(|default| {
        crate::session_push::push_authorized(repo, default).unwrap_or(false)
    });
    let submit = format!("aethyme broker submit --session {id}");
    if !push_enabled && open_pr {
        report.stop(
            exit::REFUSED,
            "repository policy does not enable session pushes",
            submit,
        );
        return Ok(());
    }
    if !push_enabled || (!open_pr && remote_oid.as_deref() == Some(head.as_str())) {
        report.reason = if push_enabled {
            "the session head matches the recorded remote ref"
        } else {
            "the committed session is ready to validate under repository policy"
        }
        .into();
        report.next_action = submit;
        return Ok(());
    }
    drop(validation);

    report.stage = "publish";
    let _publish = crate::phase_telemetry::span("advance_publish");
    // push_session redecides once on a proven pre-write tip race. Ambiguous
    // writes go through the operation journal and are never replayed here.
    let pushed = broker.push_session(id, open_pr)?;
    report.pushed_head = Some(pushed.pushed_oid.clone());
    report.pull_request = pushed.pr.clone();
    let review = super::submit::review_on_push(broker.main_root(), &pushed);
    report.state = "published";
    report.reason = "the committed session head is published".into();
    report.next_action = if pushed.uncommitted_files > 0 {
        worktree_next
    } else if pushed.default_branch.as_ref().is_some_and(|drift| {
        drift.behind > 0 && drift.sync_impact != crate::main_drift::SyncImpact::Disjoint
    }) {
        format!("aethyme broker sync --session {id}")
    } else {
        submit
    };
    report.review_status = review
        .as_ref()
        .and_then(|value| value["status"].as_str())
        .map(str::to_owned);
    Ok(())
}
