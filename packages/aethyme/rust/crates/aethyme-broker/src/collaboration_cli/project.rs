//! Explicit project enrollment and committed-descriptor binding for the
//! experimental Local identity profile (plan §5.2–5.4; D10–D12).
//!
//! A descriptor printed by `enroll` is only a proposal. It becomes this
//! clone's trusted local mapping only after its exact path is named by the
//! committed config, the operator confirms its RecordId, and its anchor is
//! independently present in this store's retained archive. The canonical
//! `proj-<base32>` key written by `collab enroll` preserves that enrollment's
//! ProjectId; other local keys remain opaque store names.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};

use aethyme_contracts::experimental_v0::project_identity::{
    PROJECT_DESCRIPTOR_PATH, ProjectDescriptor, ProjectId, ProjectIdentityError,
};
use aethyme_contracts::experimental_v0::{
    EntryKind, FileIdentityTransition, RecordId, SourceSnapshot, SourceSnapshotId,
    advance_file_identities,
};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use sha2::Digest;

use crate::collaboration_archive::{self as archive, CommitOid, ObjectDigest};
use crate::collaboration_gc;
use crate::collaboration_identity_store::{self as identity_store, ApprovedProjectBinding};
use crate::collaboration_state::{
    CollaborationRoot, CollaborationStore, ProjectKey, locate_for_repository, open_for_repository,
};

use super::{Failure, Output};

const MAX_POLICY_BYTES: usize = 1024 * 1024;
const DESCRIPTOR_SETTING: &str = "descriptor";
const CONFIG_PATH: &str = ".aethyme/config.toml";
const CONFIRM_NEXT: &str = "commit the descriptor and its exact [collaboration] descriptor pointer, then run `aethyme collab project bind --confirm <record-id>`";

pub(super) fn command(main_root: &Path, worktree: &Path, rest: &[&str]) -> Result<Output, Failure> {
    match rest {
        ["enroll", flags @ ..] => enroll(worktree, flags),
        ["bind", flags @ ..] => bind(main_root, worktree, flags),
        ["association", association_args @ ..] => {
            super::project_association::command(main_root, worktree, association_args)
        }
        _ => Err(Failure::usage(
            "project requires `enroll --commit <full-oid> [--write]`, `bind --confirm <record-id>`, or `association <propose|confirm|revoke>`,",
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
    let project_id = match existing_enrollment_project_id(worktree, &current)? {
        Some(project_id) => project_id,
        None => {
            let encoded = super::mint_project_id()?;
            ProjectId::parse(&format!("proj:{encoded}"))
                .map_err(|error| Failure::domain("invalid_project_id", error.to_string()))?
        }
    };
    let descriptor = ProjectDescriptor::from_snapshot(project_id, requested.as_str(), &snapshot)
        .map_err(enrollment_descriptor_failure)?;
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

/// Reuse the logical ProjectId that `collab enroll` encoded in its
/// canonical local key. Arbitrary legacy keys are not interpreted as IDs.
fn existing_enrollment_project_id(
    worktree: &Path,
    current: &CommitOid,
) -> Result<Option<ProjectId>, Failure> {
    // `.aethyme/config` in either format (#1223).
    let Ok(Some(config_text)) = aethyme_config::try_read_config_text_with(
        aethyme_config::CONFIG_DIR,
        "config",
        |relpath| {
            Ok::<_, Failure>(
                archive::raw_file_at_commit(worktree, current, relpath.as_bytes())
                    .map_err(|error| Failure::domain("archive", error.to_string()))?
                    .and_then(|bytes| String::from_utf8(bytes).ok()),
            )
        },
    )?
    else {
        return Ok(None);
    };
    let Ok(config_text) = config_text.toml_text() else {
        return Ok(None);
    };
    let Ok(config) = config_text.parse::<toml::Value>() else {
        return Ok(None);
    };
    let Some(project_key) = config
        .get("collaboration")
        .and_then(toml::Value::as_table)
        .and_then(|table| table.get("project"))
        .and_then(toml::Value::as_str)
    else {
        return Ok(None);
    };
    let Some(encoded) = project_key.strip_prefix("proj-") else {
        return Ok(None);
    };
    Ok(ProjectId::parse(&format!("proj:{encoded}")).ok())
}

fn enrollment_descriptor_failure(error: ProjectIdentityError) -> Failure {
    match &error {
        ProjectIdentityError::TooManyFiles { .. } => Failure::refused(
            "descriptor_too_many_files",
            error.to_string(),
            "enroll a committed source snapshot with no more than the canonical profile file limit",
        ),
        ProjectIdentityError::DescriptorTooLarge { .. } => Failure::refused(
            "descriptor_too_large",
            error.to_string(),
            "enroll a committed source snapshot whose canonical descriptor fits the record limit",
        ),
        _ => Failure::domain("invalid_descriptor", error.to_string()),
    }
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
    bind_with_root(main_root, worktree, rest, None)
}

#[cfg(test)]
fn bind_at_root(
    main_root: &Path,
    worktree: &Path,
    rest: &[&str],
    root: &CollaborationRoot,
) -> Result<Output, Failure> {
    bind_with_root(main_root, worktree, rest, Some(root))
}

fn bind_with_root(
    main_root: &Path,
    worktree: &Path,
    rest: &[&str],
    explicit_root: Option<&CollaborationRoot>,
) -> Result<Output, Failure> {
    if rest.len() != 2 || rest[0] != "--confirm" {
        return Err(Failure::usage(
            "project bind requires exactly --confirm <descriptor-record-id>",
        ));
    }
    let confirmed = RecordId::parse(rest[1]).map_err(|error| Failure::usage(error.to_string()))?;
    let head = archive::pin_commit(worktree, "HEAD")
        .map_err(|error| Failure::domain("git", error.to_string()))?;
    // The initial parse only locates this clone's existing local store. All
    // validation of the pinned commit is repeated under the archive barrier.
    let preliminary_snapshot = archive::snapshot_of_commit(worktree, &head)
        .map_err(|error| Failure::domain("archive", error.to_string()))?;
    let preliminary_config = committed_regular_file(
        worktree,
        &head,
        &preliminary_snapshot,
        CONFIG_PATH,
        MAX_POLICY_BYTES,
    )?;
    let preliminary_config = std::str::from_utf8(&preliminary_config).map_err(|_| {
        Failure::refused(
            "config_unreadable",
            "committed config is not UTF-8",
            CONFIRM_NEXT,
        )
    })?;
    let (project_key, _) = committed_enrollment(preliminary_config)?;
    let mut store = if let Some(root) = explicit_root {
        CollaborationStore::open(root, &project_key, &[])
            .map_err(|error| Failure::domain(error.code(), error.to_string()))?
    } else {
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
        open_for_repository(main_root, &project_key)
            .map_err(|error| Failure::domain(error.code(), error.to_string()))?
    };
    let archive_use = collaboration_gc::archive_use(&store).map_err(|source| {
        Failure::domain(
            "archive_lock",
            format!(
                "{}: {source}",
                collaboration_gc::lock_path(&store).display()
            ),
        )
    })?;
    let _response = crate::collaboration_sensitive::response_use(&store).map_err(|error| {
        Failure::refused(
            "sensitive_response",
            error.to_string(),
            "resolve the sensitive-content condition before binding a project",
        )
    })?;

    // Re-read only the exact pinned commit and verify every authority-bearing
    // file while reclamation is excluded. Dirty worktree files are irrelevant.
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
    let (validated_project_key, pointer) = committed_enrollment(config_text)?;
    if validated_project_key != project_key {
        return Err(Failure::domain(
            "git",
            "pinned config changed while resolving the local store".into(),
        ));
    }
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
    if snapshot.id() == *descriptor.anchor_snapshot() {
        return Err(Failure::refused(
            "bootstrap_not_committed",
            "the descriptor and its committed config pointer must be committed after the anchor",
            CONFIRM_NEXT,
        ));
    }
    if let Some(existing) =
        identity_store::resolve_approved_project_binding_with_archive_use(&store, &archive_use)
            .map_err(identity_binding_failure)?
    {
        if existing.project_id() != descriptor.project_id()
            || existing.descriptor_record_id() != &actual_record_id
        {
            return Err(Failure::refused(
                "project_binding_conflict",
                "this local store is already bound to a different committed descriptor",
                "inspect the existing descriptor binding; a project mapping is never replaced implicitly",
            ));
        }
        return Ok(binding_output(&project_key, &existing, "already_bound"));
    }

    retain_anchor(&store, &descriptor, &anchor_snapshot)?;
    let retained_bootstrap =
        archive::retain_snapshot_for_binding(&mut store, worktree, &head, &archive_use)
            .map_err(|error| Failure::domain("archive", error.to_string()))?;
    if retained_bootstrap.snapshot_id != snapshot.id() || retained_bootstrap.commit != head {
        return Err(Failure::domain(
            "archive",
            "retained project bootstrap differs from the pinned committed HEAD snapshot".into(),
        ));
    }
    let descriptor_digest = archive::put_object(&store, &descriptor_bytes)
        .map_err(|error| Failure::domain("archive", error.to_string()))?;
    let initial_state = descriptor
        .initial_file_identity_state()
        .map_err(|error| Failure::refused("invalid_descriptor", error.to_string(), CONFIRM_NEXT))?;
    let transition = advance_file_identities(
        descriptor.project_id(),
        &initial_state,
        actual_record_id.as_str(),
        &anchor_snapshot,
        &snapshot,
        &[],
    )
    .map_err(|error| {
        Failure::refused(
            "bootstrap_identity_unknown",
            error.to_string(),
            CONFIRM_NEXT,
        )
    })?;
    let bootstrap_digest = identity_store::bootstrap_transition_sha256(
        descriptor.project_id(),
        &actual_record_id,
        descriptor.anchor_snapshot(),
        &retained_bootstrap.snapshot_id,
        &head,
        &transition,
    );
    let result = persist_binding(
        &mut store,
        BindingPersistRequest {
            descriptor: &descriptor,
            record_id: &actual_record_id,
            object_digest: descriptor_digest,
            head: &head,
            bootstrap_snapshot: &retained_bootstrap.snapshot_id,
            bootstrap_transition_sha256: &bootstrap_digest,
            bootstrap_transition: &transition,
        },
    )?;
    let binding =
        identity_store::resolve_approved_project_binding_with_archive_use(&store, &archive_use)
            .map_err(identity_binding_failure)?
            .ok_or_else(|| {
                Failure::domain("identity_store", "binding row was not committed".into())
            })?;
    Ok(binding_output(&project_key, &binding, result))
}

fn identity_binding_failure(error: identity_store::IdentityStoreError) -> Failure {
    let code = error.code();
    let message = error.to_string();
    if matches!(error, identity_store::IdentityStoreError::Corrupt(_)) {
        Failure::refused(
            code,
            message,
            "repair or restore the local project identity record before binding",
        )
    } else {
        Failure::domain(code, message)
    }
}

fn binding_output(
    project_key: &ProjectKey,
    binding: &ApprovedProjectBinding,
    status: &str,
) -> Output {
    (
        json!({
            "schema":"aethyme.collab-project-bind/experimental-v0",
            "project_id":binding.project_id().as_str(),
            "project_key":project_key.as_str(),
            "descriptor_record_id":binding.descriptor_record_id().as_str(),
            "descriptor_object_sha256":binding.descriptor_object().hex(),
            "anchor_commit":binding.anchor_commit().as_str(),
            "anchor_snapshot":binding.descriptor().anchor_snapshot().as_str(),
            "committed_config_commit":binding.committed_config_commit().as_str(),
            "bootstrap_snapshot":binding.bootstrap_snapshot().as_str(),
            "bootstrap_transition_sha256":binding.bootstrap_transition_sha256(),
            "status":status,
            "grants_execution_authority":false,
            "grants_acceptance_authority":false,
        }),
        format!(
            "local project {} is {} as logical project {} (descriptor {})\nfile identities are local mappings only; no execution or acceptance authority was granted\n",
            project_key.as_str(),
            status,
            binding.project_id(),
            binding.descriptor_record_id()
        ),
    )
}

pub(super) fn committed_enrollment(config: &str) -> Result<(ProjectKey, String), Failure> {
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

/// The committed `.aethyme/config` in either format (#1223), as TOML text,
/// read with [`committed_regular_file`]'s regular-file and size checks.
pub(super) fn committed_config_toml(
    repo: &Path,
    commit: &CommitOid,
    snapshot: &SourceSnapshot,
    limit: usize,
) -> Result<String, Failure> {
    let present: Vec<String> = aethyme_config::candidate_relpaths("config")
        .into_iter()
        .filter(|path| {
            snapshot
                .entries()
                .iter()
                .any(|entry| entry.path() == path.as_bytes())
        })
        .collect();
    if present.len() > 1 {
        return Err(Failure::refused(
            "config_ambiguous_format",
            format!(
                "{} are both committed; keep exactly one configuration file",
                present.join(" and ")
            ),
            CONFIRM_NEXT,
        ));
    }
    let path = present
        .into_iter()
        .next()
        .unwrap_or_else(|| CONFIG_PATH.into());
    let bytes = committed_regular_file(repo, commit, snapshot, &path, limit)?;
    let text = String::from_utf8(bytes).map_err(|_| {
        Failure::refused(
            "config_unreadable",
            "committed config is not UTF-8",
            CONFIRM_NEXT,
        )
    })?;
    let text = aethyme_config::ConfigText {
        format: aethyme_config::ConfigFormat::from_path(Path::new(&path))
            .unwrap_or(aethyme_config::ConfigFormat::Toml),
        path: path.into(),
        text,
    };
    text.toml_text()
        .map(|toml| toml.into_owned())
        .map_err(|error| Failure::refused("config_invalid", error.to_string(), CONFIRM_NEXT))
}

pub(super) fn committed_regular_file(
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
    let blob_output = archive::git(repo)
        .args(["rev-parse", "--verify", &spec])
        .stdin(Stdio::null())
        .output()
        .map_err(|error| {
            Failure::domain(
                "git",
                format!("could not resolve committed {path}: {error}"),
            )
        })?;
    if !blob_output.status.success() {
        return Err(Failure::domain(
            "git",
            format!(
                "could not resolve committed {path}: {}",
                String::from_utf8_lossy(&blob_output.stderr).trim()
            ),
        ));
    }
    let blob_oid = String::from_utf8_lossy(&blob_output.stdout)
        .trim()
        .to_string();
    CommitOid::parse(&blob_oid)
        .map_err(|error| Failure::domain("git", format!("invalid committed blob id: {error}")))?;
    let size_output = archive::git(repo)
        .args(["cat-file", "-s", &blob_oid])
        .stdin(Stdio::null())
        .output()
        .map_err(|error| {
            Failure::domain(
                "git",
                format!("could not inspect committed {path}: {error}"),
            )
        })?;
    if !size_output.status.success() {
        return Err(Failure::domain(
            "git",
            format!(
                "could not inspect committed {path}: {}",
                String::from_utf8_lossy(&size_output.stderr).trim()
            ),
        ));
    }
    let object_size = String::from_utf8_lossy(&size_output.stdout)
        .trim()
        .parse::<u64>()
        .map_err(|error| {
            Failure::domain("git", format!("invalid committed {path} size: {error}"))
        })?;
    if object_size > limit as u64 {
        return Err(Failure::refused(
            "file_too_large",
            format!("committed {path} exceeds the {limit}-byte bound"),
            "commit a smaller bounded file",
        ));
    }

    let mut child = archive::git(repo)
        .args(["cat-file", "blob", &blob_oid])
        .stdin(Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| {
            Failure::domain("git", format!("could not read committed {path}: {error}"))
        })?;
    let mut bytes = Vec::with_capacity(object_size as usize);
    let read = child
        .stdout
        .take()
        .ok_or_else(|| Failure::domain("git", "git cat-file did not expose stdout".into()))?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes);
    let output = child.wait_with_output().map_err(|error| {
        Failure::domain(
            "git",
            format!("could not finish reading committed {path}: {error}"),
        )
    })?;
    read.map_err(|error| {
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
    if bytes.len() as u64 != object_size || bytes.len() > limit {
        return Err(Failure::refused(
            "file_too_large",
            format!("committed {path} did not match its bounded object size"),
            "commit a smaller bounded file",
        ));
    }
    if <[u8; 32]>::from(sha2::Sha256::digest(&bytes)) != *entry.content_sha256() {
        return Err(Failure::domain(
            "git",
            format!("committed {path} did not match the verified HEAD snapshot"),
        ));
    }
    Ok(bytes)
}

pub(super) fn is_ancestor(
    repo: &Path,
    ancestor: &CommitOid,
    descendant: &CommitOid,
) -> Result<bool, Failure> {
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
            "retained anchor snapshot points at a different commit".into(),
        ));
    }
    let manifest_digest = ObjectDigest::of_snapshot(descriptor.anchor_snapshot());
    let bytes = archive::read_object(store, &manifest_digest)
        .map_err(|error| Failure::domain("archive", error.to_string()))?;
    let manifest = archive::parse_manifest(&bytes)
        .ok_or_else(|| Failure::domain("archive", "retained anchor manifest is invalid".into()))?;
    if &manifest != expected {
        return Err(Failure::domain(
            "archive",
            "retained anchor manifest differs from the committed descriptor anchor".into(),
        ));
    }
    for entry in manifest.entries() {
        let digest = ObjectDigest::from_bytes(*entry.content_sha256());
        archive::read_object(store, &digest)
            .map_err(|error| Failure::domain("archive", error.to_string()))?;
    }
    Ok(())
}

struct BindingPersistRequest<'a> {
    descriptor: &'a ProjectDescriptor,
    record_id: &'a RecordId,
    object_digest: ObjectDigest,
    head: &'a CommitOid,
    bootstrap_snapshot: &'a SourceSnapshotId,
    bootstrap_transition_sha256: &'a str,
    bootstrap_transition: &'a FileIdentityTransition,
}

fn persist_binding(
    store: &mut CollaborationStore,
    request: BindingPersistRequest<'_>,
) -> Result<&'static str, Failure> {
    let BindingPersistRequest {
        descriptor,
        record_id,
        object_digest,
        head,
        bootstrap_snapshot,
        bootstrap_transition_sha256,
        bootstrap_transition,
    } = request;
    let conn = store.connection();
    let transaction = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(sqlite)?;
    type Binding = (String, String, String, String, String, String);
    let existing: Option<Binding> = transaction
        .query_row(
            "SELECT project_id,descriptor_record_id,descriptor_object_sha256,anchor_commit,
                    anchor_snapshot,confirmed_descriptor_record_id
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
            "file identity rows exist without a validated local project binding".into(),
        ));
    }
    let bound_ms: i64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| Failure::domain("io", error.to_string()))?
        .as_millis()
        .try_into()
        .map_err(|_| {
            Failure::domain("io", "system timestamp exceeds SQLite integer range".into())
        })?;
    transaction
        .execute(
            "INSERT INTO local_project_bindings
             (singleton,project_id,descriptor_record_id,descriptor_object_sha256,anchor_commit,
              anchor_snapshot,confirmed_descriptor_record_id,committed_config_commit,
              bootstrap_snapshot,bootstrap_transition_sha256,bound_ms)
             VALUES (1,?1,?2,?3,?4,?5,?2,?6,?7,?8,?9)",
            rusqlite::params![
                expected_project,
                record_id.as_str(),
                object_hex,
                expected_commit,
                expected_snapshot,
                head.as_str(),
                bootstrap_snapshot.as_str(),
                bootstrap_transition_sha256,
                bound_ms,
            ],
        )
        .map_err(sqlite)?;
    let anchor_state = descriptor
        .initial_file_identity_state()
        .map_err(|error| Failure::domain("invalid_descriptor", error.to_string()))?;
    for file in anchor_state.files() {
        insert_identity_file_row(
            &transaction,
            expected_snapshot,
            file.path.as_slice(),
            file.kind,
            &file.content_sha256,
            file.file_id.as_str(),
        )?;
    }
    for file in bootstrap_transition.result_files() {
        insert_identity_file_row(
            &transaction,
            bootstrap_snapshot.as_str(),
            file.path.as_slice(),
            file.kind,
            &file.content_sha256,
            file.file_id.as_str(),
        )?;
    }
    transaction.commit().map_err(sqlite)?;
    Ok("bound")
}

fn insert_identity_file_row(
    transaction: &rusqlite::Transaction<'_>,
    snapshot: &str,
    path: &[u8],
    kind: EntryKind,
    content_sha256: &[u8; 32],
    file_id: &str,
) -> Result<(), Failure> {
    transaction
        .execute(
            "INSERT INTO local_project_file_ids
             (snapshot_id,path,file_id,mode,content_sha256) VALUES (?1,?2,?3,?4,?5)",
            rusqlite::params![
                snapshot,
                path,
                file_id,
                kind.git_mode(),
                ObjectDigest::from_bytes(*content_sha256).hex()
            ],
        )
        .map_err(sqlite)?;
    Ok(())
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
                ObjectDigest::from_bytes(file.content_sha256).hex(),
            )
        })
        .collect();
    if actual != expected {
        return Err(Failure::domain(
            "sqlite",
            "stored anchor FileId rows differ from the validated descriptor".into(),
        ));
    }
    Ok(())
}

fn sqlite(error: rusqlite::Error) -> Failure {
    Failure::domain("sqlite", error.to_string())
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::{CONFIG_PATH, PROJECT_DESCRIPTOR_PATH, bind_at_root, enroll};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use crate::collaboration_archive::{self as archive, CommitOid};
    use crate::collaboration_capture::{
        self as capture, CaptureOutcome, CapturePolicy, CaptureReceipt, CaptureRequest,
        OperationId, RetentionBoundary,
    };
    use crate::collaboration_identity_store::{self as identity_store, ApprovedProjectBinding};
    use crate::collaboration_state::{CollaborationRoot, CollaborationStore, ProjectKey};
    use aethyme_contracts::experimental_v0::SourceSnapshotId;

    pub(crate) type FixtureError = String;

    /// Enrollment state before its proposed descriptor and pointer are
    /// committed. The fixture exists to drive real bind refusals without
    /// manufacturing an approved row.
    pub(crate) struct PendingProjectFixture {
        pub(crate) repo: tempfile::TempDir,
        _host: tempfile::TempDir,
        pub(crate) store: CollaborationStore,
        pub(crate) root: CollaborationRoot,
        pub(crate) descriptor_record_id: String,
    }

    impl PendingProjectFixture {
        pub(crate) fn new() -> Result<Self, FixtureError> {
            let repo = tempfile::tempdir().map_err(|error| error.to_string())?;
            let host = tempfile::tempdir().map_err(|error| error.to_string())?;
            let key = ProjectKey::parse("pending-project").map_err(|error| error.to_string())?;
            std::fs::create_dir_all(repo.path().join(".aethyme"))
                .map_err(|error| error.to_string())?;
            std::fs::write(
                repo.path().join(CONFIG_PATH),
                format!(
                    "[collaboration]\ncapture = \"required\"\nproject = \"{}\"\n",
                    key.as_str()
                ),
            )
            .map_err(|error| error.to_string())?;
            std::fs::write(repo.path().join("a.txt"), b"anchor\n")
                .map_err(|error| error.to_string())?;
            git(repo.path(), &["init", "-q", "-b", "main"])?;
            git(repo.path(), &["add", "-A"])?;
            git(repo.path(), &["commit", "-q", "-m", "pending anchor"])?;
            let anchor_commit =
                archive::pin_commit(repo.path(), "HEAD").map_err(|error| error.to_string())?;
            let root = CollaborationRoot::under_host_state(host.path());
            let mut store =
                CollaborationStore::open(&root, &key, &[]).map_err(|error| error.to_string())?;
            capture_anchor(&mut store, repo.path(), &anchor_commit, "pending-anchor")?;
            let enrolled = enroll(
                repo.path(),
                &["--commit", anchor_commit.as_str(), "--write"],
            )
            .map_err(|failure| format!("project enroll failed: {}", failure.message))?;
            let descriptor_record_id = enrolled
                .0
                .get("descriptor_record_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "project enroll returned no descriptor RecordId".to_string())?
                .to_string();
            Ok(Self {
                repo,
                _host: host,
                store,
                root,
                descriptor_record_id,
            })
        }

        pub(crate) fn write_working_pointer(&self, pointer: &str) -> Result<(), FixtureError> {
            let config = format!(
                "[collaboration]\ncapture = \"required\"\nproject = \"{}\"\ndescriptor = \"{}\"\n",
                self.store.project().as_str(),
                pointer
            );
            std::fs::write(self.repo.path().join(CONFIG_PATH), config)
                .map_err(|error| error.to_string())
        }

        pub(crate) fn commit_pointer(&self, pointer: &str) -> Result<CommitOid, FixtureError> {
            self.write_working_pointer(pointer)?;
            git(self.repo.path(), &["add", "-A"])?;
            git(
                self.repo.path(),
                &["commit", "-q", "-m", "commit descriptor pointer"],
            )?;
            archive::pin_commit(self.repo.path(), "HEAD").map_err(|error| error.to_string())
        }

        pub(crate) fn bind(&self, record_id: &str) -> Result<(), FixtureFailure> {
            bind_at_root(
                self.repo.path(),
                self.repo.path(),
                &["--confirm", record_id],
                &self.root,
            )
            .map(|_| ())
            .map_err(|failure| FixtureFailure {
                code: failure.code,
                exit: failure.exit,
            })
        }
    }

    pub(crate) struct FixtureFailure {
        pub(crate) code: String,
        pub(crate) exit: u8,
    }

    /// A real local enrollment/binding fixture. It drives the same `enroll`
    /// and `bind` command functions as the public CLI and owns an isolated
    /// repository plus host-state root.
    pub(crate) struct BoundProjectFixture {
        pub(crate) repo: tempfile::TempDir,
        pub(crate) host: tempfile::TempDir,
        pub(crate) store: CollaborationStore,
        pub(crate) binding: ApprovedProjectBinding,
        pub(crate) anchor_commit: CommitOid,
        pub(crate) bootstrap_commit: CommitOid,
        pub(crate) bootstrap_snapshot: SourceSnapshotId,
    }

    impl BoundProjectFixture {
        pub(crate) fn new() -> Result<Self, FixtureError> {
            let repo = tempfile::tempdir().map_err(|error| error.to_string())?;
            let host = tempfile::tempdir().map_err(|error| error.to_string())?;
            let key = ProjectKey::parse("fixture-project").map_err(|error| error.to_string())?;
            std::fs::create_dir_all(repo.path().join(".aethyme"))
                .map_err(|error| error.to_string())?;
            std::fs::write(
                repo.path().join(".aethyme/config.toml"),
                format!(
                    "[collaboration]\ncapture = \"required\"\nproject = \"{}\"\n",
                    key.as_str()
                ),
            )
            .map_err(|error| error.to_string())?;
            std::fs::write(repo.path().join("a.txt"), b"anchor\n")
                .map_err(|error| error.to_string())?;
            git(repo.path(), &["init", "-q", "-b", "main"])?;
            git(repo.path(), &["add", "-A"])?;
            git(repo.path(), &["commit", "-q", "-m", "fixture anchor"])?;
            let anchor_commit =
                archive::pin_commit(repo.path(), "HEAD").map_err(|error| error.to_string())?;

            let root = CollaborationRoot::under_host_state(host.path());
            let mut store =
                CollaborationStore::open(&root, &key, &[]).map_err(|error| error.to_string())?;
            capture_anchor(&mut store, repo.path(), &anchor_commit, "fixture-anchor")?;

            let enroll_args = ["--commit", anchor_commit.as_str(), "--write"];
            let enrolled = enroll(repo.path(), &enroll_args)
                .map_err(|failure| format!("project enroll failed: {}", failure.message))?;
            let descriptor_record_id = enrolled
                .0
                .get("descriptor_record_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "project enroll returned no descriptor RecordId".to_string())?
                .to_string();
            std::fs::write(
                repo.path().join(".aethyme/config.toml"),
                format!(
                    "[collaboration]\ncapture = \"required\"\nproject = \"{}\"\ndescriptor = \"{}\"\n",
                    key.as_str(), PROJECT_DESCRIPTOR_PATH
                ),
            )
            .map_err(|error| error.to_string())?;
            git(repo.path(), &["add", "-A"])?;
            git(
                repo.path(),
                &["commit", "-q", "-m", "fixture project binding"],
            )?;
            let bootstrap_commit =
                archive::pin_commit(repo.path(), "HEAD").map_err(|error| error.to_string())?;
            let bootstrap_snapshot = archive::snapshot_of_commit(repo.path(), &bootstrap_commit)
                .map_err(|error| error.to_string())?
                .id();
            let bind_args = ["--confirm", descriptor_record_id.as_str()];
            bind_at_root(repo.path(), repo.path(), &bind_args, &root)
                .map_err(|failure| format!("project bind failed: {}", failure.message))?;
            let binding = identity_store::resolve_approved_project_binding(&store)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "project bind did not persist an approved binding".to_string())?;
            Ok(Self {
                repo,
                host,
                store,
                binding,
                anchor_commit,
                bootstrap_commit,
                bootstrap_snapshot,
            })
        }

        pub(crate) fn repo_path(&self) -> &Path {
            self.repo.path()
        }

        pub(crate) fn store(&self) -> &CollaborationStore {
            &self.store
        }

        pub(crate) fn commit_file(
            &mut self,
            path: &str,
            bytes: &[u8],
        ) -> Result<CommitOid, FixtureError> {
            let destination = self.repo.path().join(path);
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            std::fs::write(destination, bytes).map_err(|error| error.to_string())?;
            git(self.repo.path(), &["add", "--", path])?;
            git(
                self.repo.path(),
                &["commit", "-q", "-m", "fixture file update"],
            )?;
            archive::pin_commit(self.repo.path(), "HEAD").map_err(|error| error.to_string())
        }

        pub(crate) fn capture_operation(
            &mut self,
            operation_id: &str,
            base: &CommitOid,
            result: &CommitOid,
        ) -> Result<CaptureReceipt, FixtureError> {
            let operation_id =
                OperationId::parse(operation_id).map_err(|error| error.to_string())?;
            let request = CaptureRequest {
                operation_id,
                repository: self.repo.path().to_path_buf(),
                base: base.clone(),
                result: result.clone(),
                policy: CapturePolicy::Required,
                retention: RetentionBoundary::UntilReleased,
            };
            match capture::capture(&mut self.store, &request).map_err(|error| error.to_string())? {
                CaptureOutcome::Acknowledged(receipt) => Ok(receipt),
                CaptureOutcome::Incomplete { code, detail, .. } => {
                    Err(format!("capture was incomplete ({code}): {detail}"))
                }
            }
        }

        pub(crate) fn clone_and_bind(&self) -> Result<Self, FixtureError> {
            let repo = tempfile::tempdir().map_err(|error| error.to_string())?;
            let host = tempfile::tempdir().map_err(|error| error.to_string())?;
            git_clone(self.repo.path(), repo.path())?;
            let root = CollaborationRoot::under_host_state(host.path());
            let key = self.store.project().clone();
            let mut store =
                CollaborationStore::open(&root, &key, &[]).map_err(|error| error.to_string())?;
            capture_anchor(
                &mut store,
                repo.path(),
                &self.anchor_commit,
                "fixture-clone-anchor",
            )?;
            let descriptor_record_id = self.binding.descriptor_record_id().as_str().to_string();
            let bind_args = ["--confirm", descriptor_record_id.as_str()];
            bind_at_root(repo.path(), repo.path(), &bind_args, &root)
                .map_err(|failure| format!("clone project bind failed: {}", failure.message))?;
            let binding = identity_store::resolve_approved_project_binding(&store)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "clone bind did not persist an approved binding".to_string())?;
            let bootstrap_commit =
                archive::pin_commit(repo.path(), "HEAD").map_err(|error| error.to_string())?;
            let bootstrap_snapshot = archive::snapshot_of_commit(repo.path(), &bootstrap_commit)
                .map_err(|error| error.to_string())?
                .id();
            Ok(Self {
                repo,
                host,
                store,
                binding,
                anchor_commit: self.anchor_commit.clone(),
                bootstrap_commit,
                bootstrap_snapshot,
            })
        }
    }

    fn capture_anchor(
        store: &mut CollaborationStore,
        repo: &Path,
        anchor: &CommitOid,
        operation_id: &str,
    ) -> Result<(), FixtureError> {
        let request = CaptureRequest {
            operation_id: OperationId::parse(operation_id).map_err(|error| error.to_string())?,
            repository: PathBuf::from(repo),
            base: anchor.clone(),
            result: anchor.clone(),
            policy: CapturePolicy::Required,
            retention: RetentionBoundary::UntilReleased,
        };
        match capture::capture(store, &request).map_err(|error| error.to_string())? {
            CaptureOutcome::Acknowledged(_) => Ok(()),
            CaptureOutcome::Incomplete { code, detail, .. } => {
                Err(format!("anchor capture was incomplete ({code}): {detail}"))
            }
        }
    }

    fn git(repo: &Path, args: &[&str]) -> Result<String, FixtureError> {
        let output = Command::new("git")
            .args([
                "-c",
                "user.name=Aethyme Fixture",
                "-c",
                "user.email=fixture@aethyme.invalid",
            ])
            .args(args)
            .current_dir(repo)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(format!(
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn git_clone(source: &Path, destination: &Path) -> Result<(), FixtureError> {
        let output = Command::new("git")
            .args([
                "-c",
                "protocol.file.allow=always",
                "clone",
                "--quiet",
                "--no-hardlinks",
            ])
            .arg(source)
            .arg(destination)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(format!(
                "git clone failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod binding_tests {
    use super::test_support::{BoundProjectFixture, PendingProjectFixture};
    use super::{PROJECT_DESCRIPTOR_PATH, bind_at_root, enroll};
    use crate::collaboration_identity_store;
    use crate::collaboration_state::CollaborationRoot;
    use std::process::Command;

    fn git(repo: &std::path::Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args([
                "-c",
                "user.name=Aethyme Fixture",
                "-c",
                "user.email=fixture@aethyme.invalid",
            ])
            .args(args)
            .current_dir(repo)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn binding_count(store: &crate::collaboration_state::CollaborationStore) -> i64 {
        store
            .read_connection()
            .query_row("SELECT count(*) FROM local_project_bindings", [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    #[test]
    fn enrollment_refuses_over_profile_file_count_before_writing_descriptor() {
        use aethyme_contracts::experimental_v0::project_identity::MAX_DESCRIPTOR_FILES;

        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".aethyme")).unwrap();
        git(repo.path(), &["init", "-q", "-b", "main"]);
        for index in 0..=MAX_DESCRIPTOR_FILES {
            std::fs::write(
                repo.path().join(format!("file-{index:04}.txt")),
                b"small source file\n",
            )
            .unwrap();
        }
        git(repo.path(), &["add", "--all"]);
        git(
            repo.path(),
            &["commit", "-q", "-m", "oversized descriptor fixture"],
        );
        let commit = git(repo.path(), &["rev-parse", "HEAD"]);
        let error = enroll(repo.path(), &["--commit", &commit, "--write"]).unwrap_err();

        assert_eq!(error.code, "descriptor_too_many_files");
        assert!(
            !repo.path().join(PROJECT_DESCRIPTOR_PATH).exists(),
            "refused enrollment must not write the proposed descriptor"
        );
    }

    #[test]
    fn malformed_confirmation_is_usage_and_never_creates_approval() {
        let fixture = PendingProjectFixture::new().unwrap();
        fixture.commit_pointer(PROJECT_DESCRIPTOR_PATH).unwrap();
        let error = fixture.bind("not-a-record-id").unwrap_err();
        assert_eq!(error.code, "usage");
        assert_eq!(error.exit, crate::exit_status::USAGE);
        assert_eq!(binding_count(&fixture.store), 0);
        assert!(
            collaboration_identity_store::resolve_approved_project_binding(&fixture.store)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn mismatched_descriptor_confirmation_is_refused_without_approval() {
        let fixture = PendingProjectFixture::new().unwrap();
        fixture.commit_pointer(PROJECT_DESCRIPTOR_PATH).unwrap();
        let wrong = format!("sha256:{}", "0".repeat(64));
        let error = fixture.bind(&wrong).unwrap_err();
        assert_eq!(error.code, "descriptor_confirmation_mismatch");
        assert_eq!(binding_count(&fixture.store), 0);
    }

    #[test]
    fn an_uncommitted_descriptor_pointer_does_not_authorize_binding() {
        let fixture = PendingProjectFixture::new().unwrap();
        fixture
            .write_working_pointer(PROJECT_DESCRIPTOR_PATH)
            .unwrap();
        let error = fixture.bind(&fixture.descriptor_record_id).unwrap_err();
        assert_eq!(error.code, "descriptor_pointer_missing");
        assert_eq!(binding_count(&fixture.store), 0);
    }

    #[test]
    fn a_committed_tampered_descriptor_pointer_is_refused() {
        let fixture = PendingProjectFixture::new().unwrap();
        fixture
            .commit_pointer(".aethyme/other-project.json")
            .unwrap();
        let error = fixture.bind(&fixture.descriptor_record_id).unwrap_err();
        assert_eq!(error.code, "descriptor_pointer_mismatch");
        assert_eq!(binding_count(&fixture.store), 0);
    }

    #[test]
    fn exact_binding_confirmation_is_idempotent() {
        let fixture = BoundProjectFixture::new().unwrap();
        let record_id = fixture.binding.descriptor_record_id().as_str().to_string();
        let root = CollaborationRoot::under_host_state(fixture.host.path());
        let output = bind_at_root(
            fixture.repo.path(),
            fixture.repo.path(),
            &["--confirm", &record_id],
            &root,
        )
        .unwrap();
        assert_eq!(output.0["status"], "already_bound");
        assert_eq!(binding_count(&fixture.store), 1);
    }

    #[test]
    fn approved_binding_survives_pointer_removal_and_conflicting_rebind_refuses() {
        let fixture = BoundProjectFixture::new().unwrap();
        let original_id = fixture.binding.descriptor_record_id().as_str().to_string();
        let original_project = fixture.binding.project_id().as_str().to_string();
        std::fs::write(
            fixture.repo.path().join(".aethyme/config.toml"),
            format!(
                "[collaboration]\ncapture = \"required\"\nproject = \"{}\"\n",
                fixture.store.project().as_str()
            ),
        )
        .unwrap();
        git(fixture.repo.path(), &["add", "-A"]);
        git(
            fixture.repo.path(),
            &["commit", "-q", "-m", "remove descriptor pointer"],
        );
        let resolved =
            collaboration_identity_store::resolve_approved_project_binding(&fixture.store)
                .unwrap()
                .expect("approval is anchored to the committed binding, not the current pointer");
        assert_eq!(resolved.descriptor_record_id().as_str(), original_id);
        let removed_pointer = bind_at_root(
            fixture.repo.path(),
            fixture.repo.path(),
            &["--confirm", &original_id],
            &CollaborationRoot::under_host_state(fixture.host.path()),
        )
        .unwrap_err();
        assert_eq!(removed_pointer.code, "descriptor_pointer_missing");

        std::fs::remove_file(fixture.repo.path().join(PROJECT_DESCRIPTOR_PATH)).unwrap();
        std::fs::write(
            fixture.repo.path().join("replacement.txt"),
            b"new descriptor anchor\n",
        )
        .unwrap();
        git(fixture.repo.path(), &["add", "-A"]);
        git(
            fixture.repo.path(),
            &[
                "commit",
                "-q",
                "-m",
                "prepare replacement descriptor anchor",
            ],
        );
        let replacement_anchor = git(fixture.repo.path(), &["rev-parse", "HEAD"]);
        let enrolled = enroll(
            fixture.repo.path(),
            &["--commit", &replacement_anchor, "--write"],
        )
        .unwrap();
        let replacement_id = enrolled.0["descriptor_record_id"].as_str().unwrap();
        assert_ne!(replacement_id, original_id);
        std::fs::write(
            fixture.repo.path().join(".aethyme/config.toml"),
            format!(
                "[collaboration]\ncapture = \"required\"\nproject = \"{}\"\ndescriptor = \"{}\"\n",
                fixture.store.project().as_str(),
                PROJECT_DESCRIPTOR_PATH
            ),
        )
        .unwrap();
        git(fixture.repo.path(), &["add", "-A"]);
        git(
            fixture.repo.path(),
            &["commit", "-q", "-m", "attempt conflicting descriptor"],
        );
        let error = bind_at_root(
            fixture.repo.path(),
            fixture.repo.path(),
            &["--confirm", replacement_id],
            &CollaborationRoot::under_host_state(fixture.host.path()),
        )
        .unwrap_err();
        assert_eq!(error.code, "project_binding_conflict");
        let still_approved =
            collaboration_identity_store::resolve_approved_project_binding(&fixture.store)
                .unwrap()
                .unwrap();
        assert_eq!(still_approved.project_id().as_str(), original_project);
        assert_eq!(still_approved.descriptor_record_id().as_str(), original_id);
    }

    #[test]
    fn a_corrupt_bootstrap_transition_is_a_typed_binding_refusal() {
        let mut fixture = BoundProjectFixture::new().unwrap();
        fixture
            .store
            .connection()
            .execute(
                "UPDATE local_project_bindings SET bootstrap_transition_sha256=?1",
                ["0".repeat(64)],
            )
            .unwrap();
        let record_id = fixture.binding.descriptor_record_id().as_str().to_string();
        let error = bind_at_root(
            fixture.repo.path(),
            fixture.repo.path(),
            &["--confirm", &record_id],
            &CollaborationRoot::under_host_state(fixture.host.path()),
        )
        .unwrap_err();
        assert_eq!(error.code, "corrupt_state");
        assert_eq!(error.exit, crate::exit_status::REFUSED);
    }
}
