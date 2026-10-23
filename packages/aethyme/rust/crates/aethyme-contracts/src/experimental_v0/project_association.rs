//! Explicit, experimental association between two independently enrolled projects.

use thiserror::Error;

use super::canonical_json::{self, Object, Value};
use super::project_identity::{ProjectId, ProjectIdentityError};
use super::record::{FieldKind, FieldSpec, Record, RecordError, RecordId, RecordSchema};
use super::source_snapshot::SourceSnapshotId;

pub const PROJECT_ASSOCIATION_SCHEMA_NAME: &str = "aethyme.project-association/experimental-v0";
pub const PROJECT_ASSOCIATION_PATH: &str = ".aethyme/project-association.json";
pub const MAX_PROJECT_ASSOCIATION_BYTES: usize = 16 * 1024;
const MAX_PROJECT_KEY_BYTES: usize = 64;

const fn field(name: &'static str) -> FieldSpec {
    FieldSpec {
        name,
        required: true,
        kind: FieldKind::String,
        capability: None,
    }
}

pub static PROJECT_ASSOCIATION_SCHEMA: RecordSchema = RecordSchema {
    name: PROJECT_ASSOCIATION_SCHEMA_NAME,
    fields: &[
        field("local_project_key"),
        field("peer_project_key"),
        field("local_project_id"),
        field("peer_project_id"),
        field("local_descriptor_record_id"),
        field("peer_descriptor_record_id"),
        field("local_anchor_snapshot"),
        field("peer_anchor_snapshot"),
    ],
    capabilities: &[],
};

/// A bounded proposal that names two independently approved local projects.
/// It does not itself establish authority; the broker must confirm the exact
/// record and revalidate each store's approved binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectAssociationDescriptor {
    local_project_key: String,
    peer_project_key: String,
    local_project_id: ProjectId,
    peer_project_id: ProjectId,
    local_descriptor_record_id: RecordId,
    peer_descriptor_record_id: RecordId,
    local_anchor_snapshot: SourceSnapshotId,
    peer_anchor_snapshot: SourceSnapshotId,
}

impl ProjectAssociationDescriptor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        local_project_key: impl Into<String>,
        peer_project_key: impl Into<String>,
        local_project_id: ProjectId,
        peer_project_id: ProjectId,
        local_descriptor_record_id: RecordId,
        peer_descriptor_record_id: RecordId,
        local_anchor_snapshot: SourceSnapshotId,
        peer_anchor_snapshot: SourceSnapshotId,
    ) -> Result<Self, ProjectAssociationError> {
        let local_project_key = local_project_key.into();
        let peer_project_key = peer_project_key.into();
        validate_project_key(&local_project_key, "local_project_key")?;
        validate_project_key(&peer_project_key, "peer_project_key")?;
        if local_project_key == peer_project_key {
            return Err(ProjectAssociationError::SameStore);
        }
        if local_project_id == peer_project_id {
            return Err(ProjectAssociationError::SameProjectId);
        }
        if local_descriptor_record_id == peer_descriptor_record_id {
            return Err(ProjectAssociationError::SameDescriptor);
        }
        let association = Self {
            local_project_key,
            peer_project_key,
            local_project_id,
            peer_project_id,
            local_descriptor_record_id,
            peer_descriptor_record_id,
            local_anchor_snapshot,
            peer_anchor_snapshot,
        };
        if association.canonical_bytes().len() > MAX_PROJECT_ASSOCIATION_BYTES {
            return Err(ProjectAssociationError::TooLarge);
        }
        Ok(association)
    }

    pub fn local_project_key(&self) -> &str {
        &self.local_project_key
    }

    pub fn peer_project_key(&self) -> &str {
        &self.peer_project_key
    }

    pub fn local_project_id(&self) -> &ProjectId {
        &self.local_project_id
    }

    pub fn peer_project_id(&self) -> &ProjectId {
        &self.peer_project_id
    }

    pub fn local_descriptor_record_id(&self) -> &RecordId {
        &self.local_descriptor_record_id
    }

    pub fn peer_descriptor_record_id(&self) -> &RecordId {
        &self.peer_descriptor_record_id
    }

    pub fn local_anchor_snapshot(&self) -> &SourceSnapshotId {
        &self.local_anchor_snapshot
    }

    pub fn peer_anchor_snapshot(&self) -> &SourceSnapshotId {
        &self.peer_anchor_snapshot
    }

    pub fn to_record(&self) -> (Vec<u8>, RecordId) {
        let bytes = self.canonical_bytes();
        let record = Record::decode(&bytes, &[&PROJECT_ASSOCIATION_SCHEMA])
            .expect("validated association produces a valid record");
        (bytes, record.id())
    }

    fn canonical_bytes(&self) -> Vec<u8> {
        Value::Object(
            Object::new(vec![
                ("schema".into(), text(PROJECT_ASSOCIATION_SCHEMA_NAME)),
                ("local_project_key".into(), text(&self.local_project_key)),
                ("peer_project_key".into(), text(&self.peer_project_key)),
                (
                    "local_project_id".into(),
                    text(self.local_project_id.as_str()),
                ),
                (
                    "peer_project_id".into(),
                    text(self.peer_project_id.as_str()),
                ),
                (
                    "local_descriptor_record_id".into(),
                    text(self.local_descriptor_record_id.as_str()),
                ),
                (
                    "peer_descriptor_record_id".into(),
                    text(self.peer_descriptor_record_id.as_str()),
                ),
                (
                    "local_anchor_snapshot".into(),
                    text(self.local_anchor_snapshot.as_str()),
                ),
                (
                    "peer_anchor_snapshot".into(),
                    text(self.peer_anchor_snapshot.as_str()),
                ),
            ])
            .expect("association fields are distinct"),
        )
        .to_canonical_bytes()
    }

    pub fn from_record(input: &[u8]) -> Result<(Self, RecordId), ProjectAssociationError> {
        if input.len() > MAX_PROJECT_ASSOCIATION_BYTES {
            return Err(ProjectAssociationError::TooLarge);
        }
        let value = canonical_json::parse(input)?;
        if value.to_canonical_bytes() != input {
            return Err(ProjectAssociationError::NonCanonical);
        }
        let Value::Object(raw) = value else {
            return Err(ProjectAssociationError::NotAnObject);
        };
        const ALLOWED: &[&str] = &[
            "schema",
            "local_project_key",
            "peer_project_key",
            "local_project_id",
            "peer_project_id",
            "local_descriptor_record_id",
            "peer_descriptor_record_id",
            "local_anchor_snapshot",
            "peer_anchor_snapshot",
        ];
        if raw.iter().any(|(name, _)| !ALLOWED.contains(&name)) {
            return Err(ProjectAssociationError::UnknownField);
        }
        let record = Record::decode(input, &[&PROJECT_ASSOCIATION_SCHEMA])?;
        let value = |name: &'static str| -> Result<&str, ProjectAssociationError> {
            match record.get(name) {
                Some(Value::String(value)) => Ok(value),
                _ => Err(ProjectAssociationError::InvalidField(name)),
            }
        };
        let descriptor = Self::new(
            value("local_project_key")?,
            value("peer_project_key")?,
            ProjectId::parse(value("local_project_id")?)?,
            ProjectId::parse(value("peer_project_id")?)?,
            RecordId::parse(value("local_descriptor_record_id")?)?,
            RecordId::parse(value("peer_descriptor_record_id")?)?,
            SourceSnapshotId::parse(value("local_anchor_snapshot")?)
                .map_err(|_| ProjectAssociationError::InvalidField("local_anchor_snapshot"))?,
            SourceSnapshotId::parse(value("peer_anchor_snapshot")?)
                .map_err(|_| ProjectAssociationError::InvalidField("peer_anchor_snapshot"))?,
        )?;
        if descriptor.canonical_bytes() != input {
            return Err(ProjectAssociationError::NonCanonical);
        }
        Ok((descriptor, record.id()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProjectAssociationError {
    #[error("project association exceeds {MAX_PROJECT_ASSOCIATION_BYTES} bytes")]
    TooLarge,
    #[error("project association is not canonical JSON")]
    NonCanonical,
    #[error("project association must be an object")]
    NotAnObject,
    #[error("project association contains an unsupported field")]
    UnknownField,
    #[error("project association field {0:?} is invalid")]
    InvalidField(&'static str),
    #[error("both association sides must use different local stores")]
    SameStore,
    #[error("both association sides must use different ProjectIds")]
    SameProjectId,
    #[error("both association sides must use different descriptor records")]
    SameDescriptor,
    #[error(transparent)]
    Project(#[from] ProjectIdentityError),
    #[error(transparent)]
    Record(#[from] RecordError),
    #[error(transparent)]
    CanonicalJson(#[from] canonical_json::CanonicalJsonError),
}

impl ProjectAssociationError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::TooLarge => "too_large",
            Self::NonCanonical => "non_canonical",
            Self::NotAnObject => "not_an_object",
            Self::UnknownField => "unknown_field",
            Self::InvalidField(_) => "invalid_field",
            Self::SameStore => "same_store",
            Self::SameProjectId => "same_project_id",
            Self::SameDescriptor => "same_descriptor",
            Self::Project(_) => "invalid_project_id",
            Self::Record(error) => error.code(),
            Self::CanonicalJson(error) => error.code(),
        }
    }
}

fn validate_project_key(value: &str, field: &'static str) -> Result<(), ProjectAssociationError> {
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > MAX_PROJECT_KEY_BYTES
        || bytes[0] == b'-'
        || !bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
    {
        Err(ProjectAssociationError::InvalidField(field))
    } else {
        Ok(())
    }
}

fn text(value: &str) -> Value {
    Value::String(value.into())
}
