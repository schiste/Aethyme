//! Retirement staging an earlier state of the same checkout left behind.
//!
//! A staging directory with no manifest is not an archive: nothing restores
//! from it and nothing expires it. Yet a file the checkout has since lost may
//! survive only there, so it is removed only when every byte it holds is proven
//! to be in the checkout as well, at a path the reviewed plan archives.
//! Otherwise it is left exactly as it is, and the new archive is written in
//! the one alternate staging place. There are never more than two per session.

use super::*;

/// The staging places of one session's retirement archive, in order of use.
pub(super) fn retirement_staging_names(session_id: i64) -> [String; 2] {
    [
        format!("{session_id}.retirement-v6.partial"),
        format!("{session_id}.retirement-v6-next.partial"),
    ]
}

enum Staging {
    /// Absent, or for the apply itself to verify, restart or refuse.
    Use,
    /// An earlier state's staging that holds something not proven elsewhere.
    Kept(String),
}

/// The staging name this apply uses, after removing earlier staging of the
/// same checkout that holds nothing the checkout does not. The caller holds
/// the session's retirement lock, so nothing is writing any of it.
pub(super) fn settle_retirement_staging(
    repository: &GitRepo,
    archive_root: &Path,
    plan: &CleanupResolvePlan,
    deadline: Option<Instant>,
) -> Result<String, String> {
    let mut kept = Vec::new();
    for name in retirement_staging_names(plan.session_id) {
        match settle_one(repository, &archive_root.join(&name), plan, deadline)? {
            Staging::Use => return Ok(name),
            Staging::Kept(reason) => kept.push(format!("{name}: {reason}")),
        }
    }
    Err(format!(
        "staging of earlier states of this checkout was retained in both staging places: {}",
        kept.join("; ")
    ))
}

fn settle_one(
    repository: &GitRepo,
    partial: &Path,
    plan: &CleanupResolvePlan,
    deadline: Option<Instant>,
) -> Result<Staging, String> {
    let Ok(metadata) = std::fs::symlink_metadata(partial) else {
        return Ok(Staging::Use);
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(Staging::Use);
    }
    // Never the recursive removal: this succeeds only on an empty directory,
    // which a stop between creating it and writing its record leaves.
    if std::fs::remove_dir(partial).is_ok() {
        count("recovery_archive_stale_partial_removed");
        return Ok(Staging::Use);
    }
    let Ok((record, _)) = read_retirement_archive_record(partial) else {
        return Ok(Staging::Use);
    };
    if record.schema_version != PINNED_RETIREMENT_ARCHIVE_RECORD_SCHEMA_VERSION
        || record.session_id != plan.session_id
        || record.worktree_path != plan.worktree_path
        || record.branch_ref != plan.branch_ref
        || verify_retirement_archive_record_for_content_identity(partial, plan).is_ok()
    {
        return Ok(Staging::Use);
    }

    let _span = crate::phase_telemetry::span("recovery_archive_apply_stale_partial");
    let proven = prove_partial_held_by_checkout(partial, plan, deadline).and_then(|files| {
        let git_deadline = recovery_bundle_deadline()?;
        let git_deadline = effective_archive_deadline(deadline)
            .map_or(git_deadline, |deadline| deadline.min(git_deadline));
        require_no_recovery_prerequisite_refs_for_partial(repository, partial, git_deadline)?;
        Ok(files)
    });
    // Running out of time proves nothing either way: stop here rather than
    // leave a second staging directory the next attempt would not need.
    check_archive_deadline(deadline, "earlier staging proof")?;
    ensure_auto_retirement_staging_reserve(deadline, plan.session_id)
        .map_err(|error| error.to_string())?;
    match proven.and_then(|files| remove_proven_partial(partial, &files, deadline)) {
        Ok(()) => {
            count("recovery_archive_stale_partial_removed");
            Ok(Staging::Use)
        }
        Err(reason) => {
            check_archive_deadline(deadline, "earlier staging removal")?;
            count("recovery_archive_stale_partial_kept");
            Ok(Staging::Kept(reason))
        }
    }
}

fn count(name: &'static str) {
    crate::phase_telemetry::add(name, Duration::ZERO);
}

/// Every file of `partial`, once each is proven to hold nothing the checkout
/// does not hold at a path `plan` archives. A manifest, a commit bundle, or
/// anything else the checkout's files cannot be compared with refuses.
pub(super) fn prove_partial_held_by_checkout(
    partial: &Path,
    plan: &CleanupResolvePlan,
    deadline: Option<Instant>,
) -> Result<std::collections::BTreeSet<PathBuf>, String> {
    let by_path = |files: &[ArchivedFile]| {
        files
            .iter()
            .map(|file| (file.path.clone(), file.symlink))
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let untracked = by_path(&plan.untracked_files);
    let ignored = by_path(&plan.ignored_files);
    let worktree = Path::new(&plan.worktree_path);
    let files = recovery_archive_file_set_until(partial, deadline)?;
    for relative in &files {
        check_archive_deadline(deadline, "earlier staging proof")?;
        let text = relative
            .to_str()
            .ok_or_else(|| "it holds a non-UTF-8 name".to_owned())?;
        let copy = partial.join(relative);
        match text {
            RETIREMENT_ARCHIVE_RECORD_FILE => continue,
            STAGED_PATCH_FILE | UNSTAGED_PATCH_FILE => {
                let (bytes, sha256) = if text == STAGED_PATCH_FILE {
                    (plan.staged_patch_bytes, &plan.staged_patch_sha256)
                } else {
                    (plan.unstaged_patch_bytes, &plan.unstaged_patch_sha256)
                };
                let held = describe_untracked_until(&copy, text, deadline)
                    .map_err(|error| format!("cannot read its {text}: {error}"))?;
                if !held.is_some_and(|held| {
                    !held.symlink && held.bytes == bytes && held.sha256 == *sha256
                }) {
                    return Err(format!("its {text} is not the checkout's current one"));
                }
                continue;
            }
            _ => {}
        }
        let (path, planned) = match text.split_once('/') {
            Some((UNTRACKED_DIRECTORY, path)) => (path, untracked.get(path)),
            Some((IGNORED_DIRECTORY, path)) => (path, ignored.get(path)),
            _ => {
                return Err(format!(
                    "it holds {text}, which cannot be compared with the checkout"
                ));
            }
        };
        let Some(symlink) = planned else {
            return Err(format!(
                "it holds {text}, which the current plan does not archive"
            ));
        };
        prove_copy_held(&copy, worktree, path, *symlink, deadline)
            .map_err(|reason| format!("its {text} {reason}"))?;
    }
    Ok(files)
}

/// Whether the checkout's file at `path` starts with everything `copy` holds
/// (a copy cut short is a prefix of its source; a source that only grew still
/// starts with it). A link must be the same link. Follows no link.
fn prove_copy_held(
    copy: &Path,
    worktree: &Path,
    path: &str,
    symlink: bool,
    deadline: Option<Instant>,
) -> Result<(), String> {
    if !is_contained_relative(path) {
        return Err("is not a contained path".into());
    }
    // A parent that became a link would compare with a file outside the
    // checkout.
    let mut parent = worktree.to_path_buf();
    require_real_directory(&parent)?;
    let mut components = path.split('/').peekable();
    while let Some(component) = components.next() {
        if components.peek().is_some() {
            parent.push(component);
            require_real_directory(&parent)?;
        }
    }
    let source = worktree.join(path);
    if symlink {
        let held = std::fs::read_link(copy)
            .map_err(|error| format!("cannot be read as a link: {error}"))?;
        let current = std::fs::read_link(&source)
            .map_err(|error| format!("cannot be read as a link in the checkout: {error}"))?;
        return if held == current {
            Ok(())
        } else {
            Err("is another link in the checkout".into())
        };
    }
    let (mut copy_file, _) =
        open_regular_file_no_follow(copy).map_err(|error| format!("cannot be read: {error}"))?;
    let (mut source_file, _) = open_regular_file_no_follow(&source)
        .map_err(|error| format!("cannot be read in the checkout: {error}"))?;
    let mut held = [0_u8; 64 * 1024];
    let mut current = [0_u8; 64 * 1024];
    loop {
        check_archive_deadline(deadline, "earlier staging proof")?;
        let read = copy_file
            .read(&mut held)
            .map_err(|error| format!("cannot be read: {error}"))?;
        if read == 0 {
            return Ok(());
        }
        source_file
            .read_exact(&mut current[..read])
            .map_err(|error| format!("is not wholly in the checkout: {error}"))?;
        if held[..read] != current[..read] {
            return Err("differs from the checkout".into());
        }
    }
}

/// Remove exactly the proven files, then the directories they leave empty.
/// The record goes last, so a removal that stops can be proven and resumed; a
/// file nobody proved makes a directory removal fail and stays.
fn remove_proven_partial(
    partial: &Path,
    files: &std::collections::BTreeSet<PathBuf>,
    deadline: Option<Instant>,
) -> Result<(), String> {
    let record = Path::new(RETIREMENT_ARCHIVE_RECORD_FILE);
    let failed = |path: &Path, error: std::io::Error| {
        format!("cannot remove proven {}: {error}", path.display())
    };
    for relative in files.iter().filter(|relative| relative.as_path() != record) {
        check_archive_deadline(deadline, "earlier staging removal")?;
        let path = partial.join(relative);
        std::fs::remove_file(&path).map_err(|error| failed(&path, error))?;
    }
    let mut directories = Vec::new();
    let mut pending = vec![partial.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).map_err(|error| failed(&directory, error))? {
            let entry = entry.map_err(|error| failed(&directory, error))?;
            if entry
                .file_type()
                .map_err(|error| failed(&entry.path(), error))?
                .is_dir()
            {
                directories.push(entry.path());
                pending.push(entry.path());
            }
        }
    }
    // A parent is listed before its children.
    for directory in directories.iter().rev() {
        check_archive_deadline(deadline, "earlier staging removal")?;
        std::fs::remove_dir(directory).map_err(|error| failed(directory, error))?;
    }
    let entries = std::fs::read_dir(partial)
        .map_err(|error| failed(partial, error))?
        .count();
    if entries != 1 {
        return Err("it gained a file while it was removed".into());
    }
    let record = partial.join(record);
    std::fs::remove_file(&record).map_err(|error| failed(&record, error))?;
    std::fs::remove_dir(partial).map_err(|error| failed(partial, error))
}
