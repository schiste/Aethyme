//! Immutable, experimental contribution revisions (plan §5.2–5.4, D10).
//!
//! A revision is a content-addressed value over one exact project binding,
//! contribution lineage, source transition, and optional decision brief.
//! Replacing only the brief therefore creates a different revision ID. The
//! record carries no authority claim; project/file mapping is advisory.

use data_encoding::HEXLOWER;

use super::canonical_json::{self, Object, Value};
use super::project_identity::{FileId, IdentityRelation, ProjectId};
use super::record::{FieldKind, FieldSpec, Record, RecordError, RecordId, RecordSchema};
use super::source_snapshot::{EntryKind, SourceEntry, SourceSnapshot, SourceSnapshotId};

pub const CONTRIBUTION_REVISION_SCHEMA_NAME: &str = "aethyme.contribution-revision/experimental-v0";
pub const MAX_CONTRIBUTION_REVISION_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_REVISION_CHANGES: usize = 200_000;

const fn field(name: &'static str, required: bool, kind: FieldKind) -> FieldSpec {
    FieldSpec {
        name,
        required,
        kind,
        capability: None,
    }
}

/// Random identity for one logical submitted intent. This is deliberately
/// distinct from the digest RecordId of any immutable revision.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContributionId(String);

impl ContributionId {
    pub fn parse(value: &str) -> Result<Self, ContributionRevisionError> {
        let encoded = value.strip_prefix("contrib:").unwrap_or_default();
        let valid = encoded.len() == 26
            && encoded
                .bytes()
                .all(|byte| contribution_base32_value(byte).is_some())
            && encoded.as_bytes().last().is_some_and(|last| {
                contribution_base32_value(*last).is_some_and(|value| value % 4 == 0)
            });
        if valid {
            Ok(Self(value.to_string()))
        } else {
            Err(ContributionRevisionError::InvalidContributionId(
                value.to_string(),
            ))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ContributionId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Exact immutable payload for one revision of a logical contribution.
pub static CONTRIBUTION_REVISION_SCHEMA: RecordSchema = RecordSchema {
    name: CONTRIBUTION_REVISION_SCHEMA_NAME,
    fields: &[
        field("project_id", true, FieldKind::String),
        field("contribution_id", true, FieldKind::String),
        field("capture_record_id", true, FieldKind::String),
        field("base", true, FieldKind::String),
        field("result", true, FieldKind::String),
        field("source_changes", true, FieldKind::Opaque),
        field("brief_record_id", false, FieldKind::String),
        field("brief_object_sha256", false, FieldKind::String),
        field("depends_on", true, FieldKind::StringSet),
    ],
    capabilities: &[],
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BriefReference {
    pub record_id: RecordId,
    /// Lowercase SHA-256 of the exact archive object bytes, without a prefix.
    pub object_sha256: String,
}

impl BriefReference {
    pub fn new(
        record_id: RecordId,
        object_sha256: impl Into<String>,
    ) -> Result<Self, ContributionRevisionError> {
        let object_sha256 = object_sha256.into();
        if object_sha256.len() != 64
            || !object_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ContributionRevisionError::InvalidBriefDigest);
        }
        Ok(Self {
            record_id,
            object_sha256,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContributionRevision {
    project_id: ProjectId,
    contribution_id: ContributionId,
    capture_record_id: RecordId,
    base: SourceSnapshotId,
    result: SourceSnapshotId,
    source_changes: Vec<Value>,
    brief: Option<BriefReference>,
    depends_on: Vec<RecordId>,
}

impl ContributionRevision {
    pub fn new(
        project_id: ProjectId,
        contribution_id: ContributionId,
        capture_record_id: RecordId,
        base: SourceSnapshotId,
        result: SourceSnapshotId,
        source_changes: Vec<Value>,
        brief: Option<BriefReference>,
        depends_on: Vec<RecordId>,
    ) -> Result<Self, ContributionRevisionError> {
        if source_changes.len() > MAX_REVISION_CHANGES {
            return Err(ContributionRevisionError::TooManyChanges(
                source_changes.len(),
            ));
        }
        validate_changes(&source_changes)?;
        let mut dependency_ids: Vec<String> = depends_on
            .iter()
            .map(|id| id.as_str().to_string())
            .collect();
        dependency_ids.sort();
        if dependency_ids.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(ContributionRevisionError::DuplicateDependency);
        }
        let depends_on = dependency_ids
            .iter()
            .map(|id| RecordId::parse(id).expect("input dependency IDs were RecordIds"))
            .collect();
        Ok(Self {
            project_id,
            contribution_id,
            capture_record_id,
            base,
            result,
            source_changes,
            brief,
            depends_on,
        })
    }

    pub fn project_id(&self) -> &ProjectId {
        &self.project_id
    }

    pub fn contribution_id(&self) -> &ContributionId {
        &self.contribution_id
    }

    /// Exact legacy capture lineage this revision extends. This remains a
    /// separate identity from both the logical ContributionId and revision
    /// digest, allowing the adapter to join history without rewriting old
    /// capture records.
    pub fn capture_record_id(&self) -> &RecordId {
        &self.capture_record_id
    }

    pub fn base(&self) -> &SourceSnapshotId {
        &self.base
    }

    pub fn result(&self) -> &SourceSnapshotId {
        &self.result
    }

    pub fn source_changes(&self) -> &[Value] {
        &self.source_changes
    }

    pub fn brief(&self) -> Option<&BriefReference> {
        self.brief.as_ref()
    }

    pub fn depends_on(&self) -> &[RecordId] {
        &self.depends_on
    }

    /// Canonical record bytes and their immutable revision ID.
    pub fn to_record(&self) -> (Vec<u8>, RecordId) {
        let mut fields = vec![
            ("schema".into(), text(CONTRIBUTION_REVISION_SCHEMA_NAME)),
            ("project_id".into(), text(self.project_id.as_str())),
            (
                "contribution_id".into(),
                text(self.contribution_id.as_str()),
            ),
            (
                "capture_record_id".into(),
                text(self.capture_record_id.as_str()),
            ),
            ("base".into(), text(self.base.as_str())),
            ("result".into(), text(self.result.as_str())),
            (
                "source_changes".into(),
                Value::Array(self.source_changes.clone()),
            ),
            (
                "depends_on".into(),
                Value::Array(self.depends_on.iter().map(|id| text(id.as_str())).collect()),
            ),
        ];
        if let Some(brief) = &self.brief {
            fields.push(("brief_record_id".into(), text(brief.record_id.as_str())));
            fields.push(("brief_object_sha256".into(), text(&brief.object_sha256)));
        }
        let bytes =
            Value::Object(Object::new(fields).expect("distinct contribution revision fields"))
                .to_canonical_bytes();
        let record = Record::decode(&bytes, &[&CONTRIBUTION_REVISION_SCHEMA])
            .expect("a contribution revision encodes to its schema");
        (bytes, record.id())
    }

    pub fn from_record(input: &[u8]) -> Result<(Self, RecordId), ContributionRevisionError> {
        if input.len() > MAX_CONTRIBUTION_REVISION_BYTES {
            return Err(ContributionRevisionError::TooLarge(input.len()));
        }
        let Value::Object(raw) = canonical_json::parse(input)? else {
            return Err(ContributionRevisionError::NotAnObject);
        };
        const ALLOWED: &[&str] = &[
            "schema",
            "project_id",
            "contribution_id",
            "capture_record_id",
            "base",
            "result",
            "source_changes",
            "brief_record_id",
            "brief_object_sha256",
            "depends_on",
        ];
        if let Some((name, _)) = raw.iter().find(|(name, _)| !ALLOWED.contains(name)) {
            return Err(ContributionRevisionError::UnknownField(name.to_string()));
        }
        let record = Record::decode(input, &[&CONTRIBUTION_REVISION_SCHEMA])?;
        let project_id = ProjectId::parse(&required_string(&record, "project_id")?)?;
        let contribution_id = ContributionId::parse(&required_string(&record, "contribution_id")?)?;
        let capture_record_id = RecordId::parse(&required_string(&record, "capture_record_id")?)?;
        let base = SourceSnapshotId::parse(&required_string(&record, "base")?)?;
        let result = SourceSnapshotId::parse(&required_string(&record, "result")?)?;
        let Value::Array(source_changes) = record.get("source_changes").expect("schema checked")
        else {
            return Err(ContributionRevisionError::WrongFieldType("source_changes"));
        };
        let Value::Array(dependencies) = record.get("depends_on").expect("schema checked") else {
            return Err(ContributionRevisionError::WrongFieldType("depends_on"));
        };
        let depends_on = dependencies
            .iter()
            .map(|value| match value {
                Value::String(id) => RecordId::parse(id).map_err(ContributionRevisionError::Record),
                _ => Err(ContributionRevisionError::WrongFieldType("depends_on")),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let brief_record_id = optional_string(&record, "brief_record_id")?;
        let brief_object_sha256 = optional_string(&record, "brief_object_sha256")?;
        let brief = match (brief_record_id, brief_object_sha256) {
            (None, None) => None,
            (Some(id), Some(sha)) => Some(BriefReference::new(RecordId::parse(&id)?, sha)?),
            _ => return Err(ContributionRevisionError::IncompleteBriefReference),
        };
        let revision = Self::new(
            project_id,
            contribution_id,
            capture_record_id,
            base,
            result,
            source_changes.clone(),
            brief,
            depends_on,
        )?;
        let (canonical, id) = revision.to_record();
        if canonical != input {
            return Err(ContributionRevisionError::NonCanonicalRecord);
        }
        Ok((revision, id))
    }
}

fn validate_changes(changes: &[Value]) -> Result<(), ContributionRevisionError> {
    let mut previous: Option<Vec<u8>> = None;
    for value in changes {
        let Value::Object(object) = value else {
            return Err(ContributionRevisionError::InvalidSourceChange);
        };
        const CHANGE_FIELDS: &[&str] = &[
            "before",
            "after",
            "relation",
            "derived_from",
            "possible_sources",
        ];
        if object
            .iter()
            .any(|(name, _)| !CHANGE_FIELDS.contains(&name))
        {
            return Err(ContributionRevisionError::InvalidSourceChange);
        }
        let relation_text = object_string(object, "relation")?;
        let relation = match relation_text.as_str() {
            "same_path" => IdentityRelation::SamePath,
            "explicit_rename" => IdentityRelation::ExplicitRename,
            "explicit_copy" => IdentityRelation::ExplicitCopy,
            "explicit_split" => IdentityRelation::ExplicitSplit,
            "new" => IdentityRelation::New,
            "removed" => IdentityRelation::Removed,
            "uncertain_rename" => IdentityRelation::UncertainRename,
            _ => return Err(ContributionRevisionError::InvalidSourceChange),
        };
        let before = object_state(object, "before")?;
        let after = object_state(object, "after")?;
        let sources = object_ids(object, "derived_from")?;
        let possible = object_ids(object, "possible_sources")?;
        let shape_ok = match relation {
            IdentityRelation::SamePath => {
                matches!((&before, &after), (Some((before_path, _, before_id)), Some((after_path, _, after_id))) if before_path == after_path && before_id == after_id)
                    && sources.is_empty()
                    && possible.is_empty()
            }
            IdentityRelation::ExplicitRename => {
                matches!((&before, &after), (Some((before_path, _, before_id)), Some((after_path, _, after_id))) if before_path != after_path && before_id == after_id)
                    && sources.is_empty()
                    && possible.is_empty()
            }
            IdentityRelation::ExplicitCopy | IdentityRelation::ExplicitSplit => {
                before.is_none()
                    && after
                        .as_ref()
                        .is_some_and(|(_, _, after_id)| !sources.contains(after_id))
                    && !sources.is_empty()
                    && (relation != IdentityRelation::ExplicitCopy || sources.len() == 1)
                    && possible.is_empty()
            }
            IdentityRelation::New => {
                before.is_none() && after.is_some() && sources.is_empty() && possible.is_empty()
            }
            IdentityRelation::Removed => {
                before.is_some() && after.is_none() && sources.is_empty() && possible.is_empty()
            }
            IdentityRelation::UncertainRename => {
                before.is_none() && after.is_some() && sources.is_empty() && !possible.is_empty()
            }
        };
        if !shape_ok {
            return Err(ContributionRevisionError::InvalidSourceChange);
        }
        let path = after
            .as_ref()
            .or(before.as_ref())
            .expect("valid relations always contain a side")
            .0
            .clone();
        if previous.as_ref().is_some_and(|last| last >= &path) {
            return Err(ContributionRevisionError::NonCanonicalChangeOrder);
        }
        previous = Some(path);
    }
    Ok(())
}

/// Parsed source file state: canonical raw path bytes.
fn object_state(
    object: &Object,
    field: &'static str,
) -> Result<Option<(Vec<u8>, EntryKind, FileId)>, ContributionRevisionError> {
    match object.get(field) {
        None => Ok(None),
        Some(Value::Object(state)) => {
            const FIELDS: &[&str] = &["path_hex", "mode", "content_sha256", "file_id"];
            if state.iter().any(|(name, _)| !FIELDS.contains(&name)) {
                return Err(ContributionRevisionError::InvalidSourceChange);
            }
            let path = decode_hex(object_string(state, "path_hex")?, "path_hex")?;
            let mode = object_string(state, "mode")?;
            let kind = EntryKind::from_git_mode(&mode)
                .map_err(|_| ContributionRevisionError::InvalidSourceChange)?;
            let content = decode_hex(object_string(state, "content_sha256")?, "content_sha256")?;
            let content: [u8; 32] = content
                .try_into()
                .map_err(|_| ContributionRevisionError::InvalidSourceChange)?;
            let file_id = FileId::parse(&object_string(state, "file_id")?)?;
            SourceSnapshot::new([SourceEntry::from_content_digest(
                path.clone(),
                kind,
                content,
            )])?;
            Ok(Some((path, kind, file_id)))
        }
        _ => Err(ContributionRevisionError::InvalidSourceChange),
    }
}

fn object_ids(
    object: &Object,
    field: &'static str,
) -> Result<Vec<FileId>, ContributionRevisionError> {
    match object.get(field) {
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| match value {
                Value::String(id) => FileId::parse(id).map_err(ContributionRevisionError::Project),
                _ => Err(ContributionRevisionError::InvalidSourceChange),
            })
            .collect(),
        _ => Err(ContributionRevisionError::InvalidSourceChange),
    }
}

fn object_string(
    object: &Object,
    field: &'static str,
) -> Result<String, ContributionRevisionError> {
    match object.get(field) {
        Some(Value::String(value)) => Ok(value.clone()),
        _ => Err(ContributionRevisionError::InvalidSourceChange),
    }
}

fn required_string(
    record: &Record,
    field: &'static str,
) -> Result<String, ContributionRevisionError> {
    match record.get(field) {
        Some(Value::String(value)) => Ok(value.clone()),
        _ => Err(ContributionRevisionError::WrongFieldType(field)),
    }
}

fn optional_string(
    record: &Record,
    field: &'static str,
) -> Result<Option<String>, ContributionRevisionError> {
    match record.get(field) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        _ => Err(ContributionRevisionError::WrongFieldType(field)),
    }
}

fn text(value: &str) -> Value {
    Value::String(value.to_string())
}

fn decode_hex(value: String, field: &'static str) -> Result<Vec<u8>, ContributionRevisionError> {
    let bytes = HEXLOWER
        .decode(value.as_bytes())
        .map_err(|_| ContributionRevisionError::InvalidSourceChange)?;
    if field == "content_sha256" && bytes.len() != 32 {
        return Err(ContributionRevisionError::InvalidSourceChange);
    }
    Ok(bytes)
}

fn contribution_base32_value(byte: u8) -> Option<u8> {
    match byte {
        b'a'..=b'z' => Some(byte - b'a'),
        b'2'..=b'7' => Some(byte - b'2' + 26),
        _ => None,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ContributionRevisionError {
    #[error("contribution revision exceeds the {MAX_CONTRIBUTION_REVISION_BYTES}-byte limit ({0})")]
    TooLarge(usize),
    #[error("contribution revision has more than {MAX_REVISION_CHANGES} source changes ({0})")]
    TooManyChanges(usize),
    #[error("contribution revision must be a JSON object")]
    NotAnObject,
    #[error("contribution revision contains unknown field {0:?}")]
    UnknownField(String),
    #[error("contribution revision field {0} has the wrong type")]
    WrongFieldType(&'static str),
    #[error("contribution revision has an invalid or incomplete brief reference")]
    IncompleteBriefReference,
    #[error("brief object digest must be 64 lowercase hex characters")]
    InvalidBriefDigest,
    #[error("contribution revision has duplicate dependency IDs")]
    DuplicateDependency,
    #[error("invalid logical contribution ID {0:?}; expected contrib:<random 128-bit base32>")]
    InvalidContributionId(String),
    #[error("contribution revision has an invalid source change")]
    InvalidSourceChange,
    #[error("contribution revision changes must be in strict raw-byte path order")]
    NonCanonicalChangeOrder,
    #[error("contribution revision bytes are not canonical for this profile")]
    NonCanonicalRecord,
    #[error("record error: {0}")]
    Record(#[from] RecordError),
    #[error("project/file identity error: {0}")]
    Project(#[from] super::project_identity::ProjectIdentityError),
    #[error("source snapshot error: {0}")]
    Snapshot(#[from] super::source_snapshot::SourceSnapshotError),
    #[error("canonical JSON error: {0}")]
    Canonical(#[from] canonical_json::CanonicalJsonError),
}
