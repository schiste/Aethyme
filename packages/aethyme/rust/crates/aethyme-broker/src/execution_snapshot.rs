//! Exact Local execution inputs (#670). Descriptors are not verification.
//! All source comes from the retained CAS; materialization uses raw bytes and
//! executes no Git hook/filter, preparation, build or candidate code. #671
//! enforces the read-only workload mount and supervises output writers.

use crate::collaboration_archive::{self as archive, ObjectDigest};
use crate::collaboration_state::CollaborationStore;
use crate::local_target::{self as target, ControllerOwner};
use aethyme_contracts::experimental_v0::source_snapshot::EntryKind;
use aethyme_contracts::experimental_v0::{
    FieldKind, FieldSpec, Record, RecordId, RecordSchema, SourceSnapshot, SourceSnapshotId,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

pub const SCHEMA_NAME: &str = "aethyme.execution-snapshot/provisional-v0";
pub const MATERIALIZATION_PROFILE: &str = "aethyme.raw-linux-inputs/provisional-v0";
pub(crate) const MAX_FILES: usize = 8192;
pub(crate) const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
pub(crate) const MAX_INPUT_BYTES: u64 = 256 * 1024 * 1024;
static SCHEMA: RecordSchema = RecordSchema {
    name: SCHEMA_NAME,
    fields: &[
        FieldSpec {
            name: "target_revision",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
        FieldSpec {
            name: "spec",
            required: true,
            kind: FieldKind::Opaque,
            capability: None,
        },
    ],
    capabilities: &[],
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSpec {
    /// Absolute executable in the pinned container image. This is an operator
    /// recipe; a candidate-controlled script is not an independent oracle.
    pub program: String,
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceLimits {
    pub timeout_ms: u64,
    pub memory_bytes: u64,
    pub output_bytes: u64,
    pub pids: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinuxProfile {
    /// Exact local Docker image identities, never mutable tags.
    pub build_image: String,
    pub verifier_image: String,
    pub architecture: String,
    /// Approved runtime/configuration descriptor. #671 verifies actual runtime.
    pub runtime_profile: String,
    pub build_limits: ResourceLimits,
    pub verifier_limits: ResourceLimits,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preparation {
    /// Identity of the allowlisted preparation recipe; declaring it does not
    /// prove execution. The supervisor supplies the observation in #671.
    pub recipe: String,
    pub inputs: Vec<String>,
    pub materialized_source: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionSpec {
    pub candidate_source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preparation: Option<Preparation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependencies: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configuration: Option<String>,
    pub harness: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dataset: Option<String>,
    pub assertion_inventory: String,
    pub materialization_profile: String,
    pub profile: LinuxProfile,
    pub build: CommandSpec,
    pub verify: CommandSpec,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRef {
    pub id: RecordId,
    pub object: ObjectDigest,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionSnapshot {
    reference: SnapshotRef,
    target_revision: RecordId,
    spec: ExecutionSpec,
}
impl ExecutionSnapshot {
    pub fn id(&self) -> &RecordId {
        &self.reference.id
    }
    pub fn reference(&self) -> &SnapshotRef {
        &self.reference
    }
    pub fn target_revision(&self) -> &RecordId {
        &self.target_revision
    }
    pub fn spec(&self) -> &ExecutionSpec {
        &self.spec
    }
}
#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("unsupported execution input: {0}")]
    Unsupported(String),
    #[error("invalid execution descriptor: {0}")]
    Invalid(String),
    #[error("execution inputs exceed the selected profile limit")]
    Limit,
    #[error("execution descriptor or materialized bytes differ from their identity")]
    Tampered,
    #[error("execution snapshot belongs to another target or inactive revision")]
    Stale,
    #[error(transparent)]
    Target(#[from] target::TargetError),
    #[error(transparent)]
    Sensitive(#[from] crate::collaboration_sensitive::SensitiveError),
    #[error(transparent)]
    Archive(#[from] archive::ArchiveError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Source(#[from] aethyme_contracts::experimental_v0::SourceSnapshotError),
}
fn reference(value: &str) -> Result<RecordId, SnapshotError> {
    RecordId::parse(value).map_err(|_| SnapshotError::Invalid("digest reference".into()))
}
fn source(value: &str) -> Result<SourceSnapshotId, SnapshotError> {
    SourceSnapshotId::parse(value).map_err(|_| SnapshotError::Invalid("source reference".into()))
}
fn text(value: &str) -> bool {
    value.len() <= 4096 && !value.contains('\0')
}
fn command(value: &CommandSpec) -> Result<(), SnapshotError> {
    if !value.program.starts_with('/')
        || !text(&value.program)
        || value.program.split('/').any(|v| v == ".." || v == ".")
        || value.arguments.len() > 256
        || value.arguments.iter().any(|v| !text(v))
        || value.environment.len() > 64
        || value.environment.iter().any(|(key, val)| {
            key.is_empty()
                || key.len() > 128
                || !key
                    .bytes()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
                || !text(val)
        })
    {
        return Err(SnapshotError::Invalid("command or environment".into()));
    }
    Ok(())
}
fn limits(value: ResourceLimits) -> Result<(), SnapshotError> {
    if value.timeout_ms == 0
        || value.timeout_ms > 3_600_000
        || value.memory_bytes < 16 * 1024 * 1024
        || value.memory_bytes > 8 * 1024 * 1024 * 1024
        || value.output_bytes == 0
        || value.output_bytes > MAX_INPUT_BYTES
        || value.pids == 0
        || value.pids > 512
    {
        return Err(SnapshotError::Invalid("resource limits".into()));
    }
    Ok(())
}
fn validate(spec: &ExecutionSpec) -> Result<Vec<SourceSnapshotId>, SnapshotError> {
    if spec.materialization_profile != MATERIALIZATION_PROFILE
        || !matches!(spec.profile.architecture.as_str(), "arm64" | "amd64")
    {
        return Err(SnapshotError::Unsupported(
            "materialization or architecture".into(),
        ));
    }
    reference(&spec.profile.build_image)?;
    reference(&spec.profile.verifier_image)?;
    reference(&spec.profile.runtime_profile)?;
    reference(&spec.assertion_inventory)?;
    limits(spec.profile.build_limits)?;
    limits(spec.profile.verifier_limits)?;
    command(&spec.build)?;
    command(&spec.verify)?;
    let mut inputs = vec![source(&spec.candidate_source)?, source(&spec.harness)?];
    for value in [&spec.dependencies, &spec.configuration, &spec.dataset]
        .into_iter()
        .flatten()
    {
        inputs.push(source(value)?);
    }
    if let Some(preparation) = &spec.preparation {
        reference(&preparation.recipe)?;
        if preparation.inputs.len() > 32 {
            return Err(SnapshotError::Limit);
        }
        // Order is significant: the preparation witness does not invent a set.
        for value in &preparation.inputs {
            inputs.push(source(value)?);
        }
        inputs.push(source(&preparation.materialized_source)?);
    }
    inputs.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    inputs.dedup();
    Ok(inputs)
}
fn record(target: &RecordId, spec: &ExecutionSpec) -> Result<Record, SnapshotError> {
    Record::decode(&serde_json::to_vec(&serde_json::json!({"schema":SCHEMA_NAME,"target_revision":target.as_str(),"spec":spec}))?, &[&SCHEMA])
        .map_err(|e| SnapshotError::Invalid(e.to_string()))
}
fn current(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    revision: &RecordId,
) -> Result<(), SnapshotError> {
    let r = target::load_revision(store, revision)?;
    let state = target::status(store, owner.target())?;
    if r.target != owner.target()
        || r.issuer != owner.issuer()
        || state.revision.as_ref() != Some(revision)
    {
        return Err(SnapshotError::Stale);
    }
    Ok(())
}
fn current_under_response(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    revision: &RecordId,
    response: &crate::collaboration_sensitive::ResponseUse,
) -> Result<(), SnapshotError> {
    let r = target::load_revision_under_response(store, revision, response)?;
    let state = target::status(store, owner.target())?;
    if r.target != owner.target()
        || r.issuer != owner.issuer()
        || state.revision.as_ref() != Some(revision)
    {
        return Err(SnapshotError::Stale);
    }
    Ok(())
}
/// Freeze and retain every declared input. This creates no execution permission,
/// successful result or acceptance evidence. Old descriptors remain readable.
pub fn freeze(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    revision: &RecordId,
    spec: ExecutionSpec,
) -> Result<ExecutionSnapshot, SnapshotError> {
    freeze_bounded(store, owner, revision, spec, MAX_INPUT_BYTES)
}
fn freeze_bounded(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    revision: &RecordId,
    spec: ExecutionSpec,
    materialized_limit: u64,
) -> Result<ExecutionSnapshot, SnapshotError> {
    let inputs = validate(&spec)?;
    let archive_guard = crate::collaboration_gc::archive_use(store)?;
    let response_guard = crate::collaboration_sensitive::response_use(store)?;
    let authority_guard = target::authorize(store, owner)?;
    let permit = target::ArtifactRetentionPermit::new(
        store,
        owner,
        &archive_guard,
        &response_guard,
        &authority_guard,
    )?;
    current_under_response(store, owner, revision, &response_guard)?;
    let mut total = 0u64;
    for id in &inputs {
        inspect(store, id, &mut total)?;
    }
    inspect_roles(store, &spec, materialized_limit)?;
    let record = record(revision, &spec)?;
    let bytes = record.canonical_bytes();
    if bytes.len() > 1024 * 1024 {
        return Err(SnapshotError::Limit);
    }
    let digest = archive::put_object(store, &bytes)?;
    // Pin under one retained revision and the same archive lock as reclamation.
    target::retain_artifacts_under_guards(store, revision, &inputs, &[digest], &permit)?;
    Ok(ExecutionSnapshot {
        reference: SnapshotRef {
            id: record.id(),
            object: digest,
        },
        target_revision: revision.clone(),
        spec,
    })
}
/// Inspect an immutable descriptor. A record is data, not a verifier credential.
pub fn load(
    store: &CollaborationStore,
    locator: &SnapshotRef,
) -> Result<ExecutionSnapshot, SnapshotError> {
    let _archive = crate::collaboration_gc::archive_use(store)?;
    let _response = crate::collaboration_sensitive::response_use(store)?;
    let (execution, _) = read_snapshot(store, locator)?;
    let rooted: bool = store.read_connection().query_row(
        "SELECT EXISTS(SELECT 1 FROM local_target_roots WHERE revision=?1 AND kind='object' AND reference=?2)",
        rusqlite::params![execution.target_revision().as_str(), locator.object.hex()], |row| row.get(0)
    ).map_err(target::TargetError::from)?;
    if !rooted {
        return Err(SnapshotError::Invalid("snapshot is not retained".into()));
    }
    Ok(execution)
}

/// Load an execution descriptor through the exact durable completion that
/// retained it. This is historical provenance only: it does not grant current
/// target authority. The completion's immutable revision must match the
/// descriptor, and every declared input snapshot must remain rooted by that
/// same completion before its archive closure is read.
pub(crate) fn load_from_completion(
    store: &CollaborationStore,
    locator: &SnapshotRef,
    completion: &RecordId,
) -> Result<ExecutionSnapshot, SnapshotError> {
    let completion_spec = crate::local_controller::load(store, completion)
        .map_err(|_| SnapshotError::Invalid("completion record is unavailable".into()))?;
    let _archive = crate::collaboration_gc::archive_use(store)?;
    let _response = crate::collaboration_sensitive::response_use(store)?;
    let completion_bytes: Vec<u8> = store
        .read_connection()
        .query_row(
            "SELECT body FROM local_controller_completions WHERE id=?1",
            [completion.as_str()],
            |row| row.get(0),
        )
        .map_err(target::TargetError::from)?;
    let completion_object = ObjectDigest::of(&completion_bytes);
    if archive::read_object(store, &completion_object)? != completion_bytes {
        return Err(SnapshotError::Invalid(
            "completion record is not canonically retained".into(),
        ));
    }
    require_completion_root(store, completion, "object", completion_object.hex())?;
    let (execution, inputs) = read_snapshot(store, locator)?;
    if completion_spec.revision != execution.target_revision().as_str() {
        return Err(SnapshotError::Stale);
    }
    require_completion_root(store, completion, "object", locator.object.hex())?;
    let mut total = 0;
    for input in inputs {
        require_completion_root(store, completion, "snapshot", input.as_str())?;
        inspect(store, &input, &mut total)?;
    }
    Ok(execution)
}

fn read_snapshot(
    store: &CollaborationStore,
    locator: &SnapshotRef,
) -> Result<(ExecutionSnapshot, Vec<SourceSnapshotId>), SnapshotError> {
    let path = archive::object_path(store, &locator.object);
    if std::fs::metadata(path)?.len() > 1024 * 1024 {
        return Err(SnapshotError::Limit);
    }
    let bytes = archive::read_object(store, &locator.object)?;
    let value = Record::decode(&bytes, &[&SCHEMA]).map_err(|_| SnapshotError::Tampered)?;
    if value.id() != locator.id || value.canonical_bytes() != bytes {
        return Err(SnapshotError::Tampered);
    }
    let json: serde_json::Value = serde_json::from_slice(&bytes)?;
    let revision = reference(
        json["target_revision"]
            .as_str()
            .ok_or(SnapshotError::Tampered)?,
    )?;
    let spec: ExecutionSpec = serde_json::from_value(json["spec"].clone())?;
    let inputs = validate(&spec)?;
    Ok((
        ExecutionSnapshot {
            reference: locator.clone(),
            target_revision: revision,
            spec,
        },
        inputs,
    ))
}

fn require_completion_root(
    store: &CollaborationStore,
    completion: &RecordId,
    kind: &str,
    reference: &str,
) -> Result<(), SnapshotError> {
    let rooted: bool = store
        .read_connection()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM local_controller_completion_roots
             WHERE completion=?1 AND kind=?2 AND reference=?3)",
            rusqlite::params![completion.as_str(), kind, reference],
            |row| row.get(0),
        )
        .map_err(target::TargetError::from)?;
    if !rooted {
        return Err(SnapshotError::Invalid(format!(
            "execution {kind} is not retained by completion"
        )));
    }
    Ok(())
}

fn inspect(
    store: &CollaborationStore,
    id: &SourceSnapshotId,
    total: &mut u64,
) -> Result<SourceSnapshot, SnapshotError> {
    let retained = archive::retained(store, id)?
        .ok_or_else(|| SnapshotError::Invalid("required source is unavailable".into()))?;
    if retained.content_bytes > MAX_INPUT_BYTES || retained.entry_count > MAX_FILES as u64 {
        return Err(SnapshotError::Limit);
    }
    let manifest_digest = ObjectDigest::of_snapshot(id);
    if std::fs::metadata(archive::object_path(store, &manifest_digest))?.len() > MAX_FILE_BYTES {
        return Err(SnapshotError::Limit);
    }
    let bytes = archive::read_object(store, &manifest_digest)?;
    let snapshot = archive::parse_manifest(&bytes).ok_or(SnapshotError::Tampered)?;
    if snapshot.id() != *id || snapshot.manifest_bytes() != bytes {
        return Err(SnapshotError::Tampered);
    }
    if snapshot.entries().len() > MAX_FILES {
        return Err(SnapshotError::Limit);
    }
    portable(&snapshot)?;
    for entry in snapshot.entries() {
        let digest = ObjectDigest::from_bytes(*entry.content_sha256());
        if std::fs::metadata(archive::object_path(store, &digest))?.len() > MAX_FILE_BYTES {
            return Err(SnapshotError::Limit);
        }
        let contents =
            archive::read_object(store, &ObjectDigest::from_bytes(*entry.content_sha256()))?;
        if unresolved_lfs(&contents) {
            return Err(SnapshotError::Unsupported("unresolved LFS pointer".into()));
        }
        *total = total
            .checked_add(contents.len() as u64)
            .ok_or(SnapshotError::Limit)?;
        if contents.len() as u64 > MAX_FILE_BYTES || *total > MAX_INPUT_BYTES {
            return Err(SnapshotError::Limit);
        }
    }
    Ok(snapshot)
}
pub(crate) fn portable(snapshot: &SourceSnapshot) -> Result<(), SnapshotError> {
    let mut prefixes = BTreeMap::new();
    let mut files = BTreeSet::new();
    for entry in snapshot.entries() {
        let path = std::str::from_utf8(entry.path())
            .map_err(|_| SnapshotError::Unsupported("non-UTF-8 path".into()))?;
        if entry.kind() == EntryKind::Symlink || path.contains('\\') {
            return Err(SnapshotError::Unsupported(
                "symlink or nonportable path".into(),
            ));
        }
        // Refuse both exact filesystem collisions and folded directory aliases.
        let components: Vec<_> = path.split('/').collect();
        let mut folded = String::new();
        let mut original = String::new();
        for (index, component) in components.iter().enumerate() {
            if !folded.is_empty() {
                folded.push('/');
                original.push('/');
            }
            folded.extend(component.nfc().flat_map(char::to_lowercase));
            original.push_str(component);
            if files.contains(&folded)
                || prefixes.get(&folded).is_some_and(|old| old != &original)
                || (index + 1 == components.len() && prefixes.contains_key(&folded))
            {
                return Err(SnapshotError::Unsupported("folded path collision".into()));
            }
            prefixes.insert(folded.clone(), original.clone());
        }
        files.insert(folded);
    }
    Ok(())
}
/// An owned, fresh input generation. Host permissions are defense in depth;
/// the workload must still receive enforced read-only mounts in #671.
#[derive(Debug)]
pub struct MaterializedInputs {
    workspace: tempfile::TempDir,
    snapshot: ExecutionSnapshot,
}
impl MaterializedInputs {
    pub fn snapshot(&self) -> &ExecutionSnapshot {
        &self.snapshot
    }
    pub fn root(&self) -> &Path {
        self.workspace.path()
    }
    pub fn source_dir(&self) -> PathBuf {
        self.root().join("source")
    }
    pub fn harness_dir(&self) -> PathBuf {
        self.root().join("harness")
    }
}
pub fn materialize(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    snapshot: &ExecutionSnapshot,
) -> Result<MaterializedInputs, SnapshotError> {
    materialize_in(store, owner, snapshot, None)
}
pub(crate) fn materialize_in(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    snapshot: &ExecutionSnapshot,
    temporary_root: Option<&Path>,
) -> Result<MaterializedInputs, SnapshotError> {
    let mut before_reconstruct = |_: &str, _: &Path| {};
    let mut on_reconstruct_error = |_: &archive::ArchiveError| {};
    materialize_in_inner(
        store,
        owner,
        snapshot,
        temporary_root,
        &mut before_reconstruct,
        &mut on_reconstruct_error,
    )
}

#[cfg(all(test, target_os = "macos"))]
pub(crate) fn materialize_in_observed(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    snapshot: &ExecutionSnapshot,
    temporary_root: Option<&Path>,
    before_reconstruct: &mut dyn FnMut(&str, &Path),
    on_reconstruct_error: &mut dyn FnMut(&archive::ArchiveError),
) -> Result<MaterializedInputs, SnapshotError> {
    materialize_in_inner(
        store,
        owner,
        snapshot,
        temporary_root,
        before_reconstruct,
        on_reconstruct_error,
    )
}

fn materialize_in_inner(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    snapshot: &ExecutionSnapshot,
    temporary_root: Option<&Path>,
    before_reconstruct: &mut dyn FnMut(&str, &Path),
    on_reconstruct_error: &mut dyn FnMut(&archive::ArchiveError),
) -> Result<MaterializedInputs, SnapshotError> {
    let _authority = target::authorize(store, owner)?;
    current(store, owner, snapshot.target_revision())?;
    // Never trust a caller-edited descriptor or a stale retained ref.
    let retained = load(store, snapshot.reference())?;
    if retained != *snapshot {
        return Err(SnapshotError::Tampered);
    }
    let _archive = crate::collaboration_gc::archive_use(store)?;
    let spec = snapshot.spec();
    let mut builder = tempfile::Builder::new();
    builder.prefix("aethyme-execution-");
    let workspace = if let Some(root) = temporary_root {
        builder.tempdir_in(root)?
    } else {
        builder.tempdir()?
    };
    for (role, id, manifest) in inspect_roles(store, spec, MAX_INPUT_BYTES)? {
        let path = workspace.path().join(role);
        before_reconstruct(role, &path);
        if let Err(error) = archive::reconstruct(store, &id, &path) {
            on_reconstruct_error(&error);
            return Err(error.into());
        }
        verify_materialized(&path, &manifest)?;
    }
    Ok(MaterializedInputs {
        workspace,
        snapshot: snapshot.clone(),
    })
}
fn inspect_roles(
    store: &CollaborationStore,
    spec: &ExecutionSpec,
    limit: u64,
) -> Result<Vec<(&'static str, SourceSnapshotId, SourceSnapshot)>, SnapshotError> {
    let raw = spec
        .preparation
        .as_ref()
        .map(|p| p.materialized_source.as_str())
        .unwrap_or(&spec.candidate_source);
    let mut total = 0;
    let mut roles = Vec::new();
    for (role, id) in [
        ("source", Some(raw)),
        ("harness", Some(spec.harness.as_str())),
        ("dependencies", spec.dependencies.as_deref()),
        ("configuration", spec.configuration.as_deref()),
        ("dataset", spec.dataset.as_deref()),
    ] {
        if let Some(id) = id {
            let id = source(id)?;
            let manifest = inspect(store, &id, &mut total)?;
            if total > limit {
                return Err(SnapshotError::Limit);
            }
            roles.push((role, id, manifest));
        }
    }
    Ok(roles)
}
fn unresolved_lfs(bytes: &[u8]) -> bool {
    // Git LFS trims surrounding Unicode whitespace and accepts legacy version
    // aliases. Refuse recognized pointer headers conservatively, even malformed
    // metadata; this raw profile has no LFS resolver.
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let first = text.trim().lines().next().unwrap_or("");
    matches!(
        first,
        "version https://git-lfs.github.com/spec/v1"
            | "version https://hawser.github.com/spec/v1"
            | "version http://git-media.io/v/2"
    )
}
fn verify_materialized(path: &Path, manifest: &SourceSnapshot) -> Result<(), SnapshotError> {
    for entry in manifest.entries() {
        let name = std::str::from_utf8(entry.path()).map_err(|_| SnapshotError::Tampered)?;
        let file = path.join(name);
        let metadata = std::fs::symlink_metadata(&file)?;
        if !metadata.is_file()
            || ObjectDigest::of(&std::fs::read(&file)?).raw() != *entry.content_sha256()
        {
            return Err(SnapshotError::Tampered);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let executable = metadata.permissions().mode() & 0o111 != 0;
            if executable != (entry.kind() == EntryKind::Executable) {
                return Err(SnapshotError::Tampered);
            }
            std::fs::set_permissions(
                &file,
                std::fs::Permissions::from_mode(if executable { 0o555 } else { 0o444 }),
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
