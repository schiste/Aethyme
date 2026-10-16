//! Explicit project enrollment and committed-descriptor binding for the
//! experimental Local identity profile (plan §5.2–5.4; D10–D12).
//!
//! A descriptor printed by `enroll` is only a proposal. It becomes this
//! clone's trusted local mapping only after its exact path is named by the
//! committed config, the operator confirms its RecordId, and its anchor is
//! independently present in this store's retained archive. `ProjectKey`
//! remains the local store key and is never treated as the logical ProjectId.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};

use aethyme_contracts::experimental_v0::project_identity::{
    PROJECT_DESCRIPTOR_PATH, ProjectDescriptor, ProjectId,
};
use aethyme_contracts::experimental_v0::{EntryKind, RecordId, SourceEntry, SourceSnapshot};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};

use crate::collaboration_archive::{self as archive, CommitOid, ObjectDigest};
use crate::collaboration_state::{
    CollaborationStore, ProjectKey, locate_for_repository, open_for_repository,
};
use crate::collaboration_submit::{self, Setting};
use crate::exit_status;

use super::{Failure, Output};

const MAX_POLICY_BYTES: usize = 1024 * 1024;
const DESCRIPTOR_SETTING: &str = "descriptor";
const CONFIG_PATH: &str = ".aethyme/config.toml";
const CONFIRM_NEXT: &str = "commit the descriptor and its exact [collaboration] descriptor pointer, then run `aethyme collab project bind --confirm <record-id>`";

pub(super) fn command(main_root: &Path, worktree: &Path, rest: &[&str]) -> Result<Output, Failure> {
    match rest {
        ["enroll", flags @ ..] => enroll(worktree, flags),
        ["bind", flags @ ..] => bind(main_root, worktree, flags),
        _ => Err(Failure::usage(
            "project requires `enroll --commit <full-oid> [--write]` or `bind --confirm <record-id>`,",
        )),
    }
}

fn enroll(worktree: &Path, rest: &[&str]) -> Result<Output, Failure> {
    let write = rest.contains(&"--write");
    let mut commit_text = None;
    let mut index = 0;
    while index < rest.len() {
        match rest[index] {
            "--write" => index += 1,
            "--commit" => {
                if commit_text.is_some() {
                    return Err(Failure::usage("duplicate --commit"));
                }
                commit_text = Some(
                    *rest
                        .get(index + 1)
                        .ok_or_else(|| Failure::usage("--commit needs a full commit object id"))?,
                );
                index += 2;
            }
            flag if flag.starts_with("--") => {
                return Err(Failure::usage(format!("unknown flag {flag}")));
            }
            _ => return Err(Failure::usage("unexpected project enroll argument")),
        }
    }
    let commit_text = commit_text.ok_or_else(|| {
        Failure::usage("project enroll requires --commit <full-commit-object-id>")
    })?;
    let requested = CommitOid::parse(commit_text)
        .map_err(|_| Failure::usage("--commit must be a full lowercase Git object id"))?;
    let current = archive::pin_commit(worktree, "HEAD")
        .map_err(|error| Failure::domain("git", error.to_string()))?;
    if !is_ancestor(worktree, &requested, &current)? {
        return Err(Failure::refused(
            "anchor_not_committed",
            "the enrollment anchor must be an ancestor of this clone's committed HEAD",
            "choose an explicit full commit already in the checked-out history",
        ));
    }
    let snapshot = archive::snapshot_of_commit(worktree, &requested)
        .map_err(|error| Failure::domain("archive", error.to_string()))?;
    let encoded = super::mint_project_id()?;
    let project_id = ProjectId::parse(&format!("proj:{encoded}"))
        .map_err(|error| Failure::domain("invalid_project_id", error.to_string()))?;
    let descriptor = ProjectDescriptor::from_snapshot(project_id, requested.as_str(), &snapshot)
        .map_err(|error| Failure::domain("invalid_descriptor", error.to_string()))?;
    let (bytes, record_id) = descriptor.to_record();
    if bytes.len() > aethyme_contracts::experimental_v0::project_identity::MAX_DESCRIPTOR_BYTES {
        return Err(Failure::refused(
            "descriptor_too_large",
            "the canonical project descriptor exceeds the bounded descriptor size",
            "enroll from a smaller committed source snapshot",
        ));
    }

    let written = if write {
        Some(write_descriptor(worktree, &bytes)?)
    } else {
        None
    };
    let parsed: Value = serde_json::from_slice(&bytes)
        .map_err(|error| Failure::domain("invalid_descriptor", error.to_string()))?;
    let value = json!({
        "schema": "aethyme.collab-project-enroll/experimental-v0",
        "project_id": descriptor.project_id().as_str(),
        "project_key_unchanged": true,
        "anchor_commit": requested.as_str(),
        "anchor_snapshot": snapshot.id().as_str(),
        "descriptor_record_id": record_id.as_str(),
        "descriptor": parsed,
        "written": written.is_some(),
        "path": written,
        "config_pointer": PROJECT_DESCRIPTOR_PATH,
        "next_action": CONFIRM_NEXT,
        "grants_authority": false,
    });
    let text = format!(
        "project {} anchored at {}\nsnapshot {}\ndescriptor {}\n{}\n{}
",
        descriptor.project_id(),
        requested.as_str(),
        snapshot.id(),
        record_id,
        written
            .as_ref()
            .map(|path| format!("written to {}", path.display()))
            .unwrap_or_else(|| "descriptor not written; rerun with --write to create it".into()),
        CONFIRM_NEXT,
    );
    Ok((value, text))
}

fn write_descriptor(worktree: &Path, bytes: &[u8]) -> Result<PathBuf, Failure> {
    let directory = worktree.join(".aethyme");
    let metadata = std::fs::symlink_metadata(&directory)
        .map_err(|error| Failure::domain("io", format!("{}: {error}", directory.display())))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Failure::refused(
            "symlinked_config",
            format!("{} must be a real directory", directory.display()),
            "replace the symlink with a real .aethyme directory",
        ));
    }
    let path = worktree.join(PROJECT_DESCRIPTOR_PATH);
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
            Failure::refused(
                "descriptor_exists",
                format!(
                    "{} already exists; project enroll never replaces it",
                    path.display()
                ),
                "inspect and explicitly remove or move the existing descriptor before enrolling",
            )
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

fn bind(main_root: &Path, worktree: &Path, rest: &[&str]) -> Result<Output, Failure> {
    if rest.len() != 2 || rest[0] != "--confirm" {
        return Err(Failure::usage(
            "project bind requires exactly --confirm <descriptor-record-id>",
        ));
    }
    let confirmed = RecordId::parse(rest[1]).map_err(|error| Failure::usage(error.to_string()))?;
    let head = archive::pin_commit(worktree, "HEAD")
        .map_err(|error| Failure::domain("git", error.to_string()))?;
    let snapshot = archive::snapshot_of_commit(worktree, &head)
        .map_err(|error| Failure::domain("archive", error.to_string()))?;
    let config_bytes =
        committed_regular_file(worktree, &head, &snapshot, CONFIG_PATH, MAX_POLICY_BYTES)?;
    let config_text = std::str::from_utf8(&config_bytes).map_err(|_| {
        Failure::refused(
            "config_unreadable",
            "committed config is not UTF-8",
            CONFIRM_NEXT,
        )
    })?;
    let (project_key, pointer) = committed_enrollment(config_text)?;
    if pointer != PROJECT_DESCRIPTOR_PATH {
        return Err(Failure::refused(
            "descriptor_pointer_mismatch",
            format!(
                "committed [collaboration] {DESCRIPTOR_SETTING} must be exactly {PROJECT_DESCRIPTOR_PATH:?}"
            ),
            "commit the reviewed descriptor pointer at the bounded local descriptor path",
        ));
    }
    let descriptor_bytes = committed_regular_file(
        worktree,
        &head,
        &snapshot,
        PROJECT_DESCRIPTOR_PATH,
        aethyme_contracts::experimental_v0::project_identity::MAX_DESCRIPTOR_BYTES,
    )?;
    let (descriptor, actual_record_id) = ProjectDescriptor::from_record(&descriptor_bytes)
        .map_err(|error| Failure::refused("invalid_descriptor", error.to_string(), CONFIRM_NEXT))?;
    if actual_record_id != confirmed {
        return Err(Failure::refused(
            "descriptor_confirmation_mismatch",
            format!(
                "confirmed {}, but the committed descriptor is {}",
                confirmed, actual_record_id
            ),
            "review the committed descriptor and confirm its exact RecordId",
        ));
    }
    let anchor_commit = CommitOid::parse(descriptor.anchor_commit())
        .map_err(|error| Failure::refused("invalid_descriptor", error.to_string(), CONFIRM_NEXT))?;
    if !is_ancestor(worktree, &anchor_commit, &head)? {
        return Err(Failure::refused(
            "anchor_not_committed",
            "descriptor anchor is not an ancestor of the committed HEAD containing its pointer",
            "commit or fetch the exact anchor before binding this descriptor",
        ));
    }
    let anchor_snapshot = archive::snapshot_of_commit(worktree, &anchor_commit)
        .map_err(|error| Failure::domain("archive", error.to_string()))?;
    descriptor
        .validate_anchor(anchor_commit.as_str(), &anchor_snapshot)
        .map_err(|error| Failure::refused("anchor_mismatch", error.to_string(), CONFIRM_NEXT))?;

    // The store's ProjectKey remains a local storage locator. It does not
    // become the descriptor's logical ProjectId and cannot merge clones.
    let (_, _, exists) = locate_for_repository(main_root, &project_key)
        .map_err(|error| Failure::domain(error.code(), error.to_string()))?;
    if !exists {
        return Err(Failure::refused(
            "not_initialized",
            format!(
                "local store {} does not exist; project bind never creates collaboration state",
                project_key.as_str()
            ),
            "initialize this local store through an explicit collaboration capture, then bind",
        ));
    }
    let mut store = open_for_repository(main_root, &project_key)
        .map_err(|error| Failure::domain(error.code(), error.to_string()))?;
    retain_anchor(&store, &descriptor, &anchor_snapshot)?;
    let descriptor_digest = archive::put_object(&store, &descriptor_bytes)
        .map_err(|error| Failure::domain("archive", error.to_string()))?;
    let result = persist_binding(
        &mut store,
        &descriptor,
        &actual_record_id,
        descriptor_digest,
        &head,
    )?;
    Ok((
        json!({
            "schema":"aethyme.collab-project-bind/experimental-v0",
            "project_id":descriptor.project_id().as_str(),
            "project_key":project_key.as_str(),
            "descriptor_record_id":actual_record_id.as_str(),
            "descriptor_object_sha256":descriptor_digest.hex(),
            "anchor_commit":descriptor.anchor_commit(),
            "anchor_snapshot":descriptor.anchor_snapshot().as_str(),
            "committed_config_commit":head.as_str(),
            "status":result,
            "grants_execution_authority":false,
            "grants_acceptance_authority":false,
        }),
        format!(
            "local project {} is {} as logical project {} (descriptor {})\nfile identities are local mappings only; no execution or acceptance authority was granted\n",
            project_key.as_str(),
            result,
            descriptor.project_id(),
            actual_record_id
        ),
    ))
}

fn committed_enrollment(config: &str) -> Result<(ProjectKey, String), Failure> {
    // This path validates the exact committed file, instead of consulting the
    // mutable/default-branch config selector used by submit and capture.
    let parsed = config
        .parse::<toml::Value>()
        .map_err(|error| Failure::refused("config_unreadable", error.to_string(), CONFIRM_NEXT))?;
    let table = parsed
        .get("collaboration")
        .and_then(toml::Value::as_table)
        .ok_or_else(|| {
            Failure::refused(
                "collaboration_disabled",
                "committed config has no [collaboration] table",
                CONFIRM_NEXT,
            )
        })?;
    let mut unknown: Vec<&str> = table
        .keys()
        .map(String::as_str)
        .filter(|key| !["capture", "project", DESCRIPTOR_SETTING].contains(key))
        .collect();
    unknown.sort_unstable();
    if !unknown.is_empty() {
        return Err(Failure::refused(
            "unknown_setting",
            format!(
                "committed [collaboration] has unsupported keys: {}",
                unknown.join(", ")
            ),
            CONFIRM_NEXT,
        ));
    }
    match table.get("capture").and_then(toml::Value::as_str) {
        Some("advisory" | "required") => {}
        _ => {
            return Err(Failure::refused(
                "collaboration_disabled",
                "committed [collaboration] capture must be advisory or required",
                CONFIRM_NEXT,
            ));
        }
    }
    let project = table
        .get("project")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| {
            Failure::refused(
                "not_configured",
                "committed [collaboration] must name its local project key",
                CONFIRM_NEXT,
            )
        })?;
    let project = ProjectKey::parse(project)
        .map_err(|error| Failure::refused("not_configured", error.to_string(), CONFIRM_NEXT))?;
    let pointer = table
        .get(DESCRIPTOR_SETTING)
        .and_then(toml::Value::as_str)
        .ok_or_else(|| {
            Failure::refused(
                "descriptor_pointer_missing",
                "committed [collaboration] must name the descriptor path",
                CONFIRM_NEXT,
            )
        })?
        .to_string();
    Ok((project, pointer))
}

fn committed_regular_file(
    repo: &Path,
    commit: &CommitOid,
    snapshot: &SourceSnapshot,
    path: &str,
    limit: usize,
) -> Result<Vec<u8>, Failure> {
    let entry = snapshot
        .entries()
        .iter()
        .find(|entry| entry.path() == path.as_bytes())
        .ok_or_else(|| {
            Failure::refused(
                "committed_file_missing",
                format!("{path} is not present in committed HEAD"),
                CONFIRM_NEXT,
            )
        })?;
    if !matches!(entry.kind(), EntryKind::Regular | EntryKind::Executable) {
        return Err(Failure::refused(
            "symlinked_path",
            format!("committed {path} is not a regular file"),
            "commit the pointer and descriptor as regular files",
        ));
    }
    let spec = format!("{}:{path}", commit.as_str());
    let output = archive::git(repo)
        .args(["show", "--no-ext-diff", "--format=", &spec])
        .stdin(Stdio::null())
        .output()
        .map_err(|error| {
            Failure::domain("git", format!("could not read committed {path}: {error}"))
        })?;
    if !output.status.success() {
        return Err(Failure::domain(
            "git",
            format!(
                "could not read committed {path}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ));
    }
    if output.stdout.len() > limit {
        return Err(Failure::refused(
            "file_too_large",
            format!("committed {path} exceeds the {limit}-byte bound"),
            "commit a smaller bounded file",
        ));
    }
    if <[u8; 32]>::from(sha2::Sha256::digest(&output.stdout)) != *entry.content_sha256() {
        return Err(Failure::domain(
            "git",
            format!("committed {path} did not match the verified HEAD snapshot"),
        ));
    }
    Ok(output.stdout)
}

fn is_ancestor(repo: &Path, ancestor: &CommitOid, descendant: &CommitOid) -> Result<bool, Failure> {
    let output = archive::git(repo)
        .args([
            "merge-base",
            "--is-ancestor",
            ancestor.as_str(),
            descendant.as_str(),
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|error| {
            Failure::domain("git", format!("could not verify commit ancestry: {error}"))
        })?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(Failure::domain(
            "git",
            format!(
                "could not verify commit ancestry: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        )),
    }
}

fn retain_anchor(
    store: &CollaborationStore,
    descriptor: &ProjectDescriptor,
    expected: &SourceSnapshot,
) -> Result<(), Failure> {
    let retained = archive::retained(store, descriptor.anchor_snapshot())
        .map_err(|error| Failure::domain("archive", error.to_string()))?
        .ok_or_else(|| {
            Failure::refused(
                "anchor_not_retained",
                "the exact descriptor anchor is not retained in this local archive",
                "capture and retain the anchor commit in this local store before binding",
            )
        })?;
    if retained.commit.as_str() != descriptor.anchor_commit() {
        return Err(Failure::domain(
            "archive",
            "retained anchor snapshot points at a different commit",
        ));
    }
    let manifest_digest = ObjectDigest::of_snapshot(descriptor.anchor_snapshot());
    let bytes = archive::read_object(store, &manifest_digest)
        .map_err(|error| Failure::domain("archive", error.to_string()))?;
    let manifest = archive::parse_manifest(&bytes)
        .ok_or_else(|| Failure::domain("archive", "retained anchor manifest is invalid"))?;
    if &manifest != expected {
        return Err(Failure::domain(
            "archive",
            "retained anchor manifest differs from the committed descriptor anchor",
        ));
    }
    for entry in manifest.entries() {
        let digest = ObjectDigest::from_bytes(*entry.content_sha256());
        archive::read_object(store, &digest)
            .map_err(|error| Failure::domain("archive", error.to_string()))?;
    }
    Ok(())
}

fn persist_binding(
    store: &mut CollaborationStore,
    descriptor: &ProjectDescriptor,
    record_id: &RecordId,
    object_digest: ObjectDigest,
    head: &CommitOid,
) -> Result<&'static str, Failure> {
    let conn = store.connection();
    let transaction = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(sqlite)?;
    type Binding = (String, String, String, String, String, String, String);
    let existing: Option<Binding> = transaction
        .query_row(
            "SELECT project_id,descriptor_record_id,descriptor_object_sha256,anchor_commit,
                    anchor_snapshot,confirmed_descriptor_record_id,committed_config_commit
             FROM local_project_bindings WHERE singleton=1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()
        .map_err(sqlite)?;
    let expected_project = descriptor.project_id().as_str();
    let expected_snapshot = descriptor.anchor_snapshot().as_str();
    let expected_commit = descriptor.anchor_commit();
    let object_hex = object_digest.hex();
    if let Some(binding) = existing {
        if binding.0 != expected_project
            || binding.1 != record_id.as_str()
            || binding.2 != object_hex
            || binding.3 != expected_commit
            || binding.4 != expected_snapshot
            || binding.5 != record_id.as_str()
        {
            return Err(Failure::refused(
                "project_binding_conflict",
                "this local store is already bound to a different committed descriptor",
                "inspect the existing descriptor binding; a project mapping is never replaced implicitly",
            ));
        }
        validate_file_rows(&transaction, descriptor)?;
        transaction.commit().map_err(sqlite)?;
        return Ok("already_bound");
    }
    let unexpected: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM local_project_file_ids)",
            [],
            |row| row.get(0),
        )
        .map_err(sqlite)?;
    if unexpected {
        return Err(Failure::domain(
            "sqlite",
            "file identity rows exist without a validated local project binding",
        ));
    }
    let bound_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| Failure::domain("io", error.to_string()))?
        .as_millis()
        .try_into()
        .map_err(|_| Failure::domain("io", "system timestamp exceeds SQLite integer range"))?;
    transaction
        .execute(
            "INSERT INTO local_project_bindings
             (singleton,project_id,descriptor_record_id,descriptor_object_sha256,anchor_commit,
              anchor_snapshot,confirmed_descriptor_record_id,committed_config_commit,bound_ms)
             VALUES (1,?1,?2,?3,?4,?5,?2,?6,?7)",
            rusqlite::params![
                expected_project,
                record_id.as_str(),
                object_hex,
                expected_commit,
                expected_snapshot,
                head.as_str(),
                bound_ms,
            ],
        )
        .map_err(sqlite)?;
    for file in descriptor.files() {
        transaction
            .execute(
                "INSERT INTO local_project_file_ids
                 (snapshot_id,path,file_id,mode,content_sha256) VALUES (?1,?2,?3,?4,?5)",
                rusqlite::params![
                    expected_snapshot,
                    &file.path,
                    file.file_id.as_str(),
                    file.kind.git_mode(),
                    hex::encode(file.content_sha256),
                ],
            )
            .map_err(sqlite)?;
    }
    transaction.commit().map_err(sqlite)?;
    Ok("bound")
}

fn validate_file_rows(
    transaction: &rusqlite::Transaction<'_>,
    descriptor: &ProjectDescriptor,
) -> Result<(), Failure> {
    let mut statement = transaction
        .prepare(
            "SELECT path,file_id,mode,content_sha256 FROM local_project_file_ids
             WHERE snapshot_id=?1 ORDER BY path",
        )
        .map_err(sqlite)?;
    let rows = statement
        .query_map([descriptor.anchor_snapshot().as_str()], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(sqlite)?;
    let actual: Vec<_> = rows.collect::<Result<_, _>>().map_err(sqlite)?;
    let expected: Vec<_> = descriptor
        .files()
        .iter()
        .map(|file| {
            (
                file.path.clone(),
                file.file_id.as_str().to_string(),
                file.kind.git_mode().to_string(),
                hex::encode(file.content_sha256),
            )
        })
        .collect();
    if actual != expected {
        return Err(Failure::domain(
            "sqlite",
            "stored anchor FileId rows differ from the validated descriptor",
        ));
    }
    Ok(())
}

fn sqlite(error: rusqlite::Error) -> Failure {
    Failure::domain("sqlite", error.to_string())
}
