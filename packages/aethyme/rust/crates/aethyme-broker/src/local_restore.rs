//! Bounded historical Local export and restore (#679). Import never recreates
//! native authority, a current context cache, an assignment or acceptance.
use crate::collaboration_archive::{self as archive, ObjectDigest};
use crate::collaboration_capture as capture;
use crate::collaboration_context as context;
use crate::collaboration_gc as gc;
use crate::collaboration_state::CollaborationStore;
use crate::local_target::{self as target, ControllerOwner};
use aethyme_contracts::experimental_v0::{Record, RecordId, RecordSchema, SourceSnapshotId};
use aethyme_contracts::experimental_v0::{brief, canonical_json};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const PROFILE: &str = "aethyme.restore.source-history/provisional-v0";
const MAGIC: &[u8] = b"AETHYME-LOCAL-EXPORT-v0\n";
const MAX_WIRE: usize = 32 * 1024 * 1024;
const MAX_CONTENT: usize = 16 * 1024 * 1024;
const MAX_OBJECT: usize = 8 * 1024 * 1024;
const MAX_OBJECTS: usize = 4096;
const MAX_RECORDS: usize = 128;
const MAX_SOURCES: usize = 64;
const LIMITATIONS: &[&str] = &[
    "source_and_advisory_history_only",
    "no_current_authority_or_lease",
    "original_git_commit_is_provenance_only",
    "context_cache_and_visibility_are_historical",
];
#[derive(Debug, thiserror::Error)]
pub enum RestoreError {
    #[error("invalid historical bundle: {0}")]
    Invalid(String),
    #[error("unsupported historical schema or reconstruction: {0}")]
    Unsupported(String),
    #[error("missing retained object or record: {0}")]
    Missing(String),
    #[error("historical bundle scope or digest differs from receiver policy")]
    Binding,
    #[error("reconstruction has unknown completion; inspect retained bytes")]
    Unknown,
    #[error(transparent)]
    Target(#[from] target::TargetError),
    #[error(transparent)]
    Archive(#[from] archive::ArchiveError),
    #[error(transparent)]
    Gc(#[from] gc::GcError),
    #[error(transparent)]
    Record(#[from] aethyme_contracts::experimental_v0::RecordError),
    #[error(transparent)]
    Canonical(#[from] canonical_json::CanonicalJsonError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    State(#[from] crate::collaboration_state::CollaborationStateError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Generation(#[from] crate::local_generation::GenerationError),
}
fn invalid(s: &str) -> RestoreError {
    RestoreError::Invalid(s.into())
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub origin: String,
    pub visibility: String,
    pub audience: String,
}
impl Scope {
    fn validate(&self) -> Result<(), RestoreError> {
        if !token(&self.origin) || self.visibility != "local_project" || !token(&self.audience) {
            return Err(RestoreError::Binding);
        }
        Ok(())
    }
}
fn token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
}
#[derive(Debug, Clone)]
pub struct RecordRef {
    pub id: RecordId,
    pub object: ObjectDigest,
}
#[derive(Debug, Clone)]
pub struct Selection {
    pub sources: Vec<SourceSnapshotId>,
    pub records: Vec<RecordRef>,
}
/// Trusted receiving configuration, selected independently of the wire.
#[derive(Debug, Clone)]
pub struct ReceiverPolicy {
    pub bundle: ObjectDigest,
    pub scope: Scope,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ObjectRef {
    digest: String,
    bytes: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct HistoricalRef {
    id: String,
    object: String,
    schema: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Header {
    profile: String,
    scope: Scope,
    sources: Vec<String>,
    records: Vec<HistoricalRef>,
    objects: Vec<ObjectRef>,
    limitations: Vec<String>,
}
/// Private validated data, deliberately not Deserialize. It grants no import
/// permission; fresh native receiving authority is still required.
pub struct VerifiedBundle {
    digest: ObjectDigest,
    header: Header,
    objects: BTreeMap<String, Vec<u8>>,
}
impl VerifiedBundle {
    pub fn digest(&self) -> &ObjectDigest {
        &self.digest
    }
    pub fn scope(&self) -> &Scope {
        &self.header.scope
    }
    pub fn limitations(&self) -> &[String] {
        &self.header.limitations
    }
}
fn canonical<T: Serialize>(value: &T) -> Result<Vec<u8>, RestoreError> {
    Ok(canonical_json::canonicalize(&serde_json::to_vec(value)?)?)
}
fn schema(name: &str) -> Result<&'static RecordSchema, RestoreError> {
    match name {
        archive::RETAINED_SNAPSHOT_SCHEMA_NAME => Ok(&archive::RETAINED_SNAPSHOT_SCHEMA),
        archive::CONTRIBUTION_LINEAGE_SCHEMA_NAME => Ok(&archive::CONTRIBUTION_LINEAGE_SCHEMA),
        capture::CAPTURE_RECEIPT_SCHEMA_NAME => Ok(&capture::CAPTURE_RECEIPT_SCHEMA),
        context::CONTEXT_SCHEMA_NAME => Ok(&context::CONTEXT_SCHEMA),
        brief::BRIEF_SCHEMA_NAME => Ok(&brief::BRIEF_SCHEMA),
        _ => Err(RestoreError::Unsupported(name.into())),
    }
}
fn record_value(bytes: &[u8], expected: &HistoricalRef) -> Result<serde_json::Value, RestoreError> {
    let decoder = schema(&expected.schema)?;
    let record = Record::decode(bytes, &[decoder])?;
    if record.id().as_str() != expected.id
        || record.canonical_bytes() != bytes
        || ObjectDigest::of(bytes).hex() != expected.object
    {
        return Err(invalid("historical record identity"));
    }
    if expected.schema == brief::BRIEF_SCHEMA_NAME {
        brief::Brief::from_record(bytes).map_err(|e| invalid(&e.to_string()))?;
    }
    Ok(serde_json::from_slice(bytes)?)
}
fn sorted(values: &[String]) -> bool {
    values.windows(2).all(|pair| pair[0] < pair[1])
}
fn text<'a>(value: &'a serde_json::Value, key: &str) -> Result<&'a str, RestoreError> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| invalid("historical reference shape"))
}
fn source<'a>(
    value: &serde_json::Value,
    key: &str,
    sources: &'a BTreeMap<String, aethyme_contracts::experimental_v0::SourceSnapshot>,
) -> Result<&'a aethyme_contracts::experimental_v0::SourceSnapshot, RestoreError> {
    let name = text(value, key)?;
    sources
        .get(name)
        .ok_or_else(|| RestoreError::Missing(name.into()))
}
fn linked<'a>(
    value: &serde_json::Value,
    key: &str,
    records: &'a BTreeMap<String, serde_json::Value>,
    name: &str,
) -> Result<&'a serde_json::Value, RestoreError> {
    let id = text(value, key)?;
    let record = records
        .get(id)
        .ok_or_else(|| RestoreError::Missing(id.into()))?;
    if text(record, "schema")? != name {
        return Err(invalid("historical reference schema"));
    }
    Ok(record)
}
fn check_graph(header: &Header, objects: &BTreeMap<String, Vec<u8>>) -> Result<(), RestoreError> {
    let mut sources = BTreeMap::new();
    let mut closure = BTreeSet::new();
    for name in &header.sources {
        let id = SourceSnapshotId::parse(name).map_err(|_| invalid("source identity"))?;
        let digest = ObjectDigest::of_snapshot(&id).hex();
        let bytes = objects
            .get(&digest)
            .ok_or_else(|| RestoreError::Missing(digest.clone()))?;
        let snapshot = archive::parse_manifest(bytes).ok_or_else(|| invalid("source manifest"))?;
        if snapshot.id() != id || snapshot.manifest_bytes() != *bytes {
            return Err(invalid("source manifest identity"));
        }
        closure.insert(digest);
        for entry in snapshot.entries() {
            let digest = ObjectDigest::from_bytes(*entry.content_sha256()).hex();
            if !objects.contains_key(&digest) {
                return Err(RestoreError::Missing(digest));
            }
            closure.insert(digest);
        }
        sources.insert(name.clone(), snapshot);
    }
    let mut records = BTreeMap::new();
    for reference in &header.records {
        let bytes = objects
            .get(&reference.object)
            .ok_or_else(|| RestoreError::Missing(reference.object.clone()))?;
        let value = record_value(bytes, reference)?;
        closure.insert(reference.object.clone());
        records.insert(reference.id.clone(), value);
    }
    for value in records.values() {
        match text(value, "schema")? {
            archive::RETAINED_SNAPSHOT_SCHEMA_NAME => {
                let snapshot = source(value, "snapshot", &sources)?;
                let content = snapshot.entries().iter().try_fold(0u64, |sum, entry| {
                    let digest = ObjectDigest::from_bytes(*entry.content_sha256()).hex();
                    sum.checked_add(objects[&digest].len() as u64)
                        .ok_or_else(|| invalid("source size overflow"))
                })?;
                if value["entry_count"].as_u64() != Some(snapshot.entries().len() as u64)
                    || text(value, "content_bytes")? != content.to_string()
                {
                    return Err(invalid("source record counts"));
                }
                let commit = archive::CommitOid::parse(text(&value["source"], "commit")?)?;
                let expected = match text(&value["source"], "object_format")? {
                    "sha1" => 40,
                    "sha256" => 64,
                    _ => return Err(invalid("Git provenance format")),
                };
                if commit.as_str().len() != expected
                    || text(&value["source"], "kind")? != "git_commit"
                {
                    return Err(invalid("Git provenance mapping"));
                }
            }
            archive::CONTRIBUTION_LINEAGE_SCHEMA_NAME => {
                source(value, "base", &sources)?;
                source(value, "result", &sources)?;
                let base = archive::CommitOid::parse(text(value, "base_commit")?)?;
                let result = archive::CommitOid::parse(text(value, "result_commit")?)?;
                let expected = match text(value, "object_format")? {
                    "sha1" => 40,
                    "sha256" => 64,
                    _ => return Err(invalid("lineage format")),
                };
                if base.as_str().len() != expected
                    || result.as_str().len() != expected
                    || value["first_parent_commits"].as_u64().is_none()
                {
                    return Err(invalid("lineage provenance"));
                }
            }
            capture::CAPTURE_RECEIPT_SCHEMA_NAME => {
                capture::OperationId::parse(text(value, "operation")?)
                    .map_err(|_| invalid("capture operation"))?;
                let lineage = linked(
                    value,
                    "contribution",
                    &records,
                    archive::CONTRIBUTION_LINEAGE_SCHEMA_NAME,
                )?;
                if text(value, "base")? != text(lineage, "base")?
                    || text(value, "result")? != text(lineage, "result")?
                {
                    return Err(invalid("capture lineage mapping"));
                }
                source(value, "base", &sources)?;
                source(value, "result", &sources)?;
                if !matches!(text(value, "status")?, "retained_local")
                    || !matches!(
                        text(value, "durability")?,
                        "local_durable" | "local_unverified"
                    )
                {
                    return Err(invalid("historical capture label"));
                }
            }
            context::CONTEXT_SCHEMA_NAME => {
                if text(value, "authority")? != context::CONTEXT_AUTHORITY
                    || text(value, "visibility")? != "local_project"
                {
                    return Err(RestoreError::Binding);
                }
                if value["query"]["analysis"]
                    .as_array()
                    .is_none_or(|a| !a.is_empty())
                {
                    return Err(RestoreError::Unsupported(
                        "referenced analysis evidence is outside this source-history profile"
                            .into(),
                    ));
                }
                if value["query"].get("source").is_some() {
                    source(&value["query"], "source", &sources)?;
                }
                let items = value["items"]
                    .as_array()
                    .ok_or_else(|| invalid("context items"))?;
                for item in items {
                    let lineage = linked(
                        item,
                        "contribution",
                        &records,
                        archive::CONTRIBUTION_LINEAGE_SCHEMA_NAME,
                    )?;
                    let receipt = linked(
                        item,
                        "receipt",
                        &records,
                        capture::CAPTURE_RECEIPT_SCHEMA_NAME,
                    )?;
                    if text(item, "base")? != text(lineage, "base")?
                        || text(item, "result")? != text(lineage, "result")?
                        || text(receipt, "contribution")? != text(item, "contribution")?
                    {
                        return Err(invalid("context lineage mapping"));
                    }
                    if let Some(carried) = item.get("brief") {
                        let brief = linked(carried, "record", &records, brief::BRIEF_SCHEMA_NAME)?;
                        if text(carried, "role")? != context::BRIEF_ROLE
                            || &carried["content"] != brief
                        {
                            return Err(invalid("context brief mapping"));
                        }
                    }
                    if let Some(omitted) = item.get("brief_omitted") {
                        linked(omitted, "record", &records, brief::BRIEF_SCHEMA_NAME)?;
                    }
                }
            }
            brief::BRIEF_SCHEMA_NAME => {}
            other => return Err(RestoreError::Unsupported(other.into())),
        }
    }
    if closure != objects.keys().cloned().collect() {
        return Err(invalid("unselected object payload"));
    }
    Ok(())
}
/// Validate completely before creating any native receiving binding or writing.
pub fn verify(bytes: &[u8], policy: &ReceiverPolicy) -> Result<VerifiedBundle, RestoreError> {
    policy.scope.validate()?;
    if bytes.len() > MAX_WIRE || ObjectDigest::of(bytes) != policy.bundle {
        return Err(RestoreError::Binding);
    }
    let offset = MAGIC.len();
    if !bytes.starts_with(MAGIC) || bytes.len() < offset + 8 {
        return Err(invalid("wire header"));
    }
    let length = u64::from_be_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .map_err(|_| invalid("wire length"))?,
    );
    let length = usize::try_from(length).map_err(|_| invalid("manifest length"))?;
    if length > canonical_json::MAX_DOCUMENT_BYTES {
        return Err(invalid("manifest budget"));
    }
    let mut cursor = (offset + 8)
        .checked_add(length)
        .ok_or_else(|| invalid("manifest overflow"))?;
    let header_bytes = bytes
        .get(offset + 8..cursor)
        .ok_or_else(|| invalid("incomplete header"))?;
    if canonical_json::canonicalize(header_bytes)? != header_bytes {
        return Err(invalid("noncanonical header"));
    }
    let header: Header = serde_json::from_slice(header_bytes)?;
    if header.profile != PROFILE || header.scope != policy.scope {
        return Err(RestoreError::Binding);
    }
    header.scope.validate()?;
    if header.sources.is_empty()
        || header.sources.len() > MAX_SOURCES
        || header.records.len() > MAX_RECORDS
        || header.objects.len() > MAX_OBJECTS
        || !sorted(&header.sources)
        || !header.records.windows(2).all(|w| w[0].id < w[1].id)
        || !header.objects.windows(2).all(|w| w[0].digest < w[1].digest)
        || header.limitations
            != LIMITATIONS
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
    {
        return Err(invalid("manifest limits or canonical selection"));
    }
    let mut objects = BTreeMap::new();
    let mut total = 0usize;
    for reference in &header.objects {
        let length = usize::try_from(reference.bytes).map_err(|_| invalid("object size"))?;
        total = total
            .checked_add(length)
            .ok_or_else(|| invalid("content overflow"))?;
        if length > MAX_OBJECT || total > MAX_CONTENT {
            return Err(invalid("content budget"));
        }
        let end = cursor
            .checked_add(length)
            .ok_or_else(|| invalid("object overflow"))?;
        let object = bytes
            .get(cursor..end)
            .ok_or_else(|| RestoreError::Missing(reference.digest.clone()))?;
        if ObjectDigest::of(object).hex() != reference.digest {
            return Err(invalid("object digest"));
        }
        objects.insert(reference.digest.clone(), object.to_vec());
        cursor = end;
    }
    if cursor != bytes.len() {
        return Err(invalid("trailing bytes"));
    }
    check_graph(&header, &objects)?;
    Ok(VerifiedBundle {
        digest: policy.bundle.clone(),
        header,
        objects,
    })
}
fn read_bounded(
    store: &CollaborationStore,
    digest: &ObjectDigest,
) -> Result<Vec<u8>, RestoreError> {
    use std::io::Read;
    let path = archive::object_path(store, digest);
    let metadata = std::fs::symlink_metadata(&path)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_OBJECT as u64
    {
        return Err(invalid("object file profile or budget"));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut bytes = Vec::new();
    options
        .open(path)?
        .take(MAX_OBJECT as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_OBJECT || ObjectDigest::of(&bytes) != *digest {
        return Err(invalid("retained object digest or budget"));
    }
    Ok(bytes)
}
fn collect(
    store: &CollaborationStore,
    objects: &mut BTreeMap<String, Vec<u8>>,
    digest: &ObjectDigest,
) -> Result<(), RestoreError> {
    if objects.contains_key(&digest.hex()) {
        return Ok(());
    }
    if objects.len() >= MAX_OBJECTS {
        return Err(invalid("export object count"));
    }
    let bytes = read_bounded(store, digest)?;
    let total = objects.values().map(Vec::len).sum::<usize>();
    if total
        .checked_add(bytes.len())
        .is_none_or(|size| size > MAX_CONTENT)
    {
        return Err(invalid("export content budget"));
    }
    objects.insert(digest.hex(), bytes);
    Ok(())
}
/// Fresh native export authority and archive-use protection remain held until
/// the complete validated bytes have been published to this caller.
pub fn export(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    scope: &Scope,
    selection: &Selection,
) -> Result<Vec<u8>, RestoreError> {
    let _epoch = target::authorize(store, owner)?;
    let _archive = gc::archive_use(store)?;
    scope.validate()?;
    if scope.origin != store.project().as_str() {
        return Err(RestoreError::Binding);
    }
    if selection.sources.is_empty()
        || selection.sources.len() > MAX_SOURCES
        || selection.records.len() > MAX_RECORDS
    {
        return Err(invalid("selection limits"));
    }
    let mut objects = BTreeMap::new();
    let mut sources = Vec::new();
    for id in &selection.sources {
        if archive::retained(store, id)?.is_none() {
            return Err(RestoreError::Missing(id.to_string()));
        }
        let digest = ObjectDigest::of_snapshot(id);
        let bytes = read_bounded(store, &digest)?;
        let snapshot =
            archive::parse_manifest(&bytes).ok_or_else(|| invalid("retained manifest"))?;
        if snapshot.id() != *id || snapshot.manifest_bytes() != bytes {
            return Err(invalid("retained manifest identity"));
        }
        collect(store, &mut objects, &digest)?;
        sources.push(id.to_string());
        for entry in snapshot.entries() {
            let digest = ObjectDigest::from_bytes(*entry.content_sha256());
            collect(store, &mut objects, &digest)?;
        }
    }
    sources.sort();
    if !sorted(&sources) {
        return Err(invalid("duplicate source selection"));
    }
    let mut records = Vec::new();
    for reference in &selection.records {
        let bytes = read_bounded(store, &reference.object)?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        let selected = HistoricalRef {
            id: reference.id.to_string(),
            object: reference.object.hex(),
            schema: text(&value, "schema")?.into(),
        };
        record_value(&bytes, &selected)?;
        collect(store, &mut objects, &reference.object)?;
        records.push(selected);
    }
    records.sort_by(|a, b| a.id.cmp(&b.id));
    if !records.windows(2).all(|w| w[0].id < w[1].id) {
        return Err(invalid("duplicate record selection"));
    }
    let header = Header {
        profile: PROFILE.into(),
        scope: scope.clone(),
        sources,
        records,
        objects: objects
            .iter()
            .map(|(digest, bytes)| ObjectRef {
                digest: digest.clone(),
                bytes: bytes.len() as u64,
            })
            .collect(),
        limitations: LIMITATIONS.iter().map(|s| s.to_string()).collect(),
    };
    check_graph(&header, &objects)?;
    if objects.len() > MAX_OBJECTS
        || objects.values().any(|b| b.len() > MAX_OBJECT)
        || objects.values().map(Vec::len).sum::<usize>() > MAX_CONTENT
    {
        return Err(invalid("export content budget"));
    }
    let manifest = canonical(&header)?;
    let mut bytes = MAGIC.to_vec();
    bytes.extend_from_slice(&(manifest.len() as u64).to_be_bytes());
    bytes.extend_from_slice(&manifest);
    for object in objects.values() {
        bytes.extend_from_slice(object);
    }
    if bytes.len() > MAX_WIRE {
        return Err(invalid("export wire budget"));
    }
    // Apply the same complete decoder and closure contract on the real path.
    verify(
        &bytes,
        &ReceiverPolicy {
            bundle: ObjectDigest::of(&bytes),
            scope: scope.clone(),
        },
    )?;
    Ok(bytes)
}
mod manager;
pub use manager::{HistoricalImport, ReceivingBinding, bind_receiver, import, rehearse, show};
#[cfg(test)]
mod tests;
