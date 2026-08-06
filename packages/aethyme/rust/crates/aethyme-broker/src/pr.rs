//! Pull-request follow-up observation and routing.
//!
//! The broker owns the deterministic part: read PR state, classify the
//! marker in the PR body, detect new activity since the last broker run,
//! and prepare/dispatch a bounded agent prompt. The agent owns the
//! subjective part: deciding which comments require code changes and
//! making/replying to those changes.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::broker::{AgentView, Broker, BrokerOpError};
use crate::error::BrokerError;
use crate::types::{NewPrWatchState, SessionStatus};

#[derive(Debug, thiserror::Error)]
pub enum PrError {
    #[error("failed to run gh {args}: {source}")]
    Spawn {
        args: String,
        source: std::io::Error,
    },

    #[error("gh {args} failed: {stderr}")]
    Gh { args: String, stderr: String },

    #[error("gh returned invalid json for {args}: {source}")]
    Json {
        args: String,
        source: serde_json::Error,
    },
}

#[derive(Debug, Clone)]
pub struct PrCheckOptions {
    pub target_branch: String,
    pub pr_number: Option<i64>,
    pub agent_name: String,
    pub dispatch: bool,
    pub agent_command: Option<String>,
    pub now_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrMarker {
    ThumbsUp,
    Looking,
    None,
}

impl PrMarker {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ThumbsUp => "thumbs_up",
            Self::Looking => "looking",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrDecisionStatus {
    NoOpenPr,
    AllGoodMarker,
    NoNewActivity,
    NewActivityNoAction,
    NeedsAgent,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PrDecision {
    pub status: PrDecisionStatus,
    pub should_check_activity: bool,
    pub should_dispatch: bool,
    pub summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrDispatchStatus {
    NotRequested,
    NotNeeded,
    ExistingSessionPromptReady,
    SpawnedSession,
}

impl PrDispatchStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotRequested => "not_requested",
            Self::NotNeeded => "not_needed",
            Self::ExistingSessionPromptReady => "existing_session_prompt_ready",
            Self::SpawnedSession => "spawned_session",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PrDispatchReport {
    pub status: PrDispatchStatus,
    pub session_id: Option<i64>,
    pub prompt_path: Option<String>,
    pub command: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PrSummary {
    pub number: i64,
    pub title: String,
    pub url: Option<String>,
    pub head_branch: String,
    pub head_oid: Option<String>,
    pub base_branch: String,
    pub is_draft: bool,
    pub review_decision: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PrActivityItem {
    pub kind: String,
    pub id: String,
    pub author: Option<String>,
    pub state: Option<String>,
    pub body_preview: Option<String>,
    pub url: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PrCheckRun {
    pub name: String,
    pub workflow: Option<String>,
    pub status: Option<String>,
    pub conclusion: Option<String>,
    pub details_url: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PrCheckReport {
    pub target_branch: String,
    pub head_branch: String,
    pub pr: Option<PrSummary>,
    pub marker: PrMarker,
    pub checked_activity: bool,
    pub previous_fingerprint: Option<String>,
    pub activity_fingerprint: Option<String>,
    pub new_activity: bool,
    pub comments: Vec<PrActivityItem>,
    pub reviews: Vec<PrActivityItem>,
    pub checks: Vec<PrCheckRun>,
    pub failing_checks: Vec<PrCheckRun>,
    pub decision: PrDecision,
    pub prompt_path: Option<String>,
    pub dispatch: PrDispatchReport,
    pub next_commands: Vec<String>,
}

pub fn check_pr_followup(
    broker: &mut Broker,
    options: PrCheckOptions,
) -> Result<PrCheckReport, BrokerOpError> {
    let main_root = broker.main_root_path();
    let head_branch = broker.repo_handle().current_branch()?;
    let Some(pr_value) = load_pr(&main_root, &head_branch, &options)? else {
        return Ok(no_open_pr_report(&options.target_branch, &head_branch));
    };
    let pr = pr_summary(&pr_value);
    let marker = marker_from_body(str_value(&pr_value, "body").unwrap_or_default());
    let checked_activity = marker != PrMarker::ThumbsUp;
    let comments = if checked_activity {
        activity_items(&pr_value, "comments", "comment")
    } else {
        Vec::new()
    };
    let reviews = if checked_activity {
        activity_items(&pr_value, "reviews", "review")
    } else {
        Vec::new()
    };
    let checks = if checked_activity {
        check_runs(&pr_value)
    } else {
        Vec::new()
    };
    let failing_checks = checks
        .iter()
        .filter(|check| check_is_failing(check))
        .cloned()
        .collect::<Vec<_>>();
    let activity_fingerprint = if checked_activity {
        Some(activity_fingerprint(&comments, &reviews, &checks))
    } else {
        None
    };
    let previous = broker
        .store()
        .pr_watch_state(&options.target_branch, pr.number)?;
    let previous_fingerprint = previous
        .as_ref()
        .map(|state| state.activity_fingerprint.clone());
    let new_activity = checked_activity
        && activity_fingerprint
            .as_ref()
            .map(|fingerprint| previous_fingerprint.as_deref() != Some(fingerprint.as_str()))
            .unwrap_or(false);
    let has_actionable_surface =
        !comments.is_empty() || !reviews.is_empty() || !failing_checks.is_empty();
    let should_dispatch = checked_activity && new_activity && has_actionable_surface;
    let decision = decision(marker, checked_activity, new_activity, should_dispatch);

    let prompt = if should_dispatch {
        Some(render_agent_prompt(
            &options,
            &pr,
            &comments,
            &reviews,
            &failing_checks,
        ))
    } else {
        None
    };
    let prompt_path = match prompt.as_deref() {
        Some(prompt) => Some(write_prompt(&main_root, pr.number, options.now_ms, prompt)?),
        None => None,
    };
    let dispatch = if should_dispatch {
        dispatch_to_agent(broker, &options, &pr, prompt_path.as_ref())?
    } else {
        PrDispatchReport {
            status: PrDispatchStatus::NotNeeded,
            session_id: None,
            prompt_path: prompt_path.as_ref().map(path_string),
            command: None,
            message: decision.summary.clone(),
        }
    };

    if should_persist_cursor(should_dispatch, options.dispatch) {
        broker.store().upsert_pr_watch_state(&NewPrWatchState {
            target_branch: options.target_branch.clone(),
            pr_number: pr.number,
            activity_fingerprint: activity_fingerprint.clone().unwrap_or_else(|| {
                previous
                    .as_ref()
                    .map(|state| state.activity_fingerprint.clone())
                    .unwrap_or_default()
            }),
            marker: marker.as_str().to_string(),
            last_dispatch_at: if matches!(
                dispatch.status,
                PrDispatchStatus::ExistingSessionPromptReady | PrDispatchStatus::SpawnedSession
            ) {
                Some(options.now_ms)
            } else {
                previous.as_ref().and_then(|state| state.last_dispatch_at)
            },
            last_agent_session_id: dispatch.session_id.or_else(|| {
                previous
                    .as_ref()
                    .and_then(|state| state.last_agent_session_id)
            }),
        })?;
    }

    let next_commands = next_commands(&options, &decision);

    Ok(PrCheckReport {
        target_branch: options.target_branch,
        head_branch,
        pr: Some(pr),
        marker,
        checked_activity,
        previous_fingerprint,
        activity_fingerprint,
        new_activity,
        comments,
        reviews,
        checks,
        failing_checks,
        decision,
        prompt_path: prompt_path.as_ref().map(path_string),
        dispatch,
        next_commands,
    })
}

fn should_persist_cursor(should_dispatch: bool, dispatch_requested: bool) -> bool {
    !should_dispatch || dispatch_requested
}

fn no_open_pr_report(target_branch: &str, head_branch: &str) -> PrCheckReport {
    PrCheckReport {
        target_branch: target_branch.to_string(),
        head_branch: head_branch.to_string(),
        pr: None,
        marker: PrMarker::None,
        checked_activity: false,
        previous_fingerprint: None,
        activity_fingerprint: None,
        new_activity: false,
        comments: Vec::new(),
        reviews: Vec::new(),
        checks: Vec::new(),
        failing_checks: Vec::new(),
        decision: PrDecision {
            status: PrDecisionStatus::NoOpenPr,
            should_check_activity: false,
            should_dispatch: false,
            summary: format!(
                "no open PR found for branch {head_branch:?} targeting {target_branch:?}"
            ),
        },
        prompt_path: None,
        dispatch: PrDispatchReport {
            status: PrDispatchStatus::NotNeeded,
            session_id: None,
            prompt_path: None,
            command: None,
            message: "no open PR to inspect".into(),
        },
        next_commands: vec![format!(
            "gh pr list --state open --base {} --head {}",
            shell_word(target_branch),
            shell_word(head_branch)
        )],
    }
}

fn decision(
    marker: PrMarker,
    checked_activity: bool,
    new_activity: bool,
    should_dispatch: bool,
) -> PrDecision {
    if marker == PrMarker::ThumbsUp {
        return PrDecision {
            status: PrDecisionStatus::AllGoodMarker,
            should_check_activity: false,
            should_dispatch: false,
            summary: "PR body has thumbs-up marker; broker treats it as all good".into(),
        };
    }
    if !checked_activity {
        return PrDecision {
            status: PrDecisionStatus::NoNewActivity,
            should_check_activity: false,
            should_dispatch: false,
            summary: "activity check skipped".into(),
        };
    }
    if !new_activity {
        return PrDecision {
            status: PrDecisionStatus::NoNewActivity,
            should_check_activity: true,
            should_dispatch: false,
            summary: "no new PR activity since the last broker check".into(),
        };
    }
    if !should_dispatch {
        return PrDecision {
            status: PrDecisionStatus::NewActivityNoAction,
            should_check_activity: true,
            should_dispatch: false,
            summary: "new PR activity detected, but no actionable comments/reviews/check failures were found"
                .into(),
        };
    }
    PrDecision {
        status: PrDecisionStatus::NeedsAgent,
        should_check_activity: true,
        should_dispatch,
        summary: "new PR activity needs Push2prod follow-up".into(),
    }
}

fn next_commands(options: &PrCheckOptions, decision: &PrDecision) -> Vec<String> {
    if decision.should_dispatch && !options.dispatch {
        return vec![format!(
            "aethyme broker pr check --target {}{} --dispatch",
            shell_word(&options.target_branch),
            options
                .pr_number
                .map(|number| format!(" --pr {number}"))
                .unwrap_or_default()
        )];
    }
    if decision.status == PrDecisionStatus::NoOpenPr {
        return vec![format!(
            "gh pr list --state open --base {}",
            shell_word(&options.target_branch)
        )];
    }
    Vec::new()
}

fn dispatch_to_agent(
    broker: &mut Broker,
    options: &PrCheckOptions,
    pr: &PrSummary,
    prompt_path: Option<&PathBuf>,
) -> Result<PrDispatchReport, BrokerOpError> {
    let prompt_path = prompt_path.expect("dispatch requires a prompt path");
    if !options.dispatch {
        return Ok(PrDispatchReport {
            status: PrDispatchStatus::NotRequested,
            session_id: None,
            prompt_path: Some(path_string(prompt_path)),
            command: None,
            message: "dispatch not requested; prompt is ready on disk".into(),
        });
    }

    if let Some(agent) = find_existing_agent(broker, &options.agent_name, options.now_ms)? {
        return Ok(PrDispatchReport {
            status: PrDispatchStatus::ExistingSessionPromptReady,
            session_id: Some(agent.session.id),
            prompt_path: Some(path_string(prompt_path)),
            command: None,
            message: format!(
                "existing {} session {} matched; prompt is ready on disk",
                options.agent_name, agent.session.id
            ),
        });
    }

    let task = format!("{} PR #{} follow-up", options.agent_name, pr.number);
    let command = options.agent_command.clone().unwrap_or_else(|| {
        format!(
            "codex exec --dangerously-bypass-approvals-and-sandbox \"$(cat {})\"",
            shell_word(&prompt_path.to_string_lossy())
        )
    });
    let session = broker.start_agent(&task, &command)?;
    Ok(PrDispatchReport {
        status: PrDispatchStatus::SpawnedSession,
        session_id: Some(session.id),
        prompt_path: Some(path_string(prompt_path)),
        command: Some(command),
        message: format!("spawned {} session {}", options.agent_name, session.id),
    })
}

fn find_existing_agent(
    broker: &mut Broker,
    agent_name: &str,
    now_ms: i64,
) -> Result<Option<AgentView>, BrokerOpError> {
    let agent_name = agent_name.to_lowercase();
    let mut agents = broker.agents(now_ms)?;
    agents.sort_by_key(|agent| match agent.derived_status {
        SessionStatus::Active => 0,
        SessionStatus::Idle => 1,
        SessionStatus::Stale => 2,
        SessionStatus::Exited => 3,
        SessionStatus::Cleaned => 4,
    });
    Ok(agents.into_iter().find(|agent| {
        matches!(
            agent.derived_status,
            SessionStatus::Active | SessionStatus::Idle
        ) && session_matches_agent(agent, &agent_name)
    }))
}

fn session_matches_agent(agent: &AgentView, needle: &str) -> bool {
    let haystacks = [
        agent.session.task.as_deref(),
        agent.session.command.as_deref(),
        Some(agent.session.branch.as_str()),
    ];
    haystacks.into_iter().flatten().any(|value| {
        let value = value.to_lowercase();
        value.contains(needle)
    })
}

fn write_prompt(
    main_root: &Path,
    pr_number: i64,
    now_ms: i64,
    prompt: &str,
) -> Result<PathBuf, BrokerError> {
    let dir = main_root.join(".aethyme/run/pr-follow-up");
    std::fs::create_dir_all(&dir).map_err(|source| BrokerError::Io {
        path: dir.clone(),
        source,
    })?;
    let path = dir.join(format!("pr-{pr_number}-{now_ms}.md"));
    std::fs::write(&path, prompt).map_err(|source| BrokerError::Io {
        path: path.clone(),
        source,
    })?;
    Ok(path)
}

fn render_agent_prompt(
    options: &PrCheckOptions,
    pr: &PrSummary,
    comments: &[PrActivityItem],
    reviews: &[PrActivityItem],
    failing_checks: &[PrCheckRun],
) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "You are {} for PR #{} targeting {}.",
        options.agent_name, pr.number, options.target_branch
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "PR: {}", pr.title);
    if let Some(url) = &pr.url {
        let _ = writeln!(out, "URL: {url}");
    }
    let _ = writeln!(out, "Head branch: {}", pr.head_branch);
    let _ = writeln!(out, "Base branch: {}", pr.base_branch);
    let _ = writeln!(out);
    let _ = writeln!(out, "Task:");
    let _ = writeln!(
        out,
        "1. Fetch the latest PR comments, review threads, reviews, and checks."
    );
    let _ = writeln!(out, "2. Identify actionable feedback and failing checks.");
    let _ = writeln!(
        out,
        "3. Fix the code, add or adjust focused tests, commit, and push."
    );
    let _ = writeln!(
        out,
        "4. Reply to addressed comments and resolve threads only when the fix is actually present."
    );
    let _ = writeln!(out, "5. Report what changed and any remaining blocker.");
    let _ = writeln!(out);
    let _ = writeln!(out, "Suggested commands:");
    let _ = writeln!(out, "gh pr view {} --comments", pr.number);
    let _ = writeln!(out, "gh pr checks {} --watch=false", pr.number);
    let _ = writeln!(out);
    if !comments.is_empty() {
        let _ = writeln!(out, "Recent comments observed by broker:");
        for item in comments.iter().take(10) {
            write_activity_line(&mut out, item);
        }
        let _ = writeln!(out);
    }
    if !reviews.is_empty() {
        let _ = writeln!(out, "Recent reviews observed by broker:");
        for item in reviews.iter().take(10) {
            write_activity_line(&mut out, item);
        }
        let _ = writeln!(out);
    }
    if !failing_checks.is_empty() {
        let _ = writeln!(out, "Failing checks observed by broker:");
        for check in failing_checks.iter().take(10) {
            let _ = writeln!(
                out,
                "- {} status={} conclusion={}",
                check.name,
                check.status.as_deref().unwrap_or("-"),
                check.conclusion.as_deref().unwrap_or("-")
            );
            if let Some(url) = &check.details_url {
                let _ = writeln!(out, "  {url}");
            }
        }
    }
    out
}

fn write_activity_line(out: &mut String, item: &PrActivityItem) {
    let _ = writeln!(
        out,
        "- {} {} by {} state={} updated={}",
        item.kind,
        item.id,
        item.author.as_deref().unwrap_or("-"),
        item.state.as_deref().unwrap_or("-"),
        item.updated_at.as_deref().unwrap_or("-")
    );
    if let Some(url) = &item.url {
        let _ = writeln!(out, "  {url}");
    }
    if let Some(preview) = &item.body_preview {
        let _ = writeln!(out, "  {preview}");
    }
}

fn load_pr(
    main_root: &Path,
    head_branch: &str,
    options: &PrCheckOptions,
) -> Result<Option<Value>, PrError> {
    if let Some(number) = options.pr_number {
        return run_gh_json(
            main_root,
            &[
                "pr".into(),
                "view".into(),
                number.to_string(),
                "--json".into(),
                PR_VIEW_FIELDS.into(),
            ],
        )
        .map(Some);
    }

    let listed = run_gh_json(
        main_root,
        &[
            "pr".into(),
            "list".into(),
            "--state".into(),
            "open".into(),
            "--base".into(),
            options.target_branch.clone(),
            "--head".into(),
            head_branch.to_string(),
            "--limit".into(),
            "1".into(),
            "--json".into(),
            PR_LIST_FIELDS.into(),
        ],
    )?;
    let Some(first) = listed.as_array().and_then(|items| items.first()) else {
        return Ok(None);
    };
    let number = i64_value(first, "number").unwrap_or_default();
    if number <= 0 {
        return Ok(None);
    }
    run_gh_json(
        main_root,
        &[
            "pr".into(),
            "view".into(),
            number.to_string(),
            "--json".into(),
            PR_VIEW_FIELDS.into(),
        ],
    )
    .map(Some)
}

const PR_LIST_FIELDS: &str =
    "number,title,url,body,headRefName,headRefOid,baseRefName,isDraft,reviewDecision,updatedAt";

const PR_VIEW_FIELDS: &str = "number,title,url,body,headRefName,headRefOid,baseRefName,isDraft,\
reviewDecision,updatedAt,comments,reviews,statusCheckRollup";

fn run_gh_json(cwd: &Path, args: &[String]) -> Result<Value, PrError> {
    let output = Command::new("gh")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|source| PrError::Spawn {
            args: args.join(" "),
            source,
        })?;
    if !output.status.success() {
        return Err(PrError::Gh {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    serde_json::from_slice(&output.stdout).map_err(|source| PrError::Json {
        args: args.join(" "),
        source,
    })
}

fn pr_summary(value: &Value) -> PrSummary {
    PrSummary {
        number: i64_value(value, "number").unwrap_or_default(),
        title: str_value(value, "title").unwrap_or_default().to_string(),
        url: str_value(value, "url").map(str::to_string),
        head_branch: str_value(value, "headRefName")
            .unwrap_or_default()
            .to_string(),
        head_oid: str_value(value, "headRefOid").map(str::to_string),
        base_branch: str_value(value, "baseRefName")
            .unwrap_or_default()
            .to_string(),
        is_draft: value
            .get("isDraft")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        review_decision: str_value(value, "reviewDecision").map(str::to_string),
        updated_at: str_value(value, "updatedAt").map(str::to_string),
    }
}

fn marker_from_body(body: &str) -> PrMarker {
    let lower = body.to_lowercase();
    if body.contains('\u{1f44d}') || lower.contains(":+1:") || lower.contains(":thumbsup:") {
        PrMarker::ThumbsUp
    } else if body.contains('\u{1f440}') || lower.contains(":eyes:") {
        PrMarker::Looking
    } else {
        PrMarker::None
    }
}

fn activity_items(value: &Value, field: &str, kind: &str) -> Vec<PrActivityItem> {
    value
        .get(field)
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .map(|item| PrActivityItem {
            kind: kind.to_string(),
            id: activity_id(item),
            author: author_login(item),
            state: str_value(item, "state").map(str::to_string),
            body_preview: str_value(item, "body").and_then(preview),
            url: str_value(item, "url").map(str::to_string),
            updated_at: str_value(item, "updatedAt")
                .or_else(|| str_value(item, "submittedAt"))
                .or_else(|| str_value(item, "createdAt"))
                .map(str::to_string),
        })
        .collect()
}

fn activity_id(value: &Value) -> String {
    str_value(value, "id")
        .map(str::to_string)
        .or_else(|| i64_value(value, "databaseId").map(|id| id.to_string()))
        .or_else(|| i64_value(value, "number").map(|id| id.to_string()))
        .unwrap_or_else(|| "-".into())
}

fn check_runs(value: &Value) -> Vec<PrCheckRun> {
    value
        .get("statusCheckRollup")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .map(|item| {
            let context = item.get("context").unwrap_or(item);
            PrCheckRun {
                name: str_value(item, "name")
                    .or_else(|| str_value(context, "name"))
                    .or_else(|| str_value(context, "context"))
                    .unwrap_or("-")
                    .to_string(),
                workflow: str_value(item, "workflowName")
                    .or_else(|| str_value(context, "workflowName"))
                    .map(str::to_string),
                status: str_value(item, "status")
                    .or_else(|| str_value(context, "state"))
                    .map(str::to_string),
                conclusion: str_value(item, "conclusion")
                    .or_else(|| str_value(context, "conclusion"))
                    .map(str::to_string),
                details_url: str_value(item, "detailsUrl")
                    .or_else(|| str_value(context, "detailsUrl"))
                    .or_else(|| str_value(context, "targetUrl"))
                    .map(str::to_string),
            }
        })
        .collect()
}

fn check_is_failing(check: &PrCheckRun) -> bool {
    let status = check.status.as_deref().unwrap_or("").to_ascii_uppercase();
    let conclusion = check
        .conclusion
        .as_deref()
        .unwrap_or("")
        .to_ascii_uppercase();
    if !conclusion.is_empty() {
        return !matches!(conclusion.as_str(), "SUCCESS" | "SKIPPED" | "NEUTRAL");
    }
    matches!(
        status.as_str(),
        "ERROR" | "FAILURE" | "FAILED" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED"
    )
}

fn activity_fingerprint(
    comments: &[PrActivityItem],
    reviews: &[PrActivityItem],
    checks: &[PrCheckRun],
) -> String {
    let mut parts = Vec::new();
    for item in comments.iter().chain(reviews) {
        parts.push(format!(
            "{}:{}:{}:{}",
            item.kind,
            item.id,
            item.updated_at.as_deref().unwrap_or("-"),
            item.state.as_deref().unwrap_or("-")
        ));
    }
    for check in checks {
        parts.push(format!(
            "check:{}:{}:{}",
            check.name,
            check.status.as_deref().unwrap_or("-"),
            check.conclusion.as_deref().unwrap_or("-")
        ));
    }
    parts.sort();
    stable_hash_hex(&parts.join("\n"))
}

fn stable_hash_hex(text: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn author_login(value: &Value) -> Option<String> {
    value
        .get("author")
        .and_then(|author| str_value(author, "login"))
        .map(str::to_string)
}

fn preview(body: &str) -> Option<String> {
    let first = body.lines().find(|line| !line.trim().is_empty())?.trim();
    let mut chars = first.chars();
    let capped = chars.by_ref().take(160).collect::<String>();
    if chars.next().is_some() {
        Some(format!("{capped}..."))
    } else {
        Some(capped)
    }
}

fn str_value<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value.get(field).and_then(serde_json::Value::as_str)
}

fn i64_value(value: &Value, field: &str) -> Option<i64> {
    value.get(field).and_then(serde_json::Value::as_i64)
}

fn path_string(path: &PathBuf) -> String {
    path.to_string_lossy().into_owned()
}

fn shell_word(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr_marker_detection_supports_unicode_and_aliases() {
        assert_eq!(marker_from_body("ship it \u{1f44d}"), PrMarker::ThumbsUp);
        assert_eq!(marker_from_body("ship it :+1:"), PrMarker::ThumbsUp);
        assert_eq!(marker_from_body("watching :eyes:"), PrMarker::Looking);
        assert_eq!(marker_from_body("needs review"), PrMarker::None);
    }

    #[test]
    fn pr_activity_fingerprint_is_order_insensitive() {
        let a = PrActivityItem {
            kind: "comment".into(),
            id: "1".into(),
            author: Some("alice".into()),
            state: None,
            body_preview: Some("first".into()),
            url: None,
            updated_at: Some("2026-07-31T10:00:00Z".into()),
        };
        let b = PrActivityItem {
            kind: "review".into(),
            id: "2".into(),
            author: Some("bob".into()),
            state: Some("CHANGES_REQUESTED".into()),
            body_preview: None,
            url: None,
            updated_at: Some("2026-07-31T11:00:00Z".into()),
        };

        let first = activity_fingerprint(&[a.clone()], &[b.clone()], &[]);
        let reordered = activity_fingerprint(&[], &[b, a], &[]);
        assert_eq!(first, reordered);
    }

    #[test]
    fn pr_check_failure_detection_treats_failed_conclusions_as_actionable() {
        let failed = PrCheckRun {
            name: "ci".into(),
            workflow: None,
            status: Some("COMPLETED".into()),
            conclusion: Some("FAILURE".into()),
            details_url: None,
        };
        let skipped = PrCheckRun {
            name: "lint".into(),
            workflow: None,
            status: Some("COMPLETED".into()),
            conclusion: Some("SKIPPED".into()),
            details_url: None,
        };
        let pending = PrCheckRun {
            name: "deploy".into(),
            workflow: None,
            status: Some("IN_PROGRESS".into()),
            conclusion: None,
            details_url: None,
        };
        let status_context_failure = PrCheckRun {
            name: "legacy".into(),
            workflow: None,
            status: Some("FAILURE".into()),
            conclusion: None,
            details_url: None,
        };
        assert!(check_is_failing(&failed));
        assert!(!check_is_failing(&skipped));
        assert!(!check_is_failing(&pending));
        assert!(check_is_failing(&status_context_failure));
    }

    #[test]
    fn pr_cursor_waits_for_dispatch_when_actionable_activity_is_new() {
        assert!(!should_persist_cursor(true, false));
        assert!(should_persist_cursor(true, true));
        assert!(should_persist_cursor(false, false));
    }
}
