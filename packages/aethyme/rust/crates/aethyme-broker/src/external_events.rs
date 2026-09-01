//! Authenticated external coordination events.
//!
//! Provider adapters verify transport authenticity and reduce the source event
//! to [`ExternalEventEnvelope`]. The broker validates its digest, resolves only
//! exact local provenance, and persists an allowlist projection. It never hosts
//! a listener and never stores provider payloads.

use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};

use crate::{
    Advisory, AdvisoryEvidence, AdvisorySeverity, Broker, BrokerError, NewAdvisory,
    RemoteTargetError,
};

pub const EXTERNAL_EVENT_SCHEMA_VERSION: u32 = 1;
pub const EXTERNAL_EVENT_MAX_AGE_MS: i64 = 30 * 24 * 60 * 60 * 1_000;
const EXTERNAL_EVENT_MAX_FUTURE_SKEW_MS: i64 = 5 * 60 * 1_000;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ExternalEventProvider {
    Github,
}

impl ExternalEventProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Github => "github",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, BrokerError> {
        match value {
            "github" => Ok(Self::Github),
            other => Err(BrokerError::InvalidEnumValue {
                field: "external_coordination_events.provider",
                value: other.into(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalVerificationMethod {
    WebhookSignature,
    AuthenticatedPoll,
}

impl ExternalVerificationMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WebhookSignature => "webhook_signature",
            Self::AuthenticatedPoll => "authenticated_poll",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, BrokerError> {
        match value {
            "webhook_signature" => Ok(Self::WebhookSignature),
            "authenticated_poll" => Ok(Self::AuthenticatedPoll),
            other => Err(BrokerError::InvalidEnumValue {
                field: "external_coordination_events.verification_method",
                value: other.into(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedExternalSource {
    pub method: ExternalVerificationMethod,
    pub verified_at: i64,
}

/// Strict provider-neutral input. Unknown fields are rejected so a caller
/// cannot smuggle webhook bodies or comments through a permissive parser.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalEventEnvelope {
    pub schema_version: u32,
    pub provider: ExternalEventProvider,
    pub provider_event_id: String,
    pub event_type: String,
    pub repository: String,
    pub target_branch: String,
    pub pr_number: i64,
    pub commit_sha: String,
    pub occurred_at: i64,
    pub verified_source: VerifiedExternalSource,
    pub normalized_digest: String,
}

#[derive(Debug, serde::Serialize)]
struct ExternalEventDigestFields<'a> {
    schema_version: u32,
    provider: ExternalEventProvider,
    provider_event_id: &'a str,
    event_type: &'a str,
    repository: &'a str,
    target_branch: &'a str,
    pr_number: i64,
    commit_sha: &'a str,
    occurred_at: i64,
    verified_source: &'a VerifiedExternalSource,
}

/// SHA-256 authorization digest over every normalized field except the digest
/// itself. Typed field order makes this byte-stable across adapters.
pub fn external_event_digest(event: &ExternalEventEnvelope) -> String {
    let fields = ExternalEventDigestFields {
        schema_version: event.schema_version,
        provider: event.provider,
        provider_event_id: &event.provider_event_id,
        event_type: &event.event_type,
        repository: &event.repository,
        target_branch: &event.target_branch,
        pr_number: event.pr_number,
        commit_sha: &event.commit_sha,
        occurred_at: event.occurred_at,
        verified_source: &event.verified_source,
    };
    let bytes = serde_json::to_vec(&fields).expect("external event digest fields serialize");
    format!("{:x}", Sha256::digest(bytes))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalEventKind {
    ReviewChangesRequested,
    ReviewApproved,
    QueueEjected,
    ValidationFailed,
}

impl ExternalEventKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "review_changes_requested" => Some(Self::ReviewChangesRequested),
            "review_approved" => Some(Self::ReviewApproved),
            "queue_ejected" => Some(Self::QueueEjected),
            "validation_failed" => Some(Self::ValidationFailed),
            _ => None,
        }
    }

    pub fn severity(self) -> AdvisorySeverity {
        match self {
            Self::ReviewApproved => AdvisorySeverity::Info,
            Self::ReviewChangesRequested | Self::QueueEjected | Self::ValidationFailed => {
                AdvisorySeverity::Warning
            }
        }
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ExternalEventStatus {
    PendingAdvisory,
    AdvisoryCreated,
    UnknownEventType,
    UnknownPullRequest,
    OwnerNotFound,
    AmbiguousOwner,
    RepositoryMismatch,
    Stale,
    Ignored,
}

impl ExternalEventStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PendingAdvisory => "pending_advisory",
            Self::AdvisoryCreated => "advisory_created",
            Self::UnknownEventType => "unknown_event_type",
            Self::UnknownPullRequest => "unknown_pull_request",
            Self::OwnerNotFound => "owner_not_found",
            Self::AmbiguousOwner => "ambiguous_owner",
            Self::RepositoryMismatch => "repository_mismatch",
            Self::Stale => "stale",
            Self::Ignored => "ignored",
        }
    }

    pub fn parse(value: &str) -> Result<Self, BrokerError> {
        match value {
            "pending_advisory" => Ok(Self::PendingAdvisory),
            "advisory_created" => Ok(Self::AdvisoryCreated),
            "unknown_event_type" => Ok(Self::UnknownEventType),
            "unknown_pull_request" => Ok(Self::UnknownPullRequest),
            "owner_not_found" => Ok(Self::OwnerNotFound),
            "ambiguous_owner" => Ok(Self::AmbiguousOwner),
            "repository_mismatch" => Ok(Self::RepositoryMismatch),
            "stale" => Ok(Self::Stale),
            "ignored" => Ok(Self::Ignored),
            other => Err(BrokerError::InvalidEnumValue {
                field: "external_coordination_events.status",
                value: other.into(),
            }),
        }
    }

    pub fn is_unresolved(self) -> bool {
        !matches!(self, Self::AdvisoryCreated | Self::Ignored)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ExternalEventRecord {
    pub id: i64,
    pub provider: ExternalEventProvider,
    pub provider_event_id: String,
    pub event_type: String,
    pub repository: String,
    pub target_branch: String,
    pub pr_number: i64,
    pub commit_sha: String,
    pub occurred_at: i64,
    pub verification_method: ExternalVerificationMethod,
    pub verified_at: i64,
    pub normalized_digest: String,
    pub status: ExternalEventStatus,
    pub session_id: Option<i64>,
    pub queue_entry_id: Option<i64>,
    pub advisory_id: Option<i64>,
    pub received_at: i64,
    pub reconciled_at: Option<i64>,
    pub reconciliation_kind: Option<String>,
    pub reconciliation_reason_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NewExternalEventRecord {
    pub envelope: ExternalEventEnvelope,
    pub status: ExternalEventStatus,
    pub session_id: Option<i64>,
    pub queue_entry_id: Option<i64>,
    pub received_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ExternalEventOwnershipCandidate {
    pub session_id: i64,
    pub queue_entry_ids: Vec<i64>,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ExternalEventIngestReport {
    pub event: ExternalEventRecord,
    pub deduplicated: bool,
    pub ownership_candidates: Vec<ExternalEventOwnershipCandidate>,
    pub advisory: Option<Advisory>,
    pub non_blocking: bool,
    pub remediation: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalEventReconciliation {
    Assign { session_id: i64 },
    Ignore,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ExternalEventReconcileReport {
    pub event: ExternalEventRecord,
    pub advisory: Option<Advisory>,
    pub non_blocking: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ExternalEventError {
    #[error(transparent)]
    Store(#[from] BrokerError),
    #[error(transparent)]
    Broker(#[from] crate::BrokerOpError),
    #[error(transparent)]
    Remote(#[from] RemoteTargetError),
    #[error("external event schema {found} is unsupported; expected {expected}")]
    UnsupportedSchema { found: u32, expected: u32 },
    #[error("invalid external event field {field}: {reason}")]
    InvalidField { field: &'static str, reason: String },
    #[error("external event normalized digest must be a full lowercase SHA-256")]
    InvalidDigest,
    #[error("external event normalized digest mismatch: expected {expected}, received {actual}")]
    DigestMismatch { expected: String, actual: String },
    #[error("cannot resolve the local canonical repository remote: {reason}")]
    RepositoryIdentity { reason: String },
    #[error("external event {event_id} is already terminal ({status})")]
    AlreadyTerminal { event_id: i64, status: &'static str },
    #[error("cannot assign event {event_id}: unsupported event type {event_type:?}")]
    CannotAssignUnknownType { event_id: i64, event_type: String },
    #[error("cannot assign event {event_id} from a different repository identity")]
    CannotAssignRepositoryMismatch { event_id: i64 },
    #[error("reconciliation reason must contain 1..=500 bytes")]
    InvalidReconciliationReason,
}

impl Broker {
    pub fn ingest_external_event(
        &mut self,
        envelope: ExternalEventEnvelope,
        received_at: i64,
    ) -> Result<ExternalEventIngestReport, ExternalEventError> {
        validate_envelope(&envelope, received_at)?;
        let expected = external_event_digest(&envelope);
        if envelope.normalized_digest != expected {
            return Err(ExternalEventError::DigestMismatch {
                expected,
                actual: envelope.normalized_digest,
            });
        }

        let local_repository = local_repository_identity(self)?;
        let pr_state = self
            .store()
            .pr_watch_state(&envelope.target_branch, envelope.pr_number)?;
        let candidates = self
            .store()
            .external_event_ownership_candidates(&envelope.commit_sha)?;
        let selected = select_owner(
            &candidates,
            pr_state
                .as_ref()
                .and_then(|state| state.last_agent_session_id),
        );
        let status = if envelope.repository != local_repository {
            ExternalEventStatus::RepositoryMismatch
        } else if envelope.occurred_at < received_at.saturating_sub(EXTERNAL_EVENT_MAX_AGE_MS) {
            ExternalEventStatus::Stale
        } else if ExternalEventKind::parse(&envelope.event_type).is_none() {
            ExternalEventStatus::UnknownEventType
        } else if pr_state.is_none() {
            ExternalEventStatus::UnknownPullRequest
        } else {
            match selected {
                OwnerSelection::One { .. } => ExternalEventStatus::PendingAdvisory,
                OwnerSelection::None => ExternalEventStatus::OwnerNotFound,
                OwnerSelection::Ambiguous => ExternalEventStatus::AmbiguousOwner,
            }
        };
        let (session_id, queue_entry_id) = match selected {
            OwnerSelection::One {
                session_id,
                queue_entry_id,
            } if status == ExternalEventStatus::PendingAdvisory => {
                (Some(session_id), queue_entry_id)
            }
            _ => (None, None),
        };
        let (mut event, deduplicated) =
            self.store()
                .record_external_event(&NewExternalEventRecord {
                    envelope,
                    status,
                    session_id,
                    queue_entry_id,
                    received_at,
                })?;
        let advisory = if event.status == ExternalEventStatus::PendingAdvisory {
            let advisory = self.complete_external_event_advisory(&event, false)?;
            event = self
                .store()
                .external_event(event.id)?
                .ok_or(BrokerError::ExternalEventNotFound(event.id))?;
            Some(advisory)
        } else {
            event
                .advisory_id
                .and_then(|id| self.store().advisory(id).ok().flatten())
        };
        Ok(ExternalEventIngestReport {
            remediation: remediation(&event),
            event,
            deduplicated,
            ownership_candidates: candidates,
            advisory,
            non_blocking: true,
        })
    }

    pub fn reconcile_external_event(
        &mut self,
        event_id: i64,
        reconciliation: ExternalEventReconciliation,
        reason: &str,
        now: i64,
    ) -> Result<ExternalEventReconcileReport, ExternalEventError> {
        if reason.is_empty() || reason.len() > 500 {
            return Err(ExternalEventError::InvalidReconciliationReason);
        }
        let event = self
            .store()
            .external_event(event_id)?
            .ok_or(BrokerError::ExternalEventNotFound(event_id))?;
        if !event.status.is_unresolved() {
            return Err(ExternalEventError::AlreadyTerminal {
                event_id,
                status: event.status.as_str(),
            });
        }
        let reason_digest = format!("{:x}", Sha256::digest(reason.as_bytes()));
        let advisory = match reconciliation {
            ExternalEventReconciliation::Ignore => {
                self.store()
                    .ignore_external_event(event_id, &reason_digest, now)?;
                None
            }
            ExternalEventReconciliation::Assign { session_id } => {
                self.store().session(session_id)?;
                if event.status == ExternalEventStatus::RepositoryMismatch {
                    return Err(ExternalEventError::CannotAssignRepositoryMismatch { event_id });
                }
                if ExternalEventKind::parse(&event.event_type).is_none() {
                    return Err(ExternalEventError::CannotAssignUnknownType {
                        event_id,
                        event_type: event.event_type,
                    });
                }
                self.store().prepare_external_event_assignment(
                    event_id,
                    session_id,
                    &reason_digest,
                    now,
                )?;
                let pending = self
                    .store()
                    .external_event(event_id)?
                    .ok_or(BrokerError::ExternalEventNotFound(event_id))?;
                Some(self.complete_external_event_advisory(&pending, true)?)
            }
        };
        let event = self
            .store()
            .external_event(event_id)?
            .ok_or(BrokerError::ExternalEventNotFound(event_id))?;
        Ok(ExternalEventReconcileReport {
            event,
            advisory,
            non_blocking: true,
        })
    }

    fn complete_external_event_advisory(
        &mut self,
        event: &ExternalEventRecord,
        reconciled: bool,
    ) -> Result<Advisory, ExternalEventError> {
        let session_id = event
            .session_id
            .ok_or_else(|| ExternalEventError::InvalidField {
                field: "session_id",
                reason: "pending advisory has no resolved owner".into(),
            })?;
        let kind = ExternalEventKind::parse(&event.event_type).ok_or_else(|| {
            ExternalEventError::CannotAssignUnknownType {
                event_id: event.id,
                event_type: event.event_type.clone(),
            }
        })?;
        let event_id_hash = format!("{:x}", Sha256::digest(event.provider_event_id.as_bytes()));
        let mut evidence = vec![
            AdvisoryEvidence {
                kind: "external_event_type".into(),
                summary: event.event_type.clone(),
            },
            AdvisoryEvidence {
                kind: "provider_event".into(),
                summary: format!("{} event {}", event.provider.as_str(), &event_id_hash[..12]),
            },
            AdvisoryEvidence {
                kind: "pull_request".into(),
                summary: format!("PR #{} targeting {}", event.pr_number, event.target_branch),
            },
            AdvisoryEvidence {
                kind: "commit".into(),
                summary: event.commit_sha.clone(),
            },
            AdvisoryEvidence {
                kind: "verified_source".into(),
                summary: format!(
                    "{} at {}",
                    event.verification_method.as_str(),
                    event.verified_at
                ),
            },
            AdvisoryEvidence {
                kind: "safe_next_action".into(),
                summary: format!("aethyme broker external-events show {}", event.id),
            },
        ];
        if reconciled {
            evidence.push(AdvisoryEvidence {
                kind: "ownership_resolution".into(),
                summary: "explicit operator assignment (reason retained as SHA-256 only)".into(),
            });
        }
        let advisory = self.persist_advisory(NewAdvisory {
            identity: format!("external:{}:{}", event.provider.as_str(), event_id_hash),
            session_id: Some(session_id),
            severity: kind.severity(),
            queue_entry_id: event.queue_entry_id,
            integration_sha: None,
            paths: Vec::new(),
            evidence,
        })?;
        self.store()
            .complete_external_event_advisory(event.id, advisory.id)?;
        self.refresh_advisory_projection()
            .map_err(|error| ExternalEventError::InvalidField {
                field: "advisory_projection",
                reason: error.to_string(),
            })?;
        Ok(advisory)
    }
}

#[derive(Debug, Clone, Copy)]
enum OwnerSelection {
    None,
    One {
        session_id: i64,
        queue_entry_id: Option<i64>,
    },
    Ambiguous,
}

fn select_owner(
    candidates: &[ExternalEventOwnershipCandidate],
    pr_session: Option<i64>,
) -> OwnerSelection {
    if let Some(pr_session) = pr_session {
        let matches = candidates
            .iter()
            .filter(|candidate| candidate.session_id == pr_session)
            .collect::<Vec<_>>();
        if matches.len() == 1 {
            return owner_from_candidate(matches[0]);
        }
    }
    match candidates {
        [] => OwnerSelection::None,
        [candidate] => owner_from_candidate(candidate),
        _ => OwnerSelection::Ambiguous,
    }
}

fn owner_from_candidate(candidate: &ExternalEventOwnershipCandidate) -> OwnerSelection {
    OwnerSelection::One {
        session_id: candidate.session_id,
        queue_entry_id: if candidate.queue_entry_ids.len() == 1 {
            Some(candidate.queue_entry_ids[0])
        } else {
            None
        },
    }
}

fn local_repository_identity(broker: &Broker) -> Result<String, ExternalEventError> {
    let remotes =
        broker
            .repo_handle()
            .remotes()
            .map_err(|error| ExternalEventError::RepositoryIdentity {
                reason: error.to_string(),
            })?;
    let remote = if remotes.iter().any(|remote| remote == "origin") {
        "origin"
    } else if remotes.len() == 1 {
        &remotes[0]
    } else {
        return Err(ExternalEventError::RepositoryIdentity {
            reason: if remotes.is_empty() {
                "no Git remote is configured".into()
            } else {
                "multiple remotes are configured and none is named origin".into()
            },
        });
    };
    Ok(broker
        .repo_handle()
        .resolve_remote_target(remote, None)?
        .coordination_key)
}

fn validate_envelope(
    event: &ExternalEventEnvelope,
    received_at: i64,
) -> Result<(), ExternalEventError> {
    if event.schema_version != EXTERNAL_EVENT_SCHEMA_VERSION {
        return Err(ExternalEventError::UnsupportedSchema {
            found: event.schema_version,
            expected: EXTERNAL_EVENT_SCHEMA_VERSION,
        });
    }
    validate_identifier("provider_event_id", &event.provider_event_id, 128, b"._:-")?;
    validate_identifier("event_type", &event.event_type, 64, b"_-")?;
    validate_repository(&event.repository)?;
    validate_identifier("target_branch", &event.target_branch, 200, b"._/-")?;
    if event.pr_number <= 0 {
        return Err(ExternalEventError::InvalidField {
            field: "pr_number",
            reason: "must be positive".into(),
        });
    }
    if event.commit_sha.len() != 40
        || !event
            .commit_sha
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ExternalEventError::InvalidField {
            field: "commit_sha",
            reason: "must be a full 40-character hexadecimal object ID".into(),
        });
    }
    if event.occurred_at <= 0 || event.occurred_at > received_at + EXTERNAL_EVENT_MAX_FUTURE_SKEW_MS
    {
        return Err(ExternalEventError::InvalidField {
            field: "occurred_at",
            reason: "must be positive and no more than five minutes in the future".into(),
        });
    }
    if event.verified_source.verified_at <= 0
        || event.verified_source.verified_at > received_at + EXTERNAL_EVENT_MAX_FUTURE_SKEW_MS
    {
        return Err(ExternalEventError::InvalidField {
            field: "verified_source.verified_at",
            reason: "must be positive and no more than five minutes in the future".into(),
        });
    }
    if event.normalized_digest.len() != 64
        || !event
            .normalized_digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ExternalEventError::InvalidDigest);
    }
    Ok(())
}

fn validate_identifier(
    field: &'static str,
    value: &str,
    maximum: usize,
    punctuation: &[u8],
) -> Result<(), ExternalEventError> {
    if value.is_empty()
        || value.len() > maximum
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || punctuation.iter().any(|allowed| *allowed == byte)
        })
    {
        return Err(ExternalEventError::InvalidField {
            field,
            reason: format!("must contain 1..={maximum} allowlisted ASCII identifier bytes"),
        });
    }
    Ok(())
}

fn validate_repository(value: &str) -> Result<(), ExternalEventError> {
    if value.len() > 300
        || value.starts_with('/')
        || value.ends_with('/')
        || value.contains("..")
        || value.split('/').count() < 3
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/'))
    {
        return Err(ExternalEventError::InvalidField {
            field: "repository",
            reason: "must be a canonical host/owner/repository coordination identity".into(),
        });
    }
    Ok(())
}

fn remediation(event: &ExternalEventRecord) -> Option<String> {
    event.status.is_unresolved().then(|| {
        format!(
            "aethyme broker external-events reconcile {} --outcome <assign|ignore> --reason <text> [--session <id>]",
            event.id
        )
    })
}

pub(crate) fn aggregate_ownership_candidates(
    rows: Vec<(i64, Option<i64>, String)>,
) -> Vec<ExternalEventOwnershipCandidate> {
    let mut grouped: BTreeMap<i64, (BTreeSet<i64>, BTreeSet<String>)> = BTreeMap::new();
    for (session_id, queue_entry_id, evidence) in rows {
        let entry = grouped.entry(session_id).or_default();
        if let Some(queue_entry_id) = queue_entry_id {
            entry.0.insert(queue_entry_id);
        }
        entry.1.insert(evidence);
    }
    grouped
        .into_iter()
        .map(
            |(session_id, (queue_entry_ids, evidence))| ExternalEventOwnershipCandidate {
                session_id,
                queue_entry_ids: queue_entry_ids.into_iter().collect(),
                evidence: evidence.into_iter().collect(),
            },
        )
        .collect()
}
