//! Experimental Local peer metadata exchange (#676). Serialized packets are
//! advisory data. Only an operator-appointed native adapter can supply current
//! authorization; no packet grants retention, verification or adoption.
use aethyme_contracts::experimental_v0::{
    Brief, ContributionId, ContributionRevision, FieldKind, FieldSpec, ProjectId, Record,
    RecordId, RecordSchema, SourceSnapshotId,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const PROFILE: &str = "aethyme.exchange.scoped-metadata/provisional-v0";
pub const MAX_PACKET_BYTES: usize = 16 * 1024;
/// The experimental immutable revision packet carries bounded canonical
/// revision and brief bytes as lowercase hex. The larger limit applies only
/// to that explicit variant; legacy metadata remains capped at 16 KiB.
pub const MAX_REVISION_PACKET_BYTES: usize =
    2 * aethyme_contracts::experimental_v0::contribution_revision::MAX_CONTRIBUTION_REVISION_BYTES
        + 2 * aethyme_contracts::experimental_v0::brief::MAX_BRIEF_RECORD_BYTES
        + 32 * 1024;
pub const MAX_SNAPSHOT_PAGE_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_BATCH_ITEMS: usize = 64;
pub const MAX_VISIBLE_ITEMS: usize = 1024;
pub const MAX_BUFFERED_EVENTS: usize = 256;
pub const MAX_STREAM_POSITION: u64 = (1u64 << 53) - 1;
pub const DATA_ROLE: &str = "untrusted_advisory_metadata";
static PACKET: RecordSchema = RecordSchema {
    name: "aethyme.exchange-packet/provisional-v0",
    fields: &[FieldSpec {
        name: "spec",
        required: true,
        kind: FieldKind::Opaque,
        capability: None,
    }],
    capabilities: &[],
};
static EVENT: RecordSchema = RecordSchema {
    name: "aethyme.exchange-event/provisional-v0",
    fields: &[FieldSpec {
        name: "spec",
        required: true,
        kind: FieldKind::Opaque,
        capability: None,
    }],
    capabilities: &[],
};

#[derive(Debug, thiserror::Error)]
pub enum ExchangeError {
    #[error("invalid exchange metadata: {0}")]
    Invalid(String),
    #[error("peer access is revoked or forbidden")]
    Forbidden,
    #[error("peer authorization or stream is stale")]
    Stale,
    #[error("peer is offline; remote cached access is unavailable")]
    Offline,
    #[error("peer authorization expired")]
    Expired,
    #[error("exchange limit exceeded")]
    Limit,
    #[error("peer response is incomplete")]
    Incomplete,
    #[error("peer adapter profile is unsupported")]
    Unsupported,
    #[error("native peer adapter failed: {0}")]
    Native(String),
    #[error(transparent)]
    Record(#[from] aethyme_contracts::experimental_v0::RecordError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Archive(#[from] crate::collaboration_archive::ArchiveError),
    #[error(transparent)]
    Authority(#[from] crate::local_authority::AuthorityError),
    #[error(transparent)]
    Capture(#[from] crate::collaboration_capture::CaptureError),
}
fn invalid(why: &str) -> ExchangeError {
    ExchangeError::Invalid(why.into())
}
fn token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 256
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:/@".contains(&b))
}
fn record_id(s: &str) -> Result<(), ExchangeError> {
    RecordId::parse(s)
        .map(|_| ())
        .map_err(|_| invalid("record identity"))
}
fn source_id(s: &str) -> Result<(), ExchangeError> {
    SourceSnapshotId::parse(s)
        .map(|_| ())
        .map_err(|_| invalid("source identity"))
}
fn digest_text(s: &str) -> Result<(), ExchangeError> {
    if s.len() != 64
        || !s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid("object digest"));
    }
    Ok(())
}
fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}
fn decode_lower_hex(value: &str, max_bytes: usize) -> Result<Vec<u8>, ExchangeError> {
    if value.len() % 2 != 0 || value.len() / 2 > max_bytes {
        return Err(ExchangeError::Limit);
    }
    let mut decoded = Vec::with_capacity(value.len() / 2);
    for pair in value.as_bytes().chunks_exact(2) {
        let Some(high) = hex_digit(pair[0]) else {
            return Err(invalid("lowercase hex payload"));
        };
        let Some(low) = hex_digit(pair[1]) else {
            return Err(invalid("lowercase hex payload"));
        };
        decoded.push((high << 4) | low);
    }
    Ok(decoded)
}
fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(HEX[(byte >> 4) as usize] as char);
        text.push(HEX[(byte & 0x0f) as usize] as char);
    }
    text
}
fn paths(values: &[String]) -> Result<(), ExchangeError> {
    if values.is_empty() || values.len() > 64 {
        return Err(invalid("scope paths"));
    }
    let mut unique = BTreeSet::new();
    for v in values {
        if v.len() > 1024
            || v.chars().any(char::is_control)
            || v.contains('\\')
            || v.split('/').any(|p| p.is_empty() || p == "." || p == "..")
            || !unique.insert(v)
        {
            return Err(invalid("scope path"));
        }
    }
    Ok(())
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "paths",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Scope {
    Project,
    Paths(Vec<String>),
}
impl Scope {
    pub fn validate(&self) -> Result<(), ExchangeError> {
        match self {
            Self::Project => Ok(()),
            Self::Paths(p) => paths(p),
        }
    }
}
/// Operator configuration. Deserializing this input never appoints an adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerSpec {
    pub profile: String,
    pub service_id: String,
    pub project: String,
    pub audience: String,
    pub reader: String,
    pub scope: Scope,
    pub max_attempts: u8,
    pub deadline_seconds: u8,
}
impl PeerSpec {
    pub fn validate(&self) -> Result<(), ExchangeError> {
        if self.profile != PROFILE {
            return Err(ExchangeError::Unsupported);
        }
        if [
            &self.service_id,
            &self.project,
            &self.audience,
            &self.reader,
        ]
        .iter()
        .any(|s| !token(s))
        {
            return Err(invalid("peer subject"));
        }
        if self.max_attempts == 0
            || i64::from(self.max_attempts) > crate::delivery::MAX_DELIVERY_ATTEMPTS
            || !(1..=30).contains(&self.deadline_seconds)
        {
            return Err(invalid("finite peer limits"));
        }
        self.scope.validate()
    }
}
/// Attributed source availability only. None of these claims are a receiver's
/// native retained-source receipt or executable authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceAvailability {
    LocalOnly,
    ProviderReachable {
        provider: String,
        exact_commit: String,
    },
    RemoteRetentionClaim {
        service_id: String,
        audience: String,
        until_ms: i64,
    },
}
impl SourceAvailability {
    fn validate(&self) -> Result<(), ExchangeError> {
        match self {
            Self::LocalOnly => Ok(()),
            Self::ProviderReachable {
                provider,
                exact_commit,
            } => {
                if !token(provider)
                    || crate::collaboration_archive::CommitOid::parse(exact_commit).is_err()
                {
                    return Err(invalid("provider source claim"));
                }
                Ok(())
            }
            Self::RemoteRetentionClaim {
                service_id,
                audience,
                until_ms,
            } => {
                if !token(service_id)
                    || !token(audience)
                    || *until_ms <= 0
                    || *until_ms > MAX_STREAM_POSITION as i64
                {
                    return Err(invalid("remote source claim"));
                }
                Ok(())
            }
        }
    }
}
/// Offers and announcements cannot decide a controller target or executor cwd.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Metadata {
    Contribution {
        contribution: String,
        source: String,
        availability: SourceAvailability,
        scope: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        brief: Option<String>,
    },
    /// Exact immutable revision objects plus explicit project and legacy
    /// capture bindings. These fields are untrusted claims until a local
    /// approved-project and FileId-history resolver verifies them.
    RevisionContribution {
        project_id: String,
        descriptor_record_id: String,
        descriptor_object_sha256: String,
        logical_contribution_id: String,
        capture_record_id: String,
        capture_receipt_record_id: String,
        revision_record_id: String,
        revision_object_sha256: String,
        revision_bytes_hex: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        brief_record_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        brief_object_sha256: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        brief_bytes_hex: Option<String>,
        source: String,
        availability: SourceAvailability,
        scope: Vec<String>,
    },
    GenerationOffer {
        generation: String,
        prepared_receipt: String,
        source: String,
        pending_contributions: Vec<String>,
    },
}
fn canonical(
    schema: &'static RecordSchema,
    spec: &impl Serialize,
) -> Result<Record, ExchangeError> {
    canonical_with_limit(schema, spec, MAX_PACKET_BYTES)
}
fn canonical_with_limit(
    schema: &'static RecordSchema,
    spec: &impl Serialize,
    max_bytes: usize,
) -> Result<Record, ExchangeError> {
    let bytes = serde_json::to_vec(&serde_json::json!({"schema":schema.name,"spec":spec}))?;
    if bytes.len() > max_bytes {
        return Err(ExchangeError::Limit);
    }
    Ok(Record::decode(&bytes, &[schema])?)
}
impl Metadata {
    /// Build the advisory peer envelope around one canonical immutable
    /// revision. The descriptor references are claims only; staging and use
    /// still require a locally approved project binding and replayed FileIds.
    #[allow(clippy::too_many_arguments)]
    pub fn revision_contribution(
        project_id: &ProjectId,
        descriptor_record_id: &RecordId,
        descriptor_object_sha256: &str,
        capture_receipt_record_id: &RecordId,
        revision_bytes: &[u8],
        brief_bytes: Option<&[u8]>,
        availability: SourceAvailability,
        scope: Vec<String>,
    ) -> Result<Self, ExchangeError> {
        let (revision, revision_record_id) = ContributionRevision::from_record(revision_bytes)
            .map_err(|_| invalid("revision record"))?;
        let brief_fields = match (revision.brief(), brief_bytes) {
            (None, None) => (None, None, None),
            (Some(reference), Some(bytes)) => {
                let (_, observed_id) = Brief::from_record(bytes)
                    .map_err(|_| invalid("brief record"))?;
                if observed_id != reference.record_id
                    || crate::collaboration_archive::ObjectDigest::of(bytes).hex()
                        != reference.object_sha256
                {
                    return Err(invalid("brief object binding"));
                }
                (
                    Some(reference.record_id.to_string()),
                    Some(reference.object_sha256.clone()),
                    Some(lower_hex(bytes)),
                )
            }
            _ => return Err(invalid("incomplete brief object")),
        };
        let packet = Self::RevisionContribution {
            project_id: project_id.to_string(),
            descriptor_record_id: descriptor_record_id.to_string(),
            descriptor_object_sha256: descriptor_object_sha256.into(),
            logical_contribution_id: revision.contribution_id().to_string(),
            capture_record_id: revision.capture_record_id().to_string(),
            capture_receipt_record_id: capture_receipt_record_id.to_string(),
            revision_record_id: revision_record_id.to_string(),
            revision_object_sha256: crate::collaboration_archive::ObjectDigest::of(revision_bytes)
                .hex()
                .to_string(),
            revision_bytes_hex: lower_hex(revision_bytes),
            brief_record_id: brief_fields.0,
            brief_object_sha256: brief_fields.1,
            brief_bytes_hex: brief_fields.2,
            source: revision.result().to_string(),
            availability,
            scope,
        };
        packet.validate()?;
        Ok(packet)
    }
    fn wire_limit(&self) -> usize {
        match self {
            Self::RevisionContribution { .. } => MAX_REVISION_PACKET_BYTES,
            Self::Contribution { .. } | Self::GenerationOffer { .. } => MAX_PACKET_BYTES,
        }
    }
    fn validate_revision(
        project_id: &str,
        descriptor_record_id: &str,
        descriptor_object_sha256: &str,
        logical_contribution_id: &str,
        capture_record_id: &str,
        capture_receipt_record_id: &str,
        revision_record_id: &str,
        revision_object_sha256: &str,
        revision_bytes_hex: &str,
        brief_record_id: Option<&str>,
        brief_object_sha256: Option<&str>,
        brief_bytes_hex: Option<&str>,
        source: &str,
        availability: &SourceAvailability,
        scope: &[String],
    ) -> Result<(), ExchangeError> {
        ProjectId::parse(project_id).map_err(|_| invalid("revision project identity"))?;
        record_id(descriptor_record_id)?;
        digest_text(descriptor_object_sha256)?;
        ContributionId::parse(logical_contribution_id)
            .map_err(|_| invalid("logical contribution identity"))?;
        record_id(capture_record_id)?;
        record_id(capture_receipt_record_id)?;
        record_id(revision_record_id)?;
        digest_text(revision_object_sha256)?;
        source_id(source)?;
        availability.validate()?;
        paths(scope)?;

        // Check encoded lengths before allocating decoded bytes.
        if revision_bytes_hex.len()
            > 2 * aethyme_contracts::experimental_v0::contribution_revision::MAX_CONTRIBUTION_REVISION_BYTES
        {
            return Err(ExchangeError::Limit);
        }
        let revision_bytes = decode_lower_hex(
            revision_bytes_hex,
            aethyme_contracts::experimental_v0::contribution_revision::MAX_CONTRIBUTION_REVISION_BYTES,
        )?;
        if crate::collaboration_archive::ObjectDigest::of(&revision_bytes).hex()
            != revision_object_sha256
        {
            return Err(invalid("revision object digest"));
        }
        let (revision, parsed_id) = ContributionRevision::from_record(&revision_bytes)
            .map_err(|_| invalid("revision record"))?;
        if parsed_id.as_str() != revision_record_id
            || revision.project_id().as_str() != project_id
            || revision.contribution_id().as_str() != logical_contribution_id
            || revision.capture_record_id().as_str() != capture_record_id
            || revision.result().as_str() != source
        {
            return Err(invalid("revision binding"));
        }

        match (
            revision.brief(),
            brief_record_id,
            brief_object_sha256,
            brief_bytes_hex,
        ) {
            (None, None, None, None) => {}
            (Some(reference), Some(id), Some(digest), Some(encoded)) => {
                record_id(id)?;
                digest_text(digest)?;
                if encoded.len()
                    > 2 * aethyme_contracts::experimental_v0::brief::MAX_BRIEF_RECORD_BYTES
                {
                    return Err(ExchangeError::Limit);
                }
                let brief_bytes = decode_lower_hex(
                    encoded,
                    aethyme_contracts::experimental_v0::brief::MAX_BRIEF_RECORD_BYTES,
                )?;
                if id != reference.record_id.as_str()
                    || digest != reference.object_sha256
                    || crate::collaboration_archive::ObjectDigest::of(&brief_bytes).hex() != digest
                {
                    return Err(invalid("brief object binding"));
                }
                let (brief, brief_id) = Brief::from_record(&brief_bytes)
                    .map_err(|_| invalid("brief record"))?;
                let (canonical, canonical_id) = brief.to_record();
                if brief_id.as_str() != id
                    || canonical_id != brief_id
                    || canonical != brief_bytes
                {
                    return Err(invalid("brief canonical record"));
                }
            }
            _ => return Err(invalid("incomplete brief object")),
        }
        Ok(())
    }
    pub fn validate(&self) -> Result<(), ExchangeError> {
        match self {
            Self::Contribution {
                contribution,
                source,
                availability,
                scope,
                brief,
            } => {
                record_id(contribution)?;
                source_id(source)?;
                availability.validate()?;
                paths(scope)?;
                if brief.as_ref().is_some_and(|b| b.len() > 8192) {
                    return Err(ExchangeError::Limit);
                }
            }
            Self::RevisionContribution {
                project_id,
                descriptor_record_id,
                descriptor_object_sha256,
                logical_contribution_id,
                capture_record_id,
                capture_receipt_record_id,
                revision_record_id,
                revision_object_sha256,
                revision_bytes_hex,
                brief_record_id,
                brief_object_sha256,
                brief_bytes_hex,
                source,
                availability,
                scope,
            } => Self::validate_revision(
                project_id,
                descriptor_record_id,
                descriptor_object_sha256,
                logical_contribution_id,
                capture_record_id,
                capture_receipt_record_id,
                revision_record_id,
                revision_object_sha256,
                revision_bytes_hex,
                brief_record_id.as_deref(),
                brief_object_sha256.as_deref(),
                brief_bytes_hex.as_deref(),
                source,
                availability,
                scope,
            )?,
            Self::GenerationOffer {
                generation,
                prepared_receipt,
                source,
                pending_contributions,
            } => {
                record_id(generation)?;
                record_id(prepared_receipt)?;
                source_id(source)?;
                if pending_contributions.len() > 64 {
                    return Err(ExchangeError::Limit);
                }
                let mut seen = BTreeSet::new();
                for p in pending_contributions {
                    record_id(p)?;
                    if !seen.insert(p) {
                        return Err(invalid("duplicate pending contribution"));
                    }
                }
            }
        };
        Ok(())
    }
    pub fn record(&self) -> Result<Record, ExchangeError> {
        self.validate()?;
        canonical_with_limit(&PACKET, self, self.wire_limit())
    }
}
/// A transport witness is serialized history. It must match a separate currently
/// held native authorization lease before using any event, cursor or snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamWitness {
    pub binding: String,
    pub reader: String,
    pub scope_digest: String,
    pub visibility_generation: String,
    pub incarnation: String,
    pub remote_authority_epoch: String,
}
impl StreamWitness {
    pub fn validate(&self) -> Result<(), ExchangeError> {
        record_id(&self.binding)?;
        record_id(&self.scope_digest)?;
        if [
            &self.reader,
            &self.visibility_generation,
            &self.incarnation,
            &self.remote_authority_epoch,
        ]
        .iter()
        .any(|s| !token(s))
        {
            return Err(invalid("stream witness"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cursor {
    pub stream: StreamWitness,
    pub position: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub stream: StreamWitness,
    pub sequence: u64,
    pub packet: Metadata,
}
impl Event {
    pub fn record(&self) -> Result<Record, ExchangeError> {
        self.stream.validate()?;
        self.packet.validate()?;
        if self.sequence == 0 || self.sequence > MAX_STREAM_POSITION {
            return Err(invalid("event position"));
        }
        let max_bytes = match &self.packet {
            Metadata::RevisionContribution { .. } => MAX_REVISION_PACKET_BYTES,
            Metadata::Contribution { .. } | Metadata::GenerationOffer { .. } => MAX_PACKET_BYTES,
        };
        canonical_with_limit(&EVENT, self, max_bytes)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotDescriptor {
    pub stream: StreamWitness,
    pub identity: String,
    pub item_count: u32,
    pub watermark: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotPage {
    pub snapshot: SnapshotDescriptor,
    pub offset: u32,
    pub items: Vec<Event>,
    pub next_offset: Option<u32>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetadataAck {
    pub stream: StreamWitness,
    pub message: String,
}

impl Cursor {
    /// Validate historical data only; this does not grant current peer access.
    pub fn validate(&self) -> Result<(), ExchangeError> {
        self.stream.validate()?;
        if self.position > MAX_STREAM_POSITION {
            return Err(invalid("cursor position"));
        }
        Ok(())
    }
}
impl SnapshotDescriptor {
    pub fn validate(&self) -> Result<(), ExchangeError> {
        self.stream.validate()?;
        record_id(&self.identity)?;
        if self.item_count as usize > MAX_VISIBLE_ITEMS || self.watermark > MAX_STREAM_POSITION {
            return Err(ExchangeError::Limit);
        }
        Ok(())
    }
}
impl SnapshotPage {
    /// Check page consistency against explicit data expectations. A separate
    /// native lease is still required for authorization and completeness trust.
    pub fn validate_for(
        &self,
        current: &StreamWitness,
        pinned: Option<&SnapshotDescriptor>,
        offset: u32,
    ) -> Result<(), ExchangeError> {
        current.validate()?;
        self.snapshot.validate()?;
        if &self.snapshot.stream != current || pinned.is_some_and(|s| s != &self.snapshot) {
            return Err(ExchangeError::Stale);
        }
        if self.offset != offset || self.items.len() > MAX_BATCH_ITEMS {
            return Err(ExchangeError::Limit);
        }
        let end = self
            .offset
            .checked_add(u32::try_from(self.items.len()).map_err(|_| ExchangeError::Limit)?)
            .ok_or(ExchangeError::Limit)?;
        if end > self.snapshot.item_count {
            return Err(ExchangeError::Incomplete);
        }
        match self.next_offset {
            Some(next)
                if next == end && end < self.snapshot.item_count && !self.items.is_empty() => {}
            None if end == self.snapshot.item_count => {}
            _ => return Err(ExchangeError::Incomplete),
        }
        let mut positions = BTreeSet::new();
        let mut records = BTreeSet::new();
        let mut total_bytes = 0usize;
        for event in &self.items {
            if event.stream != self.snapshot.stream || event.sequence > self.snapshot.watermark {
                return Err(ExchangeError::Stale);
            }
            let encoded = event.record()?;
            total_bytes = total_bytes
                .checked_add(encoded.canonical_bytes().len())
                .ok_or(ExchangeError::Limit)?;
            if total_bytes > MAX_SNAPSHOT_PAGE_BYTES {
                return Err(ExchangeError::Limit);
            }
            if !positions.insert(event.sequence) || !records.insert(event.packet.record()?.id()) {
                return Err(invalid("duplicate snapshot item"));
            }
        }
        Ok(())
    }
}
impl MetadataAck {
    /// A subject check is not source retention, adoption or a native lease.
    pub fn validate_for(
        &self,
        current: &StreamWitness,
        message: &RecordId,
    ) -> Result<(), ExchangeError> {
        self.stream.validate()?;
        current.validate()?;
        record_id(&self.message)?;
        if &self.stream != current {
            return Err(ExchangeError::Stale);
        }
        if self.message != message.as_str() {
            return Err(invalid("acknowledgment subject"));
        }
        Ok(())
    }
}

/// A trusted native adapter supplies and owns this lease. A deserialized event,
/// receipt, JWT-looking string or cached cursor is never an implementation of it.
/// Revocation/freshness checks and the deadline must use the adapter's real
/// authority. The lease stays held through the consumer's local commit.
/// Implementations are appointed operator code, not repository workloads.
pub trait NativeAuthorizationLease {
    fn stream(&self) -> &StreamWitness;
    fn valid_until_ms(&self) -> i64;
    fn check_current(&self) -> Result<(), ExchangeError>;
    fn visible(&self, packet: &Metadata) -> bool;
}
/// Transport boundary for an explicitly appointed native adapter. This package
/// qualifies controlled peers first; no HTTP/server authentication is inferred
/// from implementing this trait, and no default remote adapter is installed.
pub trait TrustedPeer {
    fn profile(&self) -> &str;
    fn service_id(&self) -> &str;
    fn authorize(
        &self,
        binding: &PeerSpec,
        binding_id: &RecordId,
    ) -> Result<Box<dyn NativeAuthorizationLease + '_>, ExchangeError>;
    fn send(
        &self,
        lease: &dyn NativeAuthorizationLease,
        message: &Record,
    ) -> Result<MetadataAck, ExchangeError>;
    fn events(
        &self,
        lease: &dyn NativeAuthorizationLease,
        cursor: &Cursor,
    ) -> Result<Vec<Event>, ExchangeError>;
    fn snapshot(
        &self,
        lease: &dyn NativeAuthorizationLease,
        pinned: Option<&SnapshotDescriptor>,
        offset: u32,
    ) -> Result<SnapshotPage, ExchangeError>;
}

mod manager;
pub use manager::{
    AppointedPeer, Delivery, appoint, deliver, disable, enqueue, poll, projection,
    recover_snapshot, stage_capture_metadata,
};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod revision_tests;
