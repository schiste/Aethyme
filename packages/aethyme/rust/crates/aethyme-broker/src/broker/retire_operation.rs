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
//! - `manifest.json` (size and SHA-256 of every file) and `README.md` (how to
//!   restore).
//!
//! Every copy is read back and compared before the archive is renamed into
//! place; the commits the state names are returned for the caller to pin.

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
/// Files at most, across the archive.
const MAX_OPERATION_ARCHIVE_FILES: usize = 50_000;
/// Directory depth at most, inside a copied state directory.
const MAX_OPERATION_STATE_DEPTH: usize = 32;

/// How long ago the checkout's in-progress operation was last touched (ms):
/// the newest operation marker of its Git directory, or the index when only
/// unmerged entries remain. `None` when that cannot be read.
pub(super) fn operation_state_age_ms(worktree: &Path) -> Option<i64> {
    let git_dir = crate::git::checked_worktree_git_dir(worktree)
        .ok()
        .flatten()?;
    let newest = OPERATION_MARKERS
        .iter()
        .filter_map(|name| std::fs::symlink_metadata(git_dir.join(name)).ok())
        .filter_map(|metadata| metadata.modified().ok())
        .max()
        .or_else(|| {
            std::fs::metadata(git_dir.join("index"))
                .and_then(|metadata| metadata.modified())
                .ok()
        })?;
    let age = std::time::SystemTime::now()
        .duration_since(newest)
        .unwrap_or_default();
    i64::try_from(age.as_millis()).ok()
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
    /// Commits the state names (HEAD included), to pin before the move.
    pub commits: Vec<String>,
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
    let commits = named_commits(request.worktree, &git_dir, &head).map_err(refuse)?;

    std::fs::create_dir_all(request.archive_root)
        .map_err(|error| refuse(format!("cannot create the archive root: {error}")))?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let name = format!("{}-operation-{}-{nonce:x}", request.session_id, &head[..12]);
    let partial = request.archive_root.join(format!("{name}.partial"));
    std::fs::create_dir(&partial)
        .map_err(|error| refuse(format!("cannot create {}: {error}", partial.display())))?;
    let written = fill_archive(
        request, &git_dir, &head, &changed, &ignored, &commits, &partial,
    );
    let (bytes, _files) = match written {
        Ok(written) => written,
        Err(error) => {
            remove_partial(&partial);
            return Err(error);
        }
    };
    let archive = request.archive_root.join(&name);
    if let Err(error) = std::fs::rename(&partial, &archive) {
        remove_partial(&partial);
        return Err(refuse(format!(
            "cannot move the verified archive into place: {error}"
        )));
    }
    // The archive is in place; a root that cannot be synced is only
    // counted (the archive's own files and directories were synced).
    if std::fs::File::open(request.archive_root)
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
    })
}

/// Copy everything into `partial`, verify it, and write the manifest and
/// README. Returns the payload bytes and file count.
#[allow(clippy::too_many_arguments)]
fn fill_archive(
    request: &OperationArchiveRequest<'_>,
    git_dir: &Path,
    head: &str,
    changed: &[String],
    ignored: &[crate::cleanup_resolve::ArchivedFile],
    commits: &[String],
    partial: &Path,
) -> Result<(u64, usize), OperationArchiveError> {
    let refuse = OperationArchiveError::Refused;
    let mut budget = Budget {
        bytes: 0,
        files: 0,
        max_bytes: request.max_bytes,
        deadline: request.deadline,
    };
    let mut state = Vec::new();
    for name in OPERATION_STATE_FILES {
        let source = git_dir.join(name);
        match std::fs::symlink_metadata(&source) {
            Ok(metadata) if metadata.is_file() => {
                state.push(copy_entry(
                    &source,
                    &partial.join("git-state"),
                    name,
                    &mut budget,
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
                    &mut budget,
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
                    &mut budget,
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
            &mut budget,
        )?;
        if copied.sha256 != file.sha256 {
            return Err(refuse(format!(
                "ignored {} changed while it was archived",
                file.path
            )));
        }
        ignored_files.push(copied);
    }
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
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| refuse(format!("cannot write the manifest: {error}")))?;
    write_synced(&partial.join("manifest.json"), &manifest_bytes).map_err(refuse)?;
    write_synced(&partial.join("README.md"), readme(request, head).as_bytes()).map_err(refuse)?;
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
    for directory in ["git-state", "worktree", "ignored"] {
        let path = partial.join(directory);
        if path.is_dir() {
            sync_directory(&path).map_err(refuse)?;
        }
    }
    sync_directory(partial).map_err(refuse)?;
    Ok((budget.bytes, budget.files))
}

/// Bytes and files so far, and the bounds.
struct Budget {
    bytes: u64,
    files: usize,
    max_bytes: u64,
    deadline: Instant,
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
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
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
    std::fs::set_permissions(&target, metadata.permissions())
        .map_err(|error| refuse(format!("cannot keep the mode of {relative}: {error}")))?;
    Ok(ArchivedEntry {
        path: relative.to_string(),
        bytes,
        sha256: format!("{:x}", sha2::Digest::finalize(hasher)),
        symlink: false,
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

fn readme(request: &OperationArchiveRequest<'_>, head: &str) -> String {
    format!(
        "# Abandoned Git operation of session {session}\n\n\
         The checkout `{path}` (branch `{branch}`, HEAD `{head}`) was closed with a merge, \
         rebase, cherry-pick, revert or bisect still in progress. Its operation state, raw \
         index, changed and untracked files and protected ignored files were archived here, \
         verified, and the checkout was retired. The commits the state names are pinned \
         under `refs/aethyme/retired/{session}/`.\n\n\
         To restore it:\n\n\
         ```sh\n\
         git worktree add --detach {path:?} {head}\n\
         cp -pR <this-archive>/worktree/. {path:?}/\n\
         cp -pR <this-archive>/ignored/. {path:?}/\n\
         cp -pR <this-archive>/git-state/. \"$(git -C {path:?} rev-parse --absolute-git-dir)\"/\n\
         ```\n\n\
         `git-state/HEAD` names the branch the checkout was on; paths listed under \
         `deleted` in `manifest.json` were missing from the checkout. `manifest.json` \
         records the size and SHA-256 of every archived file.\n",
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
