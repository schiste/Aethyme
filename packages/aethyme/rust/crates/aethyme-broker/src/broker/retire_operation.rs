//! The archive of an abandoned in-progress Git operation.
//!
//! A merge, rebase, cherry-pick, revert or bisect left in progress keeps its
//! state in the checkout's registration (`MERGE_HEAD`, `rebase-merge/`, the
//! index with its conflict stages), which neither Git history nor a recovery
//! archive's patches can carry: a patch of an unmerged index does not apply.
//! In a closed session whose operation state is older than
//! `[cleanup] abandoned_operation_after_hours`, the retirement lane captures
//! that state byte for byte instead, so the checkout can be retired without
//! losing anything:
//!
//! - `git-state/`: the operation files and directories of the checkout's Git
//!   directory, the raw `index` and the `HEAD` reflog;
//! - `worktree/`: every path `git status` reports changed, unmerged or
//!   untracked, as it is on disk (a link as the link); paths it reports that
//!   are missing on disk are listed as `deleted`;
//! - `ignored/`: the protected ignored files a recovery archive would carry;
//! - `objects/`: a pack of every object the raw index (all stages) and
//!   `AUTO_MERGE` need that the checkout's HEAD does not already hold, so a
//!   staged-then-edited file or a conflict stage survives `git gc`;
//! - `manifest.json` (size and SHA-256 of every file, the packed object ids)
//!   and `README.md` (how to restore, and when the archive expires).
//!
//! Every copy is read back and compared, and the pack verified, before the
//! archive is renamed into place; the commits the state names (todo lists
//! included) are returned for the caller to pin. Immediately before the
//! checkout moves, [`sources_unchanged`] re-checks every archived source.
//! Copies are private (files 0600, directories 0700) under a 0700 root.

use super::*;
use std::io::{Read, Write};
use std::time::Instant;

/// The markers whose modification time dates an operation.
const OPERATION_MARKERS: [&str; 7] = [
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "BISECT_LOG",
    "rebase-merge",
    "rebase-apply",
    "sequencer",
];
/// Operation files of a checkout's Git directory the archive copies.
const OPERATION_STATE_FILES: [&str; 18] = [
    "HEAD",
    "index",
    "ORIG_HEAD",
    "MERGE_HEAD",
    "MERGE_MSG",
    "MERGE_MODE",
    "MERGE_RR",
    "AUTO_MERGE",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "BISECT_LOG",
    "BISECT_START",
    "BISECT_TERMS",
    "BISECT_EXPECTED_REV",
    "BISECT_ANCESTORS_OK",
    "BISECT_NAMES",
    "BISECT_RUN",
    "logs/HEAD",
];
/// Operation directories of a checkout's Git directory the archive copies.
const OPERATION_STATE_DIRECTORIES: [&str; 3] = ["rebase-merge", "rebase-apply", "sequencer"];
/// Files in the state that may name commits the archive pins.
const COMMIT_NAMING_FILES: [&str; 10] = [
    "ORIG_HEAD",
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "rebase-merge/orig-head",
    "rebase-merge/onto",
    "rebase-merge/stopped-sha",
    "rebase-apply/orig-head",
    "rebase-apply/onto",
    "sequencer/head",
];
/// Todo and done lists whose commits the archive pins.
const TODO_FILES: [&str; 5] = [
    "rebase-merge/git-rebase-todo",
    "rebase-merge/done",
    "rebase-apply/patch",
    "sequencer/todo",
    "sequencer/done",
];
/// Abbreviated ids read from todo lists, at most.
const MAX_TODO_TOKENS: usize = 4_096;
/// A partial archive whose creator is gone is swept after this long.
const STALE_PARTIAL_AFTER_MS: i64 = 24 * 3_600_000;
/// Name prefix of a partial operation archive: never read as an archive.
pub(crate) const PARTIAL_PREFIX: &str = ".partial-operation-";
/// Files at most, across the archive.
const MAX_OPERATION_ARCHIVE_FILES: usize = 50_000;
/// Directory depth at most, inside a copied state directory.
const MAX_OPERATION_STATE_DEPTH: usize = 32;

/// How long ago anyone last touched the checkout's in-progress operation
/// (ms): the newest modification among the operation markers of its Git
/// directory, its index, its `logs/HEAD`, and every changed, unmerged or
/// untracked working file. Someone resolving conflicts slowly writes only
/// working files, so those count too. `None` when any of it cannot be read.
pub(super) fn operation_state_age_ms(worktree: &Path) -> Option<i64> {
    let git_dir = crate::git::checked_worktree_git_dir(worktree)
        .ok()
        .flatten()?;
    let mut candidates = OPERATION_MARKERS
        .iter()
        .map(|name| git_dir.join(name))
        .chain([git_dir.join("index"), git_dir.join("logs/HEAD")])
        .collect::<Vec<_>>();
    candidates.extend(
        changed_paths(worktree)
            .ok()?
            .iter()
            .map(|relative| worktree.join(relative)),
    );
    let mut newest = None;
    for path in candidates {
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                let modified = metadata.modified().ok()?;
                newest =
                    Some(newest.map_or(modified, |seen: std::time::SystemTime| seen.max(modified)));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
    }
    let age = std::time::SystemTime::now()
        .duration_since(newest?)
        .unwrap_or_default();
    i64::try_from(age.as_millis()).ok()
}

/// What a source looked like when it was copied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SourceStamp {
    len: u64,
    mtime: i64,
    mtime_nsec: i64,
    inode: u64,
}

impl SourceStamp {
    fn of(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            len: metadata.len(),
            mtime: metadata.mtime(),
            mtime_nsec: metadata.mtime_nsec(),
            inode: metadata.ino(),
        }
    }
}

/// What to archive, and the bounds.
pub(super) struct OperationArchiveRequest<'a> {
    pub session_id: i64,
    pub branch: &'a str,
    pub task: Option<&'a str>,
    pub worktree: &'a Path,
    pub archive_root: &'a Path,
    pub policy: &'a crate::AutoCleanupPolicy,
    pub max_bytes: u64,
    pub deadline: Instant,
}

/// A written and verified operation archive.
#[derive(Debug)]
pub(super) struct OperationArchive {
    pub path: PathBuf,
    pub bytes: u64,
    /// Commits the state names (HEAD and todo lists included), to pin
    /// before the move.
    pub commits: Vec<String>,
    /// Every archived source and how it looked when copied.
    stamps: Vec<(PathBuf, SourceStamp)>,
    /// The paths `git status` reported, and those missing on disk.
    changed: Vec<String>,
    deleted: Vec<String>,
    /// SHA-256 of the raw index when it was copied.
    index_sha256: Option<String>,
}

/// Whether every source the archive copied is still exactly as it was (size,
/// modification time, inode; the index by content), `git status` reports the
/// same paths, and the paths that were missing still are. Any difference
/// means someone worked in the checkout after the copy: it must not move.
pub(super) fn sources_unchanged(archive: &OperationArchive, worktree: &Path) -> Result<(), String> {
    let changed = changed_paths(worktree)?;
    if changed != archive.changed {
        return Err(
            "the checkout's changed or untracked files changed after the archive was written"
                .into(),
        );
    }
    for (source, stamp) in &archive.stamps {
        let current = std::fs::symlink_metadata(source).map_err(|error| {
            format!(
                "{} changed after it was archived: {error}",
                source.display()
            )
        })?;
        if SourceStamp::of(&current) != *stamp {
            return Err(format!(
                "{} changed after it was archived",
                source.display()
            ));
        }
    }
    for relative in &archive.deleted {
        if std::fs::symlink_metadata(worktree.join(relative)).is_ok() {
            return Err(format!(
                "{relative} reappeared after the archive was written"
            ));
        }
    }
    if let Some(expected) = archive.index_sha256.as_deref() {
        let git_dir = crate::git::checked_worktree_git_dir(worktree)
            .map_err(|error| format!("cannot find the Git directory: {error}"))?
            .ok_or("the checkout has no linked Git directory")?;
        let (current, _) = hash_entry(&git_dir.join("index"), false)
            .map_err(|error| format!("cannot re-read the index: {error}"))?;
        if current != expected {
            return Err("the index changed after the archive was written".into());
        }
    }
    Ok(())
}

/// Remove partial operation archives under `archive_root` older than a day
/// whose creating process (pid and start time, from the name) is gone.
/// Returns how many were removed.
pub(super) fn sweep_stale_partials(archive_root: &Path) -> usize {
    let Ok(listing) = std::fs::read_dir(archive_root) else {
        return 0;
    };
    let now = crate::clock::epoch_ms();
    let stale = listing
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter_map(|name| {
            let fields = name
                .strip_prefix(PARTIAL_PREFIX)?
                .split('-')
                .collect::<Vec<_>>();
            // <session>-<created ms>-<pid>-<started ms>-<nonce>
            let [_, created, pid, started, _] = fields[..] else {
                return None;
            };
            let created = created.parse::<i64>().ok()?;
            let pid = pid.parse::<i64>().ok()?;
            let started = started.parse::<i64>().ok()?;
            (now.saturating_sub(created) > STALE_PARTIAL_AFTER_MS && pid > 0 && started > 0)
                .then_some((name.clone(), pid, started))
        })
        .collect::<Vec<_>>();
    if stale.is_empty() {
        return 0;
    }
    let Some(table) = crate::session_holder::ProcessTable::snapshot() else {
        return 0;
    };
    let mut removed = 0;
    for (name, pid, started) in stale {
        let gone = table.process(pid).is_none_or(|process| {
            crate::session_holder::lstart_epoch_ms(&process.started) != Some(started)
        });
        let path = archive_root.join(&name);
        let real_directory = std::fs::symlink_metadata(&path)
            .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink());
        if gone && real_directory && std::fs::remove_dir_all(&path).is_ok() {
            removed += 1;
            crate::phase_telemetry::add(
                "checkout_retirement_operation_partial_swept",
                std::time::Duration::ZERO,
            );
        }
    }
    removed
}

/// The archive root, created if needed and private (0700): a root open to
/// others is tightened, and refused if it cannot be.
fn private_archive_root(root: &Path) -> Result<PathBuf, String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(root)
        .map_err(|error| format!("cannot create the archive root: {error}"))?;
    let metadata = std::fs::symlink_metadata(root)
        .map_err(|error| format!("cannot inspect the archive root: {error}"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("the archive root is not a real directory".into());
    }
    if metadata.permissions().mode() & 0o777 != 0o700 {
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot make the archive root private: {error}"))?;
        let mode = std::fs::symlink_metadata(root)
            .map_err(|error| format!("cannot inspect the archive root: {error}"))?
            .permissions()
            .mode();
        if mode & 0o777 != 0o700 {
            return Err(format!(
                "the archive root has mode {:o}, not 0700",
                mode & 0o777
            ));
        }
    }
    Ok(root.to_path_buf())
}

/// This process's id and start time (epoch ms; 0 when unknown).
fn this_process_identity() -> (i64, i64) {
    let pid = i64::from(std::process::id());
    let started = crate::session_holder::ProcessTable::snapshot()
        .and_then(|table| table.process(pid))
        .and_then(|process| crate::session_holder::lstart_epoch_ms(&process.started))
        .unwrap_or(0);
    (pid, started)
}

/// Why no archive was written; nothing is moved either way.
#[derive(Debug)]
pub(super) enum OperationArchiveError {
    /// More than the cap; the checkout is kept and shown by session id.
    TooLarge(String),
    /// Anything else, in the refusing step's words.
    Refused(String),
}

/// One archived file, as the manifest records it.
#[derive(Debug, Clone, serde::Serialize)]
struct ArchivedEntry {
    path: String,
    bytes: u64,
    sha256: String,
    symlink: bool,
    /// The source's permission bits (the copy itself is 0600).
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<u32>,
}

/// The packed objects, as the manifest records them.
#[derive(Debug, Clone, Default, serde::Serialize)]
struct PackedObjects {
    ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pack: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pack_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    index_sha256: Option<String>,
}

/// Write, read back and rename into place the archive `request` describes.
/// A failure removes the partial copy and leaves the checkout untouched.
pub(super) fn write_operation_archive(
    request: &OperationArchiveRequest<'_>,
) -> Result<OperationArchive, OperationArchiveError> {
    let refuse = OperationArchiveError::Refused;
    let git_dir = crate::git::checked_worktree_git_dir(request.worktree)
        .map_err(|error| refuse(format!("cannot find the checkout's Git directory: {error}")))?
        .ok_or_else(|| refuse("the checkout has no linked Git directory".into()))?;
    let head = git_text(request.worktree, &["rev-parse", "--verify", "HEAD"])
        .map_err(refuse)?
        .trim()
        .to_string();
    if !is_object_id(&head) {
        return Err(refuse(format!("unexpected HEAD {head:?}")));
    }
    let changed = changed_paths(request.worktree).map_err(refuse)?;
    let checkout = GitRepo::discover(request.worktree)
        .map_err(|error| refuse(format!("cannot open the checkout: {error}")))?;
    let ignored = crate::cleanup_resolve::preserved_ignored_files(
        &checkout,
        request.worktree,
        request.policy,
        request.session_id,
        Some(request.deadline),
    )
    .map_err(|error| refuse(error.to_string()))?;
    let mut commits = named_commits(request.worktree, &git_dir, &head).map_err(refuse)?;
    for commit in todo_commits(request.worktree, &git_dir) {
        if !commits.contains(&commit) {
            commits.push(commit);
        }
    }
    commits.sort();
    let objects = index_only_objects(request.worktree, &git_dir, &head).map_err(refuse)?;

    let archive_root = private_archive_root(request.archive_root).map_err(refuse)?;
    sweep_stale_partials(&archive_root);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let name = format!("{}-operation-{}-{nonce:x}", request.session_id, &head[..12]);
    let (pid, started) = this_process_identity();
    // A partial is named so nothing reads it as an archive, and so a sweep
    // can tell when its creator is gone.
    let partial = archive_root.join(format!(
        "{PARTIAL_PREFIX}{}-{}-{pid}-{started}-{nonce:x}",
        request.session_id,
        crate::clock::epoch_ms()
    ));
    std::fs::create_dir(&partial)
        .map_err(|error| refuse(format!("cannot create {}: {error}", partial.display())))?;
    let id = format!(
        "{}/{name}",
        archive_root
            .file_name()
            .map(|pool| pool.to_string_lossy().into_owned())
            .unwrap_or_default()
    );
    let mut budget = Budget {
        bytes: 0,
        files: 0,
        max_bytes: request.max_bytes,
        deadline: request.deadline,
        stamps: Vec::new(),
    };
    let written = fill_archive(
        request,
        &git_dir,
        &head,
        &changed,
        &ignored,
        &commits,
        &objects,
        &id,
        &partial,
        &mut budget,
    );
    let (bytes, deleted, index_sha256) = match written {
        Ok(written) => written,
        Err(error) => {
            remove_partial(&partial);
            return Err(error);
        }
    };
    let archive = archive_root.join(&name);
    if let Err(error) = std::fs::rename(&partial, &archive) {
        remove_partial(&partial);
        return Err(refuse(format!(
            "cannot move the verified archive into place: {error}"
        )));
    }
    // The archive is in place; a root that cannot be synced is only
    // counted (the archive's own files and directories were synced).
    if std::fs::File::open(&archive_root)
        .and_then(|root| root.sync_all())
        .is_err()
    {
        crate::phase_telemetry::add(
            "checkout_retirement_operation_archive_root_unsynced",
            std::time::Duration::ZERO,
        );
    }
    crate::phase_telemetry::add(
        "checkout_retirement_operation_archive_files",
        std::time::Duration::ZERO,
    );
    Ok(OperationArchive {
        path: archive,
        bytes,
        commits,
        stamps: budget.stamps,
        changed,
        deleted,
        index_sha256,
    })
}

/// Copy everything into `partial`, pack and verify the index-only objects,
/// verify every copy, and write the README and, last, the manifest. Returns
/// the payload bytes, the paths missing on disk and the index digest.
#[allow(clippy::too_many_arguments)]
fn fill_archive(
    request: &OperationArchiveRequest<'_>,
    git_dir: &Path,
    head: &str,
    changed: &[String],
    ignored: &[crate::cleanup_resolve::ArchivedFile],
    commits: &[String],
    objects: &[String],
    id: &str,
    partial: &Path,
    budget: &mut Budget,
) -> Result<(u64, Vec<String>, Option<String>), OperationArchiveError> {
    let refuse = OperationArchiveError::Refused;
    let mut state = Vec::new();
    for name in OPERATION_STATE_FILES {
        let source = git_dir.join(name);
        match std::fs::symlink_metadata(&source) {
            Ok(metadata) if metadata.is_file() => {
                state.push(copy_entry(
                    &source,
                    &partial.join("git-state"),
                    name,
                    budget,
                )?);
            }
            Ok(_) => return Err(refuse(format!("{name} in the Git directory is not a file"))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(refuse(format!("cannot inspect {name}: {error}"))),
        }
    }
    for name in OPERATION_STATE_DIRECTORIES {
        let source = git_dir.join(name);
        match std::fs::symlink_metadata(&source) {
            Ok(metadata) if metadata.is_dir() => {
                copy_tree(
                    git_dir,
                    name,
                    &partial.join("git-state"),
                    &mut state,
                    budget,
                )?;
            }
            Ok(_) => {
                return Err(refuse(format!(
                    "{name} in the Git directory is not a directory"
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(refuse(format!("cannot inspect {name}: {error}"))),
        }
    }
    let mut worktree_files = Vec::new();
    let mut deleted = Vec::new();
    for relative in changed {
        let source = request.worktree.join(relative);
        match std::fs::symlink_metadata(&source) {
            Ok(metadata) if metadata.is_file() || metadata.file_type().is_symlink() => {
                worktree_files.push(copy_entry(
                    &source,
                    &partial.join("worktree"),
                    relative,
                    budget,
                )?);
            }
            Ok(_) => {
                return Err(refuse(format!(
                    "{relative} is neither a regular file nor a symbolic link (a nested repository or special file), which the archive cannot preserve"
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                deleted.push(relative.clone());
            }
            Err(error) => return Err(refuse(format!("cannot inspect {relative}: {error}"))),
        }
    }
    let mut ignored_files = Vec::new();
    for file in ignored {
        let copied = copy_entry(
            &request.worktree.join(&file.path),
            &partial.join("ignored"),
            &file.path,
            budget,
        )?;
        if copied.sha256 != file.sha256 {
            return Err(refuse(format!(
                "ignored {} changed while it was archived",
                file.path
            )));
        }
        ignored_files.push(copied);
    }
    let packed = pack_objects(request.worktree, objects, &partial.join("objects"), budget)?;
    let index_sha256 = state
        .iter()
        .find(|entry| entry.path == "index")
        .map(|entry| entry.sha256.clone());
    let manifest = serde_json::json!({
        "schema": "aethyme-operation-archive-v1",
        "created_at_ms": crate::clock::epoch_ms(),
        "aethyme_version": env!("CARGO_PKG_VERSION"),
        "session_id": request.session_id,
        "task": request.task,
        "worktree_path": request.worktree.to_string_lossy(),
        "branch": request.branch,
        "head": head,
        "pinned_commits": commits,
        "git_state": state,
        "worktree": worktree_files,
        "deleted": deleted,
        "ignored": ignored_files,
        "objects": packed,
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| refuse(format!("cannot write the manifest: {error}")))?;
    write_synced(
        &partial.join("README.md"),
        readme(request, head, id).as_bytes(),
    )
    .map_err(refuse)?;
    // Read everything back against what was recorded.
    for (directory, entries) in [
        ("git-state", &state),
        ("worktree", &worktree_files),
        ("ignored", &ignored_files),
    ] {
        for entry in entries {
            budget.check_deadline()?;
            let copy = partial.join(directory).join(&entry.path);
            let (sha256, bytes) = hash_entry(&copy, entry.symlink)
                .map_err(|error| refuse(format!("cannot read back {}: {error}", copy.display())))?;
            if sha256 != entry.sha256 || bytes != entry.bytes {
                return Err(refuse(format!(
                    "{directory}/{} did not copy intact",
                    entry.path
                )));
            }
        }
    }
    restrict_tree(partial).map_err(refuse)?;
    for directory in ["git-state", "worktree", "ignored", "objects"] {
        let path = partial.join(directory);
        if path.is_dir() {
            sync_directory(&path).map_err(refuse)?;
        }
    }
    // The manifest goes last: a directory without one is never an archive.
    write_synced(&partial.join("manifest.json"), &manifest_bytes).map_err(refuse)?;
    restrict_tree(partial).map_err(refuse)?;
    sync_directory(partial).map_err(refuse)?;
    Ok((budget.bytes, deleted, index_sha256))
}

/// Make every directory under `root` (itself included) 0700 and every
/// regular file 0600; links are left as they are.
fn restrict_tree(root: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("cannot restrict {}: {error}", directory.display()))?;
        let listing = std::fs::read_dir(&directory)
            .map_err(|error| format!("cannot read {}: {error}", directory.display()))?;
        for entry in listing {
            let entry =
                entry.map_err(|error| format!("cannot read {}: {error}", directory.display()))?;
            let file_type = entry
                .file_type()
                .map_err(|error| format!("cannot inspect {}: {error}", entry.path().display()))?;
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file() {
                std::fs::set_permissions(entry.path(), std::fs::Permissions::from_mode(0o600))
                    .map_err(|error| {
                        format!("cannot restrict {}: {error}", entry.path().display())
                    })?;
            }
        }
    }
    Ok(())
}

/// Every object the raw index (all stages, gitlinks aside) and `AUTO_MERGE`
/// need that the tree of `head` does not already hold. HEAD itself is pinned,
/// so its objects outlive the checkout; these are kept only by the index.
fn index_only_objects(worktree: &Path, git_dir: &Path, head: &str) -> Result<Vec<String>, String> {
    let held = git_bytes(worktree, &["ls-tree", "-r", "-z", head])?
        .split(|byte| *byte == 0)
        .filter_map(|record| {
            let text = std::str::from_utf8(record).ok()?;
            let (meta, _) = text.split_once('\t')?;
            meta.split_whitespace().nth(2).map(str::to_string)
        })
        .collect::<std::collections::BTreeSet<_>>();
    let mut objects = std::collections::BTreeSet::new();
    for record in git_bytes(worktree, &["ls-files", "--stage", "-z"])?.split(|byte| *byte == 0) {
        let Ok(text) = std::str::from_utf8(record) else {
            return Err("the index holds a non-UTF-8 entry".into());
        };
        let Some((meta, _)) = text.split_once('\t') else {
            continue;
        };
        let mut fields = meta.split_whitespace();
        let (Some(mode), Some(id)) = (fields.next(), fields.next()) else {
            return Err(format!("unexpected index entry {meta:?}"));
        };
        if mode == "160000" || held.contains(id) {
            continue;
        }
        if !is_object_id(id) {
            return Err(format!("unexpected index object id {id:?}"));
        }
        objects.insert(id.to_string());
    }
    if let Ok(text) = std::fs::read_to_string(git_dir.join("AUTO_MERGE")) {
        let tree = text.trim();
        if is_object_id(tree) {
            for line in String::from_utf8_lossy(&git_bytes(
                worktree,
                &["rev-list", "--objects", tree, "--not", head],
            )?)
            .lines()
            {
                if let Some(id) = line.split_whitespace().next()
                    && is_object_id(id)
                {
                    objects.insert(id.to_string());
                }
            }
        }
    }
    Ok(objects.into_iter().collect())
}

/// Write `objects` into a pack under `directory`, then prove it: every id
/// exists before packing, `git verify-pack` accepts the pack, and its index
/// lists every id. Any missing object or failed check refuses.
fn pack_objects(
    worktree: &Path,
    objects: &[String],
    directory: &Path,
    budget: &mut Budget,
) -> Result<PackedObjects, OperationArchiveError> {
    let refuse = OperationArchiveError::Refused;
    if objects.is_empty() {
        return Ok(PackedObjects::default());
    }
    budget.check_deadline()?;
    std::fs::create_dir_all(directory)
        .map_err(|error| refuse(format!("cannot create {}: {error}", directory.display())))?;
    let list = directory.join("ids.txt");
    let mut text = objects.join("\n");
    text.push('\n');
    write_synced(&list, text.as_bytes()).map_err(refuse)?;
    let checked =
        git_bytes_with_input(worktree, &["cat-file", "--batch-check"], &list).map_err(refuse)?;
    let checked = String::from_utf8_lossy(&checked);
    let present = checked
        .lines()
        .filter(|line| !line.ends_with(" missing"))
        .count();
    if present != objects.len() || checked.lines().count() != objects.len() {
        return Err(refuse(format!(
            "{} of the {} objects the index needs are missing; the checkout is kept",
            objects.len().saturating_sub(present),
            objects.len()
        )));
    }
    let base = directory.join("pack");
    let base_text = base.to_string_lossy().into_owned();
    let hash = git_bytes_with_input(worktree, &["pack-objects", "-q", &base_text], &list)
        .map_err(refuse)?;
    let hash = String::from_utf8_lossy(&hash).trim().to_string();
    if !is_object_id(&hash) {
        return Err(refuse(format!("git pack-objects printed {hash:?}")));
    }
    let pack = directory.join(format!("pack-{hash}.pack"));
    let index = directory.join(format!("pack-{hash}.idx"));
    let verified = git_bytes(worktree, &["verify-pack", "-v", &index.to_string_lossy()])
        .map_err(|error| refuse(format!("the object pack did not verify: {error}")))?;
    let listed = String::from_utf8_lossy(&verified)
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|id| is_object_id(id))
        .map(str::to_string)
        .collect::<std::collections::BTreeSet<_>>();
    if let Some(missing) = objects.iter().find(|id| !listed.contains(*id)) {
        return Err(refuse(format!("the object pack lacks {missing}")));
    }
    let (pack_sha256, pack_bytes) = hash_entry(&pack, false)
        .map_err(|error| refuse(format!("cannot read back the object pack: {error}")))?;
    let (index_sha256, _) = hash_entry(&index, false)
        .map_err(|error| refuse(format!("cannot read back the pack index: {error}")))?;
    budget.take(pack_bytes)?;
    for path in [&pack, &index] {
        std::fs::File::open(path)
            .and_then(|file| file.sync_all())
            .map_err(|error| refuse(format!("cannot sync {}: {error}", path.display())))?;
    }
    Ok(PackedObjects {
        ids: objects.to_vec(),
        pack: Some(format!("objects/pack-{hash}.pack")),
        pack_sha256: Some(pack_sha256),
        index_sha256: Some(index_sha256),
    })
}

/// Commits the todo and done lists of a rebase or sequencer name, by their
/// (possibly abbreviated) ids, resolved to full commit ids.
fn todo_commits(worktree: &Path, git_dir: &Path) -> Vec<String> {
    let mut tokens = std::collections::BTreeSet::new();
    for name in TODO_FILES {
        let Ok(text) = std::fs::read_to_string(git_dir.join(name)) else {
            continue;
        };
        tokens.extend(
            text.split_whitespace()
                .filter(|token| {
                    (7..=64).contains(&token.len())
                        && token.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
                .map(str::to_string)
                .take(MAX_TODO_TOKENS),
        );
    }
    let mut commits = std::collections::BTreeSet::new();
    for token in tokens.into_iter().take(MAX_TODO_TOKENS) {
        if let Ok(full) = git_text(
            worktree,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{token}^{{commit}}"),
            ],
        ) {
            let full = full.trim();
            if is_object_id(full) {
                commits.insert(full.to_string());
            }
        }
    }
    commits.into_iter().collect()
}

/// Bytes and files so far, and the bounds.
struct Budget {
    bytes: u64,
    files: usize,
    max_bytes: u64,
    deadline: Instant,
    /// Every source copied and how it looked before it was read.
    stamps: Vec<(PathBuf, SourceStamp)>,
}

impl Budget {
    fn check_deadline(&self) -> Result<(), OperationArchiveError> {
        if Instant::now() >= self.deadline {
            return Err(OperationArchiveError::Refused(
                "the operation archive ran out of the lane's time; the partial copy was removed"
                    .into(),
            ));
        }
        Ok(())
    }

    fn take(&mut self, bytes: u64) -> Result<(), OperationArchiveError> {
        self.check_deadline()?;
        self.files += 1;
        self.bytes = self.bytes.saturating_add(bytes);
        if self.files > MAX_OPERATION_ARCHIVE_FILES {
            return Err(OperationArchiveError::TooLarge(format!(
                "the operation archive would hold more than {MAX_OPERATION_ARCHIVE_FILES} files"
            )));
        }
        if self.bytes > self.max_bytes {
            return Err(OperationArchiveError::TooLarge(format!(
                "the operation archive would exceed {} MiB",
                self.max_bytes / (1024 * 1024)
            )));
        }
        Ok(())
    }
}

/// Copy the file or link `source` to `<target_root>/<relative>`, hashing
/// what was read; the copy is synced.
fn copy_entry(
    source: &Path,
    target_root: &Path,
    relative: &str,
    budget: &mut Budget,
) -> Result<ArchivedEntry, OperationArchiveError> {
    let refuse = OperationArchiveError::Refused;
    if !is_contained(relative) {
        return Err(refuse(format!("unexpected archive path {relative:?}")));
    }
    let target = target_root.join(relative);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| refuse(format!("cannot create {}: {error}", parent.display())))?;
    }
    let metadata = std::fs::symlink_metadata(source)
        .map_err(|error| refuse(format!("cannot inspect {relative}: {error}")))?;
    budget
        .stamps
        .push((source.to_path_buf(), SourceStamp::of(&metadata)));
    if metadata.file_type().is_symlink() {
        budget.take(0)?;
        let link = std::fs::read_link(source)
            .map_err(|error| refuse(format!("cannot read link {relative}: {error}")))?;
        std::os::unix::fs::symlink(&link, &target)
            .map_err(|error| refuse(format!("cannot preserve link {relative}: {error}")))?;
        return Ok(ArchivedEntry {
            path: relative.to_string(),
            bytes: 0,
            sha256: link_digest(&link),
            symlink: true,
            mode: None,
        });
    }
    if !metadata.is_file() {
        return Err(refuse(format!(
            "{relative} is neither a regular file nor a symbolic link"
        )));
    }
    budget.take(metadata.len())?;
    let mut input = std::fs::File::open(source)
        .map_err(|error| refuse(format!("cannot open {relative}: {error}")))?;
    let mut output = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&target)
    }
    .map_err(|error| refuse(format!("cannot create the copy of {relative}: {error}")))?;
    let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut bytes = 0_u64;
    loop {
        let read = input
            .read(&mut buffer)
            .map_err(|error| refuse(format!("cannot read {relative}: {error}")))?;
        if read == 0 {
            break;
        }
        sha2::Digest::update(&mut hasher, &buffer[..read]);
        output
            .write_all(&buffer[..read])
            .map_err(|error| refuse(format!("cannot copy {relative}: {error}")))?;
        bytes = bytes.saturating_add(read as u64);
    }
    output
        .sync_all()
        .map_err(|error| refuse(format!("cannot sync the copy of {relative}: {error}")))?;
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o7777
    };
    Ok(ArchivedEntry {
        path: relative.to_string(),
        bytes,
        sha256: format!("{:x}", sha2::Digest::finalize(hasher)),
        symlink: false,
        mode: Some(mode),
    })
}

/// Copy the directory `<base>/<relative>` file by file, never following a
/// link.
fn copy_tree(
    base: &Path,
    relative: &str,
    target_root: &Path,
    entries: &mut Vec<ArchivedEntry>,
    budget: &mut Budget,
) -> Result<(), OperationArchiveError> {
    let refuse = OperationArchiveError::Refused;
    let mut pending = vec![(relative.to_string(), 0_usize)];
    while let Some((directory, depth)) = pending.pop() {
        budget.check_deadline()?;
        if depth > MAX_OPERATION_STATE_DEPTH {
            return Err(refuse(format!("{relative} is nested too deep to archive")));
        }
        let listing = std::fs::read_dir(base.join(&directory))
            .map_err(|error| refuse(format!("cannot read {directory}: {error}")))?;
        for entry in listing {
            let entry =
                entry.map_err(|error| refuse(format!("cannot read {directory}: {error}")))?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| refuse(format!("{directory} holds a non-UTF-8 name")))?;
            let child = format!("{directory}/{name}");
            let file_type = entry
                .file_type()
                .map_err(|error| refuse(format!("cannot inspect {child}: {error}")))?;
            if file_type.is_dir() {
                pending.push((child, depth + 1));
            } else if file_type.is_file() || file_type.is_symlink() {
                entries.push(copy_entry(&base.join(&child), target_root, &child, budget)?);
            } else {
                return Err(refuse(format!("{child} is a special file")));
            }
        }
    }
    Ok(())
}

/// SHA-256 and length of the copy at `path` (a link as its target).
fn hash_entry(path: &Path, symlink: bool) -> std::io::Result<(String, u64)> {
    if symlink {
        return Ok((link_digest(&std::fs::read_link(path)?), 0));
    }
    let mut file = std::fs::File::open(path)?;
    let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut bytes = 0_u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        sha2::Digest::update(&mut hasher, &buffer[..read]);
        bytes = bytes.saturating_add(read as u64);
    }
    Ok((format!("{:x}", sha2::Digest::finalize(hasher)), bytes))
}

/// The digest of a link, as recovery archives record it.
fn link_digest(target: &Path) -> String {
    let mut hashed = b"symlink\0".to_vec();
    hashed.extend_from_slice(target.as_os_str().as_encoded_bytes());
    format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(&hashed))
}

/// Sync the directory at `path` so its entries survive a crash.
fn sync_directory(path: &Path) -> Result<(), String> {
    std::fs::File::open(path)
        .and_then(|handle| handle.sync_all())
        .map_err(|error| format!("cannot sync {}: {error}", path.display()))
}

/// Remove a partial copy after a failure; one that cannot be removed is
/// counted (it holds copies only, and is named `.partial`).
fn remove_partial(partial: &Path) {
    if std::fs::remove_dir_all(partial).is_err() {
        crate::phase_telemetry::add(
            "checkout_retirement_operation_archive_partial_left",
            std::time::Duration::ZERO,
        );
    }
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

/// Every path `git status` reports changed, unmerged or untracked (each
/// untracked file, not its directory; renames as two paths).
fn changed_paths(worktree: &Path) -> Result<Vec<String>, String> {
    let listing = git_bytes(
        worktree,
        &[
            "--no-optional-locks",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
            "status",
            "--porcelain=v1",
            "-z",
            "--no-renames",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ],
    )?;
    let mut paths = std::collections::BTreeSet::new();
    for record in listing.split(|byte| *byte == 0) {
        if record.len() < 4 {
            continue;
        }
        let path = std::str::from_utf8(&record[3..])
            .map_err(|_| "the checkout holds a changed non-UTF-8 path".to_string())?;
        if !is_contained(path) {
            return Err(format!("Git reported an unexpected path {path:?}"));
        }
        paths.insert(path.to_string());
    }
    Ok(paths.into_iter().collect())
}

/// HEAD and every commit the operation state names that exists.
fn named_commits(worktree: &Path, git_dir: &Path, head: &str) -> Result<Vec<String>, String> {
    let mut candidates = std::collections::BTreeSet::from([head.to_string()]);
    for name in COMMIT_NAMING_FILES {
        let Ok(text) = std::fs::read_to_string(git_dir.join(name)) else {
            continue;
        };
        candidates.extend(
            text.split_whitespace()
                .filter(|token| is_object_id(token))
                .map(str::to_string),
        );
    }
    let mut commits = Vec::new();
    for candidate in candidates {
        if git_text(worktree, &["cat-file", "-t", &candidate])
            .is_ok_and(|kind| kind.trim() == "commit")
        {
            commits.push(candidate);
        }
    }
    Ok(commits)
}

fn readme(request: &OperationArchiveRequest<'_>, head: &str, id: &str) -> String {
    format!(
        "# Abandoned Git operation of session {session}\n\n\
         The checkout `{path}` (branch `{branch}`, HEAD `{head}`) was closed with a merge, \
         rebase, cherry-pick, revert or bisect still in progress. Its operation state, raw \
         index, changed and untracked files, protected ignored files and every object only \
         its index held were archived here, verified, and the checkout was retired. The \
         commits the state names are pinned under `refs/aethyme/retired/{session}/`.\n\n\
         ## Expiry\n\n\
         This archive (id `{id}`) expires by age like every recovery archive: by default 30 \
         days after it was written (`[budget] unlanded_archive_days` in the host state's \
         `storage.toml`), and only after `broker status` has named it for 7 days. Keep it \
         with `aethyme storage keep-archive {id}`.\n\n\
         ## Restore\n\n\
         ```sh\n\
         # 1. The objects only the index held, back into the repository:\n\
         cp <this-archive>/objects/pack-*.pack <this-archive>/objects/pack-*.idx \\\n\
            \"$(git rev-parse --git-common-dir)/objects/pack/\"\n\
         # 2. The checkout, at the archived HEAD:\n\
         git worktree add --detach {path:?} {head}\n\
         # 3. Its files, then its index and operation state:\n\
         cp -pR <this-archive>/worktree/. {path:?}/\n\
         cp -pR <this-archive>/ignored/. {path:?}/\n\
         cp -pR <this-archive>/git-state/. \"$(git -C {path:?} rev-parse --absolute-git-dir)\"/\n\
         ```\n\n\
         Copies are private (0600); `manifest.json` records each file's original mode, size \
         and SHA-256, the packed object ids, and the paths that were missing (`deleted`). \
         `git-state/HEAD` names the branch the checkout was on.\n",
        session = request.session_id,
        path = request.worktree.display().to_string(),
        branch = request.branch,
    )
}

fn is_object_id(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// A relative path that stays inside the directory it is joined to.
fn is_contained(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\0')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

fn git_text(dir: &Path, args: &[&str]) -> Result<String, String> {
    git_bytes(dir, args).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

/// Run Git in `dir`, bounded by the general Git limit, and return stdout.
fn git_bytes(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let mut command = crate::git::git_command();
    command.args(args).current_dir(dir);
    match crate::bounded_output::output_limited(
        &mut command,
        crate::git::git_timeout(),
        64 * 1024 * 1024,
    ) {
        Ok(crate::bounded_output::LimitedOutput::Completed(output)) if output.status.success() => {
            Ok(output.stdout)
        }
        Ok(crate::bounded_output::LimitedOutput::Completed(output)) => Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Ok(_) => Err(format!("git {}: stopped at its limit", args.join(" "))),
        Err(error) => Err(format!("git {}: {error}", args.join(" "))),
    }
}

/// Run Git in `dir` with `input` as its stdin, bounded by the general Git
/// limit, and return stdout.
pub(super) fn git_bytes_with_input(
    dir: &Path,
    args: &[&str],
    input: &Path,
) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(input)
        .map_err(|error| format!("cannot open {}: {error}", input.display()))?;
    let mut command = crate::git::git_command();
    command.args(args).current_dir(dir);
    match crate::bounded_output::output_with_input_limited(
        &mut command,
        file,
        crate::git::git_timeout(),
        64 * 1024 * 1024,
    ) {
        Ok(crate::bounded_output::LimitedOutput::Completed(output)) if output.status.success() => {
            Ok(output.stdout)
        }
        Ok(crate::bounded_output::LimitedOutput::Completed(output)) => Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Ok(_) => Err(format!("git {}: stopped at its limit", args.join(" "))),
        Err(error) => Err(format!("git {}: {error}", args.join(" "))),
    }
}
