use super::*;
use aethyme_contracts::experimental_v0::{FieldKind, FieldSpec};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use std::path::{Path, PathBuf};

static IMPORT: RecordSchema = RecordSchema {
    name: "aethyme.historical-import/provisional-v0",
    fields: &[FieldSpec {
        name: "spec",
        required: true,
        kind: FieldKind::Opaque,
        capability: None,
    }],
    capabilities: &[],
};
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ImportSpec {
    profile: String,
    bundle: String,
    scope: Scope,
    receiver: String,
    target: String,
    epoch: String,
    header: String,
    sources: usize,
    records: usize,
    limitations: Vec<String>,
    status: String,
}
/// An opaque fresh native binding, never reconstructed from an imported record.
pub struct ReceivingBinding {
    spec: ImportSpec,
}
#[derive(Debug, Clone)]
pub struct HistoricalImport {
    pub id: RecordId,
    pub bundle: ObjectDigest,
    pub scope: Scope,
    pub sources: Vec<SourceSnapshotId>,
    pub records: Vec<RecordRef>,
    pub limitations: Vec<String>,
}
pub(super) fn digest(s: &str) -> Result<ObjectDigest, RestoreError> {
    if s.len() != 64
        || !s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid("digest encoding"));
    }
    let mut bytes = [0u8; 32];
    for (i, out) in bytes.iter_mut().enumerate() {
        *out =
            u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|_| invalid("digest encoding"))?;
    }
    Ok(ObjectDigest::from_bytes(bytes))
}
fn native_spec(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    bundle: &VerifiedBundle,
) -> Result<ImportSpec, RestoreError> {
    let _guard = target::authorize(store, owner)?;
    let _response = crate::collaboration_sensitive::response_use(store)?;
    Ok(ImportSpec {
        profile: PROFILE.into(),
        bundle: bundle.digest.hex(),
        scope: bundle.header.scope.clone(),
        receiver: store.project().as_str().into(),
        target: owner.target().into(),
        epoch: owner.authority_epoch().as_str().into(),
        header: ObjectDigest::of(&canonical(&bundle.header)?).hex(),
        sources: bundle.header.sources.len(),
        records: bundle.header.records.len(),
        limitations: bundle.header.limitations.clone(),
        status: "historical_retained".into(),
    })
}
pub fn bind_receiver(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    bundle: &VerifiedBundle,
) -> Result<ReceivingBinding, RestoreError> {
    Ok(ReceivingBinding {
        spec: native_spec(store, owner, bundle)?,
    })
}
fn receipt(spec: &ImportSpec) -> Result<Record, RestoreError> {
    Ok(Record::decode(
        &serde_json::to_vec(&serde_json::json!({"schema":IMPORT.name,"spec":spec}))?,
        &[&IMPORT],
    )?)
}
fn pin(
    tx: &rusqlite::Transaction,
    holder: &str,
    digest: &ObjectDigest,
) -> Result<(), RestoreError> {
    tx.execute("INSERT INTO object_pins(class,object_sha256,holder,created_ms) VALUES('cited_evidence',?1,?2,?3)",params![digest.hex(),holder,crate::clock::epoch_ms()])?;
    Ok(())
}
pub fn import(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    binding: &ReceivingBinding,
    bundle: &VerifiedBundle,
) -> Result<HistoricalImport, RestoreError> {
    import_with(store, owner, binding, bundle, &mut || Ok(()))
}
pub(super) fn import_with(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    binding: &ReceivingBinding,
    bundle: &VerifiedBundle,
    after_objects: &mut dyn FnMut() -> Result<(), RestoreError>,
) -> Result<HistoricalImport, RestoreError> {
    let _guard = target::authorize(store, owner)?;
    let _archive = gc::archive_use(store)?;
    let _response = crate::collaboration_sensitive::response_use(store)?;
    if native_spec(store, owner, bundle)? != binding.spec {
        return Err(RestoreError::Binding);
    }
    check_graph(&bundle.header, &bundle.objects)?;
    let record = receipt(&binding.spec)?;
    let id = record.id();
    let body = record.canonical_bytes();
    let header = canonical(&bundle.header)?;
    for bytes in bundle.objects.values() {
        archive::put_object(store, bytes)?;
    }
    let header_digest = archive::put_object(store, &header)?;
    let receipt_digest = archive::put_object(store, &body)?;
    after_objects()?;
    // A copied binding is data until the receiving native operator is checked
    // again. The external authority guard spans object publication and commit.
    if native_spec(store, owner, bundle)? != binding.spec {
        return Err(RestoreError::Binding);
    }
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing: Option<Vec<u8>> = tx
        .query_row(
            "SELECT body FROM local_imports WHERE id=?1",
            [id.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(existing) = existing {
        if existing != body {
            return Err(invalid("historical import identity"));
        }
    } else {
        let count: i64 = tx.query_row("SELECT count(*) FROM local_imports", [], |r| r.get(0))?;
        if count >= 128 {
            return Err(invalid("historical import capacity"));
        }
        tx.execute("INSERT INTO local_imports(id,bundle,target,epoch,body,header_sha256) VALUES(?1,?2,?3,?4,?5,?6)",params![id.as_str(),binding.spec.bundle,binding.spec.target,binding.spec.epoch,body,header_digest.hex()])?;
        for source in &bundle.header.sources {
            tx.execute(
                "INSERT INTO local_imported_sources(import_id,snapshot_id) VALUES(?1,?2)",
                params![id.as_str(), source],
            )?;
        }
        for record in &bundle.header.records {
            tx.execute("INSERT INTO local_imported_records(import_id,record_id,schema,object_sha256) VALUES(?1,?2,?3,?4)",params![id.as_str(),record.id,record.schema,record.object])?;
        }
        for object in &bundle.header.objects {
            pin(&tx, id.as_str(), &digest(&object.digest)?)?;
        }
        pin(&tx, id.as_str(), &header_digest)?;
        pin(&tx, id.as_str(), &receipt_digest)?;
    }
    tx.commit()?;
    show(store, &id)
}
fn load(
    store: &CollaborationStore,
    id: &RecordId,
) -> Result<(ImportSpec, VerifiedBundle), RestoreError> {
    let row: Option<(Vec<u8>, String, String, String, String)> = store
        .read_connection()
        .query_row(
            "SELECT body,header_sha256,bundle,target,epoch FROM local_imports WHERE id=?1",
            [id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let (body, header_digest, bundle_digest, target, epoch) =
        row.ok_or_else(|| RestoreError::Missing(id.to_string()))?;
    let record = Record::decode(&body, &[&IMPORT])?;
    if record.id() != *id
        || record.canonical_bytes() != body
        || read_bounded(store, &ObjectDigest::of(&body))? != body
    {
        return Err(invalid("historical receipt seal"));
    }
    let value: serde_json::Value = serde_json::from_slice(&body)?;
    let spec: ImportSpec = serde_json::from_value(value["spec"].clone())?;
    if spec.profile != PROFILE
        || spec.status != "historical_retained"
        || spec.header != header_digest
        || spec.bundle != bundle_digest
        || spec.target != target
        || spec.epoch != epoch
        || spec.receiver != store.project().as_str()
    {
        return Err(invalid("historical receipt binding"));
    }
    let header = read_bounded(store, &digest(&header_digest)?)?;
    let manifest: Header = serde_json::from_slice(&header)?;
    if canonical(&manifest)? != header
        || manifest.scope != spec.scope
        || manifest.sources.len() != spec.sources
        || manifest.records.len() != spec.records
        || manifest.limitations != spec.limitations
    {
        return Err(invalid("historical header binding"));
    }
    let mut wire = MAGIC.to_vec();
    wire.extend_from_slice(&(header.len() as u64).to_be_bytes());
    wire.extend_from_slice(&header);
    let mut content = 0usize;
    if manifest.objects.len() > MAX_OBJECTS {
        return Err(invalid("historical object count"));
    }
    for object in &manifest.objects {
        content = content
            .checked_add(
                usize::try_from(object.bytes).map_err(|_| invalid("historical object size"))?,
            )
            .ok_or_else(|| invalid("historical size overflow"))?;
        if object.bytes > MAX_OBJECT as u64 || content > MAX_CONTENT {
            return Err(invalid("historical content budget"));
        }
        let bytes = read_bounded(store, &digest(&object.digest)?)?;
        if bytes.len() as u64 != object.bytes {
            return Err(invalid("historical object size"));
        }
        wire.extend_from_slice(&bytes);
    }
    let bundle = verify(
        &wire,
        &ReceiverPolicy {
            bundle: digest(&spec.bundle)?,
            scope: spec.scope.clone(),
        },
    )?;
    Ok((spec, bundle))
}
pub fn show(store: &CollaborationStore, id: &RecordId) -> Result<HistoricalImport, RestoreError> {
    let _archive = gc::archive_use(store)?;
    let _response = crate::collaboration_sensitive::response_use(store)?;
    let (_, bundle) = load(store, id)?;
    Ok(HistoricalImport {
        id: id.clone(),
        bundle: bundle.digest,
        scope: bundle.header.scope,
        sources: bundle
            .header
            .sources
            .iter()
            .map(|s| SourceSnapshotId::parse(s).map_err(|_| invalid("source identity")))
            .collect::<Result<_, _>>()?,
        records: bundle
            .header
            .records
            .iter()
            .map(|r| {
                Ok(RecordRef {
                    id: RecordId::parse(&r.id).map_err(|_| invalid("record identity"))?,
                    object: digest(&r.object)?,
                })
            })
            .collect::<Result<_, RestoreError>>()?,
        limitations: bundle.header.limitations,
    })
}
#[cfg(unix)]
fn verify_tree(
    root: &Path,
    snapshot: &aethyme_contracts::experimental_v0::SourceSnapshot,
) -> Result<(), RestoreError> {
    use aethyme_contracts::experimental_v0::EntryKind;
    use std::io::Read;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    crate::local_generation::check_private_directory(root)?;
    let wanted: BTreeSet<PathBuf> = snapshot
        .entries()
        .iter()
        .map(|e| root.join(std::ffi::OsString::from_vec(e.path().to_vec())))
        .collect();
    let mut found = BTreeSet::new();
    let mut directories = vec![root.to_path_buf()];
    let mut nodes = 0usize;
    while let Some(dir) = directories.pop() {
        if dir.canonicalize()? != dir || std::fs::symlink_metadata(&dir)?.file_type().is_symlink() {
            return Err(invalid("rehearsal path alias"));
        }
        for entry in std::fs::read_dir(dir)? {
            nodes += 1;
            if nodes > MAX_OBJECTS * 64 {
                return Err(invalid("rehearsal path budget"));
            }
            let path = entry?.path();
            let metadata = std::fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                return Err(RestoreError::Unsupported("symlink rehearsal".into()));
            }
            if metadata.is_dir() {
                directories.push(path);
            } else if metadata.is_file() {
                found.insert(path);
            } else {
                return Err(invalid("rehearsal entry type"));
            }
        }
    }
    if found != wanted {
        return Err(invalid("rehearsal entry set"));
    }
    for entry in snapshot.entries() {
        let path = root.join(std::ffi::OsString::from_vec(entry.path().to_vec()));
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.len() > MAX_OBJECT as u64
            || (metadata.mode() & 0o111 != 0) != (entry.kind() == EntryKind::Executable)
        {
            return Err(invalid("rehearsal file profile"));
        }
        let mut bytes = Vec::new();
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?
            .take(MAX_OBJECT as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_OBJECT
            || ObjectDigest::of(&bytes) != ObjectDigest::from_bytes(*entry.content_sha256())
        {
            return Err(invalid("rehearsal source digest"));
        }
    }
    Ok(())
}
#[cfg(not(unix))]
fn verify_tree(
    _: &Path,
    _: &aethyme_contracts::experimental_v0::SourceSnapshot,
) -> Result<(), RestoreError> {
    Err(RestoreError::Unsupported("native Unix rehearsal".into()))
}
/// A source-only private materialization. It never runs archived commands or
/// grants acceptance. Callers choose any subsequent trusted check separately.
pub fn rehearse(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    import_id: &RecordId,
    source: &SourceSnapshotId,
    operation: &str,
) -> Result<PathBuf, RestoreError> {
    rehearse_with(store, owner, import_id, source, operation, &mut |_| Ok(()))
}
pub(super) fn rehearse_with(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    import_id: &RecordId,
    source: &SourceSnapshotId,
    operation: &str,
    after_materialize: &mut dyn FnMut(&Path) -> Result<(), RestoreError>,
) -> Result<PathBuf, RestoreError> {
    if !token(operation) {
        return Err(invalid("rehearsal operation"));
    }
    let _guard = target::authorize(store, owner)?;
    let _archive = gc::archive_use(store)?;
    let _response = crate::collaboration_sensitive::response_use(store)?;
    let (_, bundle) = load(store, import_id)?;
    if !bundle.header.sources.contains(&source.to_string()) {
        return Err(RestoreError::Missing(source.to_string()));
    }
    let manifest =
        archive::parse_manifest(&bundle.objects[&ObjectDigest::of_snapshot(source).hex()])
            .ok_or_else(|| invalid("source manifest"))?;
    if manifest
        .entries()
        .iter()
        .any(|e| e.kind() == aethyme_contracts::experimental_v0::EntryKind::Symlink)
    {
        return Err(RestoreError::Unsupported(
            "symlink rehearsal (historical export preserves the raw link)".into(),
        ));
    }
    let epoch = owner.authority_epoch().as_str().to_string();
    let intent = canonical(
        &serde_json::json!({"profile":PROFILE,"import":import_id.as_str(),"source":source.as_str(),"target":owner.target(),"epoch":epoch,"operation":operation}),
    )?;
    let id = ObjectDigest::of(&intent).hex();
    let parent = store.project_dir().join("import-rehearsals");
    crate::local_generation::private_directory(&parent)?;
    let directory = parent.join(&id);
    let root = directory.join("source");
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing: Option<(Vec<u8>, String)> = tx
        .query_row(
            "SELECT body,state FROM local_import_rehearsals WHERE id=?1",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((body, state)) = existing {
        if body != intent {
            return Err(invalid("rehearsal identity"));
        }
        tx.commit()?;
        if state != "prepared" {
            return Err(RestoreError::Unknown);
        }
        crate::local_generation::check_private_directory(&parent)?;
        crate::local_generation::check_private_directory(&directory)?;
        if std::fs::read(directory.join("PREPARED"))? != intent {
            return Err(invalid("rehearsal seal"));
        }
        verify_tree(&root, &manifest)?;
        return Ok(root);
    }
    let count: i64 = tx.query_row("SELECT count(*) FROM local_import_rehearsals", [], |r| {
        r.get(0)
    })?;
    if count >= 4096 {
        return Err(invalid("rehearsal capacity"));
    }
    tx.execute(
        "INSERT INTO local_import_rehearsals(id,import_id,body,state) VALUES(?1,?2,?3,'intent')",
        params![id, import_id.as_str(), intent],
    )?;
    tx.commit()?;
    std::fs::create_dir(&directory)?;
    crate::host_state::protect_host_state_path(&directory, true)?;
    crate::local_generation::check_private_directory(&directory)?;
    archive::reconstruct_manifest(store, source, &root)?;
    crate::host_state::protect_host_state_path(&root, true)?;
    after_materialize(&root)?;
    crate::local_generation::check_private_directory(&parent)?;
    crate::local_generation::check_private_directory(&directory)?;
    verify_tree(&root, &manifest)?;
    crate::local_generation::sync_tree(&root)?;
    crate::local_generation::write_sync(&directory.join("PREPARED"), &intent)?;
    crate::collaboration_state::sync_directory(&directory)?;
    crate::collaboration_state::sync_directory(&parent)?;
    let _current = target::authorize(store, owner)?;
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "UPDATE local_import_rehearsals SET state='prepared' WHERE id=?1 AND state='intent'",
        [&id],
    )?;
    tx.commit()?;
    Ok(root)
}
