//! Experimental local project and file identity records (plan §5.2–5.4, D10).
//!
//! A descriptor is a proposal until a broker validates its committed config
//! pointer and a local operator confirms its digest. These portable values do
//! not grant capture, budget, execution, review, or acceptance authority.

use std::collections::{BTreeMap, BTreeSet};

use data_encoding::HEXLOWER;
use thiserror::Error;

use super::canonical_json::{self, Object, Value};
use super::digest;
use super::record::{FieldKind, FieldSpec, Record, RecordError, RecordId, RecordSchema};
use super::source_snapshot::{
    EntryKind, SourceEntry, SourceSnapshot, SourceSnapshotError, SourceSnapshotId,
};

pub const PROJECT_DESCRIPTOR_SCHEMA_NAME: &str = "aethyme.project-descriptor/experimental-v0";
pub const PROJECT_DESCRIPTOR_PATH: &str = ".aethyme/project-descriptor.json";
pub const PROJECT_POLICY_PATH: &str = ".aethyme/config.toml";
pub const MAX_DESCRIPTOR_BYTES: usize = canonical_json::MAX_DOCUMENT_BYTES;
pub const MAX_DESCRIPTOR_FILES: usize = canonical_json::MAX_CONTAINER_ENTRIES;
const FILE_ID_DOMAIN: &[u8] = b"aethyme file identity v0\0";
const PROPOSAL_FILE_ID_DOMAIN: &[u8] = b"aethyme proposed file identity v0\0";

const fn field(name: &'static str, kind: FieldKind) -> FieldSpec {
    FieldSpec {
        name,
        required: true,
        kind,
        capability: None,
    }
}

pub static PROJECT_DESCRIPTOR_SCHEMA: RecordSchema = RecordSchema {
    name: PROJECT_DESCRIPTOR_SCHEMA_NAME,
    fields: &[
        field("project_id", FieldKind::String),
        field("anchor_commit", FieldKind::String),
        field("anchor_snapshot", FieldKind::String),
        field("source_binding", FieldKind::String),
        field("policy_location", FieldKind::String),
        field("acceptance_target", FieldKind::String),
        field("visibility_boundary", FieldKind::String),
        field("files", FieldKind::Opaque),
    ],
    capabilities: &[],
};

/// Logical identity for a project. It is distinct from the local ProjectKey
/// used to name a host-side collaboration store.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProjectId(String);

impl ProjectId {
    pub fn parse(value: &str) -> Result<Self, ProjectIdentityError> {
        let encoded = value.strip_prefix("proj:").unwrap_or_default();
        let valid = encoded.len() == 26
            && encoded.bytes().all(|byte| base32_value(byte).is_some())
            // 128 bits occupy 26 unpadded base32 symbols; the final symbol
            // carries only two payload bits and must have three zero bits.
            && encoded.as_bytes().last().is_some_and(|last| {
                base32_value(*last).is_some_and(|value| value % 4 == 0)
            });
        if valid {
            Ok(Self(value.to_string()))
        } else {
            Err(ProjectIdentityError::InvalidProjectId(value.to_string()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProjectId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Stable identity for one file lineage. File identity says nothing about
/// semantic compatibility or whether two changes can be composed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileId(String);

impl FileId {
    pub fn parse(value: &str) -> Result<Self, ProjectIdentityError> {
        let digest = value.strip_prefix("file:").unwrap_or_default();
        if super::digest::is_canonical(digest) {
            Ok(Self(value.to_string()))
        } else {
            Err(ProjectIdentityError::InvalidFileId(value.to_string()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn enrolled(project: &ProjectId, snapshot: &SourceSnapshotId, path: &[u8]) -> Self {
        let mut input = FILE_ID_DOMAIN.to_vec();
        input.extend_from_slice(project.as_str().as_bytes());
        input.push(0);
        input.extend_from_slice(snapshot.as_str().as_bytes());
        input.push(0);
        input.extend_from_slice(&(path.len() as u64).to_be_bytes());
        input.extend_from_slice(path);
        Self(format!("file:{}", digest::encode(&input)))
    }

    fn proposed(
        project: &ProjectId,
        proposal: &str,
        result: &SourceSnapshotId,
        ordinal: u64,
        path: &[u8],
    ) -> Self {
        let mut input = PROPOSAL_FILE_ID_DOMAIN.to_vec();
        input.extend_from_slice(project.as_str().as_bytes());
        input.push(0);
        input.extend_from_slice(proposal.as_bytes());
        input.push(0);
        input.extend_from_slice(result.as_str().as_bytes());
        input.push(0);
        input.extend_from_slice(&ordinal.to_be_bytes());
        input.extend_from_slice(&(path.len() as u64).to_be_bytes());
        input.extend_from_slice(path);
        Self(format!("file:{}", digest::encode(&input)))
    }
}

impl std::fmt::Display for FileId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrolledFile {
    pub path: Vec<u8>,
    pub kind: EntryKind,
    pub content_sha256: [u8; 32],
    pub file_id: FileId,
}

/// An experimental, local-only project descriptor anchored to an explicit
/// committed source snapshot. It deliberately names no acceptance authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectDescriptor {
    project_id: ProjectId,
    anchor_commit: String,
    anchor_snapshot: SourceSnapshotId,
    files: Vec<EnrolledFile>,
}

impl ProjectDescriptor {
    pub fn from_snapshot(
        project_id: ProjectId,
        anchor_commit: impl AsRef<str>,
        snapshot: &SourceSnapshot,
    ) -> Result<Self, ProjectIdentityError> {
        let anchor_commit = anchor_commit.as_ref();
        validate_commit(anchor_commit)?;
        let anchor_commit = anchor_commit.to_string();
        if snapshot.entries().len() > MAX_DESCRIPTOR_FILES {
            return Err(ProjectIdentityError::TooManyFiles {
                count: snapshot.entries().len(),
            });
        }
        let anchor_snapshot = snapshot.id();
        let empty_descriptor = descriptor_value(
            &project_id,
            &anchor_commit,
            &anchor_snapshot,
            Value::Array(Vec::new()),
        );
        let mut encoded_size =
            canonical_json::bounded_canonical_size(&empty_descriptor, MAX_DESCRIPTOR_BYTES).ok_or(
                ProjectIdentityError::DescriptorTooLarge {
                    bytes: MAX_DESCRIPTOR_BYTES + 1,
                },
            )?;
        let mut files = Vec::with_capacity(snapshot.entries().len());
        for entry in snapshot.entries() {
            let file_id = FileId::enrolled(&project_id, &anchor_snapshot, entry.path());
            // Measure one bounded entry before retaining its path bytes or
            // constructing the next path-hex Value.
            let encoded_file =
                descriptor_file_value(entry.path(), entry.kind(), entry.content_sha256(), &file_id);
            let file_size =
                canonical_json::bounded_canonical_size(&encoded_file, MAX_DESCRIPTOR_BYTES).ok_or(
                    ProjectIdentityError::DescriptorTooLarge {
                        bytes: MAX_DESCRIPTOR_BYTES + 1,
                    },
                )?;
            let comma_size = if files.is_empty() { 0 } else { 1 };
            encoded_size = encoded_size
                .checked_add(file_size)
                .and_then(|size| size.checked_add(comma_size))
                .ok_or(ProjectIdentityError::DescriptorTooLarge {
                    bytes: MAX_DESCRIPTOR_BYTES + 1,
                })?;
            if encoded_size > MAX_DESCRIPTOR_BYTES {
                return Err(ProjectIdentityError::DescriptorTooLarge {
                    bytes: encoded_size,
                });
            }
            files.push(EnrolledFile {
                path: entry.path().to_vec(),
                kind: entry.kind(),
                content_sha256: *entry.content_sha256(),
                file_id,
            });
        }
        let descriptor = Self {
            project_id,
            anchor_commit,
            anchor_snapshot,
            files,
        };
        descriptor.validate()?;
        Ok(descriptor)
    }

    pub fn project_id(&self) -> &ProjectId {
        &self.project_id
    }

    pub fn anchor_commit(&self) -> &str {
        &self.anchor_commit
    }

    pub fn anchor_snapshot(&self) -> &SourceSnapshotId {
        &self.anchor_snapshot
    }

    pub fn files(&self) -> &[EnrolledFile] {
        &self.files
    }

    pub fn file_map(&self) -> BTreeMap<Vec<u8>, FileId> {
        self.files
            .iter()
            .map(|file| (file.path.clone(), file.file_id.clone()))
            .collect()
    }

    /// Build the opaque initial identity state for this exact enrollment.
    /// This mapping value is not proof that the descriptor was approved or
    /// associated with another clone; the broker must validate the committed
    /// binding before using it as established local history.
    pub fn initial_file_identity_state(
        &self,
    ) -> Result<ProjectFileIdentityState, ProjectIdentityError> {
        self.validate()?;
        let files = self
            .files
            .iter()
            .map(|file| {
                (
                    file.path.clone(),
                    FileState {
                        path: file.path.clone(),
                        kind: file.kind,
                        content_sha256: file.content_sha256,
                        file_id: file.file_id.clone(),
                    },
                )
            })
            .collect();
        Ok(ProjectFileIdentityState {
            project_id: self.project_id.clone(),
            snapshot: self.anchor_snapshot.clone(),
            files,
        })
    }

    pub fn anchor_source(&self) -> Result<SourceSnapshot, ProjectIdentityError> {
        SourceSnapshot::new(self.files.iter().map(|file| {
            SourceEntry::from_content_digest(file.path.clone(), file.kind, file.content_sha256)
        }))
        .map_err(ProjectIdentityError::Snapshot)
    }

    /// Canonical descriptor bytes and their immutable RecordId.
    pub fn to_record(&self) -> (Vec<u8>, RecordId) {
        let bytes = self.canonical_bytes();
        let record = Record::decode(&bytes, &[&PROJECT_DESCRIPTOR_SCHEMA])
            .expect("descriptor constructors enforce the canonical record profile");
        (bytes, record.id())
    }

    fn canonical_bytes(&self) -> Vec<u8> {
        let files = self
            .files
            .iter()
            .map(|file| {
                descriptor_file_value(&file.path, file.kind, &file.content_sha256, &file.file_id)
            })
            .collect();
        descriptor_value(
            &self.project_id,
            &self.anchor_commit,
            &self.anchor_snapshot,
            Value::Array(files),
        )
        .to_canonical_bytes()
    }

    pub fn from_record(input: &[u8]) -> Result<(Self, RecordId), ProjectIdentityError> {
        if input.len() > MAX_DESCRIPTOR_BYTES {
            return Err(ProjectIdentityError::DescriptorTooLarge { bytes: input.len() });
        }
        let Value::Object(raw) = canonical_json::parse(input)? else {
            return Err(ProjectIdentityError::NotAnObject);
        };
        let allowed = [
            "schema",
            "requires",
            "extensions",
            "project_id",
            "anchor_commit",
            "anchor_snapshot",
            "source_binding",
            "policy_location",
            "acceptance_target",
            "visibility_boundary",
            "files",
        ];
        if let Some((name, _)) = raw.iter().find(|(name, _)| !allowed.contains(name)) {
            return Err(ProjectIdentityError::UnknownField(name.to_string()));
        }
        // This experimental descriptor is a closed profile. The generic
        // record envelope can carry extensions, but this typed value cannot
        // yet preserve them, so accepting one would change the RecordId on
        // re-encoding.
        if raw.get("extensions").is_some() {
            return Err(ProjectIdentityError::ExtensionsUnsupported);
        }
        let record = Record::decode(input, &[&PROJECT_DESCRIPTOR_SCHEMA])?;
        let project_id = ProjectId::parse(&required_string(&record, "project_id")?)?;
        let anchor_commit = required_string(&record, "anchor_commit")?;
        validate_commit(&anchor_commit)?;
        let anchor_snapshot =
            SourceSnapshotId::parse(&required_string(&record, "anchor_snapshot")?)
                .map_err(|error| ProjectIdentityError::InvalidAnchor(error.to_string()))?;
        if required_string(&record, "source_binding")? != format!("git-commit:{anchor_commit}")
            || required_string(&record, "policy_location")? != PROJECT_POLICY_PATH
            || required_string(&record, "acceptance_target")? != "unbound"
            || required_string(&record, "visibility_boundary")? != "local"
        {
            return Err(ProjectIdentityError::InvalidDescriptorBoundary);
        }
        let Value::Array(values) = record.get("files").expect("schema checked") else {
            return Err(ProjectIdentityError::WrongFieldType("files"));
        };
        if values.len() > MAX_DESCRIPTOR_FILES {
            return Err(ProjectIdentityError::TooManyFiles {
                count: values.len(),
            });
        }
        let mut files = Vec::with_capacity(values.len());
        let mut previous_path: Option<Vec<u8>> = None;
        for value in values {
            let Value::Object(entry) = value else {
                return Err(ProjectIdentityError::WrongFileEntry);
            };
            const FILE_FIELDS: &[&str] = &["path_hex", "mode", "content_sha256", "file_id"];
            if let Some((name, _)) = entry.iter().find(|(name, _)| !FILE_FIELDS.contains(name)) {
                return Err(ProjectIdentityError::UnknownFileField(name.to_string()));
            }
            let path = decode_hex(required_object_string(entry, "path_hex")?, "path_hex")?;
            let mode = required_object_string(entry, "mode")?;
            let kind = EntryKind::from_git_mode(&mode)
                .map_err(|error| ProjectIdentityError::InvalidFileEntry(error.to_string()))?;
            let digest_bytes = decode_hex(
                required_object_string(entry, "content_sha256")?,
                "content_sha256",
            )?;
            let content_sha256: [u8; 32] = digest_bytes.try_into().map_err(|_| {
                ProjectIdentityError::InvalidFileEntry("content digest must be 32 bytes".into())
            })?;
            if previous_path
                .as_ref()
                .is_some_and(|previous| previous >= &path)
            {
                return Err(ProjectIdentityError::NonCanonicalFileOrder);
            }
            previous_path = Some(path.clone());
            let file_id = FileId::parse(&required_object_string(entry, "file_id")?)?;
            files.push(EnrolledFile {
                path,
                kind,
                content_sha256,
                file_id,
            });
        }
        let descriptor = Self {
            project_id,
            anchor_commit,
            anchor_snapshot,
            files,
        };
        descriptor.validate()?;
        Ok((descriptor, record.id()))
    }

    pub fn validate(&self) -> Result<(), ProjectIdentityError> {
        validate_commit(&self.anchor_commit)?;
        if self.files.len() > MAX_DESCRIPTOR_FILES {
            return Err(ProjectIdentityError::TooManyFiles {
                count: self.files.len(),
            });
        }
        let encoded_bytes = self.canonical_bytes().len();
        if encoded_bytes > MAX_DESCRIPTOR_BYTES {
            return Err(ProjectIdentityError::DescriptorTooLarge {
                bytes: encoded_bytes,
            });
        }
        let source = self.anchor_source()?;
        if source.id() != self.anchor_snapshot {
            return Err(ProjectIdentityError::AnchorSnapshotMismatch {
                expected: self.anchor_snapshot.to_string(),
                actual: source.id().to_string(),
            });
        }
        let mut ids = BTreeSet::new();
        for file in &self.files {
            if file.file_id != FileId::enrolled(&self.project_id, &self.anchor_snapshot, &file.path)
            {
                return Err(ProjectIdentityError::InvalidFileEntry(
                    "enrollment FileId does not match project, snapshot, and raw path bytes".into(),
                ));
            }
            if !ids.insert(file.file_id.clone()) {
                return Err(ProjectIdentityError::DuplicateFileId(
                    file.file_id.to_string(),
                ));
            }
        }
        Ok(())
    }

    pub fn validate_anchor(
        &self,
        commit: &str,
        snapshot: &SourceSnapshot,
    ) -> Result<(), ProjectIdentityError> {
        if self.anchor_commit != commit {
            return Err(ProjectIdentityError::AnchorCommitMismatch {
                expected: self.anchor_commit.clone(),
                actual: commit.to_string(),
            });
        }
        if self.anchor_snapshot != snapshot.id() || self.anchor_source()? != *snapshot {
            return Err(ProjectIdentityError::AnchorSnapshotMismatch {
                expected: self.anchor_snapshot.to_string(),
                actual: snapshot.id().to_string(),
            });
        }
        Ok(())
    }
}

fn descriptor_file_value(
    path: &[u8],
    kind: EntryKind,
    content_sha256: &[u8; 32],
    file_id: &FileId,
) -> Value {
    Value::Object(
        Object::new(vec![
            ("path_hex".into(), text(&HEXLOWER.encode(path))),
            ("mode".into(), text(kind.git_mode())),
            (
                "content_sha256".into(),
                text(&HEXLOWER.encode(content_sha256)),
            ),
            ("file_id".into(), text(file_id.as_str())),
        ])
        .expect("distinct file fields"),
    )
}

fn descriptor_value(
    project_id: &ProjectId,
    anchor_commit: &str,
    anchor_snapshot: &SourceSnapshotId,
    files: Value,
) -> Value {
    let fields = vec![
        ("schema".into(), text(PROJECT_DESCRIPTOR_SCHEMA_NAME)),
        ("project_id".into(), text(project_id.as_str())),
        ("anchor_commit".into(), text(anchor_commit)),
        ("anchor_snapshot".into(), text(anchor_snapshot.as_str())),
        (
            "source_binding".into(),
            text(&format!("git-commit:{anchor_commit}")),
        ),
        ("policy_location".into(), text(PROJECT_POLICY_PATH)),
        ("acceptance_target".into(), text("unbound")),
        ("visibility_boundary".into(), text("local")),
        ("files".into(), files),
    ];
    Value::Object(Object::new(fields).expect("distinct descriptor fields"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileState {
    pub path: Vec<u8>,
    pub kind: EntryKind,
    pub content_sha256: [u8; 32],
    pub file_id: FileId,
}

/// Opaque FileId map tied to one logical project and one exact snapshot.
/// Only a descriptor or a validated transition can create this value, so a
/// caller cannot attach an arbitrary map to a chosen ProjectId.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectFileIdentityState {
    project_id: ProjectId,
    snapshot: SourceSnapshotId,
    files: BTreeMap<Vec<u8>, FileState>,
}

impl ProjectFileIdentityState {
    pub fn project_id(&self) -> &ProjectId {
        &self.project_id
    }

    pub fn snapshot(&self) -> &SourceSnapshotId {
        &self.snapshot
    }

    pub fn files(&self) -> impl Iterator<Item = &FileState> {
        self.files.values()
    }

    pub fn file_id(&self, path: &[u8]) -> Option<&FileId> {
        self.files.get(path).map(|file| &file.file_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityRelation {
    SamePath,
    ExplicitRename,
    ExplicitCopy,
    ExplicitSplit,
    New,
    Removed,
    UncertainRename,
}

impl IdentityRelation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SamePath => "same_path",
            Self::ExplicitRename => "explicit_rename",
            Self::ExplicitCopy => "explicit_copy",
            Self::ExplicitSplit => "explicit_split",
            Self::New => "new",
            Self::Removed => "removed",
            Self::UncertainRename => "uncertain_rename",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub before: Option<FileState>,
    pub after: Option<FileState>,
    pub relation: IdentityRelation,
    pub derived_from: Vec<FileId>,
    pub possible_sources: Vec<FileId>,
}

impl FileChange {
    pub fn to_value(&self) -> Value {
        let state_value = |state: &FileState| {
            Value::Object(
                Object::new(vec![
                    ("path_hex".into(), text(&HEXLOWER.encode(&state.path))),
                    ("mode".into(), text(state.kind.git_mode())),
                    (
                        "content_sha256".into(),
                        text(&HEXLOWER.encode(&state.content_sha256)),
                    ),
                    ("file_id".into(), text(state.file_id.as_str())),
                ])
                .expect("distinct file state fields"),
            )
        };
        let derived = self
            .derived_from
            .iter()
            .map(|id| text(id.as_str()))
            .collect();
        let possible = self
            .possible_sources
            .iter()
            .map(|id| text(id.as_str()))
            .collect();
        let mut fields = vec![
            ("relation".into(), text(self.relation.as_str())),
            ("derived_from".into(), Value::Array(derived)),
            ("possible_sources".into(), Value::Array(possible)),
        ];
        if let Some(before) = &self.before {
            fields.push(("before".into(), state_value(before)));
        }
        if let Some(after) = &self.after {
            fields.push(("after".into(), state_value(after)));
        }
        Value::Object(Object::new(fields).expect("distinct file change fields"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOperation {
    Rename { from: Vec<u8>, to: Vec<u8> },
    Copy { from: Vec<u8>, to: Vec<u8> },
    Split { from: Vec<u8>, to: Vec<Vec<u8>> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileIdentityTransition {
    project_id: ProjectId,
    proposal_id: String,
    base: SourceSnapshotId,
    result: SourceSnapshotId,
    result_files: Vec<FileState>,
    changes: Vec<FileChange>,
}

impl FileIdentityTransition {
    pub fn project_id(&self) -> &ProjectId {
        &self.project_id
    }

    pub fn proposal_id(&self) -> &str {
        &self.proposal_id
    }

    pub fn base(&self) -> &SourceSnapshotId {
        &self.base
    }

    pub fn result(&self) -> &SourceSnapshotId {
        &self.result
    }

    pub fn result_files(&self) -> &[FileState] {
        &self.result_files
    }

    pub fn changes(&self) -> &[FileChange] {
        &self.changes
    }

    pub fn result_identity_state(&self) -> ProjectFileIdentityState {
        ProjectFileIdentityState {
            project_id: self.project_id.clone(),
            snapshot: self.result.clone(),
            files: self
                .result_files
                .iter()
                .cloned()
                .map(|file| (file.path.clone(), file))
                .collect(),
        }
    }
}

/// Advance file identities from an exact, previously known base. Unspecified
/// delete/add pairs are never treated as renames; exact-content matches are
/// surfaced as uncertain candidates until an explicit operation is supplied.
pub fn advance_file_identities(
    project_id: &ProjectId,
    base_state: &ProjectFileIdentityState,
    proposal_id: &str,
    base: &SourceSnapshot,
    result: &SourceSnapshot,
    operations: &[FileOperation],
) -> Result<FileIdentityTransition, ProjectIdentityError> {
    validate_proposal_id(proposal_id)?;
    if &base_state.project_id != project_id {
        return Err(ProjectIdentityError::ForeignProjectState);
    }
    if base_state.snapshot != base.id() {
        return Err(ProjectIdentityError::BaseIdentitySnapshotMismatch);
    }
    let base_entries: BTreeMap<Vec<u8>, &SourceEntry> = base
        .entries()
        .iter()
        .map(|entry| (entry.path().to_vec(), entry))
        .collect();
    if base_state.files.len() != base_entries.len()
        || base_entries.iter().any(|(path, entry)| {
            base_state.files.get(path).is_none_or(|state| {
                state.path != *path
                    || state.kind != entry.kind()
                    || state.content_sha256 != *entry.content_sha256()
            })
        })
    {
        return Err(ProjectIdentityError::BaseIdentitySnapshotMismatch);
    }
    let base_ids: BTreeMap<Vec<u8>, FileId> = base_state
        .files
        .iter()
        .map(|(path, state)| (path.clone(), state.file_id.clone()))
        .collect();
    let result_entries: BTreeMap<Vec<u8>, &SourceEntry> = result
        .entries()
        .iter()
        .map(|entry| (entry.path().to_vec(), entry))
        .collect();
    let unique_base_ids: BTreeSet<FileId> = base_ids.values().cloned().collect();
    if unique_base_ids.len() != base_ids.len() {
        return Err(ProjectIdentityError::DuplicateBaseFileId);
    }

    let mut operation_by_destination: BTreeMap<Vec<u8>, (IdentityRelation, FileId, Vec<FileId>)> =
        BTreeMap::new();
    let mut operation_by_source: BTreeMap<Vec<u8>, (IdentityRelation, Vec<Vec<u8>>)> =
        BTreeMap::new();
    let mut used_paths = BTreeSet::new();
    for operation in operations {
        match operation {
            FileOperation::Rename { from, to } => {
                let before = base_entries.get(from).ok_or_else(|| {
                    ProjectIdentityError::InvalidOperation(
                        "rename source is absent from the exact base snapshot".into(),
                    )
                })?;
                let after = result_entries.get(to).ok_or_else(|| {
                    ProjectIdentityError::InvalidOperation(
                        "rename destination is absent from the exact result snapshot".into(),
                    )
                })?;
                if base_entries.contains_key(to)
                    || result_entries.contains_key(from)
                    || !same_content(before, after)
                    || !used_paths.insert(from.clone())
                    || !used_paths.insert(to.clone())
                {
                    return Err(ProjectIdentityError::InvalidOperation(
                        "rename needs one removed source and one new destination with identical mode and content".into(),
                    ));
                }
                operation_by_destination.insert(
                    to.clone(),
                    (
                        IdentityRelation::ExplicitRename,
                        base_ids[from].clone(),
                        Vec::new(),
                    ),
                );
                operation_by_source.insert(
                    from.clone(),
                    (IdentityRelation::ExplicitRename, vec![to.clone()]),
                );
            }
            FileOperation::Copy { from, to } => {
                let before = base_entries.get(from).ok_or_else(|| {
                    ProjectIdentityError::InvalidOperation(
                        "copy source is absent from the exact base snapshot".into(),
                    )
                })?;
                let source_after = result_entries.get(from).ok_or_else(|| {
                    ProjectIdentityError::InvalidOperation(
                        "copy source is absent from the result snapshot".into(),
                    )
                })?;
                let copied = result_entries.get(to).ok_or_else(|| {
                    ProjectIdentityError::InvalidOperation(
                        "copy destination is absent from the exact result snapshot".into(),
                    )
                })?;
                if base_entries.contains_key(to)
                    || !same_content(before, source_after)
                    || !same_content(before, copied)
                    || !used_paths.insert(to.clone())
                {
                    return Err(ProjectIdentityError::InvalidOperation(
                        "copy needs an unchanged source and a new destination with identical mode and content".into(),
                    ));
                }
                operation_by_destination.insert(
                    to.clone(),
                    (
                        IdentityRelation::ExplicitCopy,
                        FileId::proposed(project_id, proposal_id, &result.id(), 0, to),
                        vec![base_ids[from].clone()],
                    ),
                );
            }
            FileOperation::Split { from, to } => {
                let _before = base_entries.get(from).ok_or_else(|| {
                    ProjectIdentityError::InvalidOperation(
                        "split source is absent from the exact base snapshot".into(),
                    )
                })?;
                if result_entries.contains_key(from)
                    || to.len() < 2
                    || !used_paths.insert(from.clone())
                {
                    return Err(ProjectIdentityError::InvalidOperation(
                        "split needs a removed source and at least two new result paths".into(),
                    ));
                }
                let mut destinations = to.clone();
                destinations.sort();
                destinations.dedup();
                if destinations.len() != to.len() {
                    return Err(ProjectIdentityError::InvalidOperation(
                        "split destinations repeat".into(),
                    ));
                }
                for (ordinal, path) in destinations.iter().enumerate() {
                    if base_entries.contains_key(path)
                        || !result_entries.contains_key(path)
                        || !used_paths.insert(path.clone())
                    {
                        return Err(ProjectIdentityError::InvalidOperation(
                            "split destinations must be new, unique paths in the result snapshot"
                                .into(),
                        ));
                    }
                    operation_by_destination.insert(
                        path.clone(),
                        (
                            IdentityRelation::ExplicitSplit,
                            FileId::proposed(
                                project_id,
                                proposal_id,
                                &result.id(),
                                ordinal as u64,
                                path,
                            ),
                            vec![base_ids[from].clone()],
                        ),
                    );
                }
                operation_by_source.insert(
                    from.clone(),
                    (IdentityRelation::ExplicitSplit, destinations),
                );
            }
        }
    }

    let mut ordinals = BTreeMap::new();
    for (ordinal, path) in result_entries
        .keys()
        .filter(|path| !base_entries.contains_key(*path))
        .enumerate()
    {
        ordinals.insert(path.clone(), ordinal as u64);
    }
    let mut result_files = Vec::with_capacity(result_entries.len());
    for (path, entry) in &result_entries {
        let identity = if let Some(existing) = base_ids.get(path) {
            existing.clone()
        } else if let Some((_, id, _)) = operation_by_destination.get(path) {
            id.clone()
        } else {
            FileId::proposed(project_id, proposal_id, &result.id(), ordinals[path], path)
        };
        result_files.push(FileState {
            path: path.clone(),
            kind: entry.kind(),
            content_sha256: *entry.content_sha256(),
            file_id: identity,
        });
    }
    let result_by_path: BTreeMap<Vec<u8>, FileState> = result_files
        .iter()
        .cloned()
        .map(|state| (state.path.clone(), state))
        .collect();

    let mut changes = Vec::new();
    let mut explicitly_renamed_sources = BTreeSet::new();
    for (path, before_entry) in &base_entries {
        let before_state = FileState {
            path: path.clone(),
            kind: before_entry.kind(),
            content_sha256: *before_entry.content_sha256(),
            file_id: base_ids[path].clone(),
        };
        if let Some((relation, destinations)) = operation_by_source.get(path) {
            explicitly_renamed_sources.insert(path.clone());
            if *relation == IdentityRelation::ExplicitRename {
                let after = result_by_path[&destinations[0]].clone();
                changes.push(FileChange {
                    before: Some(before_state),
                    after: Some(after),
                    relation: *relation,
                    derived_from: Vec::new(),
                    possible_sources: Vec::new(),
                });
            } else {
                changes.push(FileChange {
                    before: Some(before_state),
                    after: None,
                    relation: IdentityRelation::Removed,
                    derived_from: Vec::new(),
                    possible_sources: Vec::new(),
                });
            }
            continue;
        }
        match result_by_path.get(path) {
            Some(after)
                if before_state.kind == after.kind
                    && before_state.content_sha256 == after.content_sha256 => {}
            Some(after) => changes.push(FileChange {
                before: Some(before_state),
                after: Some(after.clone()),
                relation: IdentityRelation::SamePath,
                derived_from: Vec::new(),
                possible_sources: Vec::new(),
            }),
            None => changes.push(FileChange {
                before: Some(before_state),
                after: None,
                relation: IdentityRelation::Removed,
                derived_from: Vec::new(),
                possible_sources: Vec::new(),
            }),
        }
    }
    for (path, after) in &result_by_path {
        if base_entries.contains_key(path) {
            continue;
        }
        if let Some((relation, _, derived_from)) = operation_by_destination.get(path) {
            if *relation == IdentityRelation::ExplicitRename {
                // The paired move was emitted from its source path.
                continue;
            }
            changes.push(FileChange {
                before: None,
                after: Some(after.clone()),
                relation: *relation,
                derived_from: derived_from.clone(),
                possible_sources: Vec::new(),
            });
        } else {
            let possible_sources: Vec<FileId> = base_entries
                .iter()
                .filter(|(source, entry)| {
                    !result_entries.contains_key(*source)
                        && same_content(entry, result_entries[path])
                })
                .map(|(source, _)| base_ids[source].clone())
                .collect();
            changes.push(FileChange {
                before: None,
                after: Some(after.clone()),
                relation: if possible_sources.is_empty() {
                    IdentityRelation::New
                } else {
                    IdentityRelation::UncertainRename
                },
                derived_from: Vec::new(),
                possible_sources,
            });
        }
    }
    changes.sort_by(|left, right| {
        change_sort_path(left)
            .cmp(change_sort_path(right))
            .then_with(|| left.relation.as_str().cmp(right.relation.as_str()))
    });
    Ok(FileIdentityTransition {
        project_id: project_id.clone(),
        proposal_id: proposal_id.to_string(),
        base: base.id(),
        result: result.id(),
        result_files,
        changes,
    })
}

/// Refuse two independent proposals that create the same path, even when the
/// bytes match. Existing base paths remain for the composer to reconcile.
pub fn validate_file_proposals(
    proposals: &[FileIdentityTransition],
) -> Result<(), FileNamespaceConflict> {
    let Some(first) = proposals.first() else {
        return Ok(());
    };
    let mut proposal_ids = BTreeSet::new();
    let mut by_path: BTreeMap<Vec<u8>, BTreeSet<String>> = BTreeMap::new();
    for proposal in proposals {
        if proposal.project_id != first.project_id || proposal.base != first.base {
            return Err(FileNamespaceConflict {
                path: Vec::new(),
                proposal_ids: vec![first.proposal_id.clone(), proposal.proposal_id.clone()],
                kind: FileNamespaceConflictKind::IncompatibleNamespace,
            });
        }
        if !proposal_ids.insert(proposal.proposal_id.clone()) {
            return Err(FileNamespaceConflict {
                path: Vec::new(),
                proposal_ids: vec![proposal.proposal_id.clone()],
                kind: FileNamespaceConflictKind::DuplicateProposal,
            });
        }
        for change in &proposal.changes {
            let Some(after) = change.after.as_ref() else {
                continue;
            };
            let destination_is_new = match change.relation {
                IdentityRelation::ExplicitRename => change
                    .before
                    .as_ref()
                    .is_some_and(|before| before.path != after.path),
                IdentityRelation::New
                | IdentityRelation::UncertainRename
                | IdentityRelation::ExplicitCopy
                | IdentityRelation::ExplicitSplit => change.before.is_none(),
                _ => false,
            };
            if destination_is_new {
                by_path
                    .entry(after.path.clone())
                    .or_default()
                    .insert(proposal.proposal_id.clone());
            }
        }
    }
    if let Some((path, proposal_ids)) = by_path.into_iter().find(|(_, ids)| ids.len() > 1) {
        return Err(FileNamespaceConflict {
            path,
            proposal_ids: proposal_ids.into_iter().collect(),
            kind: FileNamespaceConflictKind::CompetingPath,
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileNamespaceConflictKind {
    CompetingPath,
    IncompatibleNamespace,
    DuplicateProposal,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("file identity proposals are incompatible ({kind:?}) at path {}: {}", HEXLOWER.encode(path), proposal_ids.join(", "))]
pub struct FileNamespaceConflict {
    pub path: Vec<u8>,
    pub proposal_ids: Vec<String>,
    pub kind: FileNamespaceConflictKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProjectIdentityError {
    #[error("invalid logical project ID {0:?}")]
    InvalidProjectId(String),
    #[error("invalid FileId {0:?}")]
    InvalidFileId(String),
    #[error("invalid anchor commit {0:?}; expected a full lowercase Git object ID")]
    InvalidCommit(String),
    #[error("descriptor is {bytes} bytes; the profile limit is {MAX_DESCRIPTOR_BYTES}")]
    DescriptorTooLarge { bytes: usize },
    #[error("descriptor contains {count} files; the profile limit is {MAX_DESCRIPTOR_FILES}")]
    TooManyFiles { count: usize },
    #[error("descriptor must be a JSON object")]
    NotAnObject,
    #[error("descriptor contains unknown field {0:?}")]
    UnknownField(String),
    #[error("descriptor extensions are unsupported by this closed experimental profile")]
    ExtensionsUnsupported,
    #[error("descriptor field {0} has the wrong type")]
    WrongFieldType(&'static str),
    #[error("descriptor file entry is not an object")]
    WrongFileEntry,
    #[error("descriptor file entry contains unknown field {0:?}")]
    UnknownFileField(String),
    #[error("descriptor files must be in strict raw-byte path order")]
    NonCanonicalFileOrder,
    #[error("base FileId map does not exactly cover the base source snapshot")]
    BaseIdentitySetMismatch,
    #[error("FileId state belongs to a different project than the requested transition")]
    ForeignProjectState,
    #[error("FileId state does not describe the exact supplied base snapshot")]
    BaseIdentitySnapshotMismatch,
    #[error("base source snapshot assigns one FileId to more than one path")]
    DuplicateBaseFileId,
    #[error("invalid descriptor file entry: {0}")]
    InvalidFileEntry(String),
    #[error("descriptor has duplicate FileId {0}")]
    DuplicateFileId(String),
    #[error("descriptor has an invalid source, policy, target, or visibility boundary")]
    InvalidDescriptorBoundary,
    #[error("anchor snapshot mismatch: expected {expected}, found {actual}")]
    AnchorSnapshotMismatch { expected: String, actual: String },
    #[error("anchor commit mismatch: expected {expected}, found {actual}")]
    AnchorCommitMismatch { expected: String, actual: String },
    #[error("base snapshot has no FileId for path hex {0}")]
    UnknownBasePath(String),
    #[error("invalid file operation: {0}")]
    InvalidOperation(String),
    #[error("proposal ID must be 1–128 ASCII letters, digits, '.', '_', ':' or '-'")]
    InvalidProposalId,
    #[error("record error: {0}")]
    Record(#[from] RecordError),
    #[error("source snapshot error: {0}")]
    Snapshot(#[from] SourceSnapshotError),
    #[error("canonical JSON error: {0}")]
    Canonical(#[from] canonical_json::CanonicalJsonError),
    #[error("invalid anchor snapshot: {0}")]
    InvalidAnchor(String),
}

fn required_string(record: &Record, name: &'static str) -> Result<String, ProjectIdentityError> {
    match record.get(name) {
        Some(Value::String(value)) => Ok(value.clone()),
        _ => Err(ProjectIdentityError::WrongFieldType(name)),
    }
}

fn required_object_string(
    object: &Object,
    name: &'static str,
) -> Result<String, ProjectIdentityError> {
    match object.get(name) {
        Some(Value::String(value)) => Ok(value.clone()),
        _ => Err(ProjectIdentityError::InvalidFileEntry(format!(
            "field {name} must be a string"
        ))),
    }
}

fn text(value: &str) -> Value {
    Value::String(value.to_string())
}

fn decode_hex(value: String, field: &'static str) -> Result<Vec<u8>, ProjectIdentityError> {
    HEXLOWER.decode(value.as_bytes()).map_err(|_| {
        ProjectIdentityError::InvalidFileEntry(format!("{field} must be lowercase hex"))
    })
}

fn validate_commit(value: &str) -> Result<(), ProjectIdentityError> {
    if (value.len() == 40 || value.len() == 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(ProjectIdentityError::InvalidCommit(value.to_string()))
    }
}

fn validate_proposal_id(value: &str) -> Result<(), ProjectIdentityError> {
    let valid = !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(ProjectIdentityError::InvalidProposalId)
    }
}

fn same_content(left: &SourceEntry, right: &SourceEntry) -> bool {
    left.kind() == right.kind() && left.content_sha256() == right.content_sha256()
}

fn change_sort_path(change: &FileChange) -> &[u8] {
    change
        .after
        .as_ref()
        .or(change.before.as_ref())
        .map(|state| state.path.as_slice())
        .unwrap_or_default()
}

fn base32_value(byte: u8) -> Option<u8> {
    match byte {
        b'a'..=b'z' => Some(byte - b'a'),
        b'2'..=b'7' => Some(byte - b'2' + 26),
        _ => None,
    }
}
