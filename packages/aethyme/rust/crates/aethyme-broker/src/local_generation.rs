//! Fresh accepted-source workspaces (#677). A prepared generation is an offer,
//! not an executor acknowledgment. Old workspaces are never written or retired.
use crate::Broker;
use crate::collaboration_archive::{self as archive, ObjectDigest};
use crate::collaboration_gc;
use crate::collaboration_state::CollaborationStore;
use crate::local_acceptance::{self as acceptance, QualifiedAcceptance};
use crate::local_source_transport::GitCarrier;
use crate::local_target::{self as target, ControllerOwner};
use aethyme_contracts::experimental_v0::{
    FieldKind, FieldSpec, Record, RecordId, RecordSchema, SourceSnapshot, SourceSnapshotId,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const PROFILE: &str = "aethyme.generation.raw-standalone-git/provisional-v0";
static INTENT: RecordSchema = schema("aethyme.generation-intent/provisional-v0");
static PREPARED: RecordSchema = schema("aethyme.generation-prepared/provisional-v0");
const fn schema(name: &'static str) -> RecordSchema {
    RecordSchema {
        name,
        fields: &[FieldSpec {
            name: "spec",
            required: true,
            kind: FieldKind::Opaque,
            capability: None,
        }],
        capabilities: &[],
    }
}
#[derive(Debug, thiserror::Error)]
pub enum GenerationError {
    #[error("invalid generation: {0}")]
    Invalid(String),
    #[error("generation is missing")]
    Missing,
    #[error("generation is incomplete or has changed; old and partial workspaces remain intact")]
    Incomplete,
    #[error("generation is not current for this controller target")]
    Stale,
    #[error(transparent)]
    Acceptance(#[from] acceptance::AcceptanceError),
    #[error(transparent)]
    Target(#[from] target::TargetError),
    #[error(transparent)]
    Archive(#[from] archive::ArchiveError),
    #[error(transparent)]
    SensitiveResponse(#[from] crate::collaboration_sensitive::SensitiveError),
    #[error(transparent)]
    Compose(#[from] crate::composer::ComposeError),
    #[error(transparent)]
    Snapshot(#[from] crate::execution_snapshot::SnapshotError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
fn invalid(s: &str) -> GenerationError {
    GenerationError::Invalid(s.into())
}
fn id(s: &str) -> Result<RecordId, GenerationError> {
    RecordId::parse(s).map_err(|_| invalid("record identity"))
}
fn record<T: Serialize>(
    schema: &'static RecordSchema,
    spec: &T,
) -> Result<Record, GenerationError> {
    Record::decode(
        &serde_json::to_vec(&serde_json::json!({"schema":schema.name,"spec":spec}))?,
        &[schema],
    )
    .map_err(|e| GenerationError::Invalid(e.to_string()))
}
fn decode<T: serde::de::DeserializeOwned>(
    schema: &'static RecordSchema,
    expected: &RecordId,
    body: &[u8],
) -> Result<T, GenerationError> {
    let rec = Record::decode(body, &[schema]).map_err(|_| invalid("record schema"))?;
    if rec.id() != *expected || rec.canonical_bytes() != body {
        return Err(invalid("record digest or encoding"));
    }
    Ok(serde_json::from_value(
        serde_json::from_slice::<serde_json::Value>(body)?["spec"].clone(),
    )?)
}
fn token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
}

/// Observation only. Retained pending contributions are explicit; this neither
/// snapshots ignored files nor prevents later writers changing the old root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OldWorkspace {
    pub session: String,
    pub absolute_root: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation: Option<String>,
    pub pending_contributions: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentSpec {
    pub profile: String,
    pub operation: String,
    pub target: String,
    pub revision: String,
    pub epoch: String,
    pub qualification: String,
    pub acceptance_receipt: String,
    pub acceptance_binding: String,
    pub source: String,
    pub accepted_commit: String,
    pub accepted_tree: String,
    pub old: OldWorkspace,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedSpec {
    pub profile: String,
    pub generation: String,
    pub source: String,
    pub provider_commit: String,
    pub tree: String,
    pub local_commit: String,
    pub git_metadata: String,
}
/// Historical preparation data. It has no authority to run code, change another
/// session's root, or acknowledge adoption. #678 supplies that separate boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedGeneration {
    id: RecordId,
    intent: IntentSpec,
    receipt: RecordId,
    mapping: PreparedSpec,
    root: PathBuf,
}
impl PreparedGeneration {
    pub fn id(&self) -> &RecordId {
        &self.id
    }
    pub fn receipt(&self) -> &RecordId {
        &self.receipt
    }
    pub fn intent(&self) -> &IntentSpec {
        &self.intent
    }
    pub fn mapping(&self) -> &PreparedSpec {
        &self.mapping
    }
    pub fn absolute_root(&self) -> &Path {
        &self.root
    }
}
fn directory(store: &CollaborationStore, generation: &RecordId) -> PathBuf {
    store.project_dir().join("generations").join(
        generation
            .as_str()
            .strip_prefix("sha256:")
            .expect("RecordId digest"),
    )
}
pub(crate) fn check_private_directory(path: &Path) -> Result<(), GenerationError> {
    let m = std::fs::symlink_metadata(path)?;
    if !m.is_dir() || m.file_type().is_symlink() || path.canonicalize()? != path {
        return Err(GenerationError::Incomplete);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid takes no pointers and only reads the process identity.
        let uid = unsafe { libc::geteuid() };
        if m.mode() & 0o077 != 0 || m.uid() != uid {
            return Err(invalid("generation directory ownership or permissions"));
        }
    }
    Ok(())
}
pub(crate) fn private_directory(path: &Path) -> Result<(), GenerationError> {
    match std::fs::create_dir(path) {
        Ok(()) => crate::host_state::protect_host_state_path(path, true)?,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    check_private_directory(path)
}
fn check_generation_path(
    store: &CollaborationStore,
    generation: &RecordId,
) -> Result<(), GenerationError> {
    check_private_directory(&store.project_dir().join("generations"))?;
    check_private_directory(&directory(store, generation))
}
fn unchanged_old(
    store: &CollaborationStore,
    old: &mut OldWorkspace,
) -> Result<(), GenerationError> {
    if !token(&old.session)
        || !old.absolute_root.is_absolute()
        || old.pending_contributions.len() > 1024
    {
        return Err(invalid("old workspace observation"));
    }
    let canonical = old.absolute_root.canonicalize()?;
    if canonical != old.absolute_root
        || !std::fs::symlink_metadata(&canonical)?.is_dir()
        || store.project_dir().starts_with(&canonical)
    {
        return Err(invalid("old workspace aliases generation storage"));
    }
    if canonical.starts_with(store.project_dir()) && old.generation.is_none() {
        return Err(invalid("old workspace aliases generation storage"));
    }
    let mut pending = BTreeSet::new();
    for reference in &old.pending_contributions {
        let reference = id(reference)?;
        if archive::retained_contribution(store, &reference)?.is_none() {
            return Err(invalid("pending contribution is not retained"));
        }
        if !pending.insert(reference.to_string()) {
            return Err(invalid("duplicate pending contribution"));
        }
    }
    old.pending_contributions = pending.into_iter().collect();
    if let Some(previous) = &old.generation {
        let previous = inspect_prepared(store, &id(previous)?)?;
        if previous.absolute_root() != old.absolute_root {
            return Err(invalid("old generation root mapping"));
        }
    }
    Ok(())
}
fn current(
    conn: &rusqlite::Connection,
    owner: &ControllerOwner,
    spec: &IntentSpec,
) -> Result<(), GenerationError> {
    let active: (Option<String>, String) = conn.query_row(
        "SELECT active_revision,phase FROM local_targets WHERE target=?1",
        [owner.target()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if spec.target != owner.target()
        || spec.epoch != owner.authority_epoch().as_str()
        || active != (Some(spec.revision.clone()), "verifying".into())
    {
        return Err(GenerationError::Stale);
    }
    Ok(())
}
fn snapshot(store: &CollaborationStore, source: &str) -> Result<SourceSnapshot, GenerationError> {
    let source = SourceSnapshotId::parse(source).map_err(|_| invalid("source identity"))?;
    if archive::retained(store, &source)?.is_none() {
        return Err(invalid("source is not retained"));
    }
    let bytes = archive::read_object(store, &ObjectDigest::of_snapshot(&source))?;
    let manifest = archive::parse_manifest(&bytes).ok_or_else(|| invalid("source manifest"))?;
    if manifest.id() != source || manifest.manifest_bytes() != bytes {
        return Err(invalid("source manifest digest"));
    }
    crate::execution_snapshot::portable(&manifest)?;
    let mut size = 0usize;
    if manifest.entries().len() > 8192 {
        return Err(invalid("generation file limit"));
    }
    for entry in manifest.entries() {
        let bytes =
            archive::read_object(store, &ObjectDigest::from_bytes(*entry.content_sha256()))?;
        if bytes.len() > 64 * 1024 * 1024 {
            return Err(invalid("generation file size"));
        }
        size = size
            .checked_add(bytes.len())
            .ok_or_else(|| invalid("generation size"))?;
        if size > 256 * 1024 * 1024 {
            return Err(invalid("generation size"));
        }
    }
    Ok(manifest)
}
fn roots(
    conn: &rusqlite::Connection,
    generation: &RecordId,
    kind: &str,
    reference: &str,
) -> Result<(), GenerationError> {
    conn.execute(
        "INSERT OR IGNORE INTO local_generation_roots(generation,kind,reference) VALUES(?1,?2,?3)",
        params![generation.as_str(), kind, reference],
    )?;
    Ok(())
}
pub(crate) fn write_sync(path: &Path, bytes: &[u8]) -> Result<(), GenerationError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
pub(crate) fn sync_tree(path: &Path) -> Result<(), GenerationError> {
    let m = std::fs::symlink_metadata(path)?;
    if m.file_type().is_symlink() {
        return Err(GenerationError::Incomplete);
    }
    if m.is_dir() {
        for child in std::fs::read_dir(path)? {
            sync_tree(&child?.path())?;
        }
        File::open(path)?.sync_all()?;
    } else if m.is_file() {
        File::open(path)?.sync_all()?;
    } else {
        return Err(GenerationError::Incomplete);
    }
    Ok(())
}
/// Read the journal. Reading a prepared record does not restore current trust.
/// Before acknowledgment the complete directory is revalidated, never repaired.
pub fn load_prepared(
    store: &CollaborationStore,
    generation: &RecordId,
) -> Result<PreparedGeneration, GenerationError> {
    let _archive = collaboration_gc::archive_use(store)?;
    let _response = crate::collaboration_sensitive::response_use(store)?;
    let result = inspect_prepared(store, generation)?;
    let manifest = snapshot(store, &result.intent.source)?;
    verify_checkout(&result.root, &manifest, &result.mapping)?;
    Ok(result)
}
fn inspect_prepared(
    store: &CollaborationStore,
    generation: &RecordId,
) -> Result<PreparedGeneration, GenerationError> {
    let _archive = collaboration_gc::archive_use(store)?;
    let _response = crate::collaboration_sensitive::response_use(store)?;
    let row:Option<(Vec<u8>,String,String,Vec<u8>)>=store.read_connection().query_row(
        "SELECT g.body,g.state,r.id,r.body FROM local_generations g JOIN local_generation_receipts r ON r.id=g.prepared_receipt AND r.generation=g.id WHERE g.id=?1",
        [generation.as_str()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    let Some((body, state, receipt, prepared)) = row else {
        return Err(GenerationError::Missing);
    };
    if state != "prepared" {
        return Err(GenerationError::Incomplete);
    }
    let intent: IntentSpec = decode(&INTENT, generation, &body)?;
    let receipt = id(&receipt)?;
    let mapping: PreparedSpec = decode(&PREPARED, &receipt, &prepared)?;
    if mapping.profile != PROFILE
        || intent.profile != PROFILE
        || mapping.generation != generation.as_str()
        || mapping.source != intent.source
        || mapping.provider_commit != intent.accepted_commit
        || mapping.tree != intent.accepted_tree
    {
        return Err(invalid("prepared source mapping"));
    }
    check_generation_path(store, generation)?;
    let root = directory(store, generation).join("source");
    if archive::read_object(store, &ObjectDigest::of(&body))? != body
        || archive::read_object(store, &ObjectDigest::of(&prepared))? != prepared
    {
        return Err(invalid("retained generation records"));
    }

    Ok(PreparedGeneration {
        id: generation.clone(),
        intent,
        receipt,
        mapping,
        root,
    })
}
fn verify_checkout(
    root: &Path,
    manifest: &SourceSnapshot,
    mapping: &PreparedSpec,
) -> Result<(), GenerationError> {
    if crate::composer::generation_metadata_digest(root)? != mapping.git_metadata {
        return Err(GenerationError::Incomplete);
    }
    crate::composer::verify_generation_checkout(
        root,
        manifest,
        &mapping.tree,
        &mapping.local_commit,
    )
    .map_err(|_| GenerationError::Incomplete)
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Intent,
    Materialized,
    Durable,
}
/// Prepare an alternative workspace. The caller supplies native acceptance,
/// never a deserialized receipt. No command is launched and no old root is edited.
#[allow(clippy::too_many_arguments)]
pub fn prepare(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    broker: &mut Broker,
    carrier: &GitCarrier,
    grant: &QualifiedAcceptance<'_>,
    operation: &str,
    old: OldWorkspace,
) -> Result<PreparedGeneration, GenerationError> {
    prepare_with(
        store,
        owner,
        broker,
        carrier,
        grant,
        operation,
        old,
        &mut |_, _| Ok(()),
    )
}
#[allow(clippy::too_many_arguments)]
fn prepare_with(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    broker: &mut Broker,
    carrier: &GitCarrier,
    grant: &QualifiedAcceptance<'_>,
    operation: &str,
    mut old: OldWorkspace,
    hook: &mut impl FnMut(Stage, &mut CollaborationStore) -> Result<(), GenerationError>,
) -> Result<PreparedGeneration, GenerationError> {
    if !token(operation) {
        return Err(invalid("operation token"));
    }
    let _archive = collaboration_gc::archive_use(store)?;
    let _response = crate::collaboration_sensitive::response_use(store)?;
    unchanged_old(store, &mut old)?;
    let mut spec = IntentSpec {
        profile: PROFILE.into(),
        operation: operation.into(),
        target: owner.target().into(),
        revision: grant.receipt().spec.revision.clone(),
        epoch: owner.authority_epoch().as_str().into(),
        qualification: grant.id().to_string(),
        acceptance_receipt: grant.receipt().id.to_string(),
        acceptance_binding: grant.receipt().spec.binding.clone(),
        source: grant.receipt().spec.source.clone(),
        accepted_commit: grant
            .receipt()
            .spec
            .accepted_commit
            .clone()
            .ok_or(GenerationError::Stale)?,
        accepted_tree: grant
            .receipt()
            .spec
            .accepted_tree
            .clone()
            .ok_or(GenerationError::Stale)?,
        old,
    };
    // Recovery needs new native proof, not possession of the original private
    // capability. Keep the original intent identity when that proof names the
    // same request, provider binding and exact source under the same epoch.
    let existing: Option<(String, Vec<u8>)> = store
        .read_connection()
        .query_row(
            "SELECT id,body FROM local_generations WHERE target=?1 AND operation=?2",
            params![owner.target(), operation],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((existing, body)) = existing {
        let old_spec: IntentSpec = decode(&INTENT, &id(&existing)?, &body)?;
        let mut comparison = spec.clone();
        comparison.qualification = old_spec.qualification.clone();
        comparison.acceptance_receipt = old_spec.acceptance_receipt.clone();
        if comparison != old_spec {
            return Err(invalid("operation was bound to another request"));
        }
        spec = old_spec;
    }
    let manifest = snapshot(store, &spec.source)?;
    current(store.read_connection(), owner, &spec)?;
    let intent = record(&INTENT, &spec)?;
    let generation = intent.id();
    // Release producer locks while copying bytes. The final fresh guard is the
    // issuance boundary; revocation/restore may proceed during materialization.
    acceptance::current_guard(store, owner, broker, carrier, grant)?.check_clock()?;
    {
        let _authority = target::authorize(store, owner)?;
        let intent_object = archive::put_object(store, &intent.canonical_bytes())?;
        let receipt_bytes: Vec<u8> = store.read_connection().query_row(
            "SELECT body FROM local_acceptance_receipts WHERE id=?1",
            [grant.receipt().id.as_str()],
            |r| r.get(0),
        )?;
        let receipt_object = archive::put_object(store, &receipt_bytes)?;
        let qualification: Vec<u8> = store.read_connection().query_row(
            "SELECT body FROM local_acceptance_qualifications WHERE id=?1",
            [grant.id().as_str()],
            |r| r.get(0),
        )?;
        let qualification_object = archive::put_object(store, &qualification)?;
        let tx = store
            .connection()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        current(&tx, owner, &spec)?;
        let existing: Option<(String, String)> = tx
            .query_row(
                "SELECT id,state FROM local_generations WHERE target=?1 AND operation=?2",
                params![owner.target(), operation],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((existing, state)) = existing {
            if existing != generation.as_str() {
                return Err(invalid("operation was already bound to another generation"));
            }
            if state == "prepared" {
                tx.commit()?;
                return load_prepared(store, &generation);
            }
            if state != "intent" {
                return Err(GenerationError::Incomplete);
            }
        } else {
            tx.execute("INSERT INTO local_generations(id,target,operation,revision,epoch,state,body) VALUES(?1,?2,?3,?4,?5,'intent',?6)",params![generation.as_str(),owner.target(),operation,spec.revision,spec.epoch,intent.canonical_bytes()])?;
        }
        // Retain complete native provenance, including evidence/input/policy
        // records already rooted by this exact revision. These are historical
        // source promises, not copied current permissions or executable leases.
        tx.execute("INSERT OR IGNORE INTO local_generation_roots(generation,kind,reference) SELECT ?1,kind,reference FROM local_target_roots WHERE revision=?2",params![generation.as_str(),spec.revision])?;
        roots(&tx, &generation, "snapshot", &spec.source)?;
        for contribution in &spec.old.pending_contributions {
            roots(&tx, &generation, "contribution", contribution)?;
        }
        for object in [intent_object, receipt_object, qualification_object] {
            roots(&tx, &generation, "object", &object.hex())?;
        }
        tx.commit()?;
    }
    hook(Stage::Intent, store)?;
    let generations = store.project_dir().join("generations");
    private_directory(&generations)?;
    let dir = directory(store, &generation);
    // A previous partial directory is evidence; never overwrite or merge it.
    match std::fs::create_dir(&dir) {
        Ok(()) => crate::host_state::protect_host_state_path(&dir, true)?,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(GenerationError::Incomplete);
        }
        Err(e) => return Err(e.into()),
    }
    let root = dir.join("source");
    archive::reconstruct(store, &manifest.id(), &root)?;
    let scratch = tempfile::tempdir_in(&dir)?;
    let (tree, commit) =
        crate::composer::generation_checkout(store, &root, scratch.path(), &manifest)?;
    if tree != spec.accepted_tree {
        return Err(GenerationError::Incomplete);
    }
    let mapping = PreparedSpec {
        profile: PROFILE.into(),
        generation: generation.to_string(),
        source: spec.source.clone(),
        provider_commit: spec.accepted_commit.clone(),
        tree,
        local_commit: commit.as_str().into(),
        git_metadata: crate::composer::generation_metadata_digest(&root)?,
    };
    verify_checkout(&root, &manifest, &mapping)?;
    hook(Stage::Materialized, store)?;
    drop(scratch);
    let receipt = record(&PREPARED, &mapping)?;
    write_sync(&dir.join("intent.json"), &intent.canonical_bytes())?;
    write_sync(&dir.join("prepared.json"), &receipt.canonical_bytes())?;
    check_generation_path(store, &generation)?;
    sync_tree(&dir)?;
    File::open(&generations)?.sync_all()?;
    hook(Stage::Durable, store)?;
    let guard = acceptance::current_guard(store, owner, broker, carrier, grant)?;
    check_generation_path(store, &generation)?;
    // Independent checks use the same held native owner and its original grant.
    verify_checkout(&root, &manifest, &mapping)?;
    let object = archive::put_object(store, &receipt.canonical_bytes())?;
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    current(&tx, owner, &spec)?;
    guard.check_clock()?;
    tx.execute(
        "INSERT INTO local_generation_receipts(id,generation,body) VALUES(?1,?2,?3)",
        params![
            receipt.id().as_str(),
            generation.as_str(),
            receipt.canonical_bytes()
        ],
    )?;
    roots(&tx, &generation, "object", &object.hex())?;
    if tx.execute("UPDATE local_generations SET state='prepared',prepared_receipt=?1 WHERE id=?2 AND state='intent'",params![receipt.id().as_str(),generation.as_str()])?!=1{return Err(GenerationError::Stale);}
    tx.commit()?;
    Ok(PreparedGeneration {
        id: generation,
        intent: spec,
        receipt: receipt.id(),
        mapping,
        root,
    })
}
#[cfg(test)]
pub(crate) mod tests;
