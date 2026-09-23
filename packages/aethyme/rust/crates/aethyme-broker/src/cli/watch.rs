//! `broker watch`, `deliveries` and `pr check`: pull request watches and their delivery.

use super::*;

pub(super) fn render_pr_check_report(
    report: &crate::PrCheckReport,
    json: bool,
) -> Result<(), UsageError> {
    if json {
        out!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }

    match &report.pr {
        Some(pr) => {
            out!(
                "PR #{} -> {}: {}",
                pr.number,
                report.target_branch,
                pr.title
            );
            if let Some(url) = &pr.url {
                out!("URL: {url}");
            }
        }
        None => {
            out!("{}", report.decision.summary);
        }
    }
    out!("Marker: {}", report.marker.as_str());
    out!(
        "Activity: {}{}",
        if report.checked_activity {
            "checked"
        } else {
            "skipped"
        },
        if report.checked_activity {
            format!(
                " (new: {}, comments: {}, reviews: {}, failing checks: {})",
                if report.new_activity { "yes" } else { "no" },
                report.comments.len(),
                report.reviews.len(),
                report.failing_checks.len()
            )
        } else {
            String::new()
        }
    );
    out!("Decision: {}", report.decision.summary);
    out!(
        "Dispatch: {}{}",
        report.dispatch.status.as_str(),
        report
            .dispatch
            .session_id
            .map(|id| format!(" (session {id})"))
            .unwrap_or_default()
    );
    if let Some(path) = &report.prompt_path {
        out!("Prompt: {path}");
    }
    for command in &report.next_commands {
        out!("run: {command}");
    }
    Ok(())
}

pub(super) fn run_pull_request_watch(parsed: Parsed) -> Result<(), UsageError> {
    if parsed.positional.first().map(String::as_str) != Some("pr") {
        return Err(UsageError::Message(
            "watch requires `pr` followed by start, list, show, poll, tick, batches, ack, pause, resume, or stop"
                .into(),
        ));
    }
    let action = parsed
        .positional
        .get(1)
        .map(String::as_str)
        .ok_or_else(|| {
            UsageError::Message(
                "watch pr requires start, list, show, poll, tick, batches, ack, pause, resume, or stop"
                    .into(),
            )
        })?;
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    match action {
        "monitoring" => {
            let mode = parsed
                .positional
                .get(2)
                .map(String::as_str)
                .ok_or_else(|| {
                    UsageError::Message(
                        "watch pr monitoring requires activate, deactivate, or status".into(),
                    )
                })?;
            let session_id = parsed.session.ok_or_else(|| {
                UsageError::Message("watch pr monitoring requires --session <id>".into())
            })?;
            // Proves the session exists before recording a flag against it.
            broker.store().session(session_id)?;
            let root = broker.main_root().to_path_buf();
            let io = |result: std::io::Result<()>| -> Result<(), UsageError> {
                result.map_err(|error| {
                    UsageError::Message(format!("cannot record PR monitoring state: {error}"))
                })
            };
            match mode {
                "activate" => io(crate::activate_pr_monitoring(&root, session_id))?,
                "deactivate" => io(crate::deactivate_pr_monitoring(&root, session_id))?,
                "status" => {}
                other => {
                    return Err(UsageError::Message(format!(
                        "unknown watch pr monitoring mode {other:?}; expected activate, deactivate, or status"
                    )));
                }
            }
            let active = crate::pr_monitoring_is_active(&root, session_id);
            if parsed.json {
                out!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "session_id": session_id,
                        "pr_monitoring_active": active,
                    }))?
                );
            } else if active {
                out!("session {session_id}: PR monitoring active");
            } else {
                out!("session {session_id}: PR monitoring off");
            }
        }
        "start" => {
            let session = parsed.session.ok_or_else(|| {
                UsageError::Message("watch pr start requires --session <id>".into())
            })?;
            let repository = parsed.repository.as_deref().ok_or_else(|| {
                UsageError::Message("watch pr start requires --repo <owner/name>".into())
            })?;
            let pr_number = parsed.pr_number.ok_or_else(|| {
                UsageError::Message("watch pr start requires --pr <number>".into())
            })?;
            let event_kinds = parse_pull_request_event_kinds(parsed.events.as_deref())?;
            let watch = broker.start_pull_request_watch(
                session,
                repository,
                pr_number,
                event_kinds,
                parsed
                    .seconds
                    .unwrap_or(crate::DEFAULT_PR_WATCH_INTERVAL_SECONDS),
                &crate::GithubCliPullRequestWatchProvider,
                now_ms(),
            )?;
            render_pull_request_watch(&watch, parsed.json)?;
        }
        "list" => {
            let watches = broker.pull_request_watches(parsed.all)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&watches)?);
            } else if watches.is_empty() {
                out!("No pull request watches.");
            } else {
                for watch in watches {
                    render_pull_request_watch(&watch, false)?;
                }
            }
        }
        "show" => {
            let id = parsed.note_id.ok_or_else(|| {
                UsageError::Message("watch pr show requires --id <watch-id>".into())
            })?;
            render_pull_request_watch(&broker.pull_request_watch(id)?, parsed.json)?;
        }
        "poll" => {
            let id = parsed.note_id.ok_or_else(|| {
                UsageError::Message("watch pr poll requires --id <watch-id>".into())
            })?;
            let report = broker.poll_pull_request_watch(
                id,
                &crate::GithubCliPullRequestWatchProvider,
                now_ms(),
            )?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                out!(
                    "Watch {} polled {}#{} at {}: {} metadata item(s), {}.",
                    report.watch.id,
                    report.watch.display_repository,
                    report.watch.pr_number,
                    short_commit(&report.watch.head_sha),
                    report.activity_count,
                    if report.changed {
                        if report.new_activity_count > 0 {
                            "new activity batched"
                        } else {
                            "metadata changed"
                        }
                    } else {
                        "no change"
                    },
                );
            }
        }
        "tick" => {
            let limit = parsed
                .limit
                .map(|limit| limit as usize)
                .unwrap_or(crate::DEFAULT_PR_SCHEDULER_LIMIT);
            let report = broker.tick_pull_request_watches(
                &crate::GithubCliPullRequestWatchProvider,
                now_ms(),
                limit,
            )?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                out!(
                    "PR scheduler tick: {} due, {} polled, {} failed, {} deferred.",
                    report.due_watch_count,
                    report.successful_watch_count,
                    report.failed_watch_count,
                    report.deferred_watch_count,
                );
                if let Some(retry_at) = report.rate_limit_until {
                    out!("Provider rate limit: retry no earlier than {retry_at}.");
                }
                match report.next_tick_at {
                    Some(next) => out!("Next due tick: {next}."),
                    None => out!("Next due tick: none."),
                }
            }
        }
        "batches" => {
            let watch_id = parsed.note_id.ok_or_else(|| {
                UsageError::Message("watch pr batches requires --id <watch-id>".into())
            })?;
            let batches = broker.pull_request_activity_batches(watch_id, parsed.all)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&batches)?);
            } else if batches.is_empty() {
                out!("No pull request activity batches.");
            } else {
                for batch in batches {
                    out!(
                        "Batch {}: watch {}, {} metadata item(s), {} at {}",
                        batch.id,
                        batch.watch_id,
                        batch.activities.len(),
                        batch.status.as_str(),
                        short_commit(&batch.head_sha),
                    );
                }
            }
        }
        "ack" => {
            let batch_id = parsed.note_id.ok_or_else(|| {
                UsageError::Message("watch pr ack requires --id <batch-id>".into())
            })?;
            let outcome = match parsed.outcome.as_deref() {
                Some("addressed") => crate::PullRequestBatchAckOutcome::Addressed,
                Some("stale") => crate::PullRequestBatchAckOutcome::Stale,
                Some("non-actionable" | "non_actionable") => {
                    crate::PullRequestBatchAckOutcome::NonActionable
                }
                Some("superseded") => crate::PullRequestBatchAckOutcome::Superseded,
                _ => {
                    return Err(UsageError::Message(
                        "watch pr ack requires --outcome addressed|stale|non-actionable|superseded"
                            .into(),
                    ));
                }
            };
            let reason = parsed.reason.as_deref().ok_or_else(|| {
                UsageError::Message("watch pr ack requires --reason <text>".into())
            })?;
            let batch = broker.acknowledge_pull_request_activity_batch(
                batch_id,
                outcome,
                reason,
                now_ms(),
            )?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&batch)?);
            } else {
                out!("Batch {} acknowledged as {}.", batch.id, outcome.as_str());
            }
        }
        "pause" | "resume" | "stop" => {
            let id = parsed.note_id.ok_or_else(|| {
                UsageError::Message(format!("watch pr {action} requires --id <watch-id>"))
            })?;
            let status = match action {
                "pause" => crate::PullRequestWatchStatus::Paused,
                "resume" => crate::PullRequestWatchStatus::Active,
                "stop" => crate::PullRequestWatchStatus::Stopped,
                _ => unreachable!(),
            };
            let watch = broker.set_pull_request_watch_status(id, status, now_ms())?;
            render_pull_request_watch(&watch, parsed.json)?;
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown watch pr action {other:?} — expected start, list, show, poll, tick, batches, ack, pause, resume, or stop"
            )));
        }
    }
    Ok(())
}

pub(super) fn run_deliveries(parsed: Parsed) -> Result<(), UsageError> {
    let action = parsed
        .positional
        .first()
        .map(String::as_str)
        .ok_or_else(|| {
            UsageError::Message("deliveries requires subscribe, list, claim, or complete".into())
        })?;
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    match action {
        "dispatch" => {
            let adapter = parsed.adapter.as_deref().unwrap_or("chau7");
            let worker = parsed.worker.as_deref().ok_or_else(|| {
                UsageError::Message("deliveries dispatch requires --worker <id>".into())
            })?;
            let seconds = parsed.seconds.unwrap_or(120);
            let claim = broker.claim_next_delivery(adapter, worker, seconds, now_ms())?;
            let Some(envelope) = claim.delivery else {
                if parsed.json {
                    out!("{}", serde_json::json!({"claimed": false}));
                } else {
                    out!("no delivery pending for adapter {adapter}");
                }
                return Ok(());
            };
            let session = broker.store().session(envelope.watch.session_id)?;
            let raw = match parsed.tabs_file.as_deref() {
                Some(path) => std::fs::read_to_string(path).map_err(|error| {
                    UsageError::Message(format!("cannot read {}: {error}", path.display()))
                })?,
                None => {
                    use std::io::Read;
                    let mut buffer = String::new();
                    std::io::stdin()
                        .read_to_string(&mut buffer)
                        .map_err(|error| {
                            UsageError::Message(format!("cannot read tabs from stdin: {error}"))
                        })?;
                    buffer
                }
            };
            let tabs: Vec<crate::Chau7Tab> = serde_json::from_str(&raw).map_err(|error| {
                UsageError::Message(format!(
                    "tab snapshot is not a Chau7 tab_list array: {error}"
                ))
            })?;
            let action = crate::dispatch_action(
                &tabs,
                &session.worktree_path,
                &session.branch,
                &envelope.prompt,
            );
            let resolved_tab_id = match &action {
                crate::Chau7DispatchAction::Send { tab_id, .. }
                | crate::Chau7DispatchAction::Defer { tab_id, .. } => Some(tab_id.as_str()),
                crate::Chau7DispatchAction::Abandon { .. } => None,
            };
            if let Some(tab) =
                resolved_tab_id.and_then(|tab_id| tabs.iter().find(|tab| tab.tab_id == tab_id))
            {
                broker
                    .store()
                    .update_session_context(session.id, &tab.session_context())?;
            }
            // Deferral and abandonment are terminal for this claim, so the
            // broker completes them. A send stays open: only the caller knows
            // whether the transport actually landed.
            match &action {
                crate::Chau7DispatchAction::Defer { why, .. } => {
                    broker.complete_delivery(
                        envelope.item.id,
                        worker,
                        envelope.item.generation,
                        crate::DeliveryCompletion::Retry,
                        Some("tab_not_ready"),
                        now_ms(),
                    )?;
                    if !parsed.json {
                        out!("deferred delivery {}: {why}", envelope.item.id);
                    }
                }
                crate::Chau7DispatchAction::Abandon { why } => {
                    broker.complete_delivery(
                        envelope.item.id,
                        worker,
                        envelope.item.generation,
                        crate::DeliveryCompletion::Failed,
                        Some("tab_unresolvable"),
                        now_ms(),
                    )?;
                    if !parsed.json {
                        out!("abandoned delivery {}: {why}", envelope.item.id);
                    }
                }
                crate::Chau7DispatchAction::Send { tab_id, .. } => {
                    if !parsed.json {
                        out!("send delivery {} to {tab_id}", envelope.item.id);
                        out!(
                            "  complete with: aethyme broker deliveries complete --id {} --worker {worker} --generation {} --outcome delivered",
                            envelope.item.id,
                            envelope.item.generation
                        );
                    }
                }
            }
            if parsed.json {
                out!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "claimed": true,
                        "delivery_id": envelope.item.id,
                        "generation": envelope.item.generation,
                        "session_id": envelope.watch.session_id,
                        "action": action,
                    }))?
                );
            }
        }
        "resolve-tab" => {
            let session_id = parsed.session.ok_or_else(|| {
                UsageError::Message("deliveries resolve-tab requires --session <id>".into())
            })?;
            let session = broker.store().session(session_id)?;
            let raw = match parsed.tabs_file.as_deref() {
                Some(path) => std::fs::read_to_string(path).map_err(|error| {
                    UsageError::Message(format!("cannot read {}: {error}", path.display()))
                })?,
                None => {
                    use std::io::Read;
                    let mut buffer = String::new();
                    std::io::stdin()
                        .read_to_string(&mut buffer)
                        .map_err(|error| {
                            UsageError::Message(format!("cannot read tabs from stdin: {error}"))
                        })?;
                    buffer
                }
            };
            let tabs: Vec<crate::Chau7Tab> = serde_json::from_str(&raw).map_err(|error| {
                UsageError::Message(format!(
                    "tab snapshot is not a Chau7 tab_list array: {error}"
                ))
            })?;
            let outcome =
                crate::resolve_session_tab(&tabs, &session.worktree_path, &session.branch);
            if let Ok(resolution) = &outcome
                && let Some(tab) = tabs.iter().find(|tab| tab.tab_id == resolution.tab_id)
            {
                broker
                    .store()
                    .update_session_context(session.id, &tab.session_context())?;
            }
            if parsed.json {
                let body = match &outcome {
                    Ok(resolution) => serde_json::json!({"resolved": resolution}),
                    Err(refusal) => serde_json::json!({"refused": refusal}),
                };
                out!("{}", serde_json::to_string_pretty(&body)?);
            } else {
                match &outcome {
                    Ok(resolution) => out!(
                        "session {} -> {} ({:?}{})",
                        session_id,
                        resolution.tab_id,
                        resolution.readiness,
                        if resolution.mcp_controlled {
                            ", mcp-controlled"
                        } else {
                            ""
                        }
                    ),
                    Err(refusal) => out!("refused: {refusal:?}"),
                }
            }
            if outcome.is_err() {
                std::process::exit(2);
            }
        }
        "subscribe" => {
            let watch_id = parsed.watch_id.ok_or_else(|| {
                UsageError::Message("deliveries subscribe requires --watch <id>".into())
            })?;
            let adapter = parsed.adapter.as_deref().ok_or_else(|| {
                UsageError::Message("deliveries subscribe requires --adapter <name>".into())
            })?;
            let target = parsed.target.as_deref().ok_or_else(|| {
                UsageError::Message("deliveries subscribe requires --target <opaque-id>".into())
            })?;
            let policy = parse_delivery_policy(parsed.policy.as_deref())?;
            let subscription = broker.subscribe_pull_request_delivery(
                watch_id,
                adapter,
                target,
                policy,
                now_ms(),
            )?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&subscription)?);
            } else {
                out!(
                    "Delivery subscription {}: watch {}, adapter {}, target {}, policy {}.",
                    subscription.id,
                    subscription.watch_id,
                    subscription.adapter,
                    subscription.target,
                    subscription.policy.as_str(),
                );
            }
        }
        "list" => {
            let items = broker.delivery_outbox(parsed.adapter.as_deref(), parsed.all)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&items)?);
            } else if items.is_empty() {
                out!("No delivery outbox items.");
            } else {
                for item in items {
                    out!(
                        "Delivery {}: batch {}, subscription {}, {}, generation {}, attempts {}",
                        item.id,
                        item.batch_id,
                        item.subscription_id,
                        item.status.as_str(),
                        item.generation,
                        item.attempt_count,
                    );
                }
            }
        }
        "claim" => {
            let adapter = parsed.adapter.as_deref().ok_or_else(|| {
                UsageError::Message("deliveries claim requires --adapter <name>".into())
            })?;
            let worker = parsed.worker.as_deref().ok_or_else(|| {
                UsageError::Message("deliveries claim requires --worker <id>".into())
            })?;
            let report = broker.claim_next_delivery(
                adapter,
                worker,
                parsed
                    .seconds
                    .unwrap_or(crate::DEFAULT_DELIVERY_CLAIM_SECONDS),
                now_ms(),
            )?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else if let Some(delivery) = report.delivery {
                out!(
                    "Claimed delivery {} generation {} for {}. Use --json to read its structured envelope and prompt.",
                    delivery.item.id,
                    delivery.item.generation,
                    delivery.subscription.target,
                );
            } else {
                out!("No pending delivery for adapter {adapter}.");
            }
        }
        "complete" => {
            let id = parsed.note_id.ok_or_else(|| {
                UsageError::Message("deliveries complete requires --id <delivery-id>".into())
            })?;
            let worker = parsed.worker.as_deref().ok_or_else(|| {
                UsageError::Message("deliveries complete requires --worker <id>".into())
            })?;
            let generation = parsed.generation.ok_or_else(|| {
                UsageError::Message("deliveries complete requires --generation <n>".into())
            })?;
            let completion = match parsed.outcome.as_deref() {
                Some("delivered") => crate::DeliveryCompletion::Delivered,
                Some("retry") => crate::DeliveryCompletion::Retry,
                Some("failed") => crate::DeliveryCompletion::Failed,
                _ => {
                    return Err(UsageError::Message(
                        "deliveries complete requires --outcome delivered|retry|failed".into(),
                    ));
                }
            };
            let item = broker.complete_delivery(
                id,
                worker,
                generation,
                completion,
                parsed.error_code.as_deref(),
                now_ms(),
            )?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&item)?);
            } else {
                out!("Delivery {} is {}.", item.id, item.status.as_str());
            }
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown deliveries action {other:?} — expected subscribe, list, claim, or complete"
            )));
        }
    }
    Ok(())
}

pub(super) fn parse_delivery_policy(
    value: Option<&str>,
) -> Result<crate::DeliveryPolicy, UsageError> {
    match value.unwrap_or("notify") {
        "notify" => Ok(crate::DeliveryPolicy::Notify),
        "resume" => Ok(crate::DeliveryPolicy::Resume),
        "review-and-push" | "review_and_push" => Ok(crate::DeliveryPolicy::ReviewAndPush),
        value => Err(UsageError::Message(format!(
            "unknown delivery policy {value:?}; expected notify, resume, or review-and-push"
        ))),
    }
}

pub(super) fn parse_repository_delivery_mode(
    value: Option<&str>,
) -> Result<Option<crate::RepositoryDeliveryMode>, UsageError> {
    value
        .map(crate::RepositoryDeliveryMode::parse)
        .transpose()
        .map_err(UsageError::Message)
}

pub(super) fn parse_pull_request_event_kinds(
    value: Option<&str>,
) -> Result<Vec<crate::PullRequestActivityKind>, UsageError> {
    let value = value.unwrap_or("comments,reviews,checks");
    let mut kinds = Vec::new();
    for item in value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        let kind = match item {
            "comment" | "comments" => crate::PullRequestActivityKind::Comment,
            "review" | "reviews" => crate::PullRequestActivityKind::Review,
            "check" | "checks" => crate::PullRequestActivityKind::Check,
            _ => {
                return Err(UsageError::Message(format!(
                    "unknown pull request event kind {item:?}; expected comments, reviews, or checks"
                )));
            }
        };
        kinds.push(kind);
    }
    if kinds.is_empty() {
        return Err(UsageError::Message(
            "--events must select comments, reviews, or checks".into(),
        ));
    }
    kinds.sort();
    kinds.dedup();
    Ok(kinds)
}

pub(super) fn render_pull_request_watch(
    watch: &crate::PullRequestWatch,
    json: bool,
) -> Result<(), UsageError> {
    if json {
        out!("{}", serde_json::to_string_pretty(watch)?);
    } else {
        out!(
            "Watch {}: {}#{} {} at {} (session {}, every {}s)",
            watch.id,
            watch.display_repository,
            watch.pr_number,
            watch.status.as_str(),
            short_commit(&watch.head_sha),
            watch.session_id,
            watch.poll_interval_seconds,
        );
    }
    Ok(())
}

/// `broker pr`.
pub(super) fn run_pr(parsed: Parsed) -> Result<(), UsageError> {
    let action = parsed
        .positional
        .first()
        .map(String::as_str)
        .ok_or(UsageError::Message("pr requires an action: check".into()))?;
    match action {
        "check" => {
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let report = broker.pr_check(crate::PrCheckOptions {
                target_branch: parsed.target.unwrap_or_else(|| "production".into()),
                pr_number: parsed.pr_number,
                agent_name: parsed.agent.unwrap_or_else(|| "Push2prod".into()),
                dispatch: parsed.dispatch,
                agent_command: parsed.cmd,
                now_ms: now_ms(),
            })?;
            render_pr_check_report(&report, parsed.json)?;
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown pr action {other:?} — expected check"
            )));
        }
    }
    Ok(())
}
