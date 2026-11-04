//! Experimental, explicit association between two independently bound Local
//! projects. The invoking project is the only side whose store is written.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use aethyme_contracts::experimental_v0::project_association::{
    MAX_PROJECT_ASSOCIATION_BYTES, PROJECT_ASSOCIATION_PATH, ProjectAssociationDescriptor,
    ProjectAssociationNonce,
};
use aethyme_contracts::experimental_v0::project_identity::{
    PROJECT_DESCRIPTOR_PATH, ProjectDescriptor,
};
use aethyme_contracts::experimental_v0::{RecordId, SourceSnapshot};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::collaboration_archive::{self as archive, CommitOid, ObjectDigest};
use crate::collaboration_gc::{self, ArchiveUse};
use crate::collaboration_identity_store::{self as identity_store, ApprovedProjectBinding};
use crate::collaboration_sensitive::{self, ResponseUse};
use crate::collaboration_state::{
    CollaborationStore, ProjectKey, inspect_store_for_repository, locate_for_repository,
    open_for_repository,
};

use super::project::{committed_enrollment, committed_regular_file, is_ancestor};
use super::{Failure, Output};

const MAX_POLICY_BYTES: usize = 1024 * 1024;
const CONFIRM_NEXT: &str = "review the exact association RecordId, then run `aethyme collab project association confirm --peer-repo <path> --confirm <record-id>`";
const REPROPOSE_NEXT: &str = "move or remove the old `.aethyme/project-association.json`, run `aethyme collab project association propose --peer-repo <path> --write`, then review and confirm the new exact RecordId";
const ASSOCIATION_NONCE_DOMAIN: &[u8] = b"aethyme association proposal nonce v1\0";
static ASSOCIATION_PROPOSAL_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) fn command(main_root: &Path, worktree: &Path, rest: &[&str]) -> Result<Output, Failure> {
    match rest {
        ["propose", flags @ ..] => propose(main_root, worktree, flags),
        ["confirm", flags @ ..] => confirm(main_root, worktree, flags),
        ["revoke", flags @ ..] => revoke(main_root, worktree, flags),
        _ => Err(Failure::usage(
            "project association requires `propose --peer-repo <path> [--write]`, `confirm --peer-repo <path> --confirm <record-id>`, or `revoke --confirm <record-id>`",
        )),
    }
}

fn propose(main_root: &Path, worktree: &Path, rest: &[&str]) -> Result<Output, Failure> {
    let (peer_path, write) = parse_peer_flags(rest)?;
    let local = inspect_project(main_root, worktree)?;
    let (peer_worktree, peer_main_root) = repository_roots(&peer_path)?;
    let peer = inspect_project(&peer_main_root, &peer_worktree)?;
    if local.root == peer.root || local.main_root == peer.main_root {
        return Err(Failure::refused(
            "same_repository",
            "an association needs two distinct repository clones",
            "choose an independently enrolled clone of the peer project",
        ));
    }
    let descriptor = association_descriptor(&local, &peer)?;
    if !local.head_is_current() || !peer.head_is_current() {
        return Err(Failure::refused(
            "association_repository_changed",
            "a repository HEAD changed while the association proposal was being prepared",
            "rerun propose against the current committed descriptors",
        ));
    }
    let (bytes, record_id) = descriptor.to_record();
    let written = if write {
        Some(write_association_descriptor(worktree, &bytes)?)
    } else {
        None
    };
    let parsed: Value = serde_json::from_slice(&bytes)
        .map_err(|error| Failure::domain("invalid_association", error.to_string()))?;
    Ok((
        json!({
            "schema":"aethyme.collab-project-association-propose/experimental-v0",
            "association_record_id":record_id.as_str(),
            "association":parsed,
            "local_project_id":local.binding.project_id().as_str(),
            "local_project_key":local.key.as_str(),
            "peer_project_id":peer.binding.project_id().as_str(),
            "peer_project_key":peer.key.as_str(),
            "written":written.is_some(),
            "path":written,
            "next_action":CONFIRM_NEXT,
            "grants_authority":false,
        }),
        format!(
            "proposed association {} between {} and {}\n{}\n{}\n",
            record_id,
            local.binding.project_id(),
            peer.binding.project_id(),
            written
                .as_ref()
                .map(|path| format!("written to {}", path.display()))
                .unwrap_or_else(|| format!("descriptor not written; rerun with --write to create {PROJECT_ASSOCIATION_PATH}")),
            CONFIRM_NEXT,
        ),
    ))
}

fn confirm(main_root: &Path, worktree: &Path, rest: &[&str]) -> Result<Output, Failure> {
    let (peer_path, confirmed) = parse_confirm_flags(rest)?;
    let bytes = read_working_association(worktree)?;
    let (association, actual_record_id) = ProjectAssociationDescriptor::from_record(&bytes)
        .map_err(|error| {
            Failure::refused("invalid_association", error.to_string(), CONFIRM_NEXT)
        })?;
    if actual_record_id != confirmed {
        if persisted_revocation_matches_local_project(main_root, worktree, &confirmed)? {
            return Err(Failure::refused(
                "association_revoked",
                "this exact association proposal was already revoked and cannot be reactivated",
                REPROPOSE_NEXT,
            ));
        }
        return Err(Failure::refused(
            "association_confirmation_mismatch",
            format!("confirmed {confirmed}, but the descriptor is {actual_record_id}"),
            "review the exact local proposal and confirm its RecordId",
        ));
    }

    let local_read = inspect_project(main_root, worktree)?;
    let (peer_worktree, peer_main_root) = repository_roots(&peer_path)?;
    let peer = inspect_project(&peer_main_root, &peer_worktree)?;
    validate_association_sides(&association, &local_read, &peer)?;

    // Reopen only the invoking project's existing database for the one local
    // identity/object-store write. The peer remains read-only through commit.
    let (_, _, exists) = locate_for_repository(main_root, &local_read.key)
        .map_err(|error| Failure::domain(error.code(), error.to_string()))?;
    if !exists {
        return Err(Failure::refused(
            "not_initialized",
            "the invoking project's collaboration store does not exist; association never creates it",
            "initialize and bind the local project before confirming an association",
        ));
    }
    let mut local_store = open_for_repository(main_root, &local_read.key)
        .map_err(|error| Failure::domain(error.code(), error.to_string()))?;
    let local_archive = collaboration_gc::archive_use(&local_store).map_err(|error| {
        Failure::domain(
            "archive_lock",
            format!(
                "{}: {error}",
                collaboration_gc::lock_path(&local_store).display()
            ),
        )
    })?;
    let _local_response = collaboration_sensitive::response_use(&local_store)
        .map_err(|error| Failure::refused("sensitive_response", error.to_string(), CONFIRM_NEXT))?;
    let local_binding = identity_store::resolve_approved_project_binding_with_archive_use(
        &local_store,
        &local_archive,
    )
    .map_err(identity_binding_failure)?
    .ok_or_else(|| {
        Failure::refused(
            "local_binding_missing",
            "the invoking project's approved descriptor binding is missing",
            "bind the exact committed local descriptor before confirming an association",
        )
    })?;
    validate_binding_matches_context(&local_binding, &local_read)?;
    validate_association_sides(&association, &local_read, &peer)?;
    if !local_read.head_is_current() || !peer.head_is_current() {
        return Err(Failure::refused(
            "association_repository_changed",
            "a repository HEAD changed while the association was being confirmed",
            "reload both committed descriptors and confirm the exact current proposal",
        ));
    }

    let object_digest = archive::put_object(&local_store, &bytes)
        .map_err(|error| Failure::domain("archive", error.to_string()))?;
    let status = persist_confirmed_association(
        &mut local_store,
        &association,
        &actual_record_id,
        object_digest,
    )?;
    drop(local_archive);
    Ok((
        json!({
            "schema":"aethyme.collab-project-association-confirm/experimental-v0",
            "association_record_id":actual_record_id.as_str(),
            "local_project_id":association.local_project_id().as_str(),
            "peer_project_id":association.peer_project_id().as_str(),
            "status":status,
            "peer_store_written":false,
            "grants_execution_authority":false,
            "grants_acceptance_authority":false,
        }),
        format!(
            "association {} is {} from local project {} to peer project {}\npeer store was read only; no execution or acceptance authority was granted\n",
            actual_record_id,
            status,
            association.local_project_id(),
            association.peer_project_id(),
        ),
    ))
}

fn revoke(main_root: &Path, worktree: &Path, rest: &[&str]) -> Result<Output, Failure> {
    if rest.len() != 2 || rest[0] != "--confirm" {
        return Err(Failure::usage(
            "project association revoke requires exactly --confirm <association-record-id>",
        ));
    }
    let confirmed = RecordId::parse(rest[1]).map_err(|error| Failure::usage(error.to_string()))?;
    let local = inspect_project(main_root, worktree)?;
    let (_, _, exists) = locate_for_repository(main_root, &local.key)
        .map_err(|error| Failure::domain(error.code(), error.to_string()))?;
    if !exists {
        return Err(Failure::refused(
            "not_initialized",
            "the invoking project's collaboration store does not exist",
            "initialize and bind the local project before revoking an association",
        ));
    }
    let mut store = open_for_repository(main_root, &local.key)
        .map_err(|error| Failure::domain(error.code(), error.to_string()))?;
    let archive_use = collaboration_gc::archive_use(&store)
        .map_err(|error| Failure::domain("archive_lock", error.to_string()))?;
    let _response = collaboration_sensitive::response_use(&store)
        .map_err(|error| Failure::refused("sensitive_response", error.to_string(), CONFIRM_NEXT))?;
    let binding =
        identity_store::resolve_approved_project_binding_with_archive_use(&store, &archive_use)
            .map_err(identity_binding_failure)?
            .ok_or_else(|| {
                Failure::refused(
                    "local_binding_missing",
                    "the invoking project's approved descriptor binding is missing",
                    "restore or bind the exact local descriptor before revoking its association",
                )
            })?;
    validate_binding_matches_context(&binding, &local)?;
    let association = load_association_row(&store, &confirmed)?;
    if association.descriptor.local_project_key() != local.key.as_str()
        || association.descriptor.local_project_id() != binding.project_id()
        || association.descriptor.local_descriptor_record_id() != binding.descriptor_record_id()
    {
        return Err(Failure::refused(
            "wrong_association_side",
            "the confirmed association does not belong to this invoking project's local side",
            "revoke the association from the project that confirmed it",
        ));
    }
    if !local.head_is_current() {
        return Err(Failure::refused(
            "association_repository_changed",
            "the invoking repository HEAD changed while the association was being revoked",
            "reload the current local descriptor before revoking its association",
        ));
    }
    let status = if association.status == "revoked" {
        "already_revoked"
    } else {
        let revoked_ms = now_ms()?;
        let transaction = store
            .connection()
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(sqlite)?;
        let changed = transaction
            .execute(
                "UPDATE local_project_associations
                 SET status='revoked',revoked_ms=?2
                 WHERE association_record_id=?1 AND status='confirmed'",
                rusqlite::params![confirmed.as_str(), revoked_ms],
            )
            .map_err(sqlite)?;
        if changed != 1 {
            return Err(Failure::refused(
                "association_state_changed",
                "the association changed while it was being revoked",
                "reload the exact local association and retry with its current status",
            ));
        }
        transaction.commit().map_err(sqlite)?;
        "revoked"
    };
    Ok((
        json!({
            "schema":"aethyme.collab-project-association-revoke/experimental-v0",
            "association_record_id":confirmed.as_str(),
            "status":status,
            "peer_store_written":false,
            "grants_execution_authority":false,
            "grants_acceptance_authority":false,
        }),
        format!(
            "association {} is {} locally\npeer store was not accessed\n",
            confirmed, status
        ),
    ))
}

pub(crate) struct ProjectContext {
    pub(crate) root: PathBuf,
    pub(crate) main_root: PathBuf,
    pub(crate) key: ProjectKey,
    pub(crate) head_commit: CommitOid,
    pub(crate) _source_snapshot: SourceSnapshot,
    pub(crate) descriptor: ProjectDescriptor,
    pub(crate) descriptor_record_id: RecordId,
    pub(crate) _store: CollaborationStore,
    pub(crate) binding: ApprovedProjectBinding,
    pub(crate) _archive: ArchiveUse,
    _response: ResponseUse,
}

impl ProjectContext {
    pub(crate) fn head_is_current(&self) -> bool {
        matches!(archive::pin_commit(&self.root, "HEAD"), Ok(current) if current == self.head_commit)
    }
}

pub(crate) fn inspect_project(
    main_root: &Path,
    worktree: &Path,
) -> Result<ProjectContext, Failure> {
    let head = archive::pin_commit(worktree, "HEAD")
        .map_err(|error| Failure::domain("git", error.to_string()))?;
    let snapshot = archive::snapshot_of_commit(worktree, &head)
        .map_err(|error| Failure::domain("archive", error.to_string()))?;
    let config_text =
        super::project::committed_config_toml(worktree, &head, &snapshot, MAX_POLICY_BYTES)?;
    let (key, pointer) = committed_enrollment(&config_text)?;
    if pointer != PROJECT_DESCRIPTOR_PATH {
        return Err(Failure::refused(
            "descriptor_pointer_mismatch",
            format!(
                "committed [collaboration] descriptor must be exactly {PROJECT_DESCRIPTOR_PATH:?}"
            ),
            "commit the reviewed project descriptor pointer before associating clones",
        ));
    }
    let descriptor_bytes = committed_regular_file(
        worktree,
        &head,
        &snapshot,
        PROJECT_DESCRIPTOR_PATH,
        aethyme_contracts::experimental_v0::project_identity::MAX_DESCRIPTOR_BYTES,
    )?;
    let (descriptor, descriptor_record_id) = ProjectDescriptor::from_record(&descriptor_bytes)
        .map_err(|error| Failure::refused("invalid_descriptor", error.to_string(), CONFIRM_NEXT))?;
    let anchor_commit = CommitOid::parse(descriptor.anchor_commit())
        .map_err(|error| Failure::refused("invalid_descriptor", error.to_string(), CONFIRM_NEXT))?;
    if !is_ancestor(worktree, &anchor_commit, &head)? {
        return Err(Failure::refused(
            "anchor_not_committed",
            "the project descriptor anchor is not an ancestor of the current committed HEAD",
            "commit or fetch the exact project anchor before associating clones",
        ));
    }
    let anchor_snapshot = archive::snapshot_of_commit(worktree, &anchor_commit)
        .map_err(|error| Failure::domain("archive", error.to_string()))?;
    descriptor
        .validate_anchor(anchor_commit.as_str(), &anchor_snapshot)
        .map_err(|error| Failure::refused("anchor_mismatch", error.to_string(), CONFIRM_NEXT))?;

    let (_, _, exists) = locate_for_repository(main_root, &key)
        .map_err(|error| Failure::domain(error.code(), error.to_string()))?;
    if !exists {
        return Err(Failure::refused(
            "binding_missing",
            format!("project store {} does not exist", key.as_str()),
            "initialize and bind both independently enrolled projects before associating them",
        ));
    }
    let store = inspect_store_for_repository(main_root, &key)
        .map_err(|error| Failure::domain(error.code(), error.to_string()))?
        .ok_or_else(|| {
            Failure::refused("binding_missing", "project store disappeared", CONFIRM_NEXT)
        })?;
    let archive_use = collaboration_gc::archive_use_read_only(&store)
        .map_err(|error| Failure::domain("archive_lock", error.to_string()))?;
    let response = collaboration_sensitive::response_use_read_only(&store)
        .map_err(|error| Failure::refused("sensitive_response", error.to_string(), CONFIRM_NEXT))?;
    let binding =
        identity_store::resolve_approved_project_binding_with_archive_use(&store, &archive_use)
            .map_err(identity_binding_failure)?
            .ok_or_else(|| {
                Failure::refused(
                    "binding_missing",
                    "approved project binding is missing",
                    CONFIRM_NEXT,
                )
            })?;
    let context = ProjectContext {
        root: worktree.to_path_buf(),
        main_root: main_root.to_path_buf(),
        key,
        head_commit: head,
        descriptor,
        descriptor_record_id,
        binding,
        _source_snapshot: snapshot,
        _store: store,
        _archive: archive_use,
        _response: response,
    };
    validate_binding_matches_context(&context.binding, &context)?;
    Ok(context)
}

fn validate_binding_matches_context(
    binding: &ApprovedProjectBinding,
    context: &ProjectContext,
) -> Result<(), Failure> {
    if binding.project_id() != context.descriptor.project_id()
        || binding.descriptor_record_id() != &context.descriptor_record_id
        || binding.descriptor() != &context.descriptor
        || binding.anchor_commit().as_str() != context.descriptor.anchor_commit()
        || binding.descriptor().anchor_snapshot() != context.descriptor.anchor_snapshot()
    {
        return Err(Failure::refused(
            "binding_mismatch",
            "the approved local database binding differs from the exact committed project descriptor",
            "repair or rebind the exact committed project descriptor before associating clones",
        ));
    }
    Ok(())
}

fn association_descriptor(
    local: &ProjectContext,
    peer: &ProjectContext,
) -> Result<ProjectAssociationDescriptor, Failure> {
    ProjectAssociationDescriptor::new(
        local.key.as_str(),
        peer.key.as_str(),
        local.binding.project_id().clone(),
        peer.binding.project_id().clone(),
        local.binding.descriptor_record_id().clone(),
        peer.binding.descriptor_record_id().clone(),
        local.binding.descriptor().anchor_snapshot().clone(),
        peer.binding.descriptor().anchor_snapshot().clone(),
        new_proposal_nonce()?,
    )
    .map_err(|error| Failure::refused("invalid_association", error.to_string(), CONFIRM_NEXT))
}

fn new_proposal_nonce() -> Result<ProjectAssociationNonce, Failure> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| Failure::domain("clock", error.to_string()))?
        .as_nanos();
    let sequence = ASSOCIATION_PROPOSAL_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let mut digest = Sha256::new();
    digest.update(ASSOCIATION_NONCE_DOMAIN);
    digest.update(timestamp.to_be_bytes());
    digest.update(std::process::id().to_be_bytes());
    digest.update(sequence.to_be_bytes());
    ProjectAssociationNonce::parse(format!("sha256:{:x}", digest.finalize()))
        .map_err(|error| Failure::domain("invalid_proposal_nonce", error.to_string()))
}

fn validate_association_sides(
    association: &ProjectAssociationDescriptor,
    local: &ProjectContext,
    peer: &ProjectContext,
) -> Result<(), Failure> {
    if local.root == peer.root || local.main_root == peer.main_root {
        return Err(Failure::refused(
            "same_repository",
            "an association needs two distinct repository clones",
            "choose an independently enrolled clone of the peer project",
        ));
    }
    if local.key.as_str() != association.local_project_key()
        || peer.key.as_str() != association.peer_project_key()
        || local.binding.project_id() != association.local_project_id()
        || peer.binding.project_id() != association.peer_project_id()
        || local.binding.descriptor_record_id() != association.local_descriptor_record_id()
        || peer.binding.descriptor_record_id() != association.peer_descriptor_record_id()
        || local.binding.descriptor().anchor_snapshot() != association.local_anchor_snapshot()
        || peer.binding.descriptor().anchor_snapshot() != association.peer_anchor_snapshot()
        || local.binding.project_id() == peer.binding.project_id()
    {
        return Err(Failure::refused(
            "association_binding_mismatch",
            "the proposal does not match the exact independently approved bindings on both sides",
            "propose a new association from the current committed descriptors and approved bindings",
        ));
    }
    Ok(())
}

struct StoredAssociation {
    descriptor: ProjectAssociationDescriptor,
    status: String,
}

fn persisted_revocation_matches_local_project(
    main_root: &Path,
    worktree: &Path,
    record_id: &RecordId,
) -> Result<bool, Failure> {
    let Ok(local) = inspect_project(main_root, worktree) else {
        return Ok(false);
    };
    let stored = match load_association_row(&local._store, record_id) {
        Ok(stored) => stored,
        Err(error)
            if matches!(
                error.code.as_str(),
                "association_missing" | "association_schema_unavailable"
            ) =>
        {
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    Ok(stored.status == "revoked"
        && stored.descriptor.local_project_key() == local.key.as_str()
        && stored.descriptor.local_project_id() == local.binding.project_id())
}

fn load_association_row(
    store: &CollaborationStore,
    expected_record_id: &RecordId,
) -> Result<StoredAssociation, Failure> {
    if store.schema_version() < 31 {
        return Err(Failure::refused(
            "association_schema_unavailable",
            "the local project store does not contain project association records",
            "upgrade the local store before resolving or revoking this association",
        ));
    }
    type Row = (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        i64,
        Option<i64>,
    );
    let row: Option<Row> = store
        .read_connection()
        .query_row(
            "SELECT association_object_sha256,local_project_key,local_project_id,peer_project_id,
                    local_descriptor_record_id,peer_descriptor_record_id,peer_project_key,
                    status,confirmed_ms,revoked_ms
             FROM local_project_associations WHERE association_record_id=?1",
            [expected_record_id.as_str()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                ))
            },
        )
        .optional()
        .map_err(sqlite)?;
    let Some((
        digest_text,
        local_key,
        local_id_text,
        peer_id_text,
        local_descriptor_text,
        peer_descriptor_text,
        peer_key,
        status,
        confirmed_ms,
        revoked_ms,
    )) = row
    else {
        return Err(Failure::refused(
            "association_missing",
            format!(
                "association {} is not persisted in this local project store",
                expected_record_id
            ),
            "confirm the exact proposal in the invoking project before resolving it",
        ));
    };
    if confirmed_ms < 0
        || (status == "confirmed" && revoked_ms.is_some())
        || (status == "revoked" && revoked_ms.is_none_or(|time| time < confirmed_ms))
        || !matches!(status.as_str(), "confirmed" | "revoked")
    {
        return Err(Failure::domain(
            "association_corrupt",
            "association status timestamps are inconsistent".into(),
        ));
    }
    let digest = archive::parse_digest(&digest_text).ok_or_else(|| {
        Failure::domain(
            "association_corrupt",
            "association object digest is malformed".into(),
        )
    })?;
    let bytes = archive::read_object_bounded(store, &digest, MAX_PROJECT_ASSOCIATION_BYTES)
        .map_err(|error| Failure::domain("association_corrupt", error.to_string()))?;
    let (descriptor, observed_record_id) = ProjectAssociationDescriptor::from_record(&bytes)
        .map_err(|error| Failure::domain("association_corrupt", error.to_string()))?;
    if observed_record_id != *expected_record_id
        || descriptor.local_project_key() != local_key
        || descriptor.local_project_id().as_str() != local_id_text
        || descriptor.peer_project_id().as_str() != peer_id_text
        || descriptor.local_descriptor_record_id().as_str() != local_descriptor_text
        || descriptor.peer_descriptor_record_id().as_str() != peer_descriptor_text
        || descriptor.peer_project_key() != peer_key
    {
        return Err(Failure::domain(
            "association_corrupt",
            "canonical association object differs from its indexed local row".into(),
        ));
    }
    let pair_count: i64 = store
        .read_connection()
        .query_row(
            "SELECT count(*) FROM local_project_associations
             WHERE status='confirmed' AND (
                 (local_project_id=?1 AND peer_project_id=?2) OR
                 (local_project_id=?2 AND peer_project_id=?1))",
            rusqlite::params![local_id_text, peer_id_text],
            |row| row.get(0),
        )
        .map_err(sqlite)?;
    if status == "confirmed" && pair_count != 1 {
        return Err(Failure::domain(
            "association_ambiguous",
            "the local store has duplicate active rows for this unordered project pair".into(),
        ));
    }
    if status == "revoked" && pair_count > 1 {
        return Err(Failure::domain(
            "association_ambiguous",
            "the local store has duplicate active rows for this unordered project pair".into(),
        ));
    }
    Ok(StoredAssociation { descriptor, status })
}

fn persist_confirmed_association(
    store: &mut CollaborationStore,
    descriptor: &ProjectAssociationDescriptor,
    record_id: &RecordId,
    object_digest: ObjectDigest,
) -> Result<&'static str, Failure> {
    if store.schema_version() < 31 {
        return Err(Failure::refused(
            "association_schema_unavailable",
            "the local project store lacks the project association schema",
            "upgrade the local project store before confirming an association",
        ));
    }
    let transaction = store
        .connection()
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(sqlite)?;
    let existing: Option<(String, String)> = transaction
        .query_row(
            "SELECT association_object_sha256,status FROM local_project_associations
             WHERE association_record_id=?1",
            [record_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(sqlite)?;
    let digest_text = object_digest.hex();
    if let Some((existing_digest, status)) = existing {
        if existing_digest != digest_text {
            return Err(Failure::domain(
                "association_corrupt",
                "record ID is indexed to a different canonical object".into(),
            ));
        }
        if status == "confirmed" {
            transaction.commit().map_err(sqlite)?;
            return Ok("already_confirmed");
        }
        return Err(Failure::refused(
            "association_revoked",
            "this exact association proposal was already revoked and cannot be reactivated",
            REPROPOSE_NEXT,
        ));
    }
    let active_count: i64 = transaction
        .query_row(
            "SELECT count(*) FROM local_project_associations
             WHERE status='confirmed' AND (
                 (local_project_id=?1 AND peer_project_id=?2) OR
                 (local_project_id=?2 AND peer_project_id=?1))",
            rusqlite::params![
                descriptor.local_project_id().as_str(),
                descriptor.peer_project_id().as_str()
            ],
            |row| row.get(0),
        )
        .map_err(sqlite)?;
    if active_count != 0 {
        return Err(Failure::refused(
            "association_conflict",
            "this unordered project pair already has an active association in the invoking store",
            "revoke the existing local association before confirming a replacement",
        ));
    }
    let confirmed_ms = now_ms()?;
    transaction
        .execute(
            "INSERT INTO local_project_associations
             (association_record_id,association_object_sha256,local_project_key,local_project_id,peer_project_id,
              local_descriptor_record_id,peer_descriptor_record_id,peer_project_key,status,
              confirmed_ms,revoked_ms)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'confirmed',?9,NULL)",
            rusqlite::params![
                record_id.as_str(),
                digest_text,
                descriptor.local_project_key(),
                descriptor.local_project_id().as_str(),
                descriptor.peer_project_id().as_str(),
                descriptor.local_descriptor_record_id().as_str(),
                descriptor.peer_descriptor_record_id().as_str(),
                descriptor.peer_project_key(),
                confirmed_ms,
            ],
        )
        .map_err(sqlite)?;
    transaction.commit().map_err(sqlite)?;
    Ok("confirmed")
}

fn read_working_association(worktree: &Path) -> Result<Vec<u8>, Failure> {
    let directory = worktree.join(".aethyme");
    let metadata = std::fs::symlink_metadata(&directory)
        .map_err(|error| Failure::domain("io", format!("{}: {error}", directory.display())))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(Failure::refused(
            "symlinked_config",
            "the .aethyme directory must be a real directory",
            CONFIRM_NEXT,
        ));
    }
    let path = worktree.join(PROJECT_ASSOCIATION_PATH);
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = options.open(&path).map_err(|error| {
        Failure::refused(
            "association_missing",
            format!("{}: {error}", path.display()),
            CONFIRM_NEXT,
        )
    })?;
    if !file
        .metadata()
        .map_err(|error| Failure::domain("io", error.to_string()))?
        .is_file()
    {
        return Err(Failure::refused(
            "invalid_association_file",
            "the association proposal must be a regular file",
            CONFIRM_NEXT,
        ));
    }
    let mut bytes = Vec::new();
    file.take((MAX_PROJECT_ASSOCIATION_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| Failure::domain("io", error.to_string()))?;
    if bytes.len() > MAX_PROJECT_ASSOCIATION_BYTES {
        return Err(Failure::refused(
            "association_too_large",
            "the association proposal exceeds its byte limit",
            CONFIRM_NEXT,
        ));
    }
    Ok(bytes)
}

fn write_association_descriptor(worktree: &Path, bytes: &[u8]) -> Result<PathBuf, Failure> {
    if bytes.len() > MAX_PROJECT_ASSOCIATION_BYTES {
        return Err(Failure::refused(
            "association_too_large",
            "the association proposal exceeds its byte limit",
            CONFIRM_NEXT,
        ));
    }
    let directory = worktree.join(".aethyme");
    let metadata = std::fs::symlink_metadata(&directory)
        .map_err(|error| Failure::domain("io", format!("{}: {error}", directory.display())))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(Failure::refused(
            "symlinked_config",
            "the .aethyme directory must be a real directory",
            CONFIRM_NEXT,
        ));
    }
    let path = worktree.join(PROJECT_ASSOCIATION_PATH);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o644)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut file = options.open(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            Failure::refused("association_exists", format!("{} already exists; association propose never replaces it", path.display()), "inspect and explicitly remove or move the existing proposal before proposing another")
        } else {
            Failure::domain("io", format!("{}: {error}", path.display()))
        }
    })?;
    use std::io::Write;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| Failure::domain("io", format!("{}: {error}", path.display())))?;
    std::fs::File::open(&directory)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| Failure::domain("io", format!("{}: {error}", directory.display())))?;
    Ok(path)
}

fn parse_peer_flags(rest: &[&str]) -> Result<(PathBuf, bool), Failure> {
    let mut peer = None;
    let mut write = false;
    let mut index = 0;
    while index < rest.len() {
        match rest[index] {
            "--peer-repo" => {
                if peer.is_some() {
                    return Err(Failure::usage("duplicate --peer-repo"));
                }
                peer = Some(PathBuf::from(
                    *rest
                        .get(index + 1)
                        .ok_or_else(|| Failure::usage("--peer-repo needs a path"))?,
                ));
                index += 2;
            }
            "--write" => {
                if write {
                    return Err(Failure::usage("duplicate --write"));
                }
                write = true;
                index += 1;
            }
            flag if flag.starts_with("--") => {
                return Err(Failure::usage(format!("unknown flag {flag}")));
            }
            _ => return Err(Failure::usage("unexpected project association argument")),
        }
    }
    let peer =
        peer.ok_or_else(|| Failure::usage("project association requires --peer-repo <path>"))?;
    Ok((peer, write))
}

fn parse_confirm_flags(rest: &[&str]) -> Result<(PathBuf, RecordId), Failure> {
    let mut peer = None;
    let mut confirmed = None;
    let mut index = 0;
    while index < rest.len() {
        match rest[index] {
            "--peer-repo" => {
                if peer.is_some() {
                    return Err(Failure::usage("duplicate --peer-repo"));
                }
                peer = Some(PathBuf::from(
                    *rest
                        .get(index + 1)
                        .ok_or_else(|| Failure::usage("--peer-repo needs a path"))?,
                ));
                index += 2;
            }
            "--confirm" => {
                if confirmed.is_some() {
                    return Err(Failure::usage("duplicate --confirm"));
                }
                let value = *rest
                    .get(index + 1)
                    .ok_or_else(|| Failure::usage("--confirm needs an association RecordId"))?;
                confirmed = Some(
                    RecordId::parse(value).map_err(|error| Failure::usage(error.to_string()))?,
                );
                index += 2;
            }
            flag if flag.starts_with("--") => {
                return Err(Failure::usage(format!("unknown flag {flag}")));
            }
            _ => return Err(Failure::usage("unexpected project association argument")),
        }
    }
    Ok((
        peer.ok_or_else(|| {
            Failure::usage("project association confirm requires --peer-repo <path>")
        })?,
        confirmed.ok_or_else(|| {
            Failure::usage("project association confirm requires --confirm <record-id>")
        })?,
    ))
}

pub(crate) fn repository_roots(path: &Path) -> Result<(PathBuf, PathBuf), Failure> {
    let repository = crate::git::GitRepo::discover(path).map_err(|error| {
        Failure::refused(
            "not_a_repository",
            format!("{}: {error}", path.display()),
            "pass the root of an independently enrolled Git repository",
        )
    })?;
    let worktree = repository.root().to_path_buf();
    let main_root = repository.main_root().map_err(|error| {
        Failure::refused(
            "not_a_repository",
            format!("{}: {error}", path.display()),
            "pass the root of an independently enrolled Git repository",
        )
    })?;
    Ok((worktree, main_root))
}

fn now_ms() -> Result<i64, Failure> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| Failure::domain("clock", error.to_string()))?
        .as_millis()
        .try_into()
        .map_err(|_| {
            Failure::domain(
                "clock",
                "system timestamp exceeds SQLite integer range".into(),
            )
        })
}

fn identity_binding_failure(error: identity_store::IdentityStoreError) -> Failure {
    let code = error.code();
    let message = error.to_string();
    if matches!(error, identity_store::IdentityStoreError::Corrupt(_)) {
        Failure::refused(
            code,
            message,
            "repair or restore the local project identity record before associating clones",
        )
    } else {
        Failure::domain(code, message)
    }
}

fn sqlite(error: rusqlite::Error) -> Failure {
    Failure::domain("sqlite", error.to_string())
}
