//! Durable, provider-neutral pull-request observation.
//!
//! Providers return metadata only. Comment and review bodies remain at the
//! provider and must be retrieved explicitly by the receiving agent.

use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Broker, BrokerOpError, resolve_github_target};

pub const PULL_REQUEST_WATCH_SCHEMA_VERSION: u32 = 1;
pub const PULL_REQUEST_SCHEDULER_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_PR_WATCH_INTERVAL_SECONDS: u64 = 60;
pub const DEFAULT_PR_SCHEDULER_LIMIT: usize = 32;
pub const MAX_PR_SCHEDULER_LIMIT: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestActivityKind {
    Comment,
    Review,
    Check,
}

impl PullRequestActivityKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Comment => "comment",
            Self::Review => "review",
            Self::Check => "check",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, crate::BrokerError> {
        match value {
            "comment" => Ok(Self::Comment),
            "review" => Ok(Self::Review),
            "check" => Ok(Self::Check),
            _ => Err(crate::BrokerError::InvalidEnumValue {
                field: "pull_request_activities.kind",
                value: value.into(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestWatchStatus {
    Active,
    Paused,
    Completed,
    Stopped,
}

impl PullRequestWatchStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Stopped => "stopped",
        }
    }

    pub(crate) fn parse(value: String) -> rusqlite::Result<Self> {
        match value.as_str() {
            "active" => Ok(Self::Active),
            "paused" => Ok(Self::Paused),
            "completed" => Ok(Self::Completed),
            "stopped" => Ok(Self::Stopped),
            _ => Err(rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                format!("invalid pull request watch status {value:?}").into(),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestActivityMetadata {
    pub kind: PullRequestActivityKind,
    pub provider_id: String,
    pub author: Option<String>,
    pub state: Option<String>,
    pub url: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestActivity {
    pub id: i64,
    pub watch_id: i64,
    #[serde(flatten)]
    pub metadata: PullRequestActivityMetadata,
    pub first_seen_at: i64,
    pub last_seen_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestBatchStatus {
    Pending,
    Acknowledged,
}

impl PullRequestBatchStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Acknowledged => "acknowledged",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, crate::BrokerError> {
        match value {
            "pending" => Ok(Self::Pending),
            "acknowledged" => Ok(Self::Acknowledged),
            _ => Err(crate::BrokerError::InvalidEnumValue {
                field: "pull_request_activity_batches.status",
                value: value.into(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestBatchAckOutcome {
    Addressed,
    Stale,
    NonActionable,
    Superseded,
}

impl PullRequestBatchAckOutcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Addressed => "addressed",
            Self::Stale => "stale",
            Self::NonActionable => "non_actionable",
            Self::Superseded => "superseded",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, crate::BrokerError> {
        match value {
            "addressed" => Ok(Self::Addressed),
            "stale" => Ok(Self::Stale),
            "non_actionable" => Ok(Self::NonActionable),
            "superseded" => Ok(Self::Superseded),
            _ => Err(crate::BrokerError::InvalidEnumValue {
                field: "pull_request_activity_batches.ack_outcome",
                value: value.into(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestActivityBatch {
    pub id: i64,
    pub watch_id: i64,
    pub head_sha: String,
    pub digest: String,
    pub activities: Vec<PullRequestActivity>,
    pub status: PullRequestBatchStatus,
    pub ack_outcome: Option<PullRequestBatchAckOutcome>,
    pub ack_reason_digest: Option<String>,
    pub created_at: i64,
    pub acknowledged_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestSnapshot {
    pub number: i64,
    pub title: String,
    pub url: String,
    pub state: String,
    pub target_branch: String,
    pub head_branch: String,
    pub head_sha: String,
    pub is_draft: bool,
    /// When the provider says the pull request was created, in Unix
    /// milliseconds.
    ///
    /// The only trustworthy source for how long a pull request existed:
    /// `created_at` on a watch row is when *Aethyme* started watching, which is
    /// always after the pull request opened, and `observed_at` on an
    /// observation row is a record of one poll. A pull request that opened and
    /// closed between two watches has no local record of ever existing.
    pub created_at_ms: Option<i64>,
    /// When the provider says it was merged, in Unix milliseconds.
    ///
    /// Written once, from the first poll that sees `mergedAt`; a later poll
    /// reports the same instant, so this does not drift.
    pub merged_at_ms: Option<i64>,
    pub activities: Vec<PullRequestActivityMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestWatchRequest {
    pub repository: String,
    pub pr_number: i64,
    pub event_kinds: Vec<PullRequestActivityKind>,
}

pub trait PullRequestWatchProvider {
    fn inspect(
        &self,
        request: &PullRequestWatchRequest,
    ) -> Result<PullRequestSnapshot, PullRequestWatchError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct GithubCliPullRequestWatchProvider;

#[derive(Debug, thiserror::Error)]
pub enum PullRequestWatchError {
    #[error("watch tick provider budget exhausted; observation deferred")]
    TickBudgetExhausted,
    #[error("invalid pull request watch: {0}")]
    Invalid(String),
    #[error("pull request watch {0} was not found")]
    NotFound(i64),
    #[error("pull request watch provider failed to start: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("pull request watch provider failed: {0}")]
    Provider(String),
    #[error("pull request watch provider returned invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone)]
pub struct NewPullRequestWatch {
    pub session_id: i64,
    pub provider: String,
    pub canonical_repository: String,
    pub display_repository: String,
    pub pr_number: i64,
    pub target_branch: String,
    pub head_sha: String,
    pub is_draft: bool,
    pub event_kinds: Vec<PullRequestActivityKind>,
    pub poll_interval_seconds: u64,
    pub cursor_digest: String,
    pub baseline_activities: Vec<PullRequestActivityMetadata>,
    pub now_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestWatch {
    pub schema_version: u32,
    pub id: i64,
    pub session_id: i64,
    pub provider: String,
    pub canonical_repository: String,
    pub display_repository: String,
    pub pr_number: i64,
    pub target_branch: String,
    pub head_sha: String,
    pub is_draft: bool,
    pub status: PullRequestWatchStatus,
    pub event_kinds: Vec<PullRequestActivityKind>,
    pub poll_interval_seconds: u64,
    pub cursor_digest: String,
    pub last_polled_at: Option<i64>,
    pub next_poll_at: Option<i64>,
    pub last_error_code: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestWatchPollReport {
    pub watch: PullRequestWatch,
    pub changed: bool,
    pub activity_count: usize,
    pub new_activity_count: usize,
    pub batch: Option<PullRequestActivityBatch>,
    pub previous_cursor_digest: String,
    pub observed_cursor_digest: String,
    pub pr_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestSchedulerDisposition {
    Polled,
    Failed,
    DeferredByRateLimit,
    DeferredByBudget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestSchedulerWatchResult {
    pub watch_id: i64,
    pub canonical_repository: String,
    pub pr_number: i64,
    pub due_at: i64,
    pub attempted: bool,
    pub disposition: PullRequestSchedulerDisposition,
    pub error_code: Option<String>,
    pub retry_at: Option<i64>,
    pub poll: Option<PullRequestWatchPollReport>,
}

/// Stable, adapter-neutral result of one explicit foreground scheduling tick.
///
/// A host scheduler decides when to invoke a tick. The broker performs no
/// background network activity and returns enough state for launchd, systemd,
/// Chau7, or another supervisor to schedule the next invocation safely.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestSchedulerTickReport {
    pub schema_version: u32,
    pub tick_at: i64,
    pub limit: usize,
    pub due_watch_count: usize,
    pub attempted_watch_count: usize,
    pub successful_watch_count: usize,
    pub failed_watch_count: usize,
    pub deferred_watch_count: usize,
    pub rate_limit_until: Option<i64>,
    pub next_tick_at: Option<i64>,
    pub results: Vec<PullRequestSchedulerWatchResult>,
}

pub(crate) struct PullRequestWatchPollStorageResult {
    pub watch: PullRequestWatch,
    pub batch: Option<PullRequestActivityBatch>,
}

impl PullRequestWatchProvider for GithubCliPullRequestWatchProvider {
    fn inspect(
        &self,
        request: &PullRequestWatchRequest,
    ) -> Result<PullRequestSnapshot, PullRequestWatchError> {
        Self::inspect_with_deadline(request, std::time::Duration::from_secs(3))
    }
}

impl GithubCliPullRequestWatchProvider {
    pub(crate) fn inspect_with_deadline(
        request: &PullRequestWatchRequest,
        deadline: std::time::Duration,
    ) -> Result<PullRequestSnapshot, PullRequestWatchError> {
        // `createdAt` and `mergedAt` are requested for `broker advanced
        // insights`, not for the snapshot: they are the provider's own record
        // of when the pull request opened and merged, and no amount of polling
        // can recover either one after the fact — the first poll is already too
        // late to observe the opening, and a merged pull request reports
        // `MERGED` on every poll thereafter.
        let fields = "number,title,url,state,baseRefName,baseRefOid,headRefName,headRefOid,isDraft,mergeStateStatus,comments,reviews,statusCheckRollup,createdAt,mergedAt";
        let mut command = Command::new("gh");
        command.args([
            "pr",
            "view",
            &request.pr_number.to_string(),
            "--repo",
            &request.repository,
            "--json",
            fields,
        ]);
        let output =
            match crate::bounded_output::output_limited(&mut command, deadline, 4 * 1024 * 1024)? {
                crate::bounded_output::LimitedOutput::Completed(output) => output,
                crate::bounded_output::LimitedOutput::Deadline => {
                    return Err(PullRequestWatchError::Provider(
                        "PR observation exceeded its deadline".into(),
                    ));
                }
                crate::bounded_output::LimitedOutput::OutputLimit => {
                    return Err(PullRequestWatchError::Provider(
                        "PR observation exceeded its output budget".into(),
                    ));
                }
            };
        if !output.status.success() {
            return Err(PullRequestWatchError::Provider(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        parse_github_snapshot(&output.stdout, &request.event_kinds)
    }
}

pub(crate) fn activity_digest(snapshot: &PullRequestSnapshot) -> String {
    let mut activities = snapshot.activities.clone();
    activities.sort_by(|a, b| {
        (a.kind, &a.provider_id, &a.updated_at).cmp(&(b.kind, &b.provider_id, &b.updated_at))
    });
    let bytes = serde_json::to_vec(&(
        &snapshot.head_sha,
        &snapshot.state,
        &snapshot.is_draft,
        activities,
    ))
    .expect("serializable pull request activity metadata");
    format!("{:x}", Sha256::digest(bytes))
}

fn parse_github_snapshot(
    bytes: &[u8],
    selected: &[PullRequestActivityKind],
) -> Result<PullRequestSnapshot, PullRequestWatchError> {
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    let string = |field: &str| {
        value
            .get(field)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let head_sha = string("headRefOid").to_ascii_lowercase();
    if head_sha.len() != 40 || !head_sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(PullRequestWatchError::Invalid(
            "provider must return a full 40-character head SHA".into(),
        ));
    }
    let mut activities = Vec::new();
    if selected.contains(&PullRequestActivityKind::Comment) {
        append_github_items(
            &mut activities,
            &value,
            "comments",
            PullRequestActivityKind::Comment,
        );
    }
    if selected.contains(&PullRequestActivityKind::Review) {
        append_github_items(
            &mut activities,
            &value,
            "reviews",
            PullRequestActivityKind::Review,
        );
    }
    if selected.contains(&PullRequestActivityKind::Check) {
        // Mergeability is a provider check observation, not an acceptance rule.
        // Include both immutable tips: an unchanged author head can become
        // conflicting when the base advances. Unknown/pending data is not clean.
        let merge_state = json_string(&value, "mergeStateStatus");
        let base_sha = json_string(&value, "baseRefOid").unwrap_or_default();
        if let Some(state @ ("DIRTY" | "BEHIND" | "CLEAN" | "BLOCKED" | "UNSTABLE" | "HAS_HOOKS")) =
            merge_state
            && base_sha.len() == 40
            && base_sha.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            let stable = serde_json::to_vec(&(&head_sha, base_sha.to_ascii_lowercase(), state))
                .expect("serializable merge state");
            activities.push(PullRequestActivityMetadata {
                kind: PullRequestActivityKind::Check,
                provider_id: format!("github:merge-state:{:x}", Sha256::digest(stable)),
                author: None,
                state: Some(format!("merge_state:{state}")),
                url: json_string(&value, "url").map(str::to_string),
                updated_at: None,
            });
        }
        for check in value
            .get("statusCheckRollup")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            // Rollup nodes are either CheckRun or legacy StatusContext.
            // https://docs.github.com/en/graphql/reference/commits#statuscontext
            let field = |name| json_string(check, name).filter(|text| !text.is_empty());
            let legacy = field("__typename") == Some("StatusContext")
                || (field("context").is_some() && field("name").is_none());
            let name = field("name")
                .or_else(|| field("context"))
                .unwrap_or("check");
            let url = field("detailsUrl")
                .or_else(|| field("targetUrl"))
                .map(str::to_string);
            let state = field("conclusion")
                .or_else(|| field("status"))
                .or_else(|| field("state"))
                .map(str::to_string);
            let provider_id = if legacy {
                // Legacy status URLs often stay the same across commits. A
                // prior failure must not suppress a failure on a later head.
                let stable = serde_json::to_vec(&(&head_sha, name, &url, &state))
                    .expect("serializable status context");
                format!("status-context:{:x}", Sha256::digest(stable))
            } else {
                // Keep existing CheckRun identities to avoid replaying them.
                let stable = serde_json::to_vec(&(name, &url, &state)).expect("serializable check");
                format!("check:{:x}", Sha256::digest(stable))
            };
            activities.push(PullRequestActivityMetadata {
                kind: PullRequestActivityKind::Check,
                provider_id,
                author: None,
                state,
                url,
                updated_at: None,
            });
        }
    }
    activities.sort_by(|a, b| (a.kind, &a.provider_id).cmp(&(b.kind, &b.provider_id)));
    Ok(PullRequestSnapshot {
        number: value
            .get("number")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or_default(),
        title: string("title"),
        url: string("url"),
        state: string("state").to_ascii_lowercase(),
        target_branch: string("baseRefName"),
        head_branch: string("headRefName"),
        head_sha,
        is_draft: value
            .get("isDraft")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        // GitHub reports ISO-8601 with a `Z` offset. Anything else is left as
        // unknown rather than guessed: a mis-parsed instant is worse than an
        // absent one, because it looks like a measurement.
        created_at_ms: json_timestamp_ms(&value, "createdAt"),
        merged_at_ms: json_timestamp_ms(&value, "mergedAt"),
        activities,
    })
}

/// Parse a GitHub ISO-8601 timestamp into Unix milliseconds.
///
/// Accepts only the shape GitHub documents for these fields: `YYYY-MM-DD` plus
/// `THH:MM:SS` and an optional fractional part, always `Z`. Returns `None` for
/// anything else, including `null` — which is how GitHub reports a pull request
/// that has not merged yet.
fn json_timestamp_ms(value: &serde_json::Value, field: &str) -> Option<i64> {
    let raw = value.get(field)?.as_str()?;
    parse_iso8601_utc_ms(raw)
}

/// `2026-09-30T20:28:04Z` (or with fractional seconds) to Unix milliseconds.
pub(crate) fn parse_iso8601_utc_ms(raw: &str) -> Option<i64> {
    let (date, time) = raw.split_once('T')?;
    if !time.ends_with('Z') {
        // A non-UTC offset would need real offset arithmetic; these fields are
        // documented as UTC and an offset means something upstream changed.
        return None;
    }
    let time = time.trim_end_matches('Z');
    let time = time.split_once('.').map_or(time, |(whole, _)| whole);

    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let mut time_parts = time.split(':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let minute: i64 = time_parts.next()?.parse().ok()?;
    let second: i64 = time_parts.next()?.parse().ok()?;
    if time_parts.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let days = days_from_civil(year, month, day);
    Some((days * 86_400 + hour * 3_600 + minute * 60 + second.min(59)) * 1_000)
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
///
/// The inverse of [`crate::cli::insights`]'s `civil_from_days`, kept here so
/// the provider timestamp parsing does not depend on a CLI module. March is
/// treated as month 0 so the leap day lands at the end of the year and no
/// separate correction is needed.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let month = i64::from(month);
    let day = i64::from(day);
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_of_year = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_of_year + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn append_github_items(
    output: &mut Vec<PullRequestActivityMetadata>,
    value: &serde_json::Value,
    field: &str,
    kind: PullRequestActivityKind,
) {
    for item in value
        .get(field)
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        let provider_id = json_string(item, "id")
            .or_else(|| json_string(item, "url"))
            .unwrap_or_default();
        if provider_id.is_empty() {
            continue;
        }
        output.push(PullRequestActivityMetadata {
            kind,
            provider_id: provider_id.to_string(),
            author: item
                .get("author")
                .and_then(|author| json_string(author, "login"))
                .map(str::to_string),
            state: json_string(item, "state").map(str::to_string),
            url: json_string(item, "url").map(str::to_string),
            updated_at: json_string(item, "updatedAt")
                .or_else(|| json_string(item, "submittedAt"))
                .or_else(|| json_string(item, "createdAt"))
                .map(str::to_string),
        });
    }
}

fn json_string<'a>(value: &'a serde_json::Value, field: &str) -> Option<&'a str> {
    value.get(field).and_then(serde_json::Value::as_str)
}

impl Broker {
    /// Automatic association never resets an existing cursor or paused choice.
    pub(crate) fn ensure_session_pull_request_watch(
        &mut self,
        session_id: i64,
        repository: &str,
        pr_number: i64,
        provider: &dyn PullRequestWatchProvider,
        now_ms: i64,
    ) -> Result<i64, BrokerOpError> {
        let target = resolve_github_target(repository, &[])?;
        if let Some(existing) = self
            .store_ref()
            .live_pull_request_watch_for_identity(&target.coordination_key, pr_number)?
        {
            if existing.session_id != session_id {
                return Err(PullRequestWatchError::Invalid(format!(
                    "PR #{pr_number} is already watched by session {}",
                    existing.session_id
                ))
                .into());
            }
            return Ok(existing.id);
        }
        self.start_pull_request_watch(
            session_id,
            repository,
            pr_number,
            vec![
                PullRequestActivityKind::Comment,
                PullRequestActivityKind::Review,
                PullRequestActivityKind::Check,
            ],
            DEFAULT_PR_WATCH_INTERVAL_SECONDS,
            provider,
            now_ms,
        )
        .map(|watch| watch.id)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn start_pull_request_watch(
        &mut self,
        session_id: i64,
        repository: &str,
        pr_number: i64,
        event_kinds: Vec<PullRequestActivityKind>,
        poll_interval_seconds: u64,
        provider: &dyn PullRequestWatchProvider,
        now_ms: i64,
    ) -> Result<PullRequestWatch, BrokerOpError> {
        let session = self.store().session(session_id)?;
        if session.status.is_closed() {
            return Err(
                PullRequestWatchError::Invalid(format!("session {session_id} is closed")).into(),
            );
        }
        validate_watch_input(pr_number, &event_kinds, poll_interval_seconds)?;
        let target = resolve_github_target(repository, &[])?;
        let request = PullRequestWatchRequest {
            repository: target.display_slug.clone(),
            pr_number,
            event_kinds: normalize_event_kinds(event_kinds),
        };
        let snapshot = provider.inspect(&request)?;
        if snapshot.number != pr_number || snapshot.state != "open" {
            return Err(PullRequestWatchError::Invalid(format!(
                "PR #{pr_number} must exist and be open"
            ))
            .into());
        }
        let cursor_digest = activity_digest(&snapshot);
        let watch = self
            .store()
            .insert_pull_request_watch(&NewPullRequestWatch {
                session_id,
                provider: "github".into(),
                canonical_repository: target.coordination_key,
                display_repository: target.display_slug,
                pr_number,
                target_branch: snapshot.target_branch,
                head_sha: snapshot.head_sha,
                is_draft: snapshot.is_draft,
                event_kinds: request.event_kinds,
                poll_interval_seconds,
                cursor_digest,
                baseline_activities: snapshot.activities,
                now_ms,
            })?;
        Ok(watch)
    }

    pub fn pull_request_watches(
        &self,
        include_terminal: bool,
    ) -> Result<Vec<PullRequestWatch>, BrokerOpError> {
        Ok(self.store_ref().pull_request_watches(include_terminal)?)
    }

    pub fn pull_request_watch(&self, id: i64) -> Result<PullRequestWatch, BrokerOpError> {
        self.store_ref()
            .pull_request_watch(id)?
            .ok_or_else(|| PullRequestWatchError::NotFound(id).into())
    }

    pub fn poll_pull_request_watch(
        &mut self,
        id: i64,
        provider: &dyn PullRequestWatchProvider,
        now_ms: i64,
    ) -> Result<PullRequestWatchPollReport, BrokerOpError> {
        let current = self.pull_request_watch(id)?;
        if current.status != PullRequestWatchStatus::Active {
            return Err(PullRequestWatchError::Invalid(format!(
                "watch {id} is {}; only active watches can be polled",
                current.status.as_str()
            ))
            .into());
        }
        let owner = self.store().session(current.session_id)?;
        if owner.status.is_closed() {
            self.store().update_pull_request_watch_status(
                id,
                PullRequestWatchStatus::Paused,
                now_ms,
                Some("owner_session_closed"),
            )?;
            return Err(PullRequestWatchError::Invalid(format!(
                "owner session {} is closed; watch {id} was paused",
                current.session_id
            ))
            .into());
        }
        let snapshot = provider.inspect(&PullRequestWatchRequest {
            repository: current.display_repository.clone(),
            pr_number: current.pr_number,
            event_kinds: current.event_kinds.clone(),
        })?;
        let digest = activity_digest(&snapshot);
        let status = if snapshot.state == "open" {
            PullRequestWatchStatus::Active
        } else {
            PullRequestWatchStatus::Completed
        };
        let result = self
            .store()
            .record_pull_request_watch_poll(id, &snapshot, &digest, status, now_ms)?;
        Ok(PullRequestWatchPollReport {
            changed: digest != current.cursor_digest,
            activity_count: snapshot.activities.len(),
            new_activity_count: result
                .batch
                .as_ref()
                .map_or(0, |batch| batch.activities.len()),
            batch: result.batch,
            previous_cursor_digest: current.cursor_digest,
            observed_cursor_digest: digest,
            pr_url: snapshot.url,
            watch: result.watch,
        })
    }

    /// Polls a bounded, deterministic prefix of watches which are due now.
    ///
    /// Provider failures are converted into persisted, bounded retry state so
    /// an external scheduler never needs to parse stderr or invent backoff.
    /// A rate-limit response defers the remainder of this tick without making
    /// additional provider calls.
    pub fn tick_pull_request_watches(
        &mut self,
        provider: &dyn PullRequestWatchProvider,
        now_ms: i64,
        limit: usize,
    ) -> Result<PullRequestSchedulerTickReport, BrokerOpError> {
        if !(1..=MAX_PR_SCHEDULER_LIMIT).contains(&limit) {
            return Err(PullRequestWatchError::Invalid(format!(
                "scheduler limit must be between 1 and {MAX_PR_SCHEDULER_LIMIT}"
            ))
            .into());
        }

        let mut due = self
            .pull_request_watches(false)?
            .into_iter()
            .filter(|watch| {
                watch.status == PullRequestWatchStatus::Active
                    && watch.next_poll_at.is_some_and(|due_at| due_at <= now_ms)
            })
            .collect::<Vec<_>>();
        due.sort_by(|left, right| {
            (
                left.next_poll_at,
                &left.canonical_repository,
                left.pr_number,
                left.id,
            )
                .cmp(&(
                    right.next_poll_at,
                    &right.canonical_repository,
                    right.pr_number,
                    right.id,
                ))
        });
        let due_watch_count = due.len();
        let mut results = Vec::new();
        let mut rate_limit_until = None;

        for watch in due.into_iter().take(limit) {
            let due_at = watch.next_poll_at.unwrap_or(now_ms);
            if let Some(retry_at) = rate_limit_until {
                let updated = self.store().record_pull_request_watch_failure(
                    watch.id,
                    "rate_limited_shared",
                    retry_at,
                    now_ms,
                    false,
                )?;
                results.push(PullRequestSchedulerWatchResult {
                    watch_id: watch.id,
                    canonical_repository: watch.canonical_repository,
                    pr_number: watch.pr_number,
                    due_at,
                    attempted: false,
                    disposition: PullRequestSchedulerDisposition::DeferredByRateLimit,
                    error_code: updated.last_error_code,
                    retry_at: updated.next_poll_at,
                    poll: None,
                });
                continue;
            }

            match self.poll_pull_request_watch(watch.id, provider, now_ms) {
                Ok(poll) => results.push(PullRequestSchedulerWatchResult {
                    watch_id: watch.id,
                    canonical_repository: watch.canonical_repository,
                    pr_number: watch.pr_number,
                    due_at,
                    attempted: true,
                    disposition: PullRequestSchedulerDisposition::Polled,
                    error_code: None,
                    retry_at: poll.watch.next_poll_at,
                    poll: Some(poll),
                }),
                Err(BrokerOpError::PullRequestWatch(
                    PullRequestWatchError::TickBudgetExhausted,
                )) => {
                    // No provider dispatch occurred. Preserve the due cursor and
                    // previous error verbatim so the next tick can resume.
                    results.push(PullRequestSchedulerWatchResult {
                        watch_id: watch.id,
                        canonical_repository: watch.canonical_repository,
                        pr_number: watch.pr_number,
                        due_at,
                        attempted: false,
                        disposition: PullRequestSchedulerDisposition::DeferredByBudget,
                        error_code: Some("tick_budget_exhausted".into()),
                        retry_at: watch.next_poll_at,
                        poll: None,
                    });
                }
                Err(BrokerOpError::PullRequestWatch(error)) => {
                    let (error_code, is_rate_limit, retry_at) =
                        scheduler_failure(&error, &watch, now_ms);
                    let current = self.pull_request_watch(watch.id)?;
                    let retry_at = if current.status == PullRequestWatchStatus::Active {
                        let updated = self.store().record_pull_request_watch_failure(
                            watch.id, error_code, retry_at, now_ms, true,
                        )?;
                        updated.next_poll_at
                    } else {
                        current.next_poll_at
                    };
                    if is_rate_limit {
                        rate_limit_until = retry_at;
                    }
                    results.push(PullRequestSchedulerWatchResult {
                        watch_id: watch.id,
                        canonical_repository: watch.canonical_repository,
                        pr_number: watch.pr_number,
                        due_at,
                        attempted: true,
                        disposition: PullRequestSchedulerDisposition::Failed,
                        error_code: Some(error_code.into()),
                        retry_at,
                        poll: None,
                    });
                }
                Err(error) => return Err(error),
            }
        }

        let next_tick_at = self
            .pull_request_watches(false)?
            .into_iter()
            .filter(|watch| watch.status == PullRequestWatchStatus::Active)
            .filter_map(|watch| watch.next_poll_at)
            .min();
        Ok(PullRequestSchedulerTickReport {
            schema_version: PULL_REQUEST_SCHEDULER_SCHEMA_VERSION,
            tick_at: now_ms,
            limit,
            due_watch_count,
            attempted_watch_count: results.iter().filter(|item| item.attempted).count(),
            successful_watch_count: results
                .iter()
                .filter(|item| item.disposition == PullRequestSchedulerDisposition::Polled)
                .count(),
            failed_watch_count: results
                .iter()
                .filter(|item| item.disposition == PullRequestSchedulerDisposition::Failed)
                .count(),
            deferred_watch_count: results
                .iter()
                .filter(|item| {
                    matches!(
                        item.disposition,
                        PullRequestSchedulerDisposition::DeferredByRateLimit
                            | PullRequestSchedulerDisposition::DeferredByBudget
                    )
                })
                .count(),
            rate_limit_until,
            next_tick_at,
            results,
        })
    }

    pub fn pull_request_activity_batches(
        &self,
        watch_id: i64,
        include_acknowledged: bool,
    ) -> Result<Vec<PullRequestActivityBatch>, BrokerOpError> {
        self.pull_request_watch(watch_id)?;
        Ok(self
            .store_ref()
            .pull_request_activity_batches(watch_id, include_acknowledged)?)
    }

    pub fn acknowledge_pull_request_activity_batch(
        &mut self,
        batch_id: i64,
        outcome: PullRequestBatchAckOutcome,
        reason: &str,
        now_ms: i64,
    ) -> Result<PullRequestActivityBatch, BrokerOpError> {
        let reason = reason.trim();
        if reason.is_empty() {
            return Err(PullRequestWatchError::Invalid(
                "batch acknowledgment requires a non-empty classification reason".into(),
            )
            .into());
        }
        let reason_digest = format!("{:x}", Sha256::digest(reason.as_bytes()));
        Ok(self.store().acknowledge_pull_request_activity_batch(
            batch_id,
            outcome,
            &reason_digest,
            now_ms,
        )?)
    }

    pub fn set_pull_request_watch_status(
        &mut self,
        id: i64,
        status: PullRequestWatchStatus,
        now_ms: i64,
    ) -> Result<PullRequestWatch, BrokerOpError> {
        let current = self.pull_request_watch(id)?;
        if matches!(
            current.status,
            PullRequestWatchStatus::Completed | PullRequestWatchStatus::Stopped
        ) {
            return Err(PullRequestWatchError::Invalid(format!(
                "terminal watch {id} cannot be resumed or paused"
            ))
            .into());
        }
        Ok(self
            .store()
            .update_pull_request_watch_status(id, status, now_ms, None)?)
    }
}

fn scheduler_failure(
    error: &PullRequestWatchError,
    watch: &PullRequestWatch,
    now_ms: i64,
) -> (&'static str, bool, i64) {
    let base_seconds = watch.poll_interval_seconds.max(15);
    let (code, rate_limited, retry_seconds) = match error {
        PullRequestWatchError::TickBudgetExhausted => {
            unreachable!("handled before failure recording")
        }
        PullRequestWatchError::Spawn(_) => ("provider_unavailable", false, base_seconds * 2),
        PullRequestWatchError::Json(_) => ("invalid_provider_payload", false, 300),
        PullRequestWatchError::Invalid(_) | PullRequestWatchError::NotFound(_) => {
            ("watch_invalid", false, 300)
        }
        PullRequestWatchError::Provider(detail) => {
            let detail = detail.to_ascii_lowercase();
            if detail.contains("rate limit") || detail.contains("secondary rate") {
                ("rate_limited", true, 900)
            } else if detail.contains("auth")
                || detail.contains("login")
                || detail.contains("credential")
                || detail.contains("http 401")
                || detail.contains("http 403")
            {
                ("authentication_failed", false, 300)
            } else {
                ("provider_error", false, base_seconds * 2)
            }
        }
    };
    (
        code,
        rate_limited,
        now_ms + retry_seconds.clamp(30, 900) as i64 * 1_000,
    )
}

fn validate_watch_input(
    pr_number: i64,
    event_kinds: &[PullRequestActivityKind],
    poll_interval_seconds: u64,
) -> Result<(), PullRequestWatchError> {
    if pr_number <= 0 {
        return Err(PullRequestWatchError::Invalid(
            "PR number must be positive".into(),
        ));
    }
    if event_kinds.is_empty() {
        return Err(PullRequestWatchError::Invalid(
            "at least one event kind is required".into(),
        ));
    }
    if !(15..=3600).contains(&poll_interval_seconds) {
        return Err(PullRequestWatchError::Invalid(
            "poll interval must be between 15 and 3600 seconds".into(),
        ));
    }
    Ok(())
}

fn normalize_event_kinds(mut kinds: Vec<PullRequestActivityKind>) -> Vec<PullRequestActivityKind> {
    kinds.sort();
    kinds.dedup();
    kinds
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_snapshot_excludes_provider_bodies() {
        let body = br#"{
          "number":7,"title":"Review","url":"https://github.com/o/r/pull/7","state":"OPEN",
          "baseRefName":"main","headRefName":"topic","headRefOid":"0123456789abcdef0123456789abcdef01234567","isDraft":true,
          "comments":[{"id":"C1","body":"untrusted secret","author":{"login":"alice"},"url":"https://example/c1","updatedAt":"2026-01-01"}],
          "reviews":[],"statusCheckRollup":[]
        }"#;
        let snapshot = parse_github_snapshot(body, &[PullRequestActivityKind::Comment]).unwrap();
        let encoded = serde_json::to_string(&snapshot).unwrap();
        assert!(!encoded.contains("untrusted secret"));
        assert_eq!(snapshot.activities[0].provider_id, "C1");
        assert!(snapshot.is_draft);
    }

    #[test]
    fn legacy_status_context_preserves_state_link_and_head_identity() {
        let mut value = serde_json::json!({
            "number": 7, "headRefOid": "a".repeat(40),
            "statusCheckRollup": [{
                "__typename": "StatusContext", "context": "external-ci",
                "state": "FAILURE", "targetUrl": "https://ci.example/build",
                "description": "untrusted provider text"
            }]
        });
        let parse = |value: &serde_json::Value| {
            parse_github_snapshot(
                &serde_json::to_vec(value).unwrap(),
                &[PullRequestActivityKind::Check],
            )
            .unwrap()
        };
        let failed = parse(&value);
        assert_eq!(failed.activities.len(), 1);
        let activity = &failed.activities[0];
        assert_eq!(activity.state.as_deref(), Some("FAILURE"));
        assert_eq!(activity.url.as_deref(), Some("https://ci.example/build"));
        assert!(activity.provider_id.starts_with("status-context:"));
        assert_eq!(failed, parse(&value));
        assert!(
            !serde_json::to_string(&failed)
                .unwrap()
                .contains("untrusted provider text")
        );
        value["statusCheckRollup"][0]["state"] = "SUCCESS".into();
        assert_ne!(
            activity.provider_id,
            parse(&value).activities[0].provider_id
        );
        value["statusCheckRollup"][0]["state"] = "FAILURE".into();
        value["headRefOid"] = "b".repeat(40).into();
        assert_ne!(
            activity.provider_id,
            parse(&value).activities[0].provider_id
        );
        let later_head = parse(&value);
        value["statusCheckRollup"][0]["context"] = "different-ci".into();
        assert_ne!(
            later_head.activities[0].provider_id,
            parse(&value).activities[0].provider_id
        );
    }

    #[test]
    fn check_run_identity_stays_compatible_and_empty_conclusion_uses_status() {
        let mut value = serde_json::json!({
            "number": 7, "headRefOid": "a".repeat(40),
            "statusCheckRollup": [{"__typename": "CheckRun", "name": "CI",
                "conclusion": "SUCCESS", "status": "COMPLETED", "detailsUrl": "https://ci.example/1"}]
        });
        let parse = |value: &serde_json::Value| {
            parse_github_snapshot(
                &serde_json::to_vec(value).unwrap(),
                &[PullRequestActivityKind::Check],
            )
            .unwrap()
        };
        let stable =
            serde_json::to_vec(&("CI", Some("https://ci.example/1"), Some("SUCCESS"))).unwrap();
        let current = parse(&value);
        assert_eq!(
            current.activities[0].provider_id,
            format!("check:{:x}", Sha256::digest(stable))
        );
        value["statusCheckRollup"][0]["conclusion"] = "".into();
        value["statusCheckRollup"][0]["status"] = "IN_PROGRESS".into();
        assert_eq!(
            parse(&value).activities[0].state.as_deref(),
            Some("IN_PROGRESS")
        );
        value["statusCheckRollup"][0]["conclusion"] = serde_json::Value::Null;
        assert_eq!(
            parse(&value).activities[0].state.as_deref(),
            Some("IN_PROGRESS")
        );
    }

    #[test]
    fn merge_state_observation_tracks_both_tips_without_inventing_policy() {
        let mut value = serde_json::json!({
            "number": 7, "state": "OPEN", "url": "https://github.com/o/r/pull/7",
            "headRefOid": "a".repeat(40), "baseRefOid": "b".repeat(40),
            "mergeStateStatus": "CLEAN"
        });
        let parse = |value: &serde_json::Value, selected: &[PullRequestActivityKind]| {
            parse_github_snapshot(&serde_json::to_vec(value).unwrap(), selected).unwrap()
        };
        let kinds = &[PullRequestActivityKind::Check];
        let clean = parse(&value, kinds);
        value["mergeStateStatus"] = "DIRTY".into();
        let dirty = parse(&value, kinds);
        assert_eq!(dirty.activities.len(), 1);
        assert_eq!(
            dirty.activities[0].state.as_deref(),
            Some("merge_state:DIRTY")
        );
        assert_eq!(
            dirty.activities[0].url.as_deref(),
            Some("https://github.com/o/r/pull/7")
        );
        assert_ne!(activity_digest(&clean), activity_digest(&dirty));
        assert_eq!(dirty, parse(&value, kinds));
        assert_ne!(
            clean.activities[0].provider_id,
            dirty.activities[0].provider_id
        );
        value["baseRefOid"] = "c".repeat(40).into();
        let advanced_base = parse(&value, kinds);
        assert_ne!(
            dirty.activities[0].provider_id,
            advanced_base.activities[0].provider_id
        );
        value["headRefOid"] = "d".repeat(40).into();
        assert_ne!(
            advanced_base.activities[0].provider_id,
            parse(&value, kinds).activities[0].provider_id
        );
        value["mergeStateStatus"] = "BEHIND".into();
        assert_eq!(
            parse(&value, kinds).activities[0].state.as_deref(),
            Some("merge_state:BEHIND")
        );
        assert!(
            parse(&value, &[PullRequestActivityKind::Comment])
                .activities
                .is_empty()
        );
        for state in ["UNKNOWN", "", "unexpected"] {
            value["mergeStateStatus"] = state.into();
            assert!(parse(&value, kinds).activities.is_empty());
        }
        value["mergeStateStatus"] = "CLEAN".into();
        for base in ["", "short", &"z".repeat(40)] {
            value["baseRefOid"] = base.into();
            assert!(parse(&value, kinds).activities.is_empty());
        }
    }

    #[test]
    fn activity_digest_is_deterministic_across_provider_order() {
        let mut snapshot = PullRequestSnapshot {
            number: 1,
            title: "x".into(),
            url: "u".into(),
            state: "open".into(),
            target_branch: "main".into(),
            head_branch: "x".into(),
            head_sha: "a".repeat(40),
            is_draft: false,
            created_at_ms: None,
            merged_at_ms: None,
            activities: vec![
                PullRequestActivityMetadata {
                    kind: PullRequestActivityKind::Review,
                    provider_id: "2".into(),
                    author: None,
                    state: None,
                    url: None,
                    updated_at: None,
                },
                PullRequestActivityMetadata {
                    kind: PullRequestActivityKind::Comment,
                    provider_id: "1".into(),
                    author: None,
                    state: None,
                    url: None,
                    updated_at: None,
                },
            ],
        };
        let first = activity_digest(&snapshot);
        snapshot.activities.reverse();
        assert_eq!(first, activity_digest(&snapshot));
    }
}
