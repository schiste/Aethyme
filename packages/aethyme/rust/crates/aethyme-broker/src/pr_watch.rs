//! Durable, provider-neutral pull-request observation.
//!
//! Providers return metadata only. Comment and review bodies remain at the
//! provider and must be retrieved explicitly by the receiving agent.

use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Broker, BrokerOpError, resolve_github_target};

pub const PULL_REQUEST_WATCH_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_PR_WATCH_INTERVAL_SECONDS: u64 = 60;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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

pub(crate) struct PullRequestWatchPollStorageResult {
    pub watch: PullRequestWatch,
    pub batch: Option<PullRequestActivityBatch>,
}

impl PullRequestWatchProvider for GithubCliPullRequestWatchProvider {
    fn inspect(
        &self,
        request: &PullRequestWatchRequest,
    ) -> Result<PullRequestSnapshot, PullRequestWatchError> {
        let fields = "number,title,url,state,baseRefName,headRefName,headRefOid,isDraft,comments,reviews,statusCheckRollup";
        let output = Command::new("gh")
            .args([
                "pr",
                "view",
                &request.pr_number.to_string(),
                "--repo",
                &request.repository,
                "--json",
                fields,
            ])
            .output()?;
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
        for check in value
            .get("statusCheckRollup")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            let name = json_string(check, "name").unwrap_or("check");
            let url = json_string(check, "detailsUrl").map(str::to_string);
            let state = json_string(check, "conclusion")
                .or_else(|| json_string(check, "status"))
                .map(str::to_string);
            let stable = serde_json::to_vec(&(name, &url, &state)).expect("serializable check");
            activities.push(PullRequestActivityMetadata {
                kind: PullRequestActivityKind::Check,
                provider_id: format!("check:{:x}", Sha256::digest(stable)),
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
        activities,
    })
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
